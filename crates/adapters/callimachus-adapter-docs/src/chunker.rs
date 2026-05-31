/// Chunk a single DocC JSON file into page + section chunks.
///
/// - One **page-grain** chunk per file: full rendered markdown (via `render::render_page`).
/// - One **section-grain** chunk per `primaryContentSections[]` entry with `kind == "content"`.
use std::path::Path;

use callimachus_core::types::{Chunk, Location};

use crate::{docc::DoccPage, render};

/// Derive the framework name from the path: the first path component under root.
pub fn framework_from_path(file_path: &Path, root: &Path) -> String {
    if let Ok(rel) = file_path.strip_prefix(root)
        && let Some(first) = rel.components().next()
    {
        return first.as_os_str().to_string_lossy().to_string();
    }
    "unknown".to_string()
}

/// Derive the slug from the file stem.
pub fn slug_from_path(file_path: &Path) -> String {
    file_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

/// Build a calli:// URI for a docs page.
pub fn page_uri(corpus_id: &str, framework: &str, slug: &str) -> String {
    format!("calli://{corpus_id}/docs/{framework}/{slug}")
}

/// Build a calli:// URI for a docs section.
pub fn section_uri(corpus_id: &str, framework: &str, slug: &str, anchor: &str) -> String {
    format!("calli://{corpus_id}/docs/{framework}/{slug}#{anchor}")
}

/// Produce a section anchor from a section title.
pub fn section_anchor(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' { '-' } else { c })
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect()
}

/// Chunk a single DocC JSON file.
///
/// Returns one page chunk and zero or more section chunks.
pub fn chunk_docs_file(
    corpus_id: &str,
    file_path: &Path,
    root: &Path,
    page: &DoccPage,
    raw_json: &serde_json::Value,
) -> Vec<Chunk> {
    let framework = framework_from_path(file_path, root);
    let slug = slug_from_path(file_path);

    // Reconstruct the source URL from the file path structure.
    // Pattern: https://developer.apple.com/tutorials/data/documentation/<framework>/<slug>.json
    let url = format!(
        "https://developer.apple.com/tutorials/data/documentation/{}/{}",
        framework.to_lowercase(),
        slug.to_lowercase()
    );

    let page_md = render::render_page(page, &framework, &url);
    let page_path = format!("docs/{framework}/{slug}");
    let page_loc = Location::new(corpus_id, &page_path);

    let mut chunks = Vec::new();

    // Page-grain chunk.
    chunks.push(Chunk::new(
        corpus_id.to_string(),
        None,
        "page".to_string(),
        page_loc.clone(),
        page_md,
    ));

    // Section-grain chunks: one per primaryContentSection with kind == "content".
    // Multiple content sections get a numeric suffix to avoid anchor collisions.
    let sections = raw_json
        .get("primaryContentSections")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let base_anchor = section_anchor(&page.metadata.title);
    let mut content_section_count: u32 = 0;

    for section in &sections {
        if section.get("kind").and_then(|v| v.as_str()) != Some("content") {
            continue;
        }
        let content = section.get("content").cloned().unwrap_or(serde_json::Value::Array(vec![]));
        let rendered = render::render_section_content(&content, &page.references);
        if rendered.trim().is_empty() {
            continue;
        }

        // First content section keeps the base anchor; subsequent ones get "-<n>" suffix.
        let anchor = if content_section_count == 0 {
            base_anchor.clone()
        } else {
            format!("{base_anchor}-{content_section_count}")
        };
        content_section_count += 1;

        let section_path = format!("docs/{framework}/{slug}#{anchor}");
        let section_loc = Location::new(corpus_id, &section_path);

        chunks.push(Chunk::new(
            corpus_id.to_string(),
            Some(page_path.clone()),
            "section".to_string(),
            section_loc,
            rendered,
        ));
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framework_from_path_basic() {
        let root = Path::new("/data/docs");
        let file = Path::new("/data/docs/AppKit/NSView.json");
        assert_eq!(framework_from_path(file, root), "AppKit");
    }

    #[test]
    fn section_anchor_basic() {
        assert_eq!(section_anchor("NSView"), "nsview");
        assert_eq!(section_anchor("My Class"), "my-class");
    }

    #[test]
    fn multiple_content_sections_get_distinct_paths() {
        use crate::docc::{DoccMetadata, DoccPage};

        let page = DoccPage {
            metadata: DoccMetadata {
                title: "MyClass".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        // Two content sections in the raw JSON.
        let raw = serde_json::json!({
            "primaryContentSections": [
                {
                    "kind": "content",
                    "content": [{"type": "paragraph", "inlineContent": [{"type": "text", "text": "First section."}]}]
                },
                {
                    "kind": "content",
                    "content": [{"type": "paragraph", "inlineContent": [{"type": "text", "text": "Second section."}]}]
                }
            ]
        });

        let root = Path::new("/data/docs");
        let file = Path::new("/data/docs/AppKit/MyClass.json");
        let chunks = chunk_docs_file("corpus", file, root, &page, &raw);

        let section_paths: Vec<&str> = chunks
            .iter()
            .filter(|c| c.kind == "section")
            .map(|c| c.location.path.as_str())
            .collect();

        assert_eq!(section_paths.len(), 2, "expected 2 section chunks");
        assert_ne!(
            section_paths[0], section_paths[1],
            "multiple content sections must have distinct location paths"
        );
        // First keeps the base anchor, second gets a suffix.
        assert!(
            section_paths[0].ends_with("#myclass"),
            "first section anchor = base: {}",
            section_paths[0]
        );
        assert!(
            section_paths[1].ends_with("#myclass-1"),
            "second section anchor = base-1: {}",
            section_paths[1]
        );
    }
}
