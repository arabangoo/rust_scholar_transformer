//! 검색 결과 캐시. L1 인메모리(moka, 기본) + L2 디스크(redb, `cache-disk` feature).
//! 같은 쿼리 반복 시 외부 API 호출 없이 즉시 반환해 속도·비용·quota 를 아낀다.

use std::time::Duration;

use crate::error::SearchReport;

/// 캐시 추상화. 엔진은 이 트레이트만 안다(구현 교체 가능).
#[async_trait::async_trait]
pub trait Cache: Send + Sync {
    async fn get(&self, key: &str) -> Option<SearchReport>;
    async fn put(&self, key: String, value: SearchReport);
}

/// L1 인메모리 캐시(TTL + 용량 상한). 순수 Rust.
pub struct MemoryCache {
    inner: moka::future::Cache<String, SearchReport>,
}

impl MemoryCache {
    pub fn new(ttl: Duration, max_capacity: u64) -> Self {
        let inner = moka::future::Cache::builder()
            .time_to_live(ttl)
            .max_capacity(max_capacity)
            .build();
        Self { inner }
    }
}

#[async_trait::async_trait]
impl Cache for MemoryCache {
    async fn get(&self, key: &str) -> Option<SearchReport> {
        self.inner.get(key).await
    }

    async fn put(&self, key: String, value: SearchReport) {
        self.inner.insert(key, value).await;
    }
}

/// L2 디스크 캐시(redb). 프로세스 재시작 후에도 유지. 값에 만료 시각을 함께 저장해 읽을 때 검사.
#[cfg(feature = "cache-disk")]
pub struct DiskCache {
    db: redb::Database,
    ttl_secs: i64,
}

#[cfg(feature = "cache-disk")]
const DISK_TABLE: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("search_cache");

#[cfg(feature = "cache-disk")]
#[derive(serde::Serialize, serde::Deserialize)]
struct DiskEntry {
    expires_at: i64, // epoch seconds
    report: SearchReport,
}

#[cfg(feature = "cache-disk")]
impl DiskCache {
    pub fn open(path: impl AsRef<std::path::Path>, ttl: Duration) -> Result<Self, String> {
        let db = redb::Database::create(path).map_err(|e| e.to_string())?;
        Ok(Self { db, ttl_secs: ttl.as_secs() as i64 })
    }
}

#[cfg(feature = "cache-disk")]
#[async_trait::async_trait]
impl Cache for DiskCache {
    async fn get(&self, key: &str) -> Option<SearchReport> {
        let bytes: Vec<u8> = {
            let rtx = self.db.begin_read().ok()?;
            let table = rtx.open_table(DISK_TABLE).ok()?;
            let guard = table.get(key).ok()??;
            guard.value().to_vec()
        };
        let entry: DiskEntry = serde_json::from_slice(&bytes).ok()?;
        if entry.expires_at < chrono::Utc::now().timestamp() {
            return None; // 만료
        }
        Some(entry.report)
    }

    async fn put(&self, key: String, value: SearchReport) {
        let entry = DiskEntry {
            expires_at: chrono::Utc::now().timestamp() + self.ttl_secs,
            report: value,
        };
        let Ok(bytes) = serde_json::to_vec(&entry) else { return };
        let Ok(wtx) = self.db.begin_write() else { return };
        {
            let Ok(mut table) = wtx.open_table(DISK_TABLE) else { return };
            let _ = table.insert(key.as_str(), bytes.as_slice());
        }
        let _ = wtx.commit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SearchReport;

    fn empty_report() -> SearchReport {
        SearchReport { docs: Vec::new(), warnings: Vec::new() }
    }

    #[tokio::test]
    async fn memory_cache_round_trips() {
        let c = MemoryCache::new(Duration::from_secs(60), 100);
        assert!(c.get("k").await.is_none());
        c.put("k".to_string(), empty_report()).await;
        assert!(c.get("k").await.is_some());
    }
}
