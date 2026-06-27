//! 소스별 최소 요청 간격 제한기. 직전 요청과의 간격이 `interval` 미만이면 그 차이만큼 대기한다.
//! arXiv 같은 소스가 권고하는 "요청 간 N초"를 강제하는 데 쓴다. interval=0 이면 무제한.

use std::time::{Duration, Instant};

use tokio::sync::Mutex;

pub struct MinIntervalLimiter {
    interval: Duration,
    last: Mutex<Option<Instant>>,
}

impl MinIntervalLimiter {
    pub fn new(interval: Duration) -> Self {
        Self { interval, last: Mutex::new(None) }
    }

    pub fn from_millis(ms: u64) -> Self {
        Self::new(Duration::from_millis(ms))
    }

    /// 다음 요청이 허용될 때까지 대기한다(필요한 만큼만 sleep).
    pub async fn acquire(&self) {
        if self.interval.is_zero() {
            return;
        }
        let mut last = self.last.lock().await;
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < self.interval {
                tokio::time::sleep(self.interval - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spaces_out_requests() {
        let lim = MinIntervalLimiter::from_millis(40);
        let start = Instant::now();
        lim.acquire().await; // 첫 호출은 즉시
        lim.acquire().await; // 두 번째는 최소 간격만큼 대기
        assert!(start.elapsed() >= Duration::from_millis(40));
    }

    #[tokio::test]
    async fn zero_interval_never_waits() {
        let lim = MinIntervalLimiter::from_millis(0);
        let start = Instant::now();
        lim.acquire().await;
        lim.acquire().await;
        assert!(start.elapsed() < Duration::from_millis(20));
    }
}
