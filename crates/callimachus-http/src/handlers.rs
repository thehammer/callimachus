use crate::error::{ApiError, tool_result_to_response};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use callimachus_core::query::{
    QueryService,
    types::{
        ChapterSummaryInput, CharacterProfileInput, CorpusListInput, CorpusOverviewInput,
        EntityEdgesInput, EntityInput, EntityMeetInput, FindSceneInput, ReadDepth, ReadInput,
        RelatedInput, SearchInput, SummarizeInput, SummarizeTarget,
    },
};
use std::collections::HashMap;
use std::sync::Arc;

// ── Health ────────────────────────────────────────────────────────────────────

pub async fn health(
    State(qs): State<Arc<QueryService>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.corpus_list(CorpusListInput {});
    match result {
        callimachus_core::types::ToolResult::Ok(success) => {
            let count = success.data.len();
            Ok(Json(
                serde_json::json!({"status": "ok", "corpus_count": count}),
            ))
        }
        _ => Ok(Json(serde_json::json!({"status": "ok", "corpus_count": 0}))),
    }
}

// ── Corpus tools ──────────────────────────────────────────────────────────────

pub async fn corpus_list(
    State(qs): State<Arc<QueryService>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.corpus_list(CorpusListInput {});
    tool_result_to_response(result)
}

pub async fn corpus_overview(
    State(qs): State<Arc<QueryService>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.corpus_overview(CorpusOverviewInput { corpus_id: id });
    tool_result_to_response(result)
}

// ── Search ────────────────────────────────────────────────────────────────────

pub async fn search(
    State(qs): State<Arc<QueryService>>,
    Path(id): Path<String>,
    Json(mut input): Json<SearchInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    input.corpus_id = id;
    let result = qs.search(input);
    tool_result_to_response(result)
}

// ── Entity tools ──────────────────────────────────────────────────────────────

