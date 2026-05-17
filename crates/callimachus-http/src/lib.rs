pub mod error;
pub mod handlers;
pub mod router;

pub use router::build_router;

/// Start the HTTP server on the given listener.
///
/// Convenience wrapper around `axum::serve` so callers don't need axum as a
/// direct dependency.
pub async fn serve(
    listener: tokio::net::TcpListener,
    qs: std::sync::Arc<callimachus_core::query::QueryService>,
) -> anyhow::Result<()> {
    let router = build_router(qs);
    axum::serve(listener, router).await?;
    Ok(())
}
