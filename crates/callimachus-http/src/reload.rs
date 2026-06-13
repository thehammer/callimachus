//! Hot-reload state: atomically swappable `QueryService` pointer.
//!
//! # Connection model
//!
//! `SqliteBackend` wraps `Arc<Mutex<Database>>` — a single SQLite connection
//! behind a mutex. There is no connection pool. Drain is therefore free: when
//! the last `Arc<QueryService>` referencing an old backend drops (every
//! in-flight handler has returned its clone), `SqliteBackend` drops and the
//! file descriptor closes. No explicit shutdown plumbing is required.
//!
//! # Zero-dropped-requests guarantee
//!
//! Handlers extract `Arc<QueryService>` from the router state via the
//! `Qs` newtype and its `FromRef` impl below. The read lock is held only for
//! the duration of `Arc::clone` (nanoseconds). The swap write lock is held
//! only for a pointer replacement (nanoseconds). No request ever blocks on
//! I/O while holding either lock, so no request can be dropped or fail due
//! to the swap.

use callimachus_core::{
    corrections::CorrectionsEngine,
    query::QueryService,
    storage::{SqliteBackend, StorageBackend},
};
use chrono::{DateTime, Utc};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

// ── Internal entry type ───────────────────────────────────────────────────────

struct ReloadEntry {
    qs: Arc<QueryService>,
    /// Absolute path of the pinakes file currently being served.
    /// Set to the startup pinakes path when `--reload-marker` is absent.
    generation: String,
    loaded_at: DateTime<Utc>,
}

// ── Public state type ─────────────────────────────────────────────────────────

/// Axum router state that supports atomic hot-reload of the served
/// `QueryService`.
///
/// Carried as `Arc<ReloadState>`. All handlers extract [`Qs`] (which derefs
/// to `QueryService`) via the [`FromRef`] impl without touching this type
/// directly. Only `/health` extracts `Arc<ReloadState>` to read `generation`,
/// `loaded_at`, and `reload_error`.
pub struct ReloadState {
    current: RwLock<ReloadEntry>,
    /// Non-`None` when the last reload attempt failed. Cleared on success.
    reload_error: RwLock<Option<String>>,
}

impl ReloadState {
    /// Wrap an existing `QueryService` with no hot-reload watcher.
    ///
    /// Used when `--reload-marker` is absent. Per-request overhead is one
    /// `Arc::clone` through a shared-read `RwLock` — effectively free.
    pub fn fixed(qs: Arc<QueryService>, generation: String) -> Arc<Self> {
        Arc::new(Self {
            current: RwLock::new(ReloadEntry { qs, generation, loaded_at: Utc::now() }),
            reload_error: RwLock::new(None),
        })
    }

    /// Clone the current `Arc<QueryService>`. Called per-request via [`Qs`]'s
    /// `FromRef` impl.
    ///
    /// Holds the read lock for the duration of the `Arc` clone only
    /// (nanoseconds), then releases it immediately.
    pub fn current_qs(&self) -> Arc<QueryService> {
        self.current.read().expect("ReloadState lock poisoned").qs.clone()
    }

    /// Build the JSON fields for `/health` (generation, loaded_at, reload_error).
    ///
    /// `reload_error` is omitted from the returned object when absent.
    pub fn health_fields(&self) -> serde_json::Value {
        let entry = self.current.read().expect("ReloadState lock poisoned");
        let error = self.reload_error.read().expect("ReloadState error lock poisoned");
        let mut v = serde_json::json!({
            "generation": entry.generation,
            "loaded_at": entry.loaded_at.to_rfc3339(),
        });
        if let Some(e) = error.as_ref() {
            v["reload_error"] = serde_json::Value::String(e.clone());
        }
        v
    }

    /// `true` when a reload error is recorded (for the `degraded` health status).
    pub fn has_reload_error(&self) -> bool {
        self.reload_error.read().expect("ReloadState error lock poisoned").is_some()
    }

