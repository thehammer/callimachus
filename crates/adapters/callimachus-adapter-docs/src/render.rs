/// Rust port of the v1 Python markdown renderer.
///
/// Produces markdown output that matches the v1 `fetch-apple-docs.py` renderer's
/// contract for the same DocC JSON input. Small whitespace differences are
/// acceptable; substantive content drift is not.
use std::collections::HashMap;

use crate::docc::{DoccDeclaration, DoccInlineNode, DoccPage, DoccReference};

// ── Inline rendering ──────────────────────────────────────────────────────────

/// Render a slice of inline content nodes to a markdown string.
pub fn render_inline(nodes: &[DoccInlineNode], references: &HashMap<String, DoccReference>) -> String {
    nodes.iter().map(|n| n.to_text(references)).collect::<Vec<_>>().join("")
}

/// Render a generic `serde_json::Value` inline-content array.
fn render_inline_value(nodes: &serde_json::Value, references: &HashMap<String, DoccReference>) -> String {
    let arr = match nodes.as_array() {
        Some(a) => a,
        None => return String::new(),
    };
    arr.iter()
        .map(|n| {
            let kind = n.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                "text" => n.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                "codeVoice" => {
                    let code = n.get("code").and_then(|v| v.as_str()).unwrap_or("");
                    format!("`{code}`")
                }
                "reference" => {
                    let id = n.get("identifier").and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(r) = references.get(id) {
                        r.title.clone()
                    } else {
                        id.rsplit('/').next().unwrap_or("").to_string()
                    }
                }
                _ => n.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

// ── Block / content node rendering ────────────────────────────────────────────

fn render_list_item_value(
    item: &serde_json::Value,
    references: &HashMap<String, DoccReference>,
    prefix: &str,
) -> String {
    let content_nodes = item.get("content").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let item_text = render_content_nodes_value(
        &serde_json::Value::Array(content_nodes),
        references,
    )
    .trim()
    .to_string();

    let mut out = String::new();
    let mut first = true;
    for line in item_text.lines() {
        if first {
            out.push_str(&format!("{prefix} {line}\n"));
            first = false;
        } else {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out
}

/// Render a `serde_json::Value` block-level content array to markdown.
///
/// This is the core renderer, matching the v1 Python `render_content_nodes` function.
pub fn render_content_nodes_value(
    nodes: &serde_json::Value,
    references: &HashMap<String, DoccReference>,
) -> String {
    let arr = match nodes.as_array() {
        Some(a) => a,
        None => return String::new(),
    };

    let mut parts = Vec::new();
    for node in arr {
        let t = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match t {
            "heading" => {
                let level = node.get("level").and_then(|v| v.as_u64()).unwrap_or(2);
                let level = level.max(2) as usize;
                let hashes = "#".repeat(level);
                let content = node.get("content").cloned().unwrap_or(serde_json::Value::Array(vec![]));
                let text = render_inline_value(&content, references);
                parts.push(format!("\n{hashes} {text}\n"));
            }
            "paragraph" => {
                let inline = node.get("inlineContent").cloned().unwrap_or(serde_json::Value::Array(vec![]));
                let text = render_inline_value(&inline, references);
                parts.push(format!("\n{text}\n"));
            }
            "aside" => {
                let style = node.get("style").and_then(|v| v.as_str()).unwrap_or("note");
                let style = capitalize_first(style);
                let content = node.get("content").cloned().unwrap_or(serde_json::Value::Array(vec![]));
                let inner = render_content_nodes_value(&content, references);
                let inner = inner.trim().to_string();
                let quoted: String = inner
                    .lines()
                    .map(|line| {
                        if line.is_empty() {
                            ">".to_string()
                        } else {
                            format!("> {line}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                parts.push(format!("\n> **{style}:**\n{quoted}\n"));
            }
            "codeListing" => {
                let syntax = node.get("syntax").and_then(|v| v.as_str()).unwrap_or("swift");
                let syntax = if syntax.is_empty() { "swift" } else { syntax };
                let code_lines: Vec<String> = node
                    .get("code")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|l| l.as_str())
                            .map(|s| s.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let code = code_lines.join("\n");
                parts.push(format!("\n```{syntax}\n{code}\n```\n"));
            }
            "unorderedList" => {
                let items = node.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let mut list_parts = String::from("\n");
                for item in &items {
                    list_parts.push_str(&render_list_item_value(item, references, "-"));
                }
                parts.push(list_parts);
            }
            "orderedList" => {
                let items = node.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let mut list_parts = String::from("\n");
                for (i, item) in items.iter().enumerate() {
                    let prefix = format!("{}.", i + 1);
                    list_parts.push_str(&render_list_item_value(item, references, &prefix));
                }
                parts.push(list_parts);
            }
            "links" => {
                // Render as plain text list — no link resolution in v1/v2 markdown output.
                let items = node.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                for item in &items {
                    let id = item
                        .as_str()
                        .or_else(|| item.get("identifier").and_then(|v| v.as_str()))
                        .unwrap_or("");
                    let title = references
                        .get(id)
                        .map(|r| r.title.as_str())
                        .unwrap_or_else(|| id.rsplit('/').next().unwrap_or(id));
                    parts.push(format!("\n- {title}\n"));
                }
            }
            _ => {
                // Best-effort fallback: try inlineContent, then recurse into content.
                let inline = node.get("inlineContent").cloned().unwrap_or(serde_json::Value::Array(vec![]));
                if !inline.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    let text = render_inline_value(&inline, references);
                    parts.push(format!("\n{text}\n"));
                }
                let content = node.get("content").cloned().unwrap_or(serde_json::Value::Array(vec![]));
                if !content.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    parts.push(render_content_nodes_value(&content, references));
                }
            }
        }
    }

    parts.join("")
}

// ── Top-level page renderers ──────────────────────────────────────────────────

/// Render the Swift declaration block from a page, if present.
pub fn render_declaration(page: &DoccPage) -> String {
    for section in &page.primary_content_sections {
        if section.kind != "declarations" {
            continue;
        }
        for decl in &section.declarations {
            let text = render_declaration_tokens(decl);
            if !text.trim().is_empty() {
                return format!("\n## Declaration\n\n```swift\n{}\n```\n", text.trim());
            }
        }
    }
    String::new()
}

fn render_declaration_tokens(decl: &DoccDeclaration) -> String {
    decl.tokens.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("")
}

/// Render the Discussion section (primaryContentSections of kind "content").
pub fn render_discussion(page: &DoccPage) -> String {
    let mut parts = Vec::new();
    for section in &page.primary_content_sections {
        if section.kind != "content" {
            continue;
        }
        let rendered = render_content_nodes_value(&section.content, &page.references);
        if !rendered.trim().is_empty() {
            parts.push(format!("\n## Discussion\n{rendered}"));
        }
    }
    parts.join("")
}

/// Render the Topics section from topicSections.
pub fn render_topics(page: &DoccPage) -> String {
    if page.topic_sections.is_empty() {
        return String::new();
    }
    let mut parts = vec!["\n## Topics\n".to_string()];
    for section in &page.topic_sections {
        if !section.title.is_empty() {
            parts.push(format!("\n### {}\n", section.title));
        }
        for id in &section.identifiers {
            if let Some(r) = page.references.get(id) {
                let title = &r.title;
                let abstract_text = render_inline(&r.abstract_text, &page.references);
                if abstract_text.is_empty() {
                    parts.push(format!("- **{title}**\n"));
                } else {
                    parts.push(format!("- **{title}** — {abstract_text}\n"));
                }
            } else {
                let fallback = id.rsplit('/').next().unwrap_or(id);
                parts.push(format!("- **{fallback}**\n"));
            }
        }
    }
    parts.join("")
}

/// Render an entire DocC JSON page to markdown (mirrors v1 Python `render_page`).
///
/// `framework` — the framework name (e.g. "AppKit").
/// `url` — the source URL this page was fetched from.
pub fn render_page(page: &DoccPage, framework: &str, url: &str) -> String {
    let title = &page.metadata.title;
    let symbol_kind = &page.metadata.symbol_kind;
    let abstract_text = render_inline(&page.abstract_text, &page.references);

    let mut lines = vec![format!("# {title}\n")];
    if !symbol_kind.is_empty() {
        lines.push(format!("**Kind:** {}", capitalize_first(symbol_kind)));
    }
    lines.push(format!("**Framework:** {framework}"));
    lines.push(format!("**Source URL:** {url}"));
    lines.push(String::new());

    if !abstract_text.is_empty() {
        lines.push(abstract_text);
        lines.push(String::new());
    }

    let mut doc = lines.join("\n");
    doc.push_str(&render_declaration(page));
    doc.push_str(&render_discussion(page));
    doc.push_str(&render_topics(page));

    doc
}

/// Render the Discussion prose for a single content section (for section-grain chunks).
pub fn render_section_content(content: &serde_json::Value, references: &HashMap<String, DoccReference>) -> String {
    render_content_nodes_value(content, references)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docc::DoccPage;

    #[test]
    fn render_paragraph_node() {
        let nodes = serde_json::json!([{
            "type": "paragraph",
            "inlineContent": [{"type": "text", "text": "Hello world"}]
        }]);
        let refs = HashMap::new();
        let result = render_content_nodes_value(&nodes, &refs);
        assert!(result.contains("Hello world"));
    }

    #[test]
    fn render_code_listing() {
        let nodes = serde_json::json!([{
            "type": "codeListing",
            "syntax": "swift",
            "code": ["let x = 1", "let y = 2"]
        }]);
        let refs = HashMap::new();
        let result = render_content_nodes_value(&nodes, &refs);
        assert!(result.contains("```swift"));
        assert!(result.contains("let x = 1"));
    }

    #[test]
    fn render_empty_page() {
        let page = DoccPage::default();
        let md = render_page(&page, "AppKit", "https://example.com");
        assert!(md.contains("**Framework:** AppKit"));
    }
}
