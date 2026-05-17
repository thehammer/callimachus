use std::path::Path;

use callimachus_core::types::{Chunk, Location};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use regex::Regex;
use walkdir::WalkDir;

/// Options controlling how a wiki directory is chunked.
pub struct WikiChunkOptions {
    pub max_section_bytes: usize,
    pub min_section_bytes: usize,
    pub exclude_globs: Vec<String>,
}

impl Default for WikiChunkOptions {
    fn default() -> Self {
        Self {
            max_section_bytes: 3000,
            min_section_bytes: 50,
            exclude_globs: vec![".git/**".into()],
        }
    }
}

/// Front-matter extracted from YAML fences at the top of a Markdown file.
#[derive(Debug, Default, Clone)]
pub struct FrontMatter {
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub raw: serde_json::Value,
}

/// Parse YAML front-matter delimited by `---` at the top of the file.
/// Uses simple regex — no YAML parser dependency.
pub fn extract_front_matter(content: &str) -> (FrontMatter, &str) {
    if !content.starts_with("---") {
        return (FrontMatter::default(), content);
    }
    let rest = &content[3..];
    let end = match rest.find("\n---") {
        Some(i) => i,
        None => return (FrontMatter::default(), content),
    };
    let yaml = &rest[..end];
    let after = &rest[end + 4..];
    let after = after.trim_start_matches('\n');

    let mut fm = FrontMatter::default();
    let mut raw_map = serde_json::Map::new();

    // Extract key: value pairs (scalar or YAML list).
    let kv_re = Regex::new(r"(?m)^(\w+):\s*(.+)$").unwrap();
    let list_start_re = Regex::new(r"(?m)^(\w+):\s*$").unwrap();
    let list_item_re = Regex::new(r"(?m)^\s+-\s+(.+)$").unwrap();

    // Simple two-pass: scalar fields first.
    for cap in kv_re.captures_iter(yaml) {
        let key = cap[1].trim();
        let val = cap[2].trim().trim_matches('"').trim_matches('\'');
        raw_map.insert(key.to_string(), serde_json::Value::String(val.to_string()));
        if key == "title" {
            fm.title = Some(val.to_string());
        }
    }

    // List fields: look for `key:` with items on subsequent lines.
    for cap in list_start_re.captures_iter(yaml) {
        let key = &cap[1];
        // Find the block after `key:\n`.
        if let Some(start) = yaml.find(&format!("{key}:\n")) {
            let block = &yaml[start + key.len() + 2..];
            let items: Vec<String> = list_item_re
                .captures_iter(block)
                .map(|c| c[1].trim().trim_matches('"').trim_matches('\'').to_string())
                .collect();
            if !items.is_empty() {
                let arr: Vec<serde_json::Value> = items
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect();
                raw_map.insert(key.to_string(), serde_json::Value::Array(arr));
                match key {
                    "tags" => fm.tags = items,
                    "aliases" => fm.aliases = items,
                    _ => {}
                }
            }
        }
    }

    fm.raw = serde_json::Value::Object(raw_map);
    (fm, after)
}

/// Convert a Markdown heading text to a GitHub-style anchor slug.
pub fn heading_slug(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' { '-' } else { c })
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect()
}

/// A parsed section within a Markdown file.
#[derive(Debug, Clone)]
pub struct WikiSection {
    pub heading: Option<String>,
    pub level: u32,
    pub content: String,
    pub slug: Option<String>,
}