    /// Attempt to atomically swap to the pinakes file at `new_path`.
    ///
    /// Steps:
    /// 1. Open `new_path` read-only.
    /// 2. Sanity-check: corpora table is readable and has ≥ 1 row.
    /// 3. Build `QueryService` (load corrections from the new backend).
    /// 4. Acquire write lock, replace the entry, release immediately.
    /// 5. Clear any previous `reload_error`.
    ///
    /// On failure: the old generation continues to be served and the error is
    /// recorded for `/health`. Returns `Err` so the caller can log.
    pub fn try_swap(&self, new_path: &str) -> anyhow::Result<String> {
        use anyhow::Context;

        let path = std::path::Path::new(new_path);
        let backend: Arc<dyn StorageBackend> = Arc::new(
            SqliteBackend::open(path)
                .with_context(|| format!("opening pinakes at {new_path}"))?,
        );

        let corpora = backend
            .corpus_list()
            .with_context(|| format!("reading corpora from {new_path}"))?;
        if corpora.is_empty() {
            anyhow::bail!("sanity check: new pinakes at {new_path} has 0 corpora");
        }

        let corrections = CorrectionsEngine::load_all(backend.as_ref())
            .unwrap_or_else(|_| CorrectionsEngine::new(vec![]));
        let new_qs = Arc::new(QueryService::with_corrections(backend, corrections));

        let old_generation = {
            let mut guard = self.current.write().expect("ReloadState lock poisoned");
            let old = guard.generation.clone();
            *guard = ReloadEntry {
                qs: new_qs,
                generation: new_path.to_string(),
                loaded_at: Utc::now(),
            };
            old
        };

        *self.reload_error.write().expect("ReloadState error lock poisoned") = None;
        Ok(old_generation)
    }

    /// Record a reload failure. Does not change the current generation.
    pub fn set_reload_error(&self, err: String) {
        *self.reload_error.write().expect("ReloadState error lock poisoned") = Some(err);
    }
}

// ── Qs newtype: Axum extractor ────────────────────────────────────────────────

/// Newtype wrapper around `Arc<QueryService>` used as the Axum extractor type.
///
/// Defined in this crate so `FromRef<Arc<ReloadState>> for Qs` satisfies the
/// orphan rule (the implementing type `Qs` is local to `callimachus-http`).
///
/// # Usage in handlers
///
/// Handlers declare `State(qs): State<Qs>`. Because `Qs` derefs to
/// `QueryService`, all `QueryService` methods are accessible directly on `qs`
/// without unwrapping the newtype.
#[derive(Clone)]
pub struct Qs(pub Arc<QueryService>);

impl std::ops::Deref for Qs {
    type Target = QueryService;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Axum calls this on every request to extract `Qs` from `Arc<ReloadState>`.
/// Cost: one shared-read `RwLock` acquisition + `Arc::clone` (nanoseconds).
impl axum::extract::FromRef<Arc<ReloadState>> for Qs {
    fn from_ref(state: &Arc<ReloadState>) -> Self {
        Qs(state.current_qs())
    }
}

// ── Watcher task ──────────────────────────────────────────────────────────────

/// Spawn a background task that polls `marker_path` every `interval`.
///
/// Protocol: the marker file contains the absolute path of the pinakes
/// generation to serve (one line, no trailing content). When the file content
/// changes, [`ReloadState::try_swap`] is called. Failures are recorded in
/// `state.reload_error` and the old generation continues to be served.
///
/// The watcher pre-reads the marker at startup so it does not trigger a
/// spurious first-tick swap when the marker already points at the current
/// generation.
pub fn spawn_reload_watcher(
    marker_path: PathBuf,
    state: Arc<ReloadState>,
    interval: Duration,
) {
    tokio::spawn(async move {
        // Pre-read the marker so the first tick skips if unchanged.
        let mut last_seen = match tokio::fs::read_to_string(&marker_path).await {
            Ok(s) => s.trim().to_string(),
            Err(_) => String::new(),
        };

        loop {
            tokio::time::sleep(interval).await;

            let raw = match tokio::fs::read_to_string(&marker_path).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(
                        path = %marker_path.display(),
                        "reload marker unreadable (will retry): {e}",
                    );
                    continue;
                }
            };
            let content = raw.trim().to_string();

            if content.is_empty() || content == last_seen {
                continue;
            }

            tracing::info!(
                from = %last_seen,
                to = %content,
                "reload marker changed; attempting generation swap",
            );

            match state.try_swap(&content) {
                Ok(old) => {
                    tracing::info!(
                        old_generation = %old,
                        new_generation = %content,
                        "generation swapped successfully",
                    );
                    last_seen = content;
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    tracing::error!(
                        path = %content,
                        "reload failed — serving old generation: {msg}",
                    );
                    state.set_reload_error(msg);
                    // Advance last_seen to avoid log spam on every tick for the
                    // same bad path. A fresh path in the marker will still be detected.
                    last_seen = content;
                }
            }
        }
    });
}
