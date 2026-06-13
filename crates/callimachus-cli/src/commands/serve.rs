use crate::config::GlobalConfig;
use anyhow::{Context, Result, bail};
use callimachus_core::{
    corrections::CorrectionsEngine,
    query::QueryService,
    storage::{SqliteBackend, StorageBackend},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub async fn run(
    host: &str,
    port: u16,
    api_key: Option<String>,
    reload_marker: Option<PathBuf>,
    db_path: &Path,
    _config: &GlobalConfig,
) -> Result<()> {
    // Safety guard: refuse to bind on a non-loopback address without a key.
    // Loopback addresses (127.x.x.x and ::1) are safe without auth because
    // only processes on the same host can reach them.
    let is_loopback = host == "127.0.0.1" || host == "localhost" || host == "::1";
    if api_key.is_none() && !is_loopback {
        bail!(
            concat!(
                "refusing to start: --host {} is not a loopback address and no API key is configured.\n",
                "\nTo fix, supply a key via one of:",
                "\n    --api-key <KEY>        pass the key directly",
                "\n    --api-key-env <VAR>    read the key from an environment variable",
                "\n    CALLI_API_KEY=<KEY>    set the default env var",
            ),
            host
        );
    }

    let db: Arc<dyn StorageBackend> = Arc::new(
        SqliteBackend::open(db_path)
            .with_context(|| format!("opening database at {}", db_path.display()))?,
    );

    let corrections =
        CorrectionsEngine::load_all(db.as_ref()).unwrap_or_else(|_| CorrectionsEngine::new(vec![]));
    let qs = Arc::new(QueryService::with_corrections(db, corrections));

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding to {addr}"))?;

    let auth_status = if api_key.is_some() {
        "API key authentication enabled"
    } else {
        "no authentication (loopback only)"
    };

    println!("Callimachus HTTP API listening on http://{addr}");
    println!("  POST /corpora/:id/search   — full-text / hybrid search");
    println!("  GET  /corpora              — list indexed corpora");
    println!("  GET  /health               — health check (always open)");
    println!("  Auth: {auth_status}");
    if let Some(m) = &reload_marker {
        println!("  Reload marker: {}", m.display());
    }
    println!();
    if api_key.is_none() {
        println!("NOTE: server is bound to {host}. Do not expose to untrusted networks.");
    }

    let generation = db_path.display().to_string();
    let state = callimachus_http::ReloadState::fixed(qs, generation);

    if let Some(marker) = reload_marker {
        callimachus_http::spawn_reload_watcher(
            marker,
            Arc::clone(&state),
            Duration::from_secs(5),
        );
    }

    callimachus_http::serve(listener, state, api_key)
        .await
        .context("HTTP server error")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use callimachus_core::storage::StorageBackend;

    /// Seed a db with one corpus so `try_swap`'s sanity check (corpus_count ≥ 1) passes.
    fn seed_corpus(db: &dyn StorageBackend, id: &str) {
        use callimachus_core::types::corpus::Corpus;
        db.corpus_insert(&Corpus::new(
            id.to_string(),
            format!("Test corpus {id}"),
            "code".to_string(),
            "/tmp".to_string(),
        ))
        .expect("seed corpus");
    }

    #[tokio::test]
    async fn serve_health_check() {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let qs = Arc::new(QueryService::new(db));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let url = format!("http://{addr}/health");

        let state = callimachus_http::ReloadState::fixed(qs, "test".to_string());
        let server_task = tokio::spawn(async move {
            callimachus_http::serve(listener, state, None).await.ok();
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect("GET /health");

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert_eq!(body["status"], "ok");

        server_task.abort();
    }

    #[tokio::test]
    async fn serve_non_loopback_without_key_is_refused() {
        let config = GlobalConfig::default();
        // 0.0.0.0 without a key should be rejected before a listener is opened.
        let result = run(
            "0.0.0.0",
            0,
            None,
            None,
            std::path::Path::new(":memory:"),
            &config,
        )
        .await;
        assert!(
            result.is_err(),
            "expected refusal for non-loopback without key"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not a loopback address"),
            "error should mention loopback: {msg}"
        );
    }

    /// Hot-swap: server starts on file A, marker changes to file B, health
    /// reports the new generation within 3 s. Zero failed requests during swap.
    #[tokio::test]
    async fn reload_marker_swaps_generation() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let path_a = tmp.path().join("a.pinakes");
        let path_b = tmp.path().join("b.pinakes");

        // Seed both files with a corpus (required by the sanity check in try_swap).
        {
            seed_corpus(&SqliteBackend::open(&path_a).expect("open a"), "corpus-a");
            seed_corpus(&SqliteBackend::open(&path_b).expect("open b"), "corpus-b");
        }

        // Marker initially points at A so the watcher pre-reads it and does not
        // spuriously swap on the first tick.
        let marker_path = tmp.path().join("reload.marker");
        tokio::fs::write(&marker_path, path_a.to_str().unwrap())
            .await
            .expect("write initial marker");

        // Build state for file A with a 100 ms poll interval.
        let db_a: Arc<dyn StorageBackend> =
            Arc::new(SqliteBackend::open(&path_a).expect("reopen a"));
        let qs_a = Arc::new(QueryService::new(db_a));
        let state = callimachus_http::ReloadState::fixed(qs_a, path_a.display().to_string());
        callimachus_http::spawn_reload_watcher(
            marker_path.clone(),
            Arc::clone(&state),
            Duration::from_millis(100),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let health_url = format!("http://{addr}/health");

        let server_task = tokio::spawn({
            let s = Arc::clone(&state);
            async move { callimachus_http::serve(listener, s, None).await.ok() }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Background poller: track any request that returns a non-2xx status.
        let failures = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let failures_bg = Arc::clone(&failures);
        let health_url_bg = health_url.clone();
        let poller = tokio::spawn(async move {
            let client = reqwest::Client::new();
            for _ in 0u64..60 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let ok = client
                    .get(&health_url_bg)
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
                if !ok {
                    failures_bg.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });

        // Flip the marker to point at B.
        tokio::fs::write(&marker_path, path_b.to_str().unwrap())
            .await
            .expect("write new marker");

        // Poll health until the generation field flips (up to 3 s).
        let client = reqwest::Client::new();
        let path_b_str = path_b.display().to_string();
        let mut swapped = false;
        for _ in 0u64..30 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let body: serde_json::Value = client
                .get(&health_url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .expect("GET /health")
                .json()
                .await
                .expect("json");
            if body["generation"] == path_b_str {
                swapped = true;
                break;
            }
        }

        poller.await.ok();
        server_task.abort();

        assert!(swapped, "generation did not swap to {path_b_str} within 3 s");
        assert_eq!(
            failures.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "some health requests failed during the swap"
        );
    }

    /// A bad marker path: the server keeps serving the old generation and marks
    /// itself degraded in `/health` — it does not crash.
    #[tokio::test]
    async fn reload_marker_corrupt_path_keeps_old_generation() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let path_a = tmp.path().join("a.pinakes");
        seed_corpus(&SqliteBackend::open(&path_a).expect("open a"), "corpus-a");

        // Marker initially points at A (so pre-read sets last_seen = path_a).
        let marker_path = tmp.path().join("reload.marker");
        tokio::fs::write(&marker_path, path_a.to_str().unwrap())
            .await
            .expect("write initial marker");

        let db_a: Arc<dyn StorageBackend> =
            Arc::new(SqliteBackend::open(&path_a).expect("reopen a"));
        let qs_a = Arc::new(QueryService::new(db_a));
        let state = callimachus_http::ReloadState::fixed(qs_a, path_a.display().to_string());
        callimachus_http::spawn_reload_watcher(
            marker_path.clone(),
            Arc::clone(&state),
            Duration::from_millis(100),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let health_url = format!("http://{addr}/health");

        let server_task = tokio::spawn(async move {
            callimachus_http::serve(listener, state, None).await.ok()
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Point the marker at a nonexistent path.
        let bad_path = tmp.path().join("nonexistent.pinakes");
        tokio::fs::write(&marker_path, bad_path.to_str().unwrap())
            .await
            .expect("write bad marker");

        // Allow at least two watcher ticks to fire.
        tokio::time::sleep(Duration::from_millis(350)).await;

        let client = reqwest::Client::new();
        let body: serde_json::Value = client
            .get(&health_url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .expect("GET /health")
            .json()
            .await
            .expect("json");

        server_task.abort();

        assert_eq!(
            body["status"], "degraded",
            "expected degraded status after bad reload; got: {body}"
        );
        assert!(
            body["generation"]
                .as_str()
                .unwrap_or("")
                .ends_with("a.pinakes"),
            "expected old generation a.pinakes to still be served; got: {body}"
        );
        assert!(
            body.get("reload_error").is_some(),
            "expected reload_error field in health; got: {body}"
        );
    }
}
