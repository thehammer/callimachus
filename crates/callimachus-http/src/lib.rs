pub mod auth;
pub mod error;
pub mod handlers;
pub mod router;

pub use router::build_router;

/// Start the HTTP server on the given listener.
///
/// When `api_key` is `Some(key)`, every route except `GET /health` requires
/// the key via `Authorization: Bearer <key>` or `X-Api-Key: <key>`.
/// When `api_key` is `None`, all routes are open (local-dev mode).
///
/// Convenience wrapper around `axum::serve` so callers don't need axum as a
/// direct dependency.
pub async fn serve(
    listener: tokio::net::TcpListener,
    qs: std::sync::Arc<callimachus_core::query::QueryService>,
    api_key: Option<String>,
) -> anyhow::Result<()> {
    let router = build_router(qs, api_key);
    axum::serve(listener, router).await?;
    Ok(())
}
