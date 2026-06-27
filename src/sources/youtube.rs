//! 유튜브 어댑터 — Data API v3 `search.list` 메타데이터만. 자막 전문은 합법 경로가 없어
//! (captions.download 는 영상 소유자 권한 필요) 다루지 않는다. API 키가 필요하다(설정 주입).

use std::collections::HashMap;

use chrono::Utc;
use serde::Deserialize;

use crate::error::FetchError;
use crate::model::{Author, DocIdentity, Document, Score, SearchQuery, SourceKind};
use crate::normalize::{canonicalize_url, normalize_title, parse_datetime, title_hash};
use crate::source::{RatePolicy, Source};

const DEFAULT_BASE_URL: &str = "https://www.googleapis.com/youtube/v3/search";

/// 유튜브 메타데이터 검색 어댑터.
pub struct YoutubeSource {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl YoutubeSource {
    /// API 키로 생성한다(Data API v3 키, 설정에서 주입).
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: api_key.into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait::async_trait]
impl Source for YoutubeSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Youtube
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError> {
        if query.text.trim().is_empty() {
            return Err(FetchError::InvalidQuery("empty query text".to_string()));
        }
        if self.api_key.trim().is_empty() {
            return Err(FetchError::InvalidQuery("youtube api key not set".to_string()));
        }
        let max_results = query.limit.clamp(1, 50).to_string();
        let params = [
            ("part", "snippet"),
            ("type", "video"),
            ("q", query.text.as_str()),
            ("maxResults", max_results.as_str()),
            ("key", self.api_key.as_str()),
        ];
        let resp = self
            .client
            .get(&self.base_url)
            .query(&params)
            .send()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(FetchError::Http(format!("YouTube HTTP {}", resp.status().as_u16())));
        }
        let body = resp.text().await.map_err(|e| FetchError::Http(e.to_string()))?;
        let parsed: YtResponse =
            serde_json::from_str(&body).map_err(|e| FetchError::Parse(e.to_string()))?;

        let now = Utc::now();
        let mut docs = Vec::new();
        for item in parsed.items {
            if item.id.video_id.is_empty() {
                continue;
            }
            let url = format!("https://www.youtube.com/watch?v={}", item.id.video_id);
            let title = normalize_title(&item.snippet.title);
            let summary = if item.snippet.description.is_empty() {
                None
            } else {
                Some(item.snippet.description)
            };
            let authors = if item.snippet.channel_title.trim().is_empty() {
                Vec::new()
            } else {
                vec![Author::new(item.snippet.channel_title)]
            };
            docs.push(Document {
                identity: DocIdentity {
                    doi: None,
                    arxiv_id: None,
                    canonical_url: canonicalize_url(&url),
                    title_hash: title_hash(&title),
                },
                source: SourceKind::Youtube,
                title,
                url,
                authors,
                published_at: parse_datetime(&item.snippet.published_at),
                fetched_at: now,
                summary,
                content: None,
                language: None,
                tags: Vec::new(),
                sources: vec![SourceKind::Youtube],
                score: Score::default(),
                extra: HashMap::new(),
            });
        }
        Ok(docs)
    }

    fn rate_policy(&self) -> RatePolicy {
        // Data API v3 기본 일일 quota 10,000 units, search.list 1회=100 units.
        RatePolicy { min_interval_ms: 0, max_concurrency: 2, daily_quota: Some(100) }
    }
}

#[derive(Deserialize, Default)]
struct YtResponse {
    #[serde(default)]
    items: Vec<YtItem>,
}

#[derive(Deserialize, Default)]
struct YtItem {
    #[serde(default)]
    id: YtId,
    #[serde(default)]
    snippet: YtSnippet,
}

#[derive(Deserialize, Default)]
struct YtId {
    #[serde(rename = "videoId", default)]
    video_id: String,
}

#[derive(Deserialize, Default)]
struct YtSnippet {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "channelTitle", default)]
    channel_title: String,
    #[serde(rename = "publishedAt", default)]
    published_at: String,
}
