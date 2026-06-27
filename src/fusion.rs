//! 순위 융합 + 2차 신호. 이종 소스의 점수는 비교 불가하므로 점수를 더하지 않고
//! RRF(Reciprocal Rank Fusion, 순위 융합)로 1차 정렬한다. 그 위에 신선도·출처 신뢰도를
//! 2차 가중·타이브레이커로 결합한다. 같은 입력 결과 집합은 항상 같은 순위(결정적).

use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::dedup::{identity_key, merge_docs};
use crate::model::{Document, SourceKind};

/// RRF 관례 상수.
pub const DEFAULT_RRF_K: f64 = 60.0;

/// 소스별 순서가 보존된 결과 묶음을 받아 RRF 융합 + 정체성 병합한 뒤, 2차 신호로 정렬한다.
///
/// 같은 문서가 여러 소스에 등장하면 각 소스에서의 순위 기여가 합산되어 위로 올라간다.
pub fn fuse(per_source: Vec<Vec<Document>>, k: f64) -> Vec<Document> {
    let now = Utc::now();
    let mut map: HashMap<String, Document> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for list in per_source {
        for (rank, mut doc) in list.into_iter().enumerate() {
            let contrib = 1.0 / (k + rank as f64 + 1.0);
            let key = identity_key(&doc);
            if let Some(existing) = map.get_mut(&key) {
                existing.score.fused += contrib;
                merge_docs(existing, doc);
            } else {
                doc.score.fused = contrib;
                map.insert(key.clone(), doc);
                order.push(key);
            }
        }
    }

    let mut docs: Vec<Document> = order.into_iter().filter_map(|k| map.remove(&k)).collect();
    for d in &mut docs {
        d.score.freshness = freshness_score(d.published_at, now);
        // 어댑터가 피드별 reliability 로 미리 authority 를 설정했으면 존중, 아니면 SourceKind 기본값.
        if d.score.authority == 0.0 {
            d.score.authority = authority_score(d.source);
        }
    }

    docs.sort_by(|a, b| {
        cmp_desc(a.score.fused, b.score.fused)
            .then_with(|| cmp_desc(a.score.freshness, b.score.freshness))
            .then_with(|| b.published_at.cmp(&a.published_at))
    });
    docs
}

/// 신선도 점수 — 오늘 1.0 / 3일 0.8 / 7일 0.6 / 30일 0.3 / 그 이상 0.1 / 날짜 없음 0.0.
pub fn freshness_score(published: Option<DateTime<Utc>>, now: DateTime<Utc>) -> f64 {
    match published {
        None => 0.0,
        Some(dt) => {
            let days = now.signed_duration_since(dt).num_days();
            if days <= 0 {
                1.0
            } else if days <= 3 {
                0.8
            } else if days <= 7 {
                0.6
            } else if days <= 30 {
                0.3
            } else {
                0.1
            }
        }
    }
}

/// 출처 신뢰도 점수(소스 종류 단위). 후속 Phase 에서 피드별 reliability 로 세분화 가능.
pub fn authority_score(source: SourceKind) -> f64 {
    match source {
        SourceKind::Arxiv => 0.90,
        SourceKind::Blog => 0.85,
        SourceKind::News => 0.80,
        SourceKind::Web => 0.55,
        SourceKind::Youtube => 0.45,
    }
}

fn cmp_desc(a: f64, b: f64) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DocIdentity, Document, Score};
    use std::collections::HashMap;

    fn doc(arxiv: &str, source: SourceKind) -> Document {
        Document {
            identity: DocIdentity {
                doi: None,
                arxiv_id: Some(arxiv.to_string()),
                canonical_url: None,
                title_hash: 0,
            },
            source,
            title: format!("title {arxiv}"),
            url: format!("http://arxiv.org/abs/{arxiv}"),
            authors: Vec::new(),
            published_at: None,
            fetched_at: Utc::now(),
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
    fn rrf_boosts_multi_source_docs_and_merges() {
        let a = vec![doc("1", SourceKind::Arxiv), doc("2", SourceKind::Arxiv)];
        let b = vec![doc("1", SourceKind::Blog), doc("3", SourceKind::Blog)];
        let out = fuse(vec![a, b], DEFAULT_RRF_K);

        // 1 은 두 소스에 모두 등장 -> 병합되어 한 번만, 그리고 최상위.
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].identity.arxiv_id.as_deref(), Some("1"));
        assert!(out[0].sources.contains(&SourceKind::Arxiv));
        assert!(out[0].sources.contains(&SourceKind::Blog));
        // 1 의 융합 점수는 단일 소스 문서보다 크다.
        assert!(out[0].score.fused > out[1].score.fused);
    }
}
