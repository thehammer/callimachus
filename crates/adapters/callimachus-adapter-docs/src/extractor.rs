/// Structured edge extraction from DocC JSON pages.
///
/// For each DocC page, produces:
/// - One primary entity with kind mapped from `metadata.symbolKind`.
/// - `inherits_from` edges from `relationships[type == "inheritsFrom"]`.
/// - `conforms_to` edges from `relationships[type == "conformsTo"]`.
/// - `references_type` edges from declaration token typeIdentifiers (de-duplicated).
/// - `member_of` edges from `topicSections[].identifiers[]` (child → parent).
/// - Availability text in the entity description prefix.
use std::collections::HashSet;

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

// ── Canonical name ────────────────────────────────────────────────────────────

/// Derive the canonical entity name from a DocC reference identifier.
///
/// Pattern: `doc://com.apple.documentation/documentation/<framework>/<slug>`
/// → take everything after `documentation/<framework>/`, replace `/` with `.`.
pub fn identifier_to_canonical_name(
    identifier: &str,
    references: &std::collections::HashMap<String, DoccReference>,
) -> String {
    // Prefer the human title from references if available.
    if let Some(r) = references.get(identifier)
        && !r.title.is_empty()
    {
        // For child symbols, the title is usually "NSView.tag" or similar.
        return r.title.clone();
    }

    // Fall back to path extraction.
    let stripped = identifier
        .trim_start_matches("doc://com.apple.documentation/documentation/");
    // Drop the framework prefix (first path component).
    let after_framework = stripped.split_once('/').map(|x| x.1).unwrap_or(stripped);
    // Convert path separators to dots and capitalize first letter.
    let parts: Vec<String> = after_framework
        .split('/')
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().to_string() + c.as_str(),
            }
        })
        .collect();
    parts.join(".")
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

    // ── Relationship edges (inheritsFrom, conformsTo) ─────────────────────────

    for rel in &page.relationships {
        let target_name = identifier_to_canonical_name(&rel.target, &page.references);
        let to_id = format!("docs:{}", target_name);
        let kind_str = match rel.kind.as_str() {
            "inheritsFrom" => "inherits_from",
            "conformsTo" => "conforms_to",
            _ => continue,
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
                let target_name = identifier_to_canonical_name(id, &page.references);
                if !target_name.is_empty() && referenced_types.insert(target_name.clone()) {
                    let to_id = format!("docs:{}", target_name);
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
            let child_name = identifier_to_canonical_name(child_id, &page.references);
            let child_entity_id = format!("docs:docs/{child_name}");
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
    fn identifier_to_canonical_name_basic() {
        let refs = std::collections::HashMap::new();
        let name = identifier_to_canonical_name(
            "doc://com.apple.documentation/documentation/appkit/nsview",
            &refs,
        );
        assert_eq!(name, "Nsview");
    }

    #[test]
    fn identifier_to_canonical_name_uses_reference_title() {
        let mut refs = std::collections::HashMap::new();
        refs.insert(
            "doc://com.apple.documentation/documentation/appkit/nsview".to_string(),
            DoccReference {
                title: "NSView".to_string(),
                ..Default::default()
            },
        );
        let name = identifier_to_canonical_name(
            "doc://com.apple.documentation/documentation/appkit/nsview",
            &refs,
        );
        assert_eq!(name, "NSView");
    }
}
