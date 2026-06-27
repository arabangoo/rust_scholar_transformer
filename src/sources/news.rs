//! Google News RSS 어댑터 — 검색형. 무료·무키 경로(기존 구현에서 검증). 질의를 검색 URL 의
//! 쿼리 파라미터로 넘기고, 상류 검색 결과를 그대로 신뢰한다(클라이언트 키워드 필터 없음).

use crate::error::FetchError;
use crate::model::{Document, SearchQuery, SourceKind};
use crate::source::{RatePolicy, Source};

use super::feed_common::feed_to_docs;

const DEFAULT_BASE_URL: &str = "https://news.google.com/rss/search";
const NEWS_USER_AGENT: &str =
    "rust_scholar_transformer/0.1 (+https://github.com/arabangoo/rust_scholar_transformer)";

/// Google News RSS 검색 어댑터.
pub struct GoogleNewsSource {
    client: reqwest::Client,
    base_url: String,
    hl: String,
    gl: String,
    ceid: String,
    language: Option<String>,
}

impl GoogleNewsSource {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
            hl: "en-US".to_string(),
            gl: "US".to_string(),
            ceid: "US:en".to_string(),
            language: Some("en".to_string()),
        }
    }

    /// 베이스 URL 교체(테스트의 모킹 서버 등).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 언어·지역 설정(hl/gl/ceid + 정규화 언어 태그).
    pub fn with_locale(
        mut self,
        hl: impl Into<String>,
        gl: impl Into<String>,
        ceid: impl Into<String>,
        language: impl Into<String>,
    ) -> Self {
        self.hl = hl.into();
        self.gl = gl.into();
        self.ceid = ceid.into();
        self.language = Some(language.into());
        self
    }
}

impl Default for GoogleNewsSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Source for GoogleNewsSource {
    fn kind(&self) -> SourceKind {
        SourceKind::News
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError> {
        if query.text.trim().is_empty() {
            return Err(FetchError::InvalidQuery("empty query text".to_string()));
        }
        let params = [
            ("q", query.text.as_str()),
            ("hl", self.hl.as_str()),
            ("gl", self.gl.as_str()),
            ("ceid", self.ceid.as_str()),
        ];
        let resp = self
            .client
            .get(&self.base_url)
            .header(reqwest::header::USER_AGENT, NEWS_USER_AGENT)
            .query(&params)
            .send()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(FetchError::Http(format!("Google News HTTP {}", resp.status().as_u16())));
        }
        let body = resp.text().await.map_err(|e| FetchError::Http(e.to_string()))?;
        // 검색형이라 클라이언트 키워드 필터 없음(None).
        feed_to_docs(body.as_bytes(), SourceKind::News, self.language.clone(), None, 0.0)
    }

    fn rate_policy(&self) -> RatePolicy {
        RatePolicy { min_interval_ms: 0, max_concurrency: 2, daily_quota: None }
    }
}
