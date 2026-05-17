use std::path::Path;

use callimachus_core::{
    adapter::ExtractedStructure,
    types::{Chunk, Edge, Entity},
};
use regex::Regex;
use std::sync::OnceLock;

use crate::{
    chunker::extract_front_matter,
    links::{WikiLink, WikiLinkKind, extract_links},
};

static ENTITY_RE: OnceLock<Regex> = OnceLock::new();

fn entity_re() -> &'static Regex {
    ENTITY_RE.get_or_init(|| Regex::new(r"\b[A-Z][a-z]+(?:\s[A-Z][a-z]+)+\b").unwrap())
}

/// Structural information extracted from a wiki page.
#[derive(Debug, Default)]
pub struct WikiStructure {
    pub page_title: Option<String>,
    pub front_matter: serde_json::Value,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub links: Vec<WikiLink>,
    /// Bare capitalized multi-word terms as candidate entity mentions.
    pub mentioned_entities: Vec<String>,
}

/// Extract structural information from a wiki chunk.
///
/// For `kind = "page"` chunks: extracts front-matter, page title, all links,
/// and entity mentions.
/// For `kind = "section"` chunks: extracts links within the section content.
pub fn extract_structure(chunk: &Chunk, _source_path: &Path) -> anyhow::Result<WikiStructure> {
    let page_rel = chunk
        .location
        .path
        .trim_start_matches("wiki/")
        .split('#')
        .next()
        .unwrap_or("")
        .to_string();

    let mut ws = WikiStructure::default();

    if chunk.kind == "page" {
        let (fm, body) = extract_front_matter(&chunk.content);
        ws.page_title = fm.title.clone().or_else(|| {
            // Fallback: first H1 in body.
            body.lines()
                .find(|l| l.starts_with("# "))
                .map(|l| l[2..].trim().to_string())
        });
        ws.front_matter = fm.raw.clone();
        ws.tags = fm.tags.clone();
        ws.aliases = fm.aliases.clone();

        // Extract all links (wikilinks + markdown links).
        ws.links = extract_links(&page_rel, &chunk.content);

        // Extract mentioned entities from body text.
        ws.mentioned_entities = entity_re()
            .find_iter(body)
            .map(|m| m.as_str().to_string())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
    } else if chunk.kind == "section" {
        ws.links = extract_links(&page_rel, &chunk.content);
        ws.mentioned_entities = entity_re()
            .find_iter(&chunk.content)
            .map(|m| m.as_str().to_string())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
    }

    Ok(ws)
}

/// Classify the entity kind for a wiki page.
///
/// Uses `type` or `category` front-matter if present, otherwise defaults to `"topic"`.
pub fn classify_entity_kind(front_matter: &serde_json::Value) -> String {
    if let Some(kind) = front_matter.get("type").and_then(|v| v.as_str()) {
        return kind.to_string();
    }
    if let Some(cat) = front_matter.get("category").and_then(|v| v.as_str()) {
        return cat.to_string();
    }
    "topic".to_string()
}

