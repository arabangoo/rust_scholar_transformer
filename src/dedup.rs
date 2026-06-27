//! 정체성 기반 중복 제거. 같은 문서가 여러 소스에 중복 인덱싱되거나 신디케이션된 경우를
//! 식별자 우선순위로 묶어 병합한다. 근접중복(MinHash-LSH)은 후속 Phase 에서 이 위에 얹는다.

use std::collections::HashMap;

use crate::model::Document;

/// 문서 정체성 키 — 우선순위: DOI -> arXiv ID -> canonical URL -> 제목 해시.
pub fn identity_key(d: &Document) -> String {
    if let Some(doi) = &d.identity.doi {
        return format!("doi:{}", doi.to_lowercase());
    }
    if let Some(ax) = &d.identity.arxiv_id {
        return format!("arxiv:{ax}");
    }
    if let Some(u) = &d.identity.canonical_url {
        return format!("url:{}", u.to_lowercase());
    }
    format!("title:{}", d.identity.title_hash)
}

/// 중복 문서를 base 로 병합한다. 비어 있는 필드를 채우고, 기여 소스를 누적하고, 원본을 보존한다.
pub fn merge_docs(base: &mut Document, other: Document) {
    if base.summary.is_none() {
        base.summary = other.summary;
    }
    if base.content.is_none() {
        base.content = other.content;
    }
    if base.published_at.is_none() {
        base.published_at = other.published_at;
    }
    if base.authors.is_empty() {
        base.authors = other.authors;
    }
    if base.identity.doi.is_none() {
        base.identity.doi = other.identity.doi;
    }
    if base.identity.arxiv_id.is_none() {
        base.identity.arxiv_id = other.identity.arxiv_id;
    }
    if base.identity.canonical_url.is_none() {
        base.identity.canonical_url = other.identity.canonical_url;
    }
    for s in other.sources {
        if !base.sources.contains(&s) {
            base.sources.push(s);
        }
    }
    for (k, v) in other.extra {
        base.extra.entry(k).or_insert(v);
    }
}

/// 정체성 키로 중복을 병합한다. 처음 등장 순서를 보존한다(순위는 호출자/융합이 별도로 정한다).
pub fn dedup(docs: Vec<Document>) -> Vec<Document> {
    let mut map: HashMap<String, Document> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for doc in docs {
        let key = identity_key(&doc);
        if let Some(existing) = map.get_mut(&key) {
            merge_docs(existing, doc);
        } else {
            map.insert(key.clone(), doc);
            order.push(key);
        }
    }
    order.into_iter().filter_map(|k| map.remove(&k)).collect()
}

const MINHASH_N: usize = 64;
const LSH_BANDS: usize = 16;

