//! arXiv OAI-PMH 어댑터 — 설계상 1순위 경로. 라이브 검색 API 와 달리 rate-limit 노출이 없어
//! 공유 IP 차단(HTTP 429) 문제를 피한다. OAI-PMH 는 키워드 검색이 아니라 날짜 기반 수확이므로,
//! 최근 N일 레코드를 ListRecords 로 가져와(`from=`) 메모리에서 키워드 필터링한다.
//!
//! 한계(정직히): 한 번에 첫 페이지만 가져온다(resumptionToken 페이지네이션 미추적). 대규모
//! 키워드 검색은 여러 페이지를 로컬 인덱스로 수확해야 하며, 그 인덱스 레이어는 후속 작업이다.

use chrono::{Duration as ChronoDuration, Utc};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::error::FetchError;
use crate::model::{Author, DocIdentity, Document, Score, SearchQuery, SourceKind};
use crate::normalize::{canonicalize_url, normalize_title, parse_datetime, strip_html, title_hash};
use crate::source::{RatePolicy, Source};

const DEFAULT_BASE_URL: &str = "https://oaipmh.arxiv.org/oai";
const OAI_USER_AGENT: &str =
    "rust_scholar_transformer/0.1 (+https://github.com/arabangoo/rust_scholar_transformer)";

/// arXiv OAI-PMH 수확 어댑터.
pub struct ArxivOaiSource {
    client: reqwest::Client,
    base_url: String,
    /// 최근 며칠치 레코드를 수확할지(ListRecords `from=`).
    from_days: i64,
    /// OAI set 한정(예: "cs"). None 이면 전체.
    set: Option<String>,
    language: Option<String>,
}

impl ArxivOaiSource {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
            from_days: 7,
            set: None,
            language: Some("en".to_string()),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_from_days(mut self, days: i64) -> Self {
        self.from_days = days;
        self
    }

    pub fn with_set(mut self, set: impl Into<String>) -> Self {
        self.set = Some(set.into());
        self
    }
}

impl Default for ArxivOaiSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Source for ArxivOaiSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Arxiv
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError> {
        let from = (Utc::now() - ChronoDuration::days(self.from_days.max(0)))
            .format("%Y-%m-%d")
            .to_string();
        let mut params: Vec<(&str, &str)> = vec![
            ("verb", "ListRecords"),
            ("metadataPrefix", "oai_dc"),
            ("from", from.as_str()),
        ];
        if let Some(set) = &self.set {
            params.push(("set", set.as_str()));
        }

        let resp = self
            .client
            .get(&self.base_url)
            .header(reqwest::header::USER_AGENT, OAI_USER_AGENT)
            .query(&params)
            .send()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(FetchError::Http(format!("arXiv OAI HTTP {}", resp.status().as_u16())));
        }
        let xml = resp.text().await.map_err(|e| FetchError::Http(e.to_string()))?;

        let records = parse_oai(&xml)?;
        let terms: Vec<String> =
            query.text.split_whitespace().map(|w| w.to_lowercase()).collect();

        let mut docs = Vec::new();
        for r in records {
            let title = normalize_title(&r.title);
            let summary = if r.description.is_empty() { None } else { Some(strip_html(&r.description)) };

            if !terms.is_empty() {
                let hay = format!("{} {}", title, summary.clone().unwrap_or_default()).to_lowercase();
                if !terms.iter().any(|t| hay.contains(t.as_str())) {
                    continue;
                }
            }

            let arxiv_id = arxiv_id_from_url(&r.identifier);
            docs.push(Document {
                identity: DocIdentity {
                    doi: None,
                    arxiv_id,
                    canonical_url: canonicalize_url(&r.identifier),
                    title_hash: title_hash(&title),
                },
                source: SourceKind::Arxiv,
                title,
                url: r.identifier,
                authors: r.creators.into_iter().map(|c| Author::new(normalize_title(&c))).collect(),
                published_at: parse_datetime(&r.date),
                fetched_at: Utc::now(),
                summary,
                content: None,
                language: self.language.clone(),
                tags: Vec::new(),
                sources: vec![SourceKind::Arxiv],
                score: Score::default(),
                extra: std::collections::HashMap::new(),
            });
        }
        Ok(docs)
    }

    fn rate_policy(&self) -> RatePolicy {
        // OAI-PMH 는 라이브 API 같은 엄격한 rate limit 이 없다. 공손하게 약간의 간격만.
        RatePolicy { min_interval_ms: 500, max_concurrency: 1, daily_quota: None }
    }
}

