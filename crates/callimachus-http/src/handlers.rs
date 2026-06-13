use crate::error::{ApiError, tool_result_to_response};
use crate::reload::{Qs, ReloadState};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use callimachus_core::query::{
    CollectionService,
    types::{
        ChapterSummaryInput, CharacterProfileInput, CollectionEntityMeetInput,
        CollectionEntityResolveInput, CollectionListEntry, CollectionListOutput,
        CollectionOverviewInput, CollectionSearchInput, CorpusListInput, CorpusOverviewInput,
        CorpusThemesInput, EntitiesWithoutTestsInput, EntityContractsInput, EntityEdgesInput,
        EntityInput, EntityMeetInput, EntitySearchByAbstractKindInput, ExplainComponentInput,
        FindInconsistenciesInput, FindSceneInput, FindUnreachableInput, ListAbstractKindsInput,
        ReadDepth, ReadInput, RelatedInput, SearchInput, SummarizeInput, SummarizeTarget,
    },
};
use callimachus_core::types::ToolResult;
use std::collections::HashMap;
use std::sync::Arc;

// ── Health ────────────────────────────────────────────────────────────────────

/// Health check handler. Always open; never blocked by auth middleware.
///
/// Returns:
/// - `status`: `"ok"` normally; `"degraded"` when the last reload attempt failed.
/// - `corpus_count`: number of corpora in the currently-served pinakes.
/// - `generation`: path of the pinakes file currently being served.
/// - `loaded_at`: RFC-3339 timestamp of when this generation was loaded.
/// - `reload_error` *(only when degraded)*: human-readable failure message.
pub async fn health(
    State(state): State<Arc<ReloadState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let qs = state.current_qs();
    let result = qs.corpus_list(CorpusListInput {});
    let count = match &result {
        callimachus_core::types::ToolResult::Ok(s) => s.data.len(),
        _ => 0,
    };

    let status = if state.has_reload_error() { "degraded" } else { "ok" };

    // Merge status + corpus_count with generation / loaded_at / reload_error.
    let mut body = serde_json::json!({
        "status": status,
        "corpus_count": count,
    });
    let health_fields = state.health_fields();
    if let (Some(base), serde_json::Value::Object(extra)) =
        (body.as_object_mut(), health_fields)
    {
        base.extend(extra);
    }

    Ok(Json(body))
}

// ── Corpus tools ──────────────────────────────────────────────────────────────

pub async fn corpus_list(
    State(qs): State<Qs>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.corpus_list(CorpusListInput {});
    tool_result_to_response(result)
}

pub async fn corpus_overview(
    State(qs): State<Qs>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.corpus_overview(CorpusOverviewInput { corpus_id: id });
    tool_result_to_response(result)
}

// ── Search ────────────────────────────────────────────────────────────────────

pub async fn search(
    State(qs): State<Qs>,
    Path(id): Path<String>,
    Json(mut input): Json<SearchInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    input.corpus_id = id;
    let result = qs.search(input);
    tool_result_to_response(result)
}

// ── Entity tools ──────────────────────────────────────────────────────────────

pub async fn entity(
    State(qs): State<Qs>,
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
    State(qs): State<Qs>,
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
    State(qs): State<Qs>,
    Path(id): Path<String>,
    Json(mut input): Json<EntityMeetInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    input.corpus_id = id;
    let result = qs.entity_meet(input);
    tool_result_to_response(result)
}

// ── Read tools ────────────────────────────────────────────────────────────────