pub async fn entity(
    State(qs): State<Arc<QueryService>>,
    Path((corpus_id, name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.entity(EntityInput {
        corpus_id,
        name_or_id: name,
        kind: None,
    });
    tool_result_to_response(result)
}

pub async fn entity_edges(
    State(qs): State<Arc<QueryService>>,
    Path((corpus_id, entity_id)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.entity_edges(EntityEdgesInput {
        corpus_id,
        entity_id,
        direction: params
            .get("direction")
            .cloned()
            .unwrap_or_else(|| "both".to_string()),
        kind: params.get("kind").cloned(),
        limit: params.get("limit").and_then(|v| v.parse().ok()),
    });
    tool_result_to_response(result)
}

pub async fn entity_meet(
    State(qs): State<Arc<QueryService>>,
    Path(id): Path<String>,
    Json(mut input): Json<EntityMeetInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    input.corpus_id = id;
    let result = qs.entity_meet(input);
    tool_result_to_response(result)
}

// ── Read tools ────────────────────────────────────────────────────────────────

pub async fn read(
    State(qs): State<Arc<QueryService>>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let location = params
        .get("location")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("missing required query parameter: location"))?;

    let depth = match params.get("depth").map(|s| s.as_str()) {
        Some("summary") => ReadDepth::Summary,
        Some("scenes") => ReadDepth::Scenes,
        _ => ReadDepth::Full,
    };

    let result = qs.read(ReadInput {
        corpus_id: Some(id),
        location,
        depth,
    });
    tool_result_to_response(result)
}

pub async fn summarize(
    State(qs): State<Arc<QueryService>>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let target = if let Some(entity_id) = params.get("entity_id") {
        SummarizeTarget::Entity {
            entity_id: entity_id.clone(),
        }
    } else if let Some(location) = params.get("location") {
        SummarizeTarget::Location {
            location: location.clone(),
        }
    } else if let (Some(from), Some(to)) = (params.get("from"), params.get("to")) {
        SummarizeTarget::Range {
            from: from.clone(),
            to: to.clone(),
        }
    } else {
        SummarizeTarget::Corpus
    };

    let result = qs.summarize(SummarizeInput {
        corpus_id: id,
        target,
    });
    tool_result_to_response(result)
}

pub async fn related(
    State(qs): State<Arc<QueryService>>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let location = params
        .get("location")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("missing required query parameter: location"))?;

    let result = qs.related(RelatedInput {
        corpus_id: id,
        location,
        limit: params.get("limit").and_then(|v| v.parse().ok()),
    });
    tool_result_to_response(result)
}

// ── Composite tools ───────────────────────────────────────────────────────────

pub async fn chapter_summary(
    State(qs): State<Arc<QueryService>>,
    Path((id, ch)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.chapter_summary(ChapterSummaryInput {
        corpus_id: id,
        chapter: ch,
    });
    tool_result_to_response(result)
}

pub async fn character_profile(
    State(qs): State<Arc<QueryService>>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.character_profile(CharacterProfileInput {
        corpus_id: id,
        name,
    });
    tool_result_to_response(result)
}

pub async fn find_scene(
    State(qs): State<Arc<QueryService>>,
    Path(id): Path<String>,
    Json(mut input): Json<FindSceneInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    input.corpus_id = id;
    let result = qs.find_scene(input);
    tool_result_to_response(result)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use callimachus_core::{
        query::QueryService,
        storage::{SqliteBackend, StorageBackend},
    };
    use std::sync::Arc;

    fn make_server() -> TestServer {
        let db: Arc<dyn StorageBackend> =
            Arc::new(SqliteBackend::open_in_memory().expect("in-memory DB"));
        let qs = Arc::new(QueryService::new(db));
        let router = crate::build_router(qs);
        TestServer::new(router).expect("test server")
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let server = make_server();
        let resp = server.get("/health").await;
        resp.assert_status_ok();
        let body: serde_json::Value = resp.json();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["corpus_count"], 0);
    }

    #[tokio::test]
    async fn corpus_list_empty() {
        let server = make_server();
        let resp = server.get("/corpora").await;
        resp.assert_status_ok();
        let body: serde_json::Value = resp.json();
        // ToolSuccess envelope: { ok: true, data: [] }
        assert_eq!(body["ok"], true);
        assert!(body["data"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn entity_not_found_returns_404() {
        let server = make_server();
        let resp = server.get("/corpora/no-such-corpus/entity/nobody").await;
        resp.assert_status(axum::http::StatusCode::NOT_FOUND);
        let body: serde_json::Value = resp.json();
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn search_missing_query_field_returns_422_or_400() {
        let server = make_server();
        // Posting with an empty body should fail with 4xx (unprocessable or bad request)
        let resp = server
            .post("/corpora/some-corpus/search")
            .content_type("application/json")
            .bytes(axum::body::Bytes::from_static(b"{}"))
            .await;
        // Either 422 (Axum JSON extraction fails) or 400 is acceptable
        let status = resp.status_code();
        assert!(status.is_client_error(), "expected 4xx, got {status}");
    }

    #[tokio::test]
    async fn read_missing_location_param_returns_400() {
        let server = make_server();
        let resp = server.get("/corpora/some-corpus/read").await;
        resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
        let body: serde_json::Value = resp.json();
        assert!(body["error"].as_str().unwrap().contains("location"));
    }

    #[tokio::test]
    async fn read_invalid_corpus_returns_404() {
        let server = make_server();
        let resp = server
            .get("/corpora/invalid-id/read")
            .add_query_param("location", "calli://invalid-id/ch/1")
            .await;
        resp.assert_status(axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cors_preflight_returns_allow_origin() {
        let server = make_server();
        let resp = server
            .method(axum::http::Method::OPTIONS, "/health")
            .add_header(
                axum::http::header::ORIGIN,
                axum::http::HeaderValue::from_static("http://localhost:3000"),
            )
            .add_header(
                axum::http::header::ACCESS_CONTROL_REQUEST_METHOD,
                axum::http::HeaderValue::from_static("GET"),
            )
            .await;
        // CORS preflight should succeed (2xx or 204)
        assert!(resp.status_code().is_success() || resp.status_code().as_u16() == 204);
    }
}