/// MinHash-LSH 근접중복 병합. 정체성 식별자가 달라 정확 중복제거가 놓친 near-dup
/// (제목·요약이 미세하게 다른 preprint vs 게재본 등)을 제목+요약 shingle 의 MinHash 유사도로
/// 잡아 병합한다. LSH 밴딩으로 후보 쌍만 추려 전수 비교를 피한다. 상위 순위(작은 인덱스)를 base 로 유지.
pub fn near_dedup(docs: Vec<Document>, threshold: f64) -> Vec<Document> {
    let n = docs.len();
    if n < 2 {
        return docs;
    }
    let sigs: Vec<[u64; MINHASH_N]> = docs.iter().map(minhash_sig).collect();

    let rows = MINHASH_N / LSH_BANDS;
    let mut buckets: HashMap<(usize, u64), Vec<usize>> = HashMap::new();
    for (i, sig) in sigs.iter().enumerate() {
        for b in 0..LSH_BANDS {
            let band = &sig[b * rows..(b + 1) * rows];
            let mut bytes = Vec::with_capacity(rows * 8);
            for v in band {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            buckets.entry((b, crate::normalize::fnv1a_64(&bytes))).or_default().push(i);
        }
    }

    let mut uf = UnionFind::new(n);
    for idxs in buckets.values() {
        for a in 0..idxs.len() {
            for c in (a + 1)..idxs.len() {
                let (x, y) = (idxs[a], idxs[c]);
                if uf.find(x) != uf.find(y) && sig_similarity(&sigs[x], &sigs[y]) >= threshold {
                    uf.union(x, y);
                }
            }
        }
    }

    let roots: Vec<usize> = (0..n).map(|i| uf.find(i)).collect();
    let mut base_of: HashMap<usize, usize> = HashMap::new();
    for (i, &r) in roots.iter().enumerate() {
        base_of.entry(r).and_modify(|b| {
            if i < *b {
                *b = i
            }
        }).or_insert(i);
    }
    let mut slots: Vec<Option<Document>> = docs.into_iter().map(Some).collect();
    for i in 0..n {
        let base = base_of[&roots[i]];
        if i != base {
            if let Some(other) = slots[i].take() {
                if let Some(b) = slots[base].as_mut() {
                    merge_docs(b, other);
                }
            }
        }
    }
    slots.into_iter().flatten().collect()
}

fn shingles(d: &Document) -> Vec<String> {
    let mut text = d.title.clone();
    if let Some(s) = &d.summary {
        text.push(' ');
        text.push_str(s);
    }
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect();
    if words.len() < 2 {
        return words;
    }
    words.windows(2).map(|w| format!("{} {}", w[0], w[1])).collect()
}

fn minhash_sig(d: &Document) -> [u64; MINHASH_N] {
    let sh = shingles(d);
    if sh.is_empty() {
        // shingle 이 없으면 URL 로 고유 시그니처를 만들어 서로 충돌하지 않게 한다.
        let seed = crate::normalize::fnv1a_64(d.url.as_bytes());
        let mut sig = [0u64; MINHASH_N];
        for (i, slot) in sig.iter_mut().enumerate() {
            *slot = seed.wrapping_add(i as u64);
        }
        return sig;
    }
    let mut sig = [u64::MAX; MINHASH_N];
    for s in &sh {
        let base = crate::normalize::fnv1a_64(s.as_bytes());
        for (i, slot) in sig.iter_mut().enumerate() {
            let a = (i as u64).wrapping_mul(0x9E3779B97F4A7C15) | 1;
            let b = (i as u64).wrapping_mul(0xBF58476D1CE4E5B9).wrapping_add(0x2B);
            let h = base.wrapping_mul(a).wrapping_add(b);
            if h < *slot {
                *slot = h;
            }
        }
    }
    sig
}

fn sig_similarity(a: &[u64; MINHASH_N], b: &[u64; MINHASH_N]) -> f64 {
    let eq = a.iter().zip(b.iter()).filter(|(x, y)| x == y).count();
    eq as f64 / MINHASH_N as f64
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self { parent: (0..n).collect() }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.parent[r] != r {
            r = self.parent[r];
        }
        let mut cur = x;
        while self.parent[cur] != r {
            let next = self.parent[cur];
            self.parent[cur] = r;
            cur = next;
        }
        r
    }
    fn union(&mut self, x: usize, y: usize) {
        let (rx, ry) = (self.find(x), self.find(y));
        if rx != ry {
            self.parent[rx] = ry;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DocIdentity, Document, Score, SourceKind};
    use std::collections::HashMap;

    fn make_doc(arxiv: Option<&str>, url: Option<&str>, title_hash: u64, source: SourceKind) -> Document {
        Document {
            identity: DocIdentity {
                doi: None,
                arxiv_id: arxiv.map(|s| s.to_string()),
                canonical_url: url.map(|s| s.to_string()),
                title_hash,
            },
            source,
            title: "t".to_string(),
            url: url.unwrap_or("http://x").to_string(),
            authors: Vec::new(),
            published_at: None,
            fetched_at: chrono::Utc::now(),
            summary: None,
            content: None,
            language: None,
            tags: Vec::new(),
            sources: vec![source],
            score: Score::default(),
            extra: HashMap::new(),
        }
    }

    #[test]
    fn merges_same_arxiv_id_across_sources() {
        let docs = vec![
            make_doc(Some("2301.00001"), None, 1, SourceKind::Arxiv),
            make_doc(Some("2301.00001"), None, 1, SourceKind::Blog),
        ];
        let out = dedup(docs);
        assert_eq!(out.len(), 1);
        assert!(out[0].sources.contains(&SourceKind::Arxiv));
        assert!(out[0].sources.contains(&SourceKind::Blog));
    }

    #[test]
    fn merges_same_canonical_url() {
        let docs = vec![
            make_doc(None, Some("https://e.com/a"), 1, SourceKind::News),
            make_doc(None, Some("https://e.com/a"), 2, SourceKind::Blog),
        ];
        assert_eq!(dedup(docs).len(), 1);
    }

    #[test]
    fn keeps_distinct_documents() {
        let docs = vec![
            make_doc(Some("2301.00001"), None, 1, SourceKind::Arxiv),
            make_doc(Some("2302.99999"), None, 2, SourceKind::Arxiv),
        ];
        assert_eq!(dedup(docs).len(), 2);
    }

    fn make_titled(title: &str, source: SourceKind) -> Document {
        let mut d = make_doc(None, Some(&format!("http://x/{}", title.replace(' ', "-"))), 0, source);
        d.title = title.to_string();
        d
    }

    #[test]
    fn near_dedup_merges_similar_titles() {
        // 정체성 식별자가 다르고 제목 해시도 다르지만(서로 다른 제목), 의미상 거의 같은 두 문서.
        let docs = vec![
            make_titled("alpha beta gamma delta", SourceKind::Arxiv),
            make_titled("alpha beta gamma delta epsilon", SourceKind::Blog),
        ];
        let out = near_dedup(docs, 0.5);
        assert_eq!(out.len(), 1);
        assert!(out[0].sources.contains(&SourceKind::Arxiv));
        assert!(out[0].sources.contains(&SourceKind::Blog));
    }

    #[test]
    fn near_dedup_keeps_unrelated_titles() {
        let docs = vec![
            make_titled("alpha beta gamma delta", SourceKind::Arxiv),
            make_titled("completely different words here", SourceKind::Blog),
        ];
        assert_eq!(near_dedup(docs, 0.5).len(), 2);
    }
}
