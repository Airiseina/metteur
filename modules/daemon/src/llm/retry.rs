//! Provider retry classification and backoff.
//!
//! A rate limit or a transient gateway error should not end a run: the request
//! is retried with an exponential backoff, and once the retry budget is gone a
//! configured fallback model takes over. Classification is driven by the
//! structured [`DaemonError::LlmStatus`]/[`DaemonError::LlmTransport`] variants,
//! never by matching error text.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use metteur_shared::config::LlmConfig;

use crate::error::DaemonError;

/// Statuses that indicate a transient provider-side condition.
const RETRYABLE_STATUSES: &[u16] = &[408, 409, 425, 429, 500, 502, 503, 504, 529];

/// Retry/fallback policy derived from `[llm]` configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first attempt (`0` disables retries).
    pub max_retries: u32,
    /// Base delay of the exponential backoff.
    pub base_delay_ms: u64,
    /// Upper bound of the backoff delay.
    pub max_delay_ms: u64,
    /// Model keys tried in order once the retries are exhausted.
    pub fallback_models: Vec<String>,
}

impl RetryPolicy {
    /// Reads the policy from the merged workspace configuration.
    pub fn from_config(config: &LlmConfig) -> Self {
        Self {
            max_retries: config.max_retries,
            base_delay_ms: config.retry_base_delay_ms,
            max_delay_ms: config.retry_max_delay_ms,
            fallback_models: config.fallback_models.clone(),
        }
    }

    /// Whether an error may be retried.
    ///
    /// Cancellation, sandbox rejections and malformed requests are never
    /// retried: repeating them wastes time and can hide a real problem.
    pub fn retryable(err: &DaemonError) -> bool {
        match err {
            DaemonError::LlmStatus {
                status,
                ..
            } => RETRYABLE_STATUSES.contains(status),
            DaemonError::LlmTransport(_) => true,
            _ => false,
        }
    }

    /// The backoff delay before retry number `attempt` (zero-based), with full
    /// jitter so parallel runs do not retry in lockstep.
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let exponent = attempt.min(16);
        let ceiling = self
            .base_delay_ms
            .saturating_mul(1u64 << exponent)
            .clamp(self.base_delay_ms, self.max_delay_ms.max(self.base_delay_ms));
        if ceiling == 0 {
            return Duration::ZERO;
        }
        Duration::from_millis(jitter_seed() % ceiling)
    }
}

/// A cheap, non-cryptographic jitter source.
///
/// Retry jitter only needs to decorrelate concurrent runs, so the nanosecond
/// clock mixed with a per-process counter is sufficient and avoids pulling an
/// RNG into the dependency tree.
fn jitter_seed() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos ^ count.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}
