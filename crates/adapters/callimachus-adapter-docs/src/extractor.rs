/// Structured edge extraction from DocC JSON pages.
///
/// For each DocC page, produces:
/// - One primary entity with kind mapped from `metadata.symbolKind`.
/// - `inherits_from` edges from `relationships[type == "inheritsFrom"]`.
/// - `conforms_to` edges from `relationships[type == "conformsTo"]`.
/// - `references_type` edges from declaration token typeIdentifiers (de-duplicated).
/// - `member_of` edges from `topicSections[].identifiers[]` (child → parent).
/// - Availability text in the entity description prefix.
use std::collections::{HashMap, HashSet};

use callimachus_core::{
    adapter::ExtractedStructure,
    types::{Chunk, Edge, Entity},
};

use crate::docc::{DoccPage, DoccReference, macos_availability};

// ── Entity kind mapping ───────────────────────────────────────────────────────

/// Map a DocC `metadata.symbolKind` (or `roleHeading`) to a Callimachus entity kind.
pub fn map_symbol_kind(symbol_kind: &str, role_heading: &str) -> &'static str {
    match symbol_kind.to_lowercase().as_str() {
        "class" => "class",
        "struct" | "structure" => "struct",
        "enum" | "enumeration" => "enum",
        "protocol" => "protocol",
        "instm" | "method" | "func" | "func.op" => "method",
        "clm" => "method", // type method — noted in description
        "instp" | "property" | "var" => "property",
        "init" | "intfcm" | "initializer" => "initializer",
        "case" | "enumelt" => "enum_case",
        "notification" => "notification",
        "typealias" | "tdef" => "typealias",
        "constant" | "let" | "data" => "constant",
        _ => {
            // Fall back to role heading.
            match role_heading.to_lowercase().as_str() {
                s if s.contains("class") => "class",
                s if s.contains("struct") => "struct",
                s if s.contains("enum") && !s.contains("case") => "enum",
                s if s.contains("protocol") => "protocol",
                s if s.contains("method") || s.contains("function") => "method",
                s if s.contains("property") || s.contains("variable") => "property",
                s if s.contains("initializer") => "initializer",
                s if s.contains("case") => "enum_case",
                s if s.contains("notification") => "notification",
                s if s.contains("typealias") => "typealias",
                s if s.contains("constant") => "constant",
                _ => "docs_topic",
            }
        }
    }
}

// ── Entity ID from DocC identifier ───────────────────────────────────────────

/// Convert a DocC identifier to a canonical entity ID.
///
/// Pattern: `doc://com.apple.documentation/documentation/<framework>/<...slug...>`
/// Returns `docs:docs/<Framework>/<slug>` where:
/// - `<Framework>` is the proper-cased framework name (from `framework_hint` when it matches).
/// - `<slug>` is the path segments after the framework joined with `-`, using
///   `pathComponents` casing from the references map when available.
///
/// Returns `None` if:
/// - The identifier does not have the expected prefix.
/// - The framework segment does not match `framework_hint` (cross-framework
///   reference — we cannot reliably determine the casing, so we skip the edge).
pub fn identifier_to_entity_id(
    identifier: &str,
    references: &HashMap<String, DoccReference>,
    framework_hint: &str,
) -> Option<String> {
    let stripped =
        identifier.strip_prefix("doc://com.apple.documentation/documentation/")?;

    let (framework_id, _rest) = stripped.split_once('/')?;

    // Only proceed when framework matches the hint — preserves casing and avoids dangling cross-framework edges.
    if framework_hint.to_lowercase() != framework_id.to_lowercase() {
        return None;
    }
    let framework = framework_hint.to_string();

    // Prefer pathComponents from the references map (PascalCase).
    let slug = if let Some(reference) = references.get(identifier) {
        if !reference.path_components.is_empty() {
            reference.path_components.join("-")
        } else {
            // Reference present but no pathComponents: fall back to lowercase identifier segments.
            stripped
                .split('/')
                .skip(1)
                .collect::<Vec<_>>()
                .join("-")
                .to_lowercase()
        }
    } else {
        // No reference entry: fall back to lowercase identifier segments.
        stripped
            .split('/')
            .skip(1)
            .collect::<Vec<_>>()
            .join("-")
            .to_lowercase()
    };

    if slug.is_empty() {
        return None;
    }

    Some(format!("docs:docs/{framework}/{slug}"))
}

// ── Edge ID generation ────────────────────────────────────────────────────────

