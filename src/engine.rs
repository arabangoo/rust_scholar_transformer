//! Fan-out 오케스트레이터 — 등록된 소스들을 동시에 질의(소스별 rate limit 적용)하고, RRF 융합 +
//! 정체성/근접 중복제거 + 날짜 필터를 거쳐 상위 결과를 돌려준다. 한 소스가 느리거나 실패해도
//! [`SearchReport`] 의 경고로 남기고 나머지 결과는 살린다. 선택적으로 결과를 캐시한다.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};

use crate::cache::Cache;
use crate::dedup;
use crate::error::{ConnectorWarning, SearchReport};
use crate::fusion::{self, DEFAULT_RRF_K};
use crate::model::{Document, Recency, SearchQuery};
use crate::ratelimit::MinIntervalLimiter;
use crate::source::Source;

/// 등록된 소스 + (선택) rate limiter.
struct Registered {
    source: Box<dyn Source>,
    limiter: Option<Arc<MinIntervalLimiter>>,
}

pub struct Engine {
    sources: Vec<Registered>,
    max_concurrency: usize,
    timeout: Duration,
    cache: Option<Box<dyn Cache>>,
    near_dup_threshold: f64,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            max_concurrency: 8,
            timeout: Duration::from_secs(10),
            cache: None,
            near_dup_threshold: 0.7,
        }
    }

    /// 소스 어댑터를 등록한다. 소스의 rate_policy 에 최소 간격이 있으면 자동으로 limiter 를 단다.
    pub fn register(&mut self, source: Box<dyn Source>) -> &mut Self {
        let policy = source.rate_policy();
        let limiter = if policy.min_interval_ms > 0 {
            Some(Arc::new(MinIntervalLimiter::from_millis(policy.min_interval_ms)))
        } else {
            None
        };
        self.sources.push(Registered { source, limiter });
        self
    }

    /// 소스별 timeout 을 설정한다.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 결과 캐시를 단다(L1 메모리 또는 L2 디스크).
    pub fn with_cache(mut self, cache: Box<dyn Cache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// 근접중복 병합 임계값(0.0-1.0)을 설정한다. 높을수록 보수적(덜 병합).
    pub fn with_near_dup_threshold(mut self, threshold: f64) -> Self {
        self.near_dup_threshold = threshold;
        self
    }

    /// 질의의 sources 와 교집합인 소스들을 동시 fan-out 한 뒤 RRF 융합 + 중복제거 + 날짜 필터를 거쳐
    /// 상위 limit 개를 돌려준다. 빈 sources 면 전체 대상.
    pub async fn search(&self, query: SearchQuery) -> SearchReport {
        let key = cache_key(&query);
        if let Some(cache) = &self.cache {
            if let Some(hit) = cache.get(&key).await {
                return hit;
            }
        }

        let q = &query;
        let timeout = self.timeout;

        let selected = self
            .sources
            .iter()
            .filter(|r| query.sources.is_empty() || query.sources.contains(&r.source.kind()));

        let outcomes: Vec<_> = stream::iter(selected)
            .map(|r| async move {
                if let Some(limiter) = &r.limiter {
                    limiter.acquire().await;
                }
                let kind = r.source.kind();
                let res = tokio::time::timeout(timeout, r.source.search(q)).await;
                (kind, res)
            })
            .buffer_unordered(self.max_concurrency)
            .collect()
            .await;

        // 소스별 결과 순서를 보존(RRF 가 소스 내 순위를 쓴다). 부분 실패는 경고로.
        let mut per_source: Vec<Vec<Document>> = Vec::new();
        let mut warnings: Vec<ConnectorWarning> = Vec::new();
        for (kind, outcome) in outcomes {
            match outcome {
                Ok(Ok(docs)) => per_source.push(docs),
                Ok(Err(e)) => warnings.push(ConnectorWarning::new(kind, e.to_string())),
                Err(_elapsed) => warnings.push(ConnectorWarning::timeout(kind)),
            }
        }

        // RRF 융합(+정체성 병합) -> 근접중복 병합 -> 날짜 필터 -> top-K.
        let mut docs = fusion::fuse(per_source, DEFAULT_RRF_K);
        docs = dedup::near_dedup(docs, self.near_dup_threshold);
        let now = Utc::now();
        docs.retain(|d| passes_date_filter(d, &query, now));
        docs.truncate(query.limit);

        let report = SearchReport { docs, warnings };
        if let Some(cache) = &self.cache {
            cache.put(key, report.clone()).await;
        }
        report
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

/// 캐시 키 = 정규화 쿼리 텍스트 + 정렬된 소스 종류 + limit + 날짜 조건.
fn cache_key(q: &SearchQuery) -> String {
    let mut kinds: Vec<String> = q.sources.iter().map(|k| format!("{k:?}")).collect();
    kinds.sort();
    format!(
        "{}|{}|{}|{:?}|{:?}|{:?}",
        q.text.trim().to_lowercase(),
        kinds.join(","),
        q.limit,
        q.recency,
        q.from_date,
        q.to_date,
    )
}

/// recency / from_date / to_date 필터. 날짜가 없는 문서는 필터로 떨구지 않는다(불확실 보존).
fn passes_date_filter(d: &Document, q: &SearchQuery, now: DateTime<Utc>) -> bool {
    if let (Some(rec), Some(published)) = (q.recency, d.published_at) {
        let max_days = match rec {
            Recency::Day => 1,
            Recency::Week => 7,
            Recency::Month => 31,
            Recency::Year => 366,
        };
        if now.signed_duration_since(published).num_days() > max_days {
            return false;
        }
    }
    if let (Some(from), Some(published)) = (q.from_date, d.published_at) {
        if published < from {
            return false;
        }
    }
    if let (Some(to), Some(published)) = (q.to_date, d.published_at) {
        if published > to {
            return false;
        }
    }
    true
}
