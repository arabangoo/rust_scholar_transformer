//! 웹 검색 어댑터 — provider 추상화. 검색 공급망(Brave/Tavily/Exa/SearXNG)이 자주 바뀌고
//! 대부분 유료라, [`WebProvider`] 트레이트 뒤에 두고 갈아끼운다. 스크래핑 기반 provider 는
//! 법적 리스크가 있으므로 자체 인덱스·합법 API provider 와 분리해 명시 사용한다.
//!
//! 기본 reference provider = Brave Search API(자체 인덱스, API 키 필요).

use std::collections::HashMap;

use chrono::Utc;
use serde::Deserialize;

use crate::error::FetchError;
use crate::model::{DocIdentity, Document, Score, SearchQuery, SourceKind};
use crate::normalize::{canonicalize_url, normalize_title, parse_datetime, strip_html, title_hash};
use crate::source::{RatePolicy, Source};

/// 웹 검색 provider. 코어를 건드리지 않고 새 provider 를 끼운다.
#[async_trait::async_trait]
pub trait WebProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn fetch(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError>;
}

/// provider 를 감싸 [`Source`](crate::source::Source) 로 노출한다(kind = Web).
pub struct WebSource {
    provider: Box<dyn WebProvider>,
}

impl WebSource {
    pub fn new(provider: Box<dyn WebProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait::async_trait]
impl Source for WebSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Web
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError> {
        if query.text.trim().is_empty() {
            return Err(FetchError::InvalidQuery("empty query text".to_string()));
        }
        self.provider.fetch(query).await
    }

    fn rate_policy(&self) -> RatePolicy {
        RatePolicy { min_interval_ms: 0, max_concurrency: 2, daily_quota: None }
    }
}

const BRAVE_DEFAULT_URL: &str = "https://api.search.brave.com/res/v1/web/search";

/// Brave Search API provider(자체 독립 인덱스). 구독 토큰(API 키) 필요.
pub struct BraveProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl BraveProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: BRAVE_DEFAULT_URL.to_string(),
            api_key: api_key.into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait::async_trait]
impl WebProvider for BraveProvider {
    fn name(&self) -> &'static str {
        "brave"
    }

    async fn fetch(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError> {
        if self.api_key.trim().is_empty() {
            return Err(FetchError::InvalidQuery("brave api key not set".to_string()));
        }
        let count = query.limit.clamp(1, 20).to_string();
        let params = [("q", query.text.as_str()), ("count", count.as_str())];
        let resp = self
            .client
            .get(&self.base_url)
            .header("X-Subscription-Token", self.api_key.as_str())
            .header(reqwest::header::ACCEPT, "application/json")
            .query(&params)
            .send()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(FetchError::Http(format!("Brave HTTP {}", resp.status().as_u16())));
        }
        let body = resp.text().await.map_err(|e| FetchError::Http(e.to_string()))?;
        let parsed: BraveResp =
            serde_json::from_str(&body).map_err(|e| FetchError::Parse(e.to_string()))?;

        let now = Utc::now();
        let mut docs = Vec::new();
        for r in parsed.web.results {
            if r.url.is_empty() {
                continue;
            }
            let title = normalize_title(&r.title);
            docs.push(Document {
                identity: DocIdentity {
                    doi: None,
                    arxiv_id: None,
                    canonical_url: canonicalize_url(&r.url),
                    title_hash: title_hash(&title),
                },
                source: SourceKind::Web,
                title,
                url: r.url,
                authors: Vec::new(),
                published_at: r.page_age.as_deref().and_then(parse_datetime),
                fetched_at: now,
                summary: if r.description.is_empty() { None } else { Some(strip_html(&r.description)) },
                content: None,
                language: None,
                tags: Vec::new(),
                sources: vec![SourceKind::Web],
                score: Score::default(),
                extra: HashMap::new(),
            });
        }
        Ok(docs)
    }
}

#[derive(Deserialize, Default)]
struct BraveResp {
    #[serde(default)]
    web: BraveWeb,
}

#[derive(Deserialize, Default)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Deserialize, Default)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "page_age", default)]
    page_age: Option<String>,
}