/// Map a `WikiStructure` to an `ExtractedStructure` for the indexing pipeline.
pub fn to_extracted_structure(
    chunk: &Chunk,
    ws: &WikiStructure,
    corpus_id: &str,
) -> anyhow::Result<ExtractedStructure> {
    let mut entities: Vec<Entity> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    if chunk.kind == "page" {
        let kind = classify_entity_kind(&ws.front_matter);
        let name = ws.page_title.clone().unwrap_or_else(|| {
            // Derive from path.
            chunk
                .location
                .path
                .trim_start_matches("wiki/")
                .replace(['-', '_'], " ")
        });

        let page_entity = Entity {
            id: format!("page:{}", chunk.location.path),
            corpus_id: corpus_id.to_string(),
            canonical_name: name,
            kind,
            aliases: ws.aliases.clone(),
            description: None,
            first_location: Some(chunk.location.clone()),
            last_location: Some(chunk.location.clone()),
            appearance_count: 1,
            confidence: 0.9,
        };
        entities.push(page_entity.clone());

        // Create `references` edges for each wikilink/markdown link.
        for link in &ws.links {
            if link.kind == WikiLinkKind::External {
                continue; // Don't create edges for external links.
            }
            let to_id = format!("page:wiki/{}", link.to_page.replace(' ', "_"));
            edges.push(Edge {
                id: format!("ref:{}→{}", page_entity.id, to_id),
                corpus_id: corpus_id.to_string(),
                from_entity_id: page_entity.id.clone(),
                to_entity_id: to_id,
                kind: "references".to_string(),
                location: chunk.location.clone(),
                confidence: 0.8,
            });
        }

        // Low-confidence `mentions` edges for capitalized entity candidates.
        for mention in &ws.mentioned_entities {
            let mention_id = format!("mention:{mention}");
            edges.push(Edge {
                id: format!("mention:{}→{}", page_entity.id, mention_id),
                corpus_id: corpus_id.to_string(),
                from_entity_id: page_entity.id.clone(),
                to_entity_id: mention_id,
                kind: "mentions".to_string(),
                location: chunk.location.clone(),
                confidence: 0.4,
            });
        }
    } else if chunk.kind == "section" {
        // Section-level entity.
        let heading = chunk
            .content
            .lines()
            .find(|l| l.starts_with('#'))
            .map(|l| l.trim_start_matches('#').trim().to_string())
            .unwrap_or_else(|| chunk.location.path.clone());

        let section_entity = Entity {
            id: format!("section:{}", chunk.location.path),
            corpus_id: corpus_id.to_string(),
            canonical_name: heading,
            kind: "section".to_string(),
            aliases: vec![],
            description: None,
            first_location: Some(chunk.location.clone()),
            last_location: Some(chunk.location.clone()),
            appearance_count: 1,
            confidence: 0.8,
        };
        entities.push(section_entity.clone());

        // Section-level references edges.
        for link in &ws.links {
            if link.kind == WikiLinkKind::External {
                continue;
            }
            let to_id = format!("page:wiki/{}", link.to_page.replace(' ', "_"));
            edges.push(Edge {
                id: format!("ref:{}→{}", section_entity.id, to_id),
                corpus_id: corpus_id.to_string(),
                from_entity_id: section_entity.id.clone(),
                to_entity_id: to_id,
                kind: "references".to_string(),
                location: chunk.location.clone(),
                confidence: 0.8,
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

#[cfg(test)]
mod tests {
    use super::*;
    use callimachus_core::types::{Chunk, Location};

    #[test]
    fn classify_uses_type_field() {
        let fm = serde_json::json!({ "type": "person" });
        assert_eq!(classify_entity_kind(&fm), "person");
    }

    #[test]
    fn classify_uses_category_field() {
        let fm = serde_json::json!({ "category": "organization" });
        assert_eq!(classify_entity_kind(&fm), "organization");
    }

    #[test]
    fn classify_defaults_to_topic() {
        let fm = serde_json::json!({});
        assert_eq!(classify_entity_kind(&fm), "topic");
    }

    #[test]
    fn extract_structure_page_chunk() {
        let content = "---\ntitle: Characters\ntype: character-index\ntags:\n  - wh40k\n---\n\nSee [[Eisenhorn]] for details.\n\nGregor Eisenhorn is an inquisitor.";
        let loc = Location::new("mywiki", "wiki/characters");
        let chunk = Chunk::new(
            "mywiki".into(),
            None,
            "page".into(),
            loc,
            content.to_string(),
        );

        let ws = extract_structure(&chunk, Path::new("/")).unwrap();
        assert_eq!(ws.page_title, Some("Characters".into()));
        assert_eq!(ws.tags, vec!["wh40k"]);
        assert!(!ws.links.is_empty());
        assert!(ws.links.iter().any(|l| l.to_page == "Eisenhorn"));
    }

    #[test]
    fn to_extracted_structure_creates_references_edge() {
        let content = "See [[Eisenhorn]] for details.";
        let loc = Location::new("mywiki", "wiki/characters");
        let chunk = Chunk::new(
            "mywiki".into(),
            None,
            "page".into(),
            loc,
            content.to_string(),
        );
        let ws = extract_structure(&chunk, Path::new("/")).unwrap();
        let extracted = to_extracted_structure(&chunk, &ws, "mywiki").unwrap();

        let ref_edges: Vec<_> = extracted
            .structural_edges
            .iter()
            .filter(|e| e.kind == "references")
            .collect();
        assert!(!ref_edges.is_empty());
    }
}