/// Parse a Markdown file into sections split at `##`+ headings.
/// The text before the first heading is the "intro" section (level 0, no heading).
pub fn split_sections(content: &str) -> Vec<WikiSection> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(content, opts);

    let mut sections: Vec<WikiSection> = Vec::new();
    let mut current_content = String::new();
    let mut current_heading: Option<String> = None;
    let mut current_level: u32 = 0;
    let mut current_slug: Option<String> = None;
    let mut heading_buf = String::new();
    let mut in_heading = false;
    let mut heading_level: u32 = 0;

    let flush = |sections: &mut Vec<WikiSection>,
                 content: &mut String,
                 heading: &mut Option<String>,
                 level: &mut u32,
                 slug: &mut Option<String>| {
        let trimmed = content.trim().to_string();
        if !trimmed.is_empty() {
            sections.push(WikiSection {
                heading: heading.clone(),
                level: *level,
                content: trimmed,
                slug: slug.clone(),
            });
        }
        content.clear();
        *heading = None;
        *slug = None;
        *level = 0;
    };

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                in_heading = true;
                heading_level = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                heading_buf.clear();
            }
            Event::Text(t) if in_heading => {
                heading_buf.push_str(&t);
            }
            Event::End(TagEnd::Heading(_)) => {
                in_heading = false;
                // Only split on ## and deeper (level >= 2).
                // H1 is treated as "part of the intro section header" — we capture its
                // text but don't start a new section yet.
                if heading_level >= 2 {
                    flush(
                        &mut sections,
                        &mut current_content,
                        &mut current_heading,
                        &mut current_level,
                        &mut current_slug,
                    );
                    let slug = heading_slug(&heading_buf);
                    current_heading = Some(heading_buf.clone());
                    current_slug = Some(slug);
                    current_level = heading_level;
                }
                // Always write the heading text back into content so the raw markdown
                // is preserved (makes snippets meaningful).
                let prefix = "#".repeat(heading_level as usize);
                current_content.push_str(&format!("{prefix} {}\n\n", heading_buf));
                heading_buf.clear();
            }
            Event::Text(t) => {
                current_content.push_str(&t);
                current_content.push('\n');
            }
            Event::SoftBreak | Event::HardBreak => {
                current_content.push('\n');
            }
            Event::Code(c) => {
                current_content.push('`');
                current_content.push_str(&c);
                current_content.push('`');
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                current_content.push('\n');
            }
            _ => {}
        }
    }

    flush(
        &mut sections,
        &mut current_content,
        &mut current_heading,
        &mut current_level,
        &mut current_slug,
    );

    sections
}

/// Walk a wiki directory and produce `Chunk`s for all Markdown files.
///
/// Returns one `kind = "page"` chunk per file (full content) plus one
/// `kind = "section"` chunk per `##`+ heading section.
pub async fn chunk_wiki_directory(
    corpus_id: &str,
    source_path: &Path,
    opts: &WikiChunkOptions,
) -> anyhow::Result<Vec<Chunk>> {
    let mut chunks = Vec::new();

    for entry in WalkDir::new(source_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        // Check exclusion globs using simple prefix/contains matching.
        let rel = match path.strip_prefix(source_path) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let rel_normalized = rel.replace('\\', "/");
        let excluded = opts.exclude_globs.iter().any(|glob| {
            // Support patterns like ".git/**" → check if path starts with ".git/"
            let prefix = glob.trim_end_matches("/**").trim_end_matches("/*");
            rel_normalized == prefix
                || rel_normalized.starts_with(&format!("{prefix}/"))
                || rel_normalized.contains(prefix)
        });
        if excluded {
            continue;
        }

        // Derive the page's location path: wiki/<relative-stem>.
        let page_path = {
            let stem = path.with_extension("");
            let rel_stem = match stem.strip_prefix(source_path) {
                Ok(r) => r.to_string_lossy().to_string(),
                Err(_) => continue,
            };
            // Normalize path separators.
            format!("wiki/{}", rel_stem.replace('\\', "/"))
        };

        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "wiki: failed to read file");
                continue;
            }
        };

        let (front_matter, body) = extract_front_matter(&raw);

        // Page chunk (full content, including front-matter).
        let page_loc = Location::new(corpus_id, &page_path);
        let page_chunk = Chunk::new(
            corpus_id.to_string(),
            None,
            "page".to_string(),
            page_loc.clone(),
            raw.clone(),
        );
        chunks.push(page_chunk.clone());

        // Section chunks.
        let sections = split_sections(body);
        for section in &sections {
            if section.content.len() < opts.min_section_bytes {
                continue;
            }

            // Truncate oversized sections.
            let content = if section.content.len() > opts.max_section_bytes {
                section.content[..opts.max_section_bytes].to_string()
            } else {
                section.content.clone()
            };

            let section_path = if let Some(slug) = &section.slug {
                format!("{page_path}#{slug}")
            } else {
                format!("{page_path}#intro")
            };

            let section_loc = Location::new(corpus_id, &section_path);
            let section_chunk = Chunk::new(
                corpus_id.to_string(),
                Some(page_path.clone()),
                "section".to_string(),
                section_loc,
                content,
            );
            chunks.push(section_chunk);
        }

        let _ = front_matter; // Used by extractor, not needed here directly.
    }

    Ok(chunks)
}

