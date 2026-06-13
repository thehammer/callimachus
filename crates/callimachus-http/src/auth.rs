use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use std::sync::Arc;
use subtle::ConstantTimeEq;

/// Check API key authentication for an incoming request.
///
/// - If `api_key` is `None`, all requests pass through (open mode).
/// - `GET /health` always passes through regardless of key config.
/// - Otherwise, the request must supply a matching key via either:
///   - `Authorization: Bearer <key>`
///   - `X-Api-Key: <key>`
///
/// Missing or incorrect credentials produce `401 {"error": "unauthorized"}`.
/// The key comparison is constant-time to prevent timing attacks.
pub async fn check_auth(
    api_key: Option<Arc<String>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(expected) = api_key else {
        return next.run(request).await;
    };

    // Health endpoint is always open — ALB/ECS health checks must never be gated.
    if request.uri().path() == "/health" {
        return next.run(request).await;
    }

    let provided = extract_key(request.headers());
    match provided {
        Some(key) if constant_time_eq(key, &expected) => next.run(request).await,
        _ => unauthorized(),
    }
}

/// Extract the API key from request headers.
///
/// Tries `Authorization: Bearer <key>` first, then `X-Api-Key: <key>`.
fn extract_key(headers: &axum::http::HeaderMap) -> Option<&str> {
    if let Some(auth) = headers.get(axum::http::header::AUTHORIZATION)
        && let Ok(s) = auth.to_str()
        && let Some(key) = s.strip_prefix("Bearer ")
    {
        return Some(key);
    }
    if let Some(key_val) = headers.get("x-api-key")
        && let Ok(s) = key_val.to_str()
    {
        return Some(s);
    }
    None
}

/// Constant-time byte-level comparison.
///
/// Uses `subtle::ConstantTimeEq` to prevent timing side-channels when
/// comparing the provided key against the expected key. Returns `false`
/// immediately (and cheaply) when lengths differ — leaking length is
/// acceptable since it cannot be hidden from a network observer anyway.
fn constant_time_eq(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "unauthorized"})),
    )
        .into_response()
}
