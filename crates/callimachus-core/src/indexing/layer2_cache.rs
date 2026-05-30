//! Layer-2 cache plumbing shared by the five LLM-derived passes.
//!
//! The five Layer-2 passes (purpose, contract, summarize, theme, embed) all
//! follow the same shape: compute a [`Layer2CacheKey`], consult the cache, and
//! either reuse the stored payload (skipping the LLM call entirely) or derive a
//! fresh value and store it. These helpers centralise the serialize/deserialize
//! boundary so each pass only has to describe its key and its payload type.
//!
//! The cache is content-addressed by `(artifact_kind, entity_id, content_hash,
//! file_shape_hash, model, stable_sampling)`; see [`Layer2CacheKey::cache_key`].
//! It is consulted **regardless of `IndexOptions::full`** — `--full` bypasses
//! the per-pass head-table idempotency guard so head rows are re-written, but a
//! previously-derived artifact is still reused, which is what makes a re-run of
//! an unchanged corpus do zero LLM work.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::storage::StorageBackend;
use crate::types::Layer2CacheKey;

/// Look up a cached Layer-2 payload and deserialize it to `T`.
///
/// Returns `Ok(Some(value))` on a cache hit, `Ok(None)` on a miss. A malformed
/// payload (e.g. a schema change) is treated as a miss rather than an error so
/// a stale entry can never wedge a pass.
pub fn cache_get<T: DeserializeOwned>(
    db: &dyn StorageBackend,
    key: &Layer2CacheKey,
) -> anyhow::Result<Option<T>> {
    match db.layer2_cache_get(key)? {
        Some(cached) => match serde_json::from_str::<T>(&cached.payload) {
            Ok(value) => Ok(Some(value)),
            Err(e) => {
                tracing::warn!(
                    artifact_kind = %key.artifact_kind,
                    error = %e,
                    "layer2 cache payload failed to deserialize; treating as miss"
                );
                Ok(None)
            }
        },
        None => Ok(None),
    }
}

/// Serialize `value` and store it under `key`.
pub fn cache_put<T: Serialize>(
    db: &dyn StorageBackend,
    key: &Layer2CacheKey,
    value: &T,
    first_seen_at_sha: &str,
) -> anyhow::Result<()> {
    let payload = serde_json::to_string(value)?;
    db.layer2_cache_put(key, &payload, first_seen_at_sha)?;
    Ok(())
}
