pub mod auth;
pub mod error;
pub mod handlers;
pub mod reload;
pub mod router;

pub use reload::{ReloadState, spawn_reload_watcher};
pub use router::build_router;

/// Start the HTTP server on the given listener.
///
/// When `api_key` is `Some(key)`, every route except `GET /health` requires
/// the key via `Authorization: Bearer <key>` or `X-Api-Key: <key>`.
/// When `api_key` is `None`, all routes are open (local-dev mode).
///
/// Pass an `Arc<ReloadState>` to enable optional hot-reload; wrap your
/// `QueryService` via [`ReloadState::fixed`] if no hot-reload is needed.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: std::sync::Arc<ReloadState>,
    api_key: Option<String>,
) -> anyhow::Result<()> {
    let router = build_router(state, api_key);
    axum::serve(listener, router).await?;
    Ok(())
}
