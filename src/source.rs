//! Source 트레이트 — 모든 출처의 공통 인터페이스. 새 소스는 이 트레이트만 구현하면
//! 엔진에 등록된다(코어 불변).

use crate::error::FetchError;
use crate::model::{Document, SearchQuery, SourceKind};

/// 소스별 rate-limit 정책. 오케스트레이터가 RateLimiter 구성에 사용한다.
#[derive(Debug, Clone)]
pub struct RatePolicy {
    pub min_interval_ms: u64,
    pub max_concurrency: usize,
    pub daily_quota: Option<u32>,
}

impl Default for RatePolicy {
    fn default() -> Self {
        Self { min_interval_ms: 0, max_concurrency: 4, daily_quota: None }
    }
}

/// 모든 출처가 구현하는 공통 인터페이스.
#[async_trait::async_trait]
pub trait Source: Send + Sync {
    /// 소스 종류.
    fn kind(&self) -> SourceKind;

    /// 질의 -> 정규화된 문서. 네트워크·파싱 에러는 타입드 에러로.
    async fn search(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError>;

    /// 소스별 rate-limit 정책.
    fn rate_policy(&self) -> RatePolicy {
        RatePolicy::default()
    }
}
