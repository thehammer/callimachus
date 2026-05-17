use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};

/// Simple token-bucket rate limiter.
///
/// Allows `calls_per_minute` calls per 60-second window. When the bucket is
/// empty, `acquire` sleeps until the next token refills.
#[derive(Clone, Debug)]
pub struct RateLimiter {
    inner: Arc<Mutex<Inner>>,
    calls_per_minute: u32,
}

#[derive(Debug)]
struct Inner {
    /// Timestamps of recent calls (within the current window).
    call_times: std::collections::VecDeque<Instant>,
}

impl RateLimiter {
    pub fn new(calls_per_minute: u32) -> Self {
        RateLimiter {
            inner: Arc::new(Mutex::new(Inner {
                call_times: std::collections::VecDeque::new(),
            })),
            calls_per_minute,
        }
    }

    /// Waits until a token is available, then records the call.
    pub async fn acquire(&self) {
        loop {
            let sleep_duration = {
                let mut inner = self.inner.lock().unwrap();
                let now = Instant::now();
                let window = Duration::from_secs(60);

                // Evict calls older than the window.
                while inner
                    .call_times
                    .front()
                    .is_some_and(|t| now.duration_since(*t) >= window)
                {
                    inner.call_times.pop_front();
                }

                if inner.call_times.len() < self.calls_per_minute as usize {
                    // Token available — consume it.
                    inner.call_times.push_back(now);
                    return;
                }

                // Calculate how long until the oldest call leaves the window.
                let oldest = *inner.call_times.front().unwrap();
                let elapsed = now.duration_since(oldest);
                window.saturating_sub(elapsed) + Duration::from_millis(10)
            };

            tokio::time::sleep(sleep_duration).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn allows_calls_up_to_limit_immediately() {
        let limiter = RateLimiter::new(3);
        // First 3 calls should return immediately (no waiting).
        let start = Instant::now();
        limiter.acquire().await;
        limiter.acquire().await;
        limiter.acquire().await;
        // Should complete in well under a second.
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn blocks_when_limit_exceeded() {
        // Use calls_per_minute=2 so we don't wait too long in tests.
        // We manipulate time via tokio's pause/advance.
        let limiter = RateLimiter::new(2);

        // These two should be instant.
        limiter.acquire().await;
        limiter.acquire().await;

        // The third call should block. We verify it doesn't complete instantly
        // by timing it — in real time it would wait up to 60s, so we just
        // confirm the tokio test would need to advance time. Since we don't
        // control the clock here, we use a timeout to confirm it *would* block.
        let limiter_clone = limiter.clone();
        let result =
            tokio::time::timeout(Duration::from_millis(100), limiter_clone.acquire()).await;

        // Should time out because the third call is blocked.
        assert!(result.is_err(), "third call should block");
    }
}
