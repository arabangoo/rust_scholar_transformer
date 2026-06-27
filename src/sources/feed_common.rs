//! RSS/Atom/JSON Feed 공용 파싱·정규화 (feed-rs). 블로그·뉴스 어댑터가 공유한다.

use std::collections::HashMap;

use chrono::Utc;
use feed_rs::parser;

use crate::error::FetchError;
use crate::model::{Author, DocIdentity, Document, Score, SourceKind};
use crate::normalize::{canonicalize_url, normalize_title, strip_html, title_hash};

/// 피드 바이트를 파싱해 [`Document`] 목록으로 정규화한다.
///
/// - `filter_text` 가 Some 이고 비어 있지 않으면, 제목+요약에 어느 한 단어라도 포함된 항목만
///   남긴다(구독형 블로그 피드의 키워드 필터). 검색형 피드(예: Google News RSS)는 None 을 넘겨
///   상류 검색 결과를 그대로 신뢰한다.
pub fn feed_to_docs(
    bytes: &[u8],
    source: SourceKind,
    language: Option<String>,
    filter_text: Option<&str>,
    reliability: f64,
) -> Result<Vec<Document>, FetchError> {
    let feed = parser::parse(bytes).map_err(|e| FetchError::Parse(e.to_string()))?;
    let now = Utc::now();

    let terms: Vec<String> = match filter_text {
        Some(t) if !t.trim().is_empty() => t.split_whitespace().map(|w| w.to_lowercase()).collect(),
        _ => Vec::new(),
    };

    let mut docs = Vec::new();
    for entry in feed.entries {
        let title = entry.title.map(|t| normalize_title(&t.content)).unwrap_or_default();
        let summary = entry
            .summary
            .map(|t| t.content)
            .or_else(|| entry.content.and_then(|c| c.body))
            .map(|s| strip_html(&s));

        if !terms.is_empty() {
            let hay = format!("{} {}", title, summary.clone().unwrap_or_default()).to_lowercase();
            if !terms.iter().any(|t| hay.contains(t.as_str())) {
                continue;
            }
        }

        let url = entry
            .links
            .into_iter()
            .next()
            .map(|l| l.href)
            .unwrap_or_else(|| entry.id.clone());
        let published_at = entry.published.or(entry.updated);
        let authors = entry
            .authors
            .into_iter()
            .filter(|p| !p.name.trim().is_empty())
            .map(|p| Author::new(normalize_title(&p.name)))
            .collect();
        let canonical_url = canonicalize_url(&url);

        docs.push(Document {
            identity: DocIdentity {
                doi: None,
                arxiv_id: None,
                canonical_url,
                title_hash: title_hash(&title),
            },
            source,
            title,
            url,
            authors,
            published_at,
            fetched_at: now,
            summary,
            content: None,
            language: language.clone(),
            tags: Vec::new(),
            sources: vec![source],
            score: {
                // 피드별 reliability 가 주어지면 authority 로 미리 설정(융합에서 SourceKind 기본값 대신 사용).
                let mut s = Score::default();
                if reliability > 0.0 {
                    s.authority = reliability;
                }
                s
            },
            extra: HashMap::new(),
        });
    }
    Ok(docs)
}
