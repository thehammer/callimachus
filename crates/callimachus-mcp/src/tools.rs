use once_cell::sync::Lazy;
use serde_json::{Value, json};

/// Descriptor for one MCP tool.
#[derive(Debug, Clone)]
pub struct ToolDesc {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

/// All 27 Callimachus tools (22 corpus-scoped + 5 collection-scoped).
pub static TOOL_LIST: Lazy<Vec<ToolDesc>> = Lazy::new(|| {
    vec![
        ToolDesc {
            name: "corpus_list",
            description: "List all indexed corpora with counts of chunks and entities.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDesc {
            name: "corpus_overview",
            description: "Get an overview of a corpus including top entities and a summary if available.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string", "description": "The corpus identifier." }
                },
                "required": ["corpus_id"]
            }),
        },
        ToolDesc {
            name: "search",
            description: "Full-text keyword search over a corpus. Returns ranked results with snippets.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string", "description": "The corpus to search." },
                    "query": { "type": "string", "description": "The search query." },
                    "mode": {
                        "type": "string",
                        "enum": ["keyword", "semantic", "hybrid"],
                        "description": "Search mode. Only 'keyword' is active in this version.",
                        "default": "keyword"
                    },
                    "scope": {
                        "type": "object",
                        "description": "Optional scope filter (e.g. spoiler boundary).",
                        "properties": {
                            "position": { "type": "object", "description": "Stop results after this location." }
                        }
                    },
                    "limit": { "type": "integer", "description": "Max results (default 20).", "default": 20 }
                },
                "required": ["corpus_id", "query"]
            }),
        },
        ToolDesc {
            name: "entity",
            description: "Look up an entity by name or ID. Returns full entity details including aliases and locations.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string" },
                    "name_or_id": { "type": "string", "description": "Entity canonical name or ID." }
                },
                "required": ["corpus_id", "name_or_id"]
            }),
        },
        ToolDesc {
            name: "entity_edges",
            description: "Get the relationship edges for an entity (inbound, outbound, or both).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string" },
                    "entity_id": { "type": "string", "description": "Entity ID." },
                    "direction": {
                        "type": "string",
                        "enum": ["inbound", "outbound", "both"],
                        "default": "both"
                    },
                    "kind": { "type": "string", "description": "Filter by edge kind (optional)." },
                    "limit": { "type": "integer", "default": 50 }
                },
                "required": ["corpus_id", "entity_id"]
            }),
        },
        ToolDesc {
            name: "entity_meet",
            description: "Find all locations where two entities co-occur.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string" },
                    "entity_a": { "type": "string", "description": "First entity name or ID." },
                    "entity_b": { "type": "string", "description": "Second entity name or ID." }
                },
                "required": ["corpus_id", "entity_a", "entity_b"]
            }),
        },
        ToolDesc {
            name: "read",
            description: "Read a chunk by its location URI at summary, scenes, or full depth.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string", "description": "Corpus ID (optional if embedded in location URI)." },
                    "location": { "type": "string", "description": "Location URI (e.g. calli://corpus/ch/3/sc/1)." },
                    "depth": {
                        "type": "string",
                        "enum": ["summary", "scenes", "full"],
                        "default": "full"
                    }
                },
                "required": ["location"]
            }),
        },
        ToolDesc {
            name: "summarize",
            description: "Get a stored summary for a corpus, entity, location, or range.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string" },
                    "target": {
                        "type": "object",
                        "description": "The summarization target.",
                        "properties": {
                            "kind": {
                                "type": "string",
                                "enum": ["corpus", "entity", "location", "range"]
                            },
                            "entity_id": { "type": "string" },
                            "location": { "type": "string" },
                            "from": { "type": "string" },
                            "to": { "type": "string" }
                        },
                        "required": ["kind"]
                    }
                },
                "required": ["corpus_id", "target"]
            }),
        },
        ToolDesc {
            name: "related",
            description: "Find chunks most related to a given location by shared entity overlap.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string" },
                    "location": { "type": "string" },
                    "limit": { "type": "integer", "default": 10 }
                },
                "required": ["corpus_id", "location"]
            }),
        },
        ToolDesc {
            name: "chapter_summary",
            description: "Get the summary of a chapter by number (e.g. '3') or title.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string" },
                    "chapter": { "type": "string", "description": "Chapter number ('3'), ordinal ('Three'), or title." }
                },
                "required": ["corpus_id", "chapter"]
            }),
        },
        ToolDesc {
            name: "character_profile",
            description: "Get a full character profile: entity details, all relationships, and summary.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string" },
                    "name": { "type": "string", "description": "Character name." }
                },
                "required": ["corpus_id", "name"]
            }),
        },
        ToolDesc {
            name: "find_scene",
            description: "Find the first scene where two characters appear together and return its full content.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string" },
                    "entity_a": { "type": "string", "description": "First character name or ID." },
                    "entity_b": { "type": "string", "description": "Second character name or ID." }
                },
                "required": ["corpus_id", "entity_a", "entity_b"]
            }),
        },
        // ── Collection tools ──────────────────────────────────────────────────
        ToolDesc {
            name: "collection_list",
            description: "List all collections with member counts and kinds.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDesc {
            name: "collection_overview",
            description: "Get an overview of a collection: member corpora/collections, entity counts, and link counts by kind.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "collection_id": { "type": "string", "description": "The collection identifier." }
                },
                "required": ["collection_id"]
            }),
        },
        ToolDesc {
            name: "collection_search",
            description: "Keyword search across all corpora reachable from the collection, with results ranked and labelled by source corpus.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "collection_id": { "type": "string", "description": "The collection to search." },
                    "query": { "type": "string", "description": "The search query." },
                    "mode": {
                        "type": "string",
                        "enum": ["keyword", "semantic", "hybrid"],
                        "default": "keyword"
                    },
                    "limit": { "type": "integer", "description": "Max results (default 20).", "default": 20 }
                },
                "required": ["collection_id", "query"]
            }),
        },
        ToolDesc {
            name: "collection_entity_resolve",
            description: "Resolve an entity name across all corpora in the collection. Returns direct matches with SameAs metadata and related entities via typed links.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "collection_id": { "type": "string" },
                    "name": { "type": "string", "description": "Entity name to resolve." }
                },
                "required": ["collection_id", "name"]
            }),
        },
        ToolDesc {
            name: "collection_entity_meet",
            description: "Find all co-occurrences of two entities across the collection. SameAs-aware: equivalent entities across corpora are treated as the same.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "collection_id": { "type": "string" },
                    "entity_a": { "type": "string", "description": "First entity name." },
                    "entity_b": { "type": "string", "description": "Second entity name." }
                },
                "required": ["collection_id", "entity_a", "entity_b"]
            }),
        },
        // ── Phase 12 tools (23 total) ────────────────────────────────────────
        ToolDesc {
            name: "entity_contracts",
            description: "Retrieve the pre-indexed contract (risks, assumptions, signals) and purpose for a specific code entity.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string", "description": "The corpus identifier." },
                    "entity_id": { "type": "string", "description": "The entity ID." }
                },
                "required": ["corpus_id", "entity_id"]
            }),
        },
        ToolDesc {
            name: "find_inconsistencies",
            description: "Find code entities whose contracts signal inconsistency or technical debt: incomplete implementations, non-empty risks on infallible functions, or known intent gaps.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string", "description": "The corpus identifier." }
                },
                "required": ["corpus_id"]
            }),
        },
        ToolDesc {
            name: "find_unreachable",
            description: "Find code entities that have no inbound 'calls' edges — potentially dead or unreachable code.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string", "description": "The corpus identifier." }
                },
                "required": ["corpus_id"]
            }),
        },
        ToolDesc {
            name: "corpus_themes",
            description: "List corpus-level architectural themes and invariants extracted during indexing.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string", "description": "The corpus identifier." },
                    "include_edges": { "type": "boolean", "description": "Also return upheld_by / violated_by edge sets.", "default": false }
                },
                "required": ["corpus_id"]
            }),
        },
        ToolDesc {
            name: "entities_without_tests",
            description: "Find code entities that have no inbound 'verified_by' edges — indicating no test coverage.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string", "description": "The corpus identifier." }
                },
                "required": ["corpus_id"]
            }),
        },
        ToolDesc {
            name: "explain_component",
            description: "Assemble a diegesis (multi-paragraph narrative) explaining a component via BFS over call edges, using pre-indexed purposes, summaries, and block blurbs. Zero LLM calls at query time.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string", "description": "The corpus identifier." },
                    "entity_id": { "type": "string", "description": "Seed entity ID (use this OR module_prefix)." },
                    "module_prefix": { "type": "string", "description": "Name prefix to seed the BFS (use this OR entity_id)." },
                    "max_depth": { "type": "integer", "description": "BFS depth limit (default 3).", "default": 3 }
                },
                "required": ["corpus_id"]
            }),
        },
        // ── Taxonomy tools ────────────────────────────────────────────────────
        ToolDesc {
            name: "entity_search_by_abstract_kind",
            description: "List entities across one or more corpora filtered by abstract taxonomy kind (e.g. 'process', 'component', 'person', 'place'). Enables cross-corpus queries like 'all processes'.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "One or more corpus IDs to search across."
                    },
                    "abstract_kind": {
                        "type": "string",
                        "description": "Abstract taxonomy kind: process | component | person | place | organization | concept"
                    },
                    "limit": { "type": "integer", "description": "Max results (default 50).", "default": 50 }
                },
                "required": ["corpus_ids", "abstract_kind"]
            }),
        },
        ToolDesc {
            name: "list_abstract_kinds",
            description: "Return the full kind_taxonomy table: each row maps a concrete adapter kind to its cross-corpus abstract kind.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // ── Scholia tools (Phase 2 Pinakes terminology) ───────────────────────
        ToolDesc {
            name: "list_scholia",
            description: "List all scholia (marginal annotations / post-indexing corrections) for a corpus, ordered by application time.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": { "type": "string", "description": "The corpus identifier." }
                },
                "required": ["corpus_id"]
            }),
        },
        ToolDesc {
            name: "apply_scholion",
            description: "Apply a scholion (correction) to the index. Supports merge, unmerge, rename, alias, edit_summary, and entity_link operations.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "corpus_id": {
                        "type": "string",
                        "description": "The corpus to apply the scholion to (required for all operations except entity_link)."
                    },
                    "collection_id": {
                        "type": "string",
                        "description": "The collection to apply the scholion to (required for entity_link only)."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["merge", "unmerge", "rename", "alias", "edit_summary", "entity_link"],
                        "description": "The type of correction to apply."
                    },
                    "entity_a_id": { "type": "string", "description": "First entity ID (merge, entity_link)." },
                    "entity_b_id": { "type": "string", "description": "Second entity ID (merge, entity_link)." },
                    "canonical_id": { "type": "string", "description": "Which entity to keep as canonical (merge)." },
                    "entity_id": { "type": "string", "description": "Entity ID (unmerge, rename, alias)." },
                    "split_by": { "type": "string", "description": "Granularity for unmerge: 'scene' or 'chapter'." },
                    "new_name": { "type": "string", "description": "New canonical name (rename)." },
                    "add": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Aliases to add (alias)."
                    },
                    "remove": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Aliases to remove (alias)."
                    },
                    "target_kind": { "type": "string", "description": "Target kind for edit_summary: 'chunk', 'entity', or 'corpus'." },
                    "target_id": { "type": "string", "description": "Target ID for edit_summary." },
                    "text": { "type": "string", "description": "Replacement summary text (edit_summary)." },
                    "corpus_a_id": { "type": "string", "description": "First corpus ID (entity_link)." },
                    "corpus_b_id": { "type": "string", "description": "Second corpus ID (entity_link)." },
                    "link_kind": {
                        "type": "string",
                        "enum": ["same_as", "implements", "exemplifies", "references", "contrasts"],
                        "description": "Relationship kind (entity_link)."
                    },
                    "note": { "type": "string", "description": "Optional note (entity_link)." }
                },
                "required": ["kind"]
            }),
        },
    ]
});

