use std::num::NonZeroU32;
use std::sync::Arc;

use governor::{DefaultDirectRateLimiter, Quota};

/// Token-bucket rate limiter for LLM API calls using the governor crate.
pub struct RateLimiter {
    /// Stored for potential future introspection/logging; not read elsewhere.
    _config_requests_per_minute: u32,
    _config_tokens_per_minute: u32,
    request_limiter: Arc<DefaultDirectRateLimiter>,
    token_limiter: Arc<DefaultDirectRateLimiter>,
}

impl RateLimiter {
    pub fn new(requests_per_minute: u32, tokens_per_minute: u32) -> Self {
        // Ensure at least 1 permit to avoid governor panic on zero
        let r = if requests_per_minute == 0 {
            1
        } else {
            requests_per_minute
        };
        let t = if tokens_per_minute == 0 {
            1
        } else {
            tokens_per_minute
        };

        Self {
            _config_requests_per_minute: r,
            _config_tokens_per_minute: t,
            request_limiter: Arc::new(governor::RateLimiter::direct(
                Quota::per_minute(NonZeroU32::new(r).unwrap()),
            )),
            token_limiter: Arc::new(governor::RateLimiter::direct(
                Quota::per_minute(NonZeroU32::new(t).unwrap()),
            )),
        }
    }

    /// Acquire permission for one LLM request. Blocks until a token is available.
    pub async fn acquire(&self) {
        // governor's until_ready() blocks until capacity is available
        self.request_limiter.until_ready().await;
    }

    /// Acquire permission for `token_count` tokens.
    pub async fn acquire_tokens(&self, token_count: u32) {
        let count = if token_count == 0 { 1 } else { token_count };
        // For simplicity: acquire N sequential token permits.
        // A real implementation would use a multi-token gauge.
        for _ in 0..count {
            self.token_limiter.until_ready().await;
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(60, 100_000)
    }
}