/// Convenience: chunk a single `.md` file.
pub async fn chunk_single_file(
    corpus_id: &str,
    file_path: &Path,
    root: &Path,
) -> anyhow::Result<Vec<Chunk>> {
    let opts = WikiChunkOptions::default();

    let rel_stem = {
        let stem = file_path.with_extension("");
        stem.strip_prefix(root)
            .map(|r| r.to_string_lossy().to_string())
            .unwrap_or_else(|_| {
                file_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            })
    };
    let page_path = format!("wiki/{}", rel_stem.replace('\\', "/"));

    let raw = std::fs::read_to_string(file_path)?;
    let (_, body) = extract_front_matter(&raw);

    let mut chunks = Vec::new();

    let page_loc = Location::new(corpus_id, &page_path);
    chunks.push(Chunk::new(
        corpus_id.to_string(),
        None,
        "page".to_string(),
        page_loc,
        raw.clone(),
    ));

    let sections = split_sections(body);
    for section in &sections {
        if section.content.len() < opts.min_section_bytes {
            continue;
        }
        let content = if section.content.len() > opts.max_section_bytes {
            section.content[..opts.max_section_bytes].to_string()
        } else {
            section.content.clone()
        };

        let section_path = if let Some(slug) = &section.slug {
            format!("{page_path}#{slug}")
        } else {
            format!("{page_path}#intro")
        };

        let section_loc = Location::new(corpus_id, &section_path);
        chunks.push(Chunk::new(
            corpus_id.to_string(),
            Some(page_path.clone()),
            "section".to_string(),
            section_loc,
            content,
        ));
    }

    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_slug_standard() {
        assert_eq!(heading_slug("OAuth Flow"), "oauth-flow");
        assert_eq!(heading_slug("My Section!"), "my-section");
        assert_eq!(heading_slug("Hello World 2"), "hello-world-2");
    }

    #[test]
    fn extract_front_matter_title_and_tags() {
        let content = "---\ntitle: My Page\ntags:\n  - foo\n  - bar\n---\n# Heading\n\nBody text.";
        let (fm, rest) = extract_front_matter(content);
        assert_eq!(fm.title, Some("My Page".into()));
        assert_eq!(fm.tags, vec!["foo", "bar"]);
        assert!(rest.contains("# Heading"));
    }

    #[test]
    fn extract_front_matter_aliases() {
        let content = "---\naliases:\n  - old name\n  - another\n---\nBody.";
        let (fm, _) = extract_front_matter(content);
        assert_eq!(fm.aliases, vec!["old name", "another"]);
    }

    #[test]
    fn extract_front_matter_missing() {
        let content = "# No front matter\n\nJust body.";
        let (fm, rest) = extract_front_matter(content);
        assert!(fm.title.is_none());
        assert_eq!(rest, content);
    }

    #[test]
    fn split_sections_basic() {
        let md =
            "Intro text.\n\n## Section One\n\nContent here.\n\n## Section Two\n\nMore content.";
        let sections = split_sections(md);
        // Should have 3: intro + two sections.
        assert!(sections.len() >= 2);
        let has_intro = sections.iter().any(|s| s.heading.is_none());
        let section_headings: Vec<_> = sections
            .iter()
            .filter_map(|s| s.heading.as_deref())
            .collect();
        assert!(has_intro);
        assert!(section_headings.iter().any(|h| h.contains("Section One")));
    }

    #[test]
    fn split_sections_with_h1() {
        let md = "# Title\n\nIntro paragraph.\n\n## Sub Section\n\nContent.";
        let sections = split_sections(md);
        // H1 should be part of intro, not start a new section.
        assert!(
            sections
                .iter()
                .any(|s| s.content.contains("Intro paragraph"))
        );
        assert!(
            sections
                .iter()
                .any(|s| { s.heading.as_deref() == Some("Sub Section") })
        );
    }
}