/// Serialize the tool list for `tools/list` response.
pub fn tools_list_json() -> Vec<Value> {
    TOOL_LIST
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Scholia MCP tools must appear in TOOL_LIST ────────────────────────────

    /// `list_scholia` must be registered as an MCP tool.
    ///
    /// This test fails until the tool descriptor is added to `TOOL_LIST` as
    /// part of the Pinakes Terminology Phase 2 implementation.
    #[test]
    fn list_scholia_tool_in_list() {
        let names: Vec<&str> = TOOL_LIST.iter().map(|t| t.name).collect();
        assert!(
            names.contains(&"list_scholia"),
            "list_scholia must be registered in TOOL_LIST; found tools: {:?}",
            names
        );
    }

    /// `apply_scholion` must be registered as an MCP tool.
    ///
    /// This test fails until the tool descriptor is added to `TOOL_LIST` as
    /// part of the Pinakes Terminology Phase 2 implementation.
    #[test]
    fn apply_scholion_tool_in_list() {
        let names: Vec<&str> = TOOL_LIST.iter().map(|t| t.name).collect();
        assert!(
            names.contains(&"apply_scholion"),
            "apply_scholion must be registered in TOOL_LIST; found tools: {:?}",
            names
        );
    }

    /// Both new scholia tools must appear together — registering one without
    /// the other indicates an incomplete migration.
    #[test]
    fn both_scholia_tools_registered_together() {
        let names: Vec<&str> = TOOL_LIST.iter().map(|t| t.name).collect();
        let has_list = names.contains(&"list_scholia");
        let has_apply = names.contains(&"apply_scholion");
        assert!(
            has_list && has_apply,
            "both list_scholia and apply_scholion must be in TOOL_LIST; \
             list_scholia={}, apply_scholion={}",
            has_list,
            has_apply
        );
    }

    /// Sanity check: existing tools must not have been removed during the
    /// scholia migration.  Guards against an accidental full replacement of
    /// TOOL_LIST.
    #[test]
    fn pre_existing_tools_not_removed_by_scholia_migration() {
        let names: Vec<&str> = TOOL_LIST.iter().map(|t| t.name).collect();
        for expected in &[
            "corpus_list",
            "search",
            "entity",
            "entity_contracts",
            "collection_list",
        ] {
            assert!(
                names.contains(expected),
                "pre-existing tool '{}' must still be present in TOOL_LIST",
                expected
            );
        }
    }
}