fn edge_id(from: &str, to: &str, kind: &str) -> String {
    format!("{kind}:{from}→{to}")
}

// ── Availability description prefix ──────────────────────────────────────────

fn availability_prefix(page: &DoccPage) -> String {
    if let Some((introduced, deprecated)) = macos_availability(page) {
        match (introduced, deprecated) {
            (Some(i), Some(d)) => format!("**Availability:** macOS {i}+ (deprecated in {d})\n\n"),
            (Some(i), None) => format!("**Availability:** macOS {i}+\n\n"),
            (None, Some(d)) => format!("**Availability:** deprecated in macOS {d}\n\n"),
            (None, None) => String::new(),
        }
    } else {
        String::new()
    }
}

// ── Main extractor ────────────────────────────────────────────────────────────

/// Extract entities and edges from a parsed DocC page and its chunk.
///
/// `entity_id` — the canonical entity ID for this page (used as the `from`
/// side of edges). The pipeline derives this from the entity's canonical name;
/// pass the chunk's primary entity ID.
pub fn extract_structure(
    chunk: &Chunk,
    page: &DoccPage,
    corpus_id: &str,
) -> anyhow::Result<ExtractedStructure> {
    let mut entities: Vec<Entity> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    if chunk.kind != "page" {
        // Section chunks: no additional entities or edges — the page chunk owns them.
        return Ok(ExtractedStructure {
            parent_path: chunk.parent_path.clone(),
            child_paths: vec![],
            structural_entities: entities,
            structural_edges: edges,
        });
    }

    // ── Primary entity ────────────────────────────────────────────────────────

    let title = &page.metadata.title;
    let symbol_kind = &page.metadata.symbol_kind;
    let role_heading = &page.metadata.role_heading;
    let kind = map_symbol_kind(symbol_kind, role_heading);

    // Build description: availability prefix + Discussion prose (first 2000 chars).
    let avail = availability_prefix(page);
    let discussion_prose: String = page
        .primary_content_sections
        .iter()
        .filter(|s| s.kind == "content")
        .map(|s| crate::render::render_section_content(&s.content, &page.references))
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(2000)
        .collect();

    let description = if avail.is_empty() && discussion_prose.is_empty() {
        None
    } else {
        Some(format!("{avail}{discussion_prose}"))
    };

    let entity_id = format!("docs:{}", chunk.location.path);

    let entity = Entity {
        id: entity_id.clone(),
        corpus_id: corpus_id.to_string(),
        canonical_name: title.clone(),
        kind: kind.to_string(),
        abstract_kind: String::new(),
        aliases: vec![],
        description,
        first_location: Some(chunk.location.clone()),
        last_location: Some(chunk.location.clone()),
        appearance_count: 1,
        confidence: 0.95,
        derived_at_version: None,
    };
    entities.push(entity);

    // Framework hint for resolving same-framework edge IDs.
    let framework_hint = page
        .metadata
        .modules
        .first()
        .map(|m| m.name.as_str())
        .unwrap_or("");

    // ── Relationship edges (inheritsFrom, conformsTo) ─────────────────────────

    for rel in &page.relationships {
        let kind_str = match rel.kind.as_str() {
            "inheritsFrom" => "inherits_from",
            "conformsTo" => "conforms_to",
            _ => continue,
        };
        let Some(to_id) =
            identifier_to_entity_id(&rel.target, &page.references, framework_hint)
        else {
            continue;
        };
        edges.push(Edge {
            id: edge_id(&entity_id, &to_id, kind_str),
            corpus_id: corpus_id.to_string(),
            from_entity_id: entity_id.clone(),
            to_entity_id: to_id,
            kind: kind_str.to_string(),
            location: chunk.location.clone(),
            confidence: 0.95,
            derived_at_version: None,
        });
    }

    // ── references_type edges from declaration token typeIdentifiers ──────────

    let mut referenced_types: HashSet<String> = HashSet::new();
    for section in &page.primary_content_sections {
        if section.kind != "declarations" {
            continue;
        }
        for decl in &section.declarations {
            for token in &decl.tokens {
                if token.kind != "typeIdentifier" {
                    continue;
                }
                let id = token
                    .identifier
                    .as_deref()
                    .or(token.precise_identifier.as_deref())
                    .unwrap_or(&token.text);
                if id.is_empty() {
                    continue;
                }
                let Some(to_id) =
                    identifier_to_entity_id(id, &page.references, framework_hint)
                else {
                    continue;
                };
                if referenced_types.insert(to_id.clone()) {
                    edges.push(Edge {
                        id: edge_id(&entity_id, &to_id, "references_type"),
                        corpus_id: corpus_id.to_string(),
                        from_entity_id: entity_id.clone(),
                        to_entity_id: to_id,
                        kind: "references_type".to_string(),
                        location: chunk.location.clone(),
                        confidence: 0.8,
                        derived_at_version: None,
                    });
                }
            }
        }
    }

    // ── member_of edges (child → this page) ──────────────────────────────────

    for topic_section in &page.topic_sections {
        for child_id in &topic_section.identifiers {
            let Some(child_entity_id) =
                identifier_to_entity_id(child_id, &page.references, framework_hint)
            else {
                continue;
            };
            edges.push(Edge {
                id: edge_id(&child_entity_id, &entity_id, "member_of"),
                corpus_id: corpus_id.to_string(),
                from_entity_id: child_entity_id,
                to_entity_id: entity_id.clone(),
                kind: "member_of".to_string(),
                location: chunk.location.clone(),
                confidence: 0.9,
                derived_at_version: None,
            });
        }
    }

    Ok(ExtractedStructure {
        parent_path: chunk.parent_path.clone(),
        child_paths: vec![],
        structural_entities: entities,
        structural_edges: edges,
    })
}

