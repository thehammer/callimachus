//! Integration tests for API key authentication.
//!
//! RED PHASE — these tests drive the two-argument `build_router(qs, api_key)`
//! signature that does not exist yet. They will not compile until the feature
//! lands. That compile failure is the correct red state.
//!
//! Behavior contract enforced here:
//!   - When `api_key = None` all routes are open (open mode).
//!   - When `api_key = Some(_)` all routes except GET /health require a
//!     matching `Authorization: Bearer <key>` or `X-Api-Key: <key>` header.
//!   - Missing or wrong credentials → 401 JSON `{"error": "unauthorized"}`.
//!   - GET /health is always open, regardless of key configuration.

use axum_test::TestServer;
use callimachus_core::{
    query::QueryService,
    storage::{SqliteBackend, StorageBackend},
};
use std::sync::Arc;

// ── Helpers ───────────────────────────────────────────────────────────────────

#[allow(dead_code)]
fn make_qs() -> Arc<QueryService> {
    let db: Arc<dyn StorageBackend> =
        Arc::new(SqliteBackend::open_in_memory().expect("in-memory DB"));
    Arc::new(QueryService::new(db))
}

fn make_state() -> Arc<callimachus_http::ReloadState> {
    callimachus_http::ReloadState::fixed(make_qs(), "test".to_string())
}

/// Build a test server with a required API key.
#[allow(dead_code)]
fn make_server_with_key(key: &str) -> TestServer {
    let router = callimachus_http::build_router(make_state(), Some(key.to_string()));
    TestServer::new(router).expect("test server")
}

/// Build a test server with no authentication (open mode).
#[allow(dead_code)]
fn make_server_open() -> TestServer {
    let router = callimachus_http::build_router(make_state(), None);
    TestServer::new(router).expect("test server")
}

// ── Open-mode tests ───────────────────────────────────────────────────────────

/// No key configured → all routes are publicly accessible.
#[tokio::test]
async fn auth_open_mode_corpora_accessible() {
    let server = make_server_open();
    let resp = server.get("/corpora").await;
    resp.assert_status_ok();
}

// ── Protected-mode: missing credentials ───────────────────────────────────────

/// Key configured, no Authorization/X-Api-Key header → 401 with JSON error.
#[tokio::test]
async fn auth_required_missing_header_returns_401() {
    let server = make_server_with_key("secret");
    let resp = server.get("/corpora").await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = resp.json();
    assert_eq!(
        body["error"], "unauthorized",
        "expected {{\"error\": \"unauthorized\"}}, got {body}"
    );
}

// ── Protected-mode: valid credentials ────────────────────────────────────────

/// Key configured, correct `Authorization: Bearer <key>` → 200.
#[tokio::test]
async fn auth_bearer_header_grants_access() {
    let server = make_server_with_key("secret");
    let resp = server
        .get("/corpora")
        .add_header(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer secret"),
        )
        .await;
    resp.assert_status_ok();
}

/// Key configured, correct `X-Api-Key: <key>` → 200.
#[tokio::test]
async fn auth_x_api_key_header_grants_access() {
    let server = make_server_with_key("secret");
    let resp = server
        .get("/corpora")
        .add_header(
            axum::http::HeaderName::from_static("x-api-key"),
            axum::http::HeaderValue::from_static("secret"),
        )
        .await;
    resp.assert_status_ok();
}

// ── Protected-mode: wrong credentials ────────────────────────────────────────

/// Key configured, wrong key in X-Api-Key → 401.
#[tokio::test]
async fn auth_wrong_key_returns_401() {
    let server = make_server_with_key("secret");
    let resp = server
        .get("/corpora")
        .add_header(
            axum::http::HeaderName::from_static("x-api-key"),
            axum::http::HeaderValue::from_static("wrongkey"),
        )
        .await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = resp.json();
    assert_eq!(body["error"], "unauthorized");
}

/// A key that is a prefix of the real key must not be accepted (no partial match).
#[tokio::test]
async fn auth_wrong_key_does_not_partially_succeed() {
    let server = make_server_with_key("secret");
    let resp = server
        .get("/corpora")
        .add_header(
            axum::http::HeaderName::from_static("x-api-key"),
            axum::http::HeaderValue::from_static("secr"), // prefix of "secret"
        )
        .await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

// ── Health endpoint bypass ────────────────────────────────────────────────────

/// No key configured → GET /health returns 200 with {"status": "ok"}.
#[tokio::test]
async fn auth_health_always_open_no_key() {
    let server = make_server_open();
    let resp = server.get("/health").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "ok");
}

/// Key configured, NO auth header → GET /health still returns 200 (health bypasses auth).
#[tokio::test]
async fn auth_health_always_open_with_key() {
    let server = make_server_with_key("secret");
    // Deliberately send no Authorization or X-Api-Key header.
    let resp = server.get("/health").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(
        body["status"], "ok",
        "/health must be reachable without credentials even when auth is enabled"
    );
}

/// Key configured, correct key header → GET /health still returns 200.
#[tokio::test]
async fn auth_health_correct_key_also_works() {
    let server = make_server_with_key("secret");
    let resp = server
        .get("/health")
        .add_header(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer secret"),
        )
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "ok");
}
