use crate::handlers;
use axum::routing::{get, post};
use callimachus_core::query::QueryService;
use std::sync::Arc;

/// Build the Axum router.
///
/// CORS is open (`allow_origin(Any)`) because `calli serve` binds to 127.0.0.1
/// by default and is intended for local use only. Do not expose this server to
/// an untrusted network without adding authentication.
pub fn build_router(qs: Arc<QueryService>) -> axum::Router {
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
        // Health
        .route("/health", get(handlers::health))
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods(tower_http::cors::Any)
                .allow_headers(tower_http::cors::Any),
        )
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(qs)
}
