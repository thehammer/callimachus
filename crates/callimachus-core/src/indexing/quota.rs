//! Shared "stop on subscription quota exhaustion" signal for LLM passes.
//!
//! A pass creates one [`QuotaGate`], checks it before each LLM call, and trips
//! it when a call fails with [`LlmError::QuotaExhausted`]. Entities that have
//! not started yet are skipped; in-flight ones finish or fail on their own.

use std::sync::atomic::{AtomicBool, Ordering};

use callimachus_llm::LlmError;

#[derive(Debug, Default)]
pub(crate) struct QuotaGate(AtomicBool);

impl QuotaGate {
    pub(crate) fn is_tripped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub(crate) fn trip(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// If `err` is a quota-exhaustion error, trip the gate and return the
    /// `stopped_reason` string (`"quota_exhausted: <resets_at>"`).
    pub(crate) fn check(&self, err: &anyhow::Error) -> Option<String> {
        let reason = quota_reason(err)?;
        self.trip();
        Some(reason)
    }
}

/// `"quota_exhausted: <resets_at or message>"` when `err` is a quota error.
pub(crate) fn quota_reason(err: &anyhow::Error) -> Option<String> {
    match err.downcast_ref::<LlmError>() {
        Some(LlmError::QuotaExhausted { message, resets_at }) => Some(format!(
            "quota_exhausted: {}",
            resets_at.as_deref().unwrap_or(message)
        )),
        _ => None,
    }
}