pub async fn read(
    State(qs): State<Qs>,
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
    State(qs): State<Qs>,
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
    State(qs): State<Qs>,
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
    State(qs): State<Qs>,
    Path((id, ch)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.chapter_summary(ChapterSummaryInput {
        corpus_id: id,
        chapter: ch,
    });
    tool_result_to_response(result)
}

pub async fn character_profile(
    State(qs): State<Qs>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.character_profile(CharacterProfileInput {
        corpus_id: id,
        name,
    });
    tool_result_to_response(result)
}

pub async fn find_scene(
    State(qs): State<Qs>,
    Path(id): Path<String>,
    Json(mut input): Json<FindSceneInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    input.corpus_id = id;
    let result = qs.find_scene(input);
    tool_result_to_response(result)
}

// ── Collection tools ──────────────────────────────────────────────────────────

pub async fn collection_list(
    State(qs): State<Qs>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let backend = qs.backend();
    let collections = backend.collection_list().map_err(ApiError::from)?;
    let entries: Vec<CollectionListEntry> = collections
        .into_iter()
        .map(|c| {
            let member_count = c.members.len() as u64;
            let corpus_count = backend
                .collection_resolve_corpus_ids(&c.id)
                .map(|v| v.len() as u64)
                .unwrap_or(0);
            CollectionListEntry {
                id: c.id,
                name: c.name,
                kind: c.kind.as_str().to_string(),
                member_count,
                corpus_count,
            }
        })
        .collect();
    tool_result_to_response(ToolResult::ok(CollectionListOutput {
        collections: entries,
    }))
}

pub async fn collection_overview(
    State(qs): State<Qs>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let svc = CollectionService::load(qs.backend(), &id).map_err(ApiError::from)?;
    let result = svc.collection_overview(CollectionOverviewInput { collection_id: id });
    tool_result_to_response(result)
}

pub async fn collection_search(
    State(qs): State<Qs>,
    Path(id): Path<String>,
    Json(mut input): Json<CollectionSearchInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    input.collection_id = id;
    let svc =
        CollectionService::load(qs.backend(), &input.collection_id).map_err(ApiError::from)?;
    tool_result_to_response(svc.collection_search(input))
}

pub async fn collection_entity_resolve(
    State(qs): State<Qs>,
    Path(id): Path<String>,
    Json(mut input): Json<CollectionEntityResolveInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    input.collection_id = id;
    let svc =
        CollectionService::load(qs.backend(), &input.collection_id).map_err(ApiError::from)?;
    tool_result_to_response(svc.collection_entity_resolve(input))
}

pub async fn collection_entity_meet(
    State(qs): State<Qs>,
    Path(id): Path<String>,
    Json(mut input): Json<CollectionEntityMeetInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    input.collection_id = id;
    let svc =
        CollectionService::load(qs.backend(), &input.collection_id).map_err(ApiError::from)?;
    tool_result_to_response(svc.collection_entity_meet(input))
}

// ── Code-analysis tools ───────────────────────────────────────────────────────

pub async fn entity_contracts(
    State(qs): State<Qs>,
    Path((corpus_id, entity_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.entity_contracts(EntityContractsInput {
        corpus_id,
        entity_id,
    });
    tool_result_to_response(result)
}

pub async fn find_inconsistencies(
    State(qs): State<Qs>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.find_inconsistencies(FindInconsistenciesInput { corpus_id: id });
    tool_result_to_response(result)
}

pub async fn find_unreachable(
    State(qs): State<Qs>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.find_unreachable(FindUnreachableInput { corpus_id: id });
    tool_result_to_response(result)
}

pub async fn corpus_themes(
    State(qs): State<Qs>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let include_edges = params.get("include_edges").and_then(|v| v.parse().ok());
    let result = qs.corpus_themes(CorpusThemesInput {
        corpus_id: id,
        include_edges,
    });
    tool_result_to_response(result)
}

pub async fn entities_without_tests(
    State(qs): State<Qs>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.entities_without_tests(EntitiesWithoutTestsInput { corpus_id: id });
    tool_result_to_response(result)
}

pub async fn explain_component(
    State(qs): State<Qs>,
    Path(id): Path<String>,
    Json(mut input): Json<ExplainComponentInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    input.corpus_id = id;
    let result = qs.explain_component(input);
    tool_result_to_response(result)
}

// ── Taxonomy tools ────────────────────────────────────────────────────────────

pub async fn entity_search_by_abstract_kind(
    State(qs): State<Qs>,
    Json(input): Json<EntitySearchByAbstractKindInput>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.entity_search_by_abstract_kind(input);
    tool_result_to_response(result)
}

pub async fn list_abstract_kinds(
    State(qs): State<Qs>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = qs.list_abstract_kinds(ListAbstractKindsInput {});
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
        let state = crate::reload::ReloadState::fixed(qs, "test".to_string());
        let router = crate::build_router(state, None);
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

    // ── Collection routes ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn collection_list_empty() {
        let server = make_server();
        let resp = server.get("/collections").await;
        resp.assert_status_ok();
        let body: serde_json::Value = resp.json();
        assert_eq!(body["ok"], true);
        assert!(body["data"]["collections"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn collection_overview_unknown_returns_404() {
        let server = make_server();
        let resp = server.get("/collections/no-such-collection").await;
        resp.assert_status(axum::http::StatusCode::NOT_FOUND);
        let body: serde_json::Value = resp.json();
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn collection_search_unknown_returns_404() {
        let server = make_server();
        // collection_id in body is required by serde; the handler overrides it with the path param
        let resp = server
            .post("/collections/no-such/search")
            .content_type("application/json")
            .bytes(axum::body::Bytes::from_static(
                b"{\"collection_id\":\"no-such\",\"query\":\"hello\"}",
            ))
            .await;
        resp.assert_status(axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn collection_search_missing_query_returns_4xx() {
        let server = make_server();
        let resp = server
            .post("/collections/any/search")
            .content_type("application/json")
            .bytes(axum::body::Bytes::from_static(b"{}"))
            .await;
        let status = resp.status_code();
        assert!(status.is_client_error(), "expected 4xx, got {status}");
    }

    #[tokio::test]
    async fn collection_entity_resolve_unknown_collection_returns_404() {
        let server = make_server();
        let resp = server
            .post("/collections/no-such/entity/resolve")
            .content_type("application/json")
            .bytes(axum::body::Bytes::from_static(
                b"{\"collection_id\":\"no-such\",\"name\":\"Alice\"}",
            ))
            .await;
        resp.assert_status(axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn collection_entity_meet_unknown_collection_returns_404() {
        let server = make_server();
        let resp = server
            .post("/collections/no-such/meet")
            .content_type("application/json")
            .bytes(axum::body::Bytes::from_static(
                b"{\"collection_id\":\"no-such\",\"entity_a\":\"Alice\",\"entity_b\":\"Bob\"}",
            ))
            .await;
        resp.assert_status(axum::http::StatusCode::NOT_FOUND);
    }

    // ── Code-analysis routes ──────────────────────────────────────────────────

    #[tokio::test]
    async fn entity_contracts_unknown_corpus_returns_404() {
        let server = make_server();
        let resp = server
            .get("/corpora/no-corpus/entity/no-entity/contracts")
            .await;
        resp.assert_status(axum::http::StatusCode::NOT_FOUND);
        let body: serde_json::Value = resp.json();
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn find_inconsistencies_returns_empty_for_unknown_corpus() {
        let server = make_server();
        // These analysis tools don't gate on corpus existence — they return empty results.
        let resp = server.get("/corpora/no-corpus/inconsistencies").await;
        resp.assert_status_ok();
        let body: serde_json::Value = resp.json();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["count"], 0);
    }

    #[tokio::test]
    async fn find_unreachable_returns_empty_for_unknown_corpus() {
        let server = make_server();
        let resp = server.get("/corpora/no-corpus/unreachable").await;
        resp.assert_status_ok();
        let body: serde_json::Value = resp.json();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["count"], 0);
    }

    #[tokio::test]
    async fn corpus_themes_returns_empty_for_unknown_corpus() {
        let server = make_server();
        let resp = server.get("/corpora/no-corpus/themes").await;
        resp.assert_status_ok();
        let body: serde_json::Value = resp.json();
        assert_eq!(body["ok"], true);
        assert!(body["data"]["themes"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn entities_without_tests_returns_empty_for_unknown_corpus() {
        let server = make_server();
        let resp = server.get("/corpora/no-corpus/untested").await;
        resp.assert_status_ok();
        let body: serde_json::Value = resp.json();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["count"], 0);
    }

    #[tokio::test]
    async fn explain_component_unknown_entity_returns_404() {
        let server = make_server();
        // corpus_id in body required by serde; handler overrides it with path param
        let resp = server
            .post("/corpora/no-corpus/explain")
            .content_type("application/json")
            .bytes(axum::body::Bytes::from_static(
                b"{\"corpus_id\":\"no-corpus\",\"entity_id\":\"nonexistent-entity-id\"}",
            ))
            .await;
        resp.assert_status(axum::http::StatusCode::NOT_FOUND);
    }

    // ── Taxonomy routes ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_abstract_kinds_returns_ok() {
        let server = make_server();
        let resp = server.get("/abstract-kinds").await;
        resp.assert_status_ok();
        let body: serde_json::Value = resp.json();
        assert_eq!(body["ok"], true);
        assert!(body["data"]["rows"].is_array());
    }

    #[tokio::test]
    async fn entity_search_by_abstract_kind_missing_fields_returns_4xx() {
        let server = make_server();
        let resp = server
            .post("/search/by-abstract-kind")
            .content_type("application/json")
            .bytes(axum::body::Bytes::from_static(b"{}"))
            .await;
        let status = resp.status_code();
        assert!(status.is_client_error(), "expected 4xx, got {status}");
    }

    #[tokio::test]
    async fn entity_search_by_abstract_kind_empty_corpora_returns_ok() {
        let server = make_server();
        let resp = server
            .post("/search/by-abstract-kind")
            .content_type("application/json")
            .bytes(axum::body::Bytes::from_static(
                b"{\"corpus_ids\":[],\"abstract_kind\":\"component\"}",
            ))
            .await;
        resp.assert_status_ok();
        let body: serde_json::Value = resp.json();
        assert_eq!(body["ok"], true);
    }
}
