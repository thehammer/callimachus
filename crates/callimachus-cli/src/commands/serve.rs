use crate::config::GlobalConfig;
use anyhow::{Context, Result, bail};
use callimachus_core::{
    corrections::CorrectionsEngine,
    query::QueryService,
    storage::{SqliteBackend, StorageBackend},
};
use std::{path::Path, sync::Arc};

pub async fn run(
    host: &str,
    port: u16,
    api_key: Option<String>,
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
    println!();
    if api_key.is_none() {
        println!("NOTE: server is bound to {host}. Do not expose to untrusted networks.");
    }

    callimachus_http::serve(listener, qs, api_key)
        .await
        .context("HTTP server error")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn serve_health_check() {
        // Use port 0 to get a random free port.
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let qs = Arc::new(QueryService::new(db));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let url = format!("http://{addr}/health");

        // Spawn the server on a background task so we can query it.
        let qs2 = qs.clone();
        let server_task = tokio::spawn(async move {
            callimachus_http::serve(listener, qs2, None).await.ok();
        });

        // Give the server a moment to start accepting connections.
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
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let _qs = Arc::new(QueryService::new(db));

        let config = GlobalConfig::default();
        // 0.0.0.0 without a key should be rejected before a listener is opened.
        let result = run(
            "0.0.0.0",
            0,
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
}
