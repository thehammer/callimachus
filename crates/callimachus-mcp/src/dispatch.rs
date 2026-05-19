use callimachus_core::corrections::types::CorrectionKind;
use callimachus_core::query::{
    ChapterSummaryInput, CharacterProfileInput, CollectionEntityMeetInput,
    CollectionEntityResolveInput, CollectionListInput, CollectionOverviewInput,
    CollectionSearchInput, CollectionService, CorpusListInput, CorpusOverviewInput,
    CorpusThemesInput, EntitiesWithoutTestsInput, EntityContractsInput, EntityEdgesInput,
    EntityInput, EntityMeetInput, EntitySearchByAbstractKindInput, ExplainComponentInput,
    FindInconsistenciesInput, FindSceneInput, FindUnreachableInput, ListAbstractKindsInput,
    QueryService, ReadInput, RelatedInput, SearchInput, SummarizeInput,
};
use callimachus_core::storage::StorageBackend;
use serde_json::Value;
use std::sync::Arc;

/// Dispatch a tool call by name.
///
/// Deserializes `args` into the appropriate input type, calls the matching
/// service method, and returns the result as a `serde_json::Value`.
pub async fn dispatch(
    qs: &QueryService,
    backend: &Arc<dyn StorageBackend>,
    name: &str,
    args: Value,
) -> Value {
    match name {
        "corpus_list" => {
            let input = serde_json::from_value::<CorpusListInput>(args).unwrap_or_default();
            serde_json::to_value(qs.corpus_list(input)).unwrap_or_else(err_json)
        }
        "corpus_overview" => match serde_json::from_value::<CorpusOverviewInput>(args) {
            Ok(input) => serde_json::to_value(qs.corpus_overview(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "search" => match serde_json::from_value::<SearchInput>(args) {
            Ok(input) => serde_json::to_value(qs.search(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "entity" => match serde_json::from_value::<EntityInput>(args) {
            Ok(input) => serde_json::to_value(qs.entity(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "entity_edges" => match serde_json::from_value::<EntityEdgesInput>(args) {
            Ok(input) => serde_json::to_value(qs.entity_edges(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "entity_meet" => match serde_json::from_value::<EntityMeetInput>(args) {
            Ok(input) => serde_json::to_value(qs.entity_meet(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "read" => match serde_json::from_value::<ReadInput>(args) {
            Ok(input) => serde_json::to_value(qs.read(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "summarize" => match serde_json::from_value::<SummarizeInput>(args) {
            Ok(input) => serde_json::to_value(qs.summarize(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "related" => match serde_json::from_value::<RelatedInput>(args) {
            Ok(input) => serde_json::to_value(qs.related(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "chapter_summary" => match serde_json::from_value::<ChapterSummaryInput>(args) {
            Ok(input) => serde_json::to_value(qs.chapter_summary(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "character_profile" => match serde_json::from_value::<CharacterProfileInput>(args) {
            Ok(input) => serde_json::to_value(qs.character_profile(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "find_scene" => match serde_json::from_value::<FindSceneInput>(args) {
            Ok(input) => serde_json::to_value(qs.find_scene(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        // ── Collection tools ────────────────────────────────────────────────
        "collection_list" => {
            let _input = serde_json::from_value::<CollectionListInput>(args).unwrap_or_default();
            match backend.collection_list() {
                Err(e) => serde_json::json!({
                    "ok": false,
                    "kind": "error",
                    "code": "storage_error",
                    "message": e.to_string(),
                    "retriable": false
                }),
                Ok(collections) => {
                    let entries: Vec<_> = collections
                        .into_iter()
                        .map(|c| {
                            let member_count = c.members.len() as u64;
                            let corpus_count = backend
                                .collection_resolve_corpus_ids(&c.id)
                                .map(|v| v.len() as u64)
                                .unwrap_or(0);
                            serde_json::json!({
                                "id": c.id,
                                "name": c.name,
                                "kind": c.kind.as_str(),
                                "member_count": member_count,
                                "corpus_count": corpus_count,
                            })
                        })
                        .collect();
                    serde_json::json!({ "ok": true, "data": { "collections": entries } })
                }
            }
        }
        "collection_overview" => match serde_json::from_value::<CollectionOverviewInput>(args) {
            Err(e) => invalid_input(e),
            Ok(input) => match CollectionService::load(Arc::clone(backend), &input.collection_id) {
                Err(e) => not_found(e),
                Ok(svc) => {
                    serde_json::to_value(svc.collection_overview(input)).unwrap_or_else(err_json)
                }
            },
        },
        "collection_search" => match serde_json::from_value::<CollectionSearchInput>(args) {
            Err(e) => invalid_input(e),
            Ok(input) => match CollectionService::load(Arc::clone(backend), &input.collection_id) {
                Err(e) => not_found(e),
                Ok(svc) => {
                    serde_json::to_value(svc.collection_search(input)).unwrap_or_else(err_json)
                }
            },
        },
        "collection_entity_resolve" => {
            match serde_json::from_value::<CollectionEntityResolveInput>(args) {
                Err(e) => invalid_input(e),
                Ok(input) => {
                    match CollectionService::load(Arc::clone(backend), &input.collection_id) {
                        Err(e) => not_found(e),
                        Ok(svc) => serde_json::to_value(svc.collection_entity_resolve(input))
                            .unwrap_or_else(err_json),
                    }
                }
            }
        }
        "collection_entity_meet" => {
            match serde_json::from_value::<CollectionEntityMeetInput>(args) {
                Err(e) => invalid_input(e),
                Ok(input) => {
                    match CollectionService::load(Arc::clone(backend), &input.collection_id) {
                        Err(e) => not_found(e),
                        Ok(svc) => serde_json::to_value(svc.collection_entity_meet(input))
                            .unwrap_or_else(err_json),
                    }
                }
            }
        }
        // ── Phase 12 tools ──────────────────────────────────────────────────
        "entity_contracts" => match serde_json::from_value::<EntityContractsInput>(args) {
            Ok(input) => serde_json::to_value(qs.entity_contracts(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "find_inconsistencies" => match serde_json::from_value::<FindInconsistenciesInput>(args) {
            Ok(input) => {
                serde_json::to_value(qs.find_inconsistencies(input)).unwrap_or_else(err_json)
            }
            Err(e) => invalid_input(e),
        },
        "find_unreachable" => match serde_json::from_value::<FindUnreachableInput>(args) {
            Ok(input) => serde_json::to_value(qs.find_unreachable(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "corpus_themes" => match serde_json::from_value::<CorpusThemesInput>(args) {
            Ok(input) => serde_json::to_value(qs.corpus_themes(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "entities_without_tests" => {
            match serde_json::from_value::<EntitiesWithoutTestsInput>(args) {
                Ok(input) => {
                    serde_json::to_value(qs.entities_without_tests(input)).unwrap_or_else(err_json)
                }
                Err(e) => invalid_input(e),
            }
        }
        "explain_component" => match serde_json::from_value::<ExplainComponentInput>(args) {
            Ok(input) => serde_json::to_value(qs.explain_component(input)).unwrap_or_else(err_json),
            Err(e) => invalid_input(e),
        },
        "entity_search_by_abstract_kind" => {
            match serde_json::from_value::<EntitySearchByAbstractKindInput>(args) {
                Ok(input) => serde_json::to_value(qs.entity_search_by_abstract_kind(input))
                    .unwrap_or_else(err_json),
                Err(e) => invalid_input(e),
            }
        }
        "list_abstract_kinds" => {
            let input = serde_json::from_value::<ListAbstractKindsInput>(args).unwrap_or_default();
            serde_json::to_value(qs.list_abstract_kinds(input)).unwrap_or_else(err_json)
        }
        // ── Scholia tools ───────────────────────────────────────────────────
        "list_scholia" => {
            let corpus_id = match args.get("corpus_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => {
                    return serde_json::json!({
                        "ok": false,
                        "kind": "invalid_input",
                        "message": "corpus_id is required"
                    });
                }
            };
            match backend.correction_list(&corpus_id) {
                Err(e) => serde_json::json!({
                    "ok": false,
                    "kind": "error",
                    "code": "storage_error",
                    "message": e.to_string(),
                    "retriable": false
                }),
                Ok(scholia) => {
                    let items: Vec<_> = scholia
                        .into_iter()
                        .map(|s| {
                            serde_json::json!({
                                "id": s.id,
                                "corpus_id": s.corpus_id,
                                "kind": s.kind.kind_name(),
                                "applied_at": s.applied_at,
                            })
                        })
                        .collect();
                    serde_json::json!({ "ok": true, "data": { "scholia": items } })
                }
            }
        }
        "apply_scholion" => {
            let kind_str = match args.get("kind").and_then(|v| v.as_str()) {
                Some(k) => k,
                None => {
                    return serde_json::json!({
                        "ok": false,
                        "kind": "invalid_input",
                        "message": "kind is required"
                    });
                }
            };

            let corpus_id = args.get("corpus_id").and_then(|v| v.as_str());
            let collection_id = args.get("collection_id").and_then(|v| v.as_str());

            let correction_kind = match kind_str {
                "merge" => {
                    let entity_a_id = args
                        .get("entity_a_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let entity_b_id = args
                        .get("entity_b_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let canonical_id = args
                        .get("canonical_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&entity_a_id)
                        .to_string();
                    CorrectionKind::Merge {
                        entity_a_id,
                        entity_b_id,
                        canonical_id,
                    }
                }
                "unmerge" => {
                    use callimachus_core::corrections::types::SplitGranularity;
                    let entity_id = args
                        .get("entity_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let split_by = match args.get("split_by").and_then(|v| v.as_str()) {
                        Some("chapter") => SplitGranularity::Chapter,
                        _ => SplitGranularity::Scene,
                    };
                    CorrectionKind::Unmerge {
                        entity_id,
                        split_by,
                    }
                }
                "rename" => {
                    let entity_id = args
                        .get("entity_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let new_name = args
                        .get("new_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    CorrectionKind::Rename {
                        entity_id,
                        new_name,
                    }
                }
                "alias" => {
                    let entity_id = args
                        .get("entity_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let add = args
                        .get("add")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str())
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();
                    let remove = args
                        .get("remove")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str())
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();
                    CorrectionKind::Alias {
                        entity_id,
                        add,
                        remove,
                    }
                }
                "edit_summary" => {
                    let target_kind = args
                        .get("target_kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("entity")
                        .to_string();
                    let target_id = args
                        .get("target_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let text = args
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    CorrectionKind::EditSummary {
                        target_kind,
                        target_id,
                        text,
                    }
                }
                "entity_link" => {
                    use callimachus_core::corrections::types::EntityLinkKind;
                    let corpus_a_id = args
                        .get("corpus_a_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let entity_a_id = args
                        .get("entity_a_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let corpus_b_id = args
                        .get("corpus_b_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let entity_b_id = args
                        .get("entity_b_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let link_kind_str =
                        args.get("link_kind").and_then(|v| v.as_str()).unwrap_or("");
                    let link_kind = match EntityLinkKind::from_str(link_kind_str) {
                        Some(k) => k,
                        None => {
                            return serde_json::json!({
                                "ok": false,
                                "kind": "invalid_input",
                                "message": format!("unknown link_kind: {link_kind_str}")
                            });
                        }
                    };
                    let note = args.get("note").and_then(|v| v.as_str()).map(String::from);
                    CorrectionKind::EntityLink {
                        corpus_a_id,
                        entity_a_id,
                        corpus_b_id,
                        entity_b_id,
                        kind: link_kind,
                        note,
                    }
                }
                other => {
                    return serde_json::json!({
                        "ok": false,
                        "kind": "invalid_input",
                        "message": format!("unknown scholion kind: {other}")
                    });
                }
            };

            match backend.correction_insert(corpus_id, collection_id, &correction_kind) {
                Err(e) => serde_json::json!({
                    "ok": false,
                    "kind": "error",
                    "code": "storage_error",
                    "message": e.to_string(),
                    "retriable": false
                }),
                Ok(id) => serde_json::json!({ "ok": true, "data": { "id": id } }),
            }
        }

        unknown => serde_json::json!({
            "ok": false,
            "kind": "invalid_input",
            "message": format!("Unknown tool: {unknown}")
        }),
    }
}

fn invalid_input(e: serde_json::Error) -> Value {
    serde_json::json!({
        "ok": false,
        "kind": "invalid_input",
        "message": format!("Invalid arguments: {e}")
    })
}

fn not_found(e: callimachus_core::error::CalError) -> Value {
    serde_json::json!({
        "ok": false,
        "kind": "not_found",
        "message": e.to_string()
    })
}

fn err_json(_e: serde_json::Error) -> Value {
    serde_json::json!({
        "ok": false,
        "kind": "error",
        "code": "serialization_error",
        "message": "Failed to serialize response",
        "retriable": false
    })
}
