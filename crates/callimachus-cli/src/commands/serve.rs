use crate::config::GlobalConfig;
use anyhow::{Context, Result};
use callimachus_core::{
    corrections::CorrectionsEngine,
    query::QueryService,
    storage::{SqliteBackend, StorageBackend},
};
use std::{path::Path, sync::Arc};

pub async fn run(host: &str, port: u16, db_path: &Path, _config: &GlobalConfig) -> Result<()> {
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

    println!("Callimachus HTTP API listening on http://{addr}");
    println!("  POST /corpora/:id/search   — full-text / hybrid search");
    println!("  GET  /corpora              — list indexed corpora");
    println!("  GET  /health               — health check");
    println!();
    println!("NOTE: server is bound to {host}. Do not expose to untrusted networks.");

    callimachus_http::serve(listener, qs)
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
            callimachus_http::serve(listener, qs2).await.ok();
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
}
