use crate::{auth, handlers};
use axum::{
    body::Body,
    http::Request,
    middleware::Next,
    routing::{get, post},
};
use callimachus_core::query::QueryService;
use std::sync::Arc;

/// Build the Axum router.
///
/// When `api_key` is `Some(key)`:
/// - Every route except `GET /health` requires `Authorization: Bearer <key>` or
///   `X-Api-Key: <key>`. Missing/wrong credentials → 401 JSON `{"error":"unauthorized"}`.
/// - CORS is deny-by-default (the only consumer is a server-side proxy; browsers
///   never call this API directly).
///
/// When `api_key` is `None` (default local-dev mode):
/// - All routes are open. CORS allows any origin (safe because the server
///   binds to 127.0.0.1 by default; see the startup guard in `calli serve`).
pub fn build_router(qs: Arc<QueryService>, api_key: Option<String>) -> axum::Router {
    let key: Option<Arc<String>> = api_key.map(Arc::new);

    let cors = if key.is_some() {
        // Server-side proxy (sidecar) mode: no browser ever calls this directly.
        // Return a restrictive layer that emits no Allow-Origin headers.
        tower_http::cors::CorsLayer::new()
    } else {
        // Local-dev mode: open CORS (safe because binding is loopback-only).
        tower_http::cors::CorsLayer::new()
            .allow_origin(tower_http::cors::Any)
            .allow_methods(tower_http::cors::Any)
            .allow_headers(tower_http::cors::Any)
    };

    axum::Router::new()
        // Corpus tools
        .route("/corpora", get(handlers::corpus_list))
        .route("/corpora/:id", get(handlers::corpus_overview))
        // Search
        .route("/corpora/:id/search", post(handlers::search))
        // Entity tools
        .route("/corpora/:id/entity/:name", get(handlers::entity))
        .route("/corpora/:id/entity/:id/edges", get(handlers::entity_edges))
        .route("/corpora/:id/meet", post(handlers::entity_meet))
        // Read tools
        .route("/corpora/:id/read", get(handlers::read))
        .route("/corpora/:id/summary", get(handlers::summarize))
        .route("/corpora/:id/related", get(handlers::related))
        // Composite tools
        .route("/corpora/:id/chapter/:ch", get(handlers::chapter_summary))
        .route(
            "/corpora/:id/character/:name",
            get(handlers::character_profile),
        )
        .route("/corpora/:id/scene", post(handlers::find_scene))
        // Health — always open; auth middleware exempts this path explicitly
        .route("/health", get(handlers::health))
        // Auth middleware runs outermost: rejects bad keys before CORS/trace.
        .layer(axum::middleware::from_fn(
            move |req: Request<Body>, next: Next| {
                let key = key.clone();
                async move { auth::check_auth(key, req, next).await }
            },
        ))
        .layer(cors)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(qs)
}