/// Convenience wrapper: parse the raw JSON, chunk it, and extract structure for the page chunk.
pub fn extract_from_value(
    raw: &serde_json::Value,
    corpus_id: &str,
    file_path: &std::path::Path,
    root: &std::path::Path,
) -> anyhow::Result<(Vec<Chunk>, ExtractedStructure)> {
    use crate::docc::DoccPage;

    let page = DoccPage::from_value(raw);
    let chunks = crate::chunker::chunk_docs_file(corpus_id, file_path, root, &page, raw);

    // Extract structure from the page chunk (first one).
    let page_chunk = chunks
        .iter()
        .find(|c| c.kind == "page")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no page chunk produced for {:?}", file_path))?;

    let structure = extract_structure(&page_chunk, &page, corpus_id)?;
    Ok((chunks, structure))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_symbol_kind_class() {
        assert_eq!(map_symbol_kind("class", ""), "class");
    }

    #[test]
    fn map_symbol_kind_instm() {
        assert_eq!(map_symbol_kind("instm", ""), "method");
    }

    #[test]
    fn map_symbol_kind_clm() {
        assert_eq!(map_symbol_kind("clm", ""), "method");
    }

    #[test]
    fn map_symbol_kind_instp() {
        assert_eq!(map_symbol_kind("instp", ""), "property");
    }

    #[test]
    fn map_symbol_kind_fallback() {
        assert_eq!(map_symbol_kind("unknown_xyz", ""), "docs_topic");
    }

    #[test]
    fn identifier_to_entity_id_basic_with_path_components() {
        let mut refs = HashMap::new();
        refs.insert(
            "doc://com.apple.documentation/documentation/appkit/nsview".to_string(),
            DoccReference {
                path_components: vec!["NSView".to_string()],
                ..Default::default()
            },
        );
        let id = identifier_to_entity_id(
            "doc://com.apple.documentation/documentation/appkit/nsview",
            &refs,
            "AppKit",
        );
        assert_eq!(id, Some("docs:docs/AppKit/NSView".to_string()));
    }

    #[test]
    fn identifier_to_entity_id_member_slug() {
        let mut refs = HashMap::new();
        refs.insert(
            "doc://com.apple.documentation/documentation/appkit/nsview/tag".to_string(),
            DoccReference {
                path_components: vec!["NSView".to_string(), "tag".to_string()],
                ..Default::default()
            },
        );
        let id = identifier_to_entity_id(
            "doc://com.apple.documentation/documentation/appkit/nsview/tag",
            &refs,
            "AppKit",
        );
        assert_eq!(id, Some("docs:docs/AppKit/NSView-tag".to_string()));
    }

    #[test]
    fn identifier_to_entity_id_cross_framework_returns_none() {
        let refs = HashMap::new();
        let id = identifier_to_entity_id(
            "doc://com.apple.documentation/documentation/foundation/nsobject",
            &refs,
            "AppKit",
        );
        assert_eq!(id, None, "cross-framework reference should return None");
    }

    #[test]
    fn identifier_to_entity_id_wrong_prefix_returns_none() {
        let refs = HashMap::new();
        let id = identifier_to_entity_id("https://example.com/nsview", &refs, "AppKit");
        assert_eq!(id, None);
    }
}