#[derive(Default)]
struct OaiRecord {
    title: String,
    creators: Vec<String>,
    description: String,
    date: String,
    identifier: String,
}

/// 네임스페이스 프리픽스에 견고하도록 quick-xml 이벤트를 직접 순회해 로컬 이름으로 필드를 잡는다.
fn parse_oai(xml: &str) -> Result<Vec<OaiRecord>, FetchError> {
    let mut reader = Reader::from_str(xml);
    let mut records = Vec::new();
    let mut cur: Option<OaiRecord> = None;
    let mut text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if local_name(e.name().as_ref()) == "record" {
                    cur = Some(OaiRecord::default());
                }
                text.clear();
            }
            Ok(Event::Text(t)) => {
                text.push_str(&t.unescape().map(|c| c.into_owned()).unwrap_or_default());
            }
            Ok(Event::CData(t)) => {
                text.push_str(&String::from_utf8_lossy(&t.into_inner()));
            }
            Ok(Event::End(e)) => {
                let local = local_name(e.name().as_ref());
                if let Some(r) = cur.as_mut() {
                    let val = text.trim().to_string();
                    match local.as_str() {
                        "title" if r.title.is_empty() => r.title = val,
                        "creator" if !val.is_empty() => r.creators.push(val),
                        "description" if r.description.is_empty() => r.description = val,
                        "date" if r.date.is_empty() => r.date = val,
                        // header 의 oai:arXiv.org:.. 보다 dc:identifier 의 http URL 을 선호.
                        "identifier" if val.starts_with("http") || r.identifier.is_empty() => {
                            r.identifier = val
                        }
                        _ => {}
                    }
                }
                if local == "record" {
                    if let Some(r) = cur.take() {
                        records.push(r);
                    }
                }
                text.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(FetchError::Parse(e.to_string())),
            _ => {}
        }
    }
    Ok(records)
}

fn local_name(qname: &[u8]) -> String {
    let s = String::from_utf8_lossy(qname);
    s.rsplit(':').next().unwrap_or(&s).to_string()
}

fn arxiv_id_from_url(url: &str) -> Option<String> {
    let after = url.split("/abs/").nth(1)?;
    let core = match after.rfind('v') {
        Some(pos)
            if pos + 1 < after.len() && after[pos + 1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            &after[..pos]
        }
        _ => after,
    };
    Some(core.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_oai_dc_records() {
        let xml = r#"<?xml version="1.0"?>
<OAI-PMH xmlns="http://www.openarchives.org/OAI/2.0/">
 <ListRecords>
  <record>
   <header><identifier>oai:arXiv.org:2401.00001</identifier><datestamp>2024-01-02</datestamp></header>
   <metadata>
    <oai_dc:dc xmlns:oai_dc="http://www.openarchives.org/OAI/2.0/oai_dc/" xmlns:dc="http://purl.org/dc/elements/1.1/">
     <dc:title>Agentic retrieval methods</dc:title>
     <dc:creator>Doe, Jane</dc:creator>
     <dc:creator>Roe, Richard</dc:creator>
     <dc:description>We study agentic retrieval.</dc:description>
     <dc:date>2024-01-01</dc:date>
     <dc:identifier>http://arxiv.org/abs/2401.00001v1</dc:identifier>
    </oai_dc:dc>
   </metadata>
  </record>
 </ListRecords>
</OAI-PMH>"#;
        let recs = parse_oai(xml).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].title, "Agentic retrieval methods");
        assert_eq!(recs[0].creators.len(), 2);
        assert_eq!(recs[0].identifier, "http://arxiv.org/abs/2401.00001v1");
        assert_eq!(arxiv_id_from_url(&recs[0].identifier).as_deref(), Some("2401.00001"));
    }
}
