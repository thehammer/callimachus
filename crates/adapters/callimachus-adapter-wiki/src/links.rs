use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use regex::Regex;
use std::sync::OnceLock;

/// A resolved link found in a wiki page.
#[derive(Debug, Clone, PartialEq)]
pub struct WikiLink {
    /// Relative path of the source page (without `.md` extension).
    pub from_page: String,
    /// Resolved relative path of the target page, or bare title if unresolved.
    pub to_page: String,
    pub anchor: Option<String>,
    pub display_text: Option<String>,
    pub kind: WikiLinkKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WikiLinkKind {
    /// `[[Target Page]]` or `[[Target Page|Display text]]`
    Wikilink,
    /// `[Display text](../path/to/page.md)`
    Markdown,
    /// `[text](https://...)` — recorded but not followed
    External,
}

static WIKILINK_RE: OnceLock<Regex> = OnceLock::new();

fn wikilink_re() -> &'static Regex {
    WIKILINK_RE
        .get_or_init(|| Regex::new(r"\[\[([^\]|#]+)(?:#([^\]|]*))?(?:\|([^\]]*))?\]\]").unwrap())
}

/// Extract all links from a Markdown source string.
///
/// `source_page` is the relative path of the file being parsed (without `.md`).
pub fn extract_links(source_page: &str, content: &str) -> Vec<WikiLink> {
    let mut links = Vec::new();

    // ── 1. Wikilinks via regex ───────────────────────────────────────────────
    for cap in wikilink_re().captures_iter(content) {
        let target = cap[1].trim().to_string();
        let anchor = cap.get(2).map(|m| m.as_str().to_string());
        let display = cap.get(3).map(|m| m.as_str().to_string());

        links.push(WikiLink {
            from_page: source_page.to_string(),
            to_page: target,
            anchor,
            display_text: display,
            kind: WikiLinkKind::Wikilink,
        });
    }

    // ── 2. Markdown links via pulldown-cmark ────────────────────────────────
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(content, opts);

    // Track whether we're inside a link so we can capture the display text.
    let mut in_link: Option<(String, WikiLinkKind)> = None;
    let mut link_text_buf = String::new();

    for event in parser {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                let href = dest_url.to_string();
                let kind = if href.starts_with("http://") || href.starts_with("https://") {
                    WikiLinkKind::External
                } else {
                    WikiLinkKind::Markdown
                };
                in_link = Some((href, kind));
                link_text_buf.clear();
            }
            Event::Text(t) if in_link.is_some() => {
                link_text_buf.push_str(&t);
            }
            Event::End(TagEnd::Link) => {
                if let Some((href, kind)) = in_link.take() {
                    // Resolve relative .md paths to bare path (strip .md).
                    let to_page = href.trim_end_matches(".md").to_string();
                    let display = if link_text_buf.is_empty() {
                        None
                    } else {
                        Some(link_text_buf.clone())
                    };
                    links.push(WikiLink {
                        from_page: source_page.to_string(),
                        to_page,
                        anchor: None,
                        display_text: display,
                        kind,
                    });
                }
            }
            _ => {}
        }
    }

    links
}

/// Heading slug following GitHub's anchor convention:
/// lowercase, spaces → hyphens, strip non-alphanumeric except hyphens.
pub fn heading_slug(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' { '-' } else { c })
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_wikilink() {
        let links = extract_links("home", "See [[Target Page]] for details.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].to_page, "Target Page");
        assert_eq!(links[0].kind, WikiLinkKind::Wikilink);
        assert!(links[0].display_text.is_none());
    }

    #[test]
    fn extracts_wikilink_with_display_text() {
        let links = extract_links("home", "See [[Eisenhorn|the Inquisitor]] in action.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].to_page, "Eisenhorn");
        assert_eq!(links[0].display_text, Some("the Inquisitor".into()));
        assert_eq!(links[0].kind, WikiLinkKind::Wikilink);
    }

    #[test]
    fn extracts_wikilink_with_anchor() {
        let links = extract_links("home", "[[Authentication#OAuth Flow]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].to_page, "Authentication");
        assert_eq!(links[0].anchor, Some("OAuth Flow".into()));
    }

    #[test]
    fn extracts_markdown_link() {
        let links = extract_links("home", "[See here](./other-page.md) for info.");
        // Two links expected: wikilink regex won't match, markdown parser will.
        let md_links: Vec<_> = links
            .iter()
            .filter(|l| l.kind == WikiLinkKind::Markdown)
            .collect();
        assert_eq!(md_links.len(), 1);
        assert_eq!(md_links[0].to_page, "./other-page");
        assert_eq!(md_links[0].display_text, Some("See here".into()));
    }

    #[test]
    fn classifies_external_link() {
        let links = extract_links("home", "[Rust](https://rust-lang.org)");
        let ext_links: Vec<_> = links
            .iter()
            .filter(|l| l.kind == WikiLinkKind::External)
            .collect();
        assert_eq!(ext_links.len(), 1);
        assert_eq!(ext_links[0].to_page, "https://rust-lang.org");
    }

    #[test]
    fn heading_slug_standard() {
        assert_eq!(heading_slug("OAuth Flow"), "oauth-flow");
        assert_eq!(heading_slug("Hello World!"), "hello-world");
        assert_eq!(heading_slug("My Section 2"), "my-section-2");
    }
}
