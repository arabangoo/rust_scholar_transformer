//! RSS/Atom 블로그 어댑터 — 구독형. 신뢰 소스의 피드 목록을 동시에 가져와 파싱하고,
//! 질의 키워드로 항목을 필터링한다(검색 API 가 아니라 관심 소스 기반 수집).

use futures::future::join_all;

use crate::error::FetchError;
use crate::model::{Document, SearchQuery, SourceKind};
use crate::source::{RatePolicy, Source};

use super::feed_common::feed_to_docs;

const RSS_USER_AGENT: &str =
    "rust_scholar_transformer/0.1 (+https://github.com/arabangoo/rust_scholar_transformer)";

/// 구독 피드 하나. `reliability` 가 0 보다 크면 그 값을 authority 점수로 쓴다(0 이면 SourceKind 기본값).
#[derive(Debug, Clone)]
pub struct FeedSource {
    pub name: String,
    pub url: String,
    pub reliability: f64,
}

impl FeedSource {
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self { name: name.into(), url: url.into(), reliability: 0.0 }
    }

    /// 이 피드의 신뢰도(0.0-1.0)를 지정한다.
    pub fn with_reliability(mut self, reliability: f64) -> Self {
        self.reliability = reliability;
        self
    }
}

/// 블로그/RSS 구독 어댑터.
pub struct RssSource {
    client: reqwest::Client,
    feeds: Vec<FeedSource>,
    language: Option<String>,
}

impl RssSource {
    pub fn new(feeds: Vec<FeedSource>) -> Self {
        Self { client: reqwest::Client::new(), feeds, language: None }
    }

    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    async fn fetch_one(&self, feed: &FeedSource, query: &SearchQuery) -> Result<Vec<Document>, FetchError> {
        let resp = self
            .client
            .get(&feed.url)
            .header(reqwest::header::USER_AGENT, RSS_USER_AGENT)
            .send()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(FetchError::Http(format!("RSS HTTP {}", resp.status().as_u16())));
        }
        let body = resp.text().await.map_err(|e| FetchError::Http(e.to_string()))?;
        feed_to_docs(
            body.as_bytes(),
            SourceKind::Blog,
            self.language.clone(),
            Some(&query.text),
            feed.reliability,
        )
    }
}

#[async_trait::async_trait]
impl Source for RssSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Blog
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError> {
        if self.feeds.is_empty() {
            return Ok(Vec::new());
        }

        // 피드들을 동시에 가져온다. 일부 피드 실패는 best-effort 로 건너뛰되, 전부 실패면 에러.
        // for 루프로 future 를 모아 join_all 한다(async 클로저는 HRTB 수명 추론에 걸림).
        let mut futs = Vec::with_capacity(self.feeds.len());
        for feed in &self.feeds {
            futs.push(self.fetch_one(feed, query));
        }
        let results: Vec<Result<Vec<Document>, FetchError>> = join_all(futs).await;

        let mut docs = Vec::new();
        let mut failures = 0usize;
        for r in results {
            match r {
                Ok(mut ds) => docs.append(&mut ds),
                Err(_) => failures += 1,
            }
        }
        if docs.is_empty() && failures > 0 {
            return Err(FetchError::Http(format!("all {failures} RSS feeds failed")));
        }
        Ok(docs)
    }

    fn rate_policy(&self) -> RatePolicy {
        RatePolicy { min_interval_ms: 0, max_concurrency: self.feeds.len().max(1), daily_quota: None }
    }
}
