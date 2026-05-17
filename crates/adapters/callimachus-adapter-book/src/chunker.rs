use callimachus_core::adapter::DiscoveredSource;
use callimachus_core::types::{Chunk, Location};

const MIN_SCENE_CHARS: usize = 200;
const MAX_SCENE_CHARS: usize = 4000;

// ── EPUB ──────────────────────────────────────────────────────────────────────

pub fn chunk_epub(source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
    use epub::doc::EpubDoc;

    let mut doc =
        EpubDoc::new(&source.path).map_err(|e| anyhow::anyhow!("failed to open EPUB: {e}"))?;

    // Derive corpus_id from the source meta if present, or fall back to filename.
    let corpus_id = source
        .meta
        .get("corpus_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let mut chunks = Vec::new();
    let mut chapter_num = 0usize;

    let spine_idrefs: Vec<String> = doc.spine.iter().map(|s| s.idref.clone()).collect();

    for spine_id in &spine_idrefs {
        let (content_bytes, _mime) = match doc.get_resource(spine_id) {
            Some(r) => r,
            None => continue,
        };

        let html = String::from_utf8_lossy(&content_bytes).to_string();
        let text = html2text::from_read(html.as_bytes(), usize::MAX);
        let text = text.trim().to_string();

        if text.is_empty() {
            continue;
        }

        chapter_num += 1;
        let chapter_path = format!("ch/{chapter_num}");

        let chapter_chunk = Chunk::new(
            corpus_id.clone(),
            None,
            "chapter".to_string(),
            Location::new(&corpus_id, &chapter_path),
            text.clone(),
        );
        chunks.push(chapter_chunk);

        // Split into scenes.
        let scenes = split_into_scenes(&text);
        for (scene_num, scene_text) in scenes.iter().enumerate() {
            let scene_path = format!("ch/{chapter_num}/sc/{}", scene_num + 1);
            let scene_chunk = Chunk::new(
                corpus_id.clone(),
                Some(chapter_path.clone()),
                "scene".to_string(),
                Location::new(&corpus_id, &scene_path),
                scene_text.clone(),
            );
            chunks.push(scene_chunk);
        }
    }

    Ok(chunks)
}

// ── Markdown ──────────────────────────────────────────────────────────────────

pub fn chunk_markdown(source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
    use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

    let corpus_id = source
        .meta
        .get("corpus_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let raw = std::fs::read_to_string(&source.path)?;
    let parser = Parser::new_ext(&raw, Options::all());

    let mut chunks = Vec::new();
    let mut chapter_num = 0usize;
    let mut scene_num = 0usize;
    let mut current_chapter_path: Option<String> = None;
    let mut current_text = String::new();
    let mut current_kind = "scene";

    let flush = |kind: &str,
                 chapter_num: usize,
                 scene_num: usize,
                 chapter_path: &Option<String>,
                 text: &str,
                 corpus_id: &str,
                 chunks: &mut Vec<Chunk>| {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let (path, parent) = match kind {
            "chapter" => (format!("ch/{chapter_num}"), None),
            _ => (
                format!("ch/{chapter_num}/sc/{scene_num}"),
                chapter_path.clone(),
            ),
        };
        chunks.push(Chunk::new(
            corpus_id.to_string(),
            parent,
            kind.to_string(),
            Location::new(corpus_id, &path),
            text.to_string(),
        ));
    };

    for event in parser {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) => {
                // Flush previous.
                if !current_text.trim().is_empty() {
                    flush(
                        current_kind,
                        chapter_num,
                        scene_num,
                        &current_chapter_path,
                        &current_text,
                        &corpus_id,
                        &mut chunks,
                    );
                }
                chapter_num += 1;
                scene_num = 0;
                current_text.clear();
                current_kind = "chapter";
                current_chapter_path = Some(format!("ch/{chapter_num}"));
            }
            Event::Start(Tag::Heading {
                level: HeadingLevel::H2,
                ..
            }) => {
                if !current_text.trim().is_empty() {
                    flush(
                        current_kind,
                        chapter_num,
                        scene_num,
                        &current_chapter_path,
                        &current_text,
                        &corpus_id,
                        &mut chunks,
                    );
                }
                scene_num += 1;
                current_text.clear();
                current_kind = "scene";
            }
            Event::End(TagEnd::Heading(_)) => {
                current_text.push('\n');
            }
            Event::Text(text) => {
                current_text.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak => {
                current_text.push('\n');
            }
            _ => {}
        }
    }

    // Flush final.
    if !current_text.trim().is_empty() {
        flush(
            current_kind,
            chapter_num,
            scene_num,
            &current_chapter_path,
            &current_text,
            &corpus_id,
            &mut chunks,
        );
    }

    Ok(chunks)
}

// ── Plain text ────────────────────────────────────────────────────────────────

pub fn chunk_text(source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
    let corpus_id = source
        .meta
        .get("corpus_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let raw = std::fs::read_to_string(&source.path)?;

    // Split on 3+ consecutive blank lines (part/section boundary).
    let parts = split_on_blank_lines(&raw, 3);

    let mut chunks = Vec::new();
    let mut chapter_num = 0usize;

    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        chapter_num += 1;
        let chapter_path = format!("ch/{chapter_num}");

        let chapter_chunk = Chunk::new(
            corpus_id.clone(),
            None,
            "chapter".to_string(),
            Location::new(&corpus_id, &chapter_path),
            part.to_string(),
        );
        chunks.push(chapter_chunk);

        // Split chapter into scenes on 2 consecutive blank lines.
        let scenes = split_into_scenes(part);
        for (scene_num, scene_text) in scenes.iter().enumerate() {
            let scene_path = format!("ch/{chapter_num}/sc/{}", scene_num + 1);
            let scene_chunk = Chunk::new(
                corpus_id.clone(),
                Some(chapter_path.clone()),
                "scene".to_string(),
                Location::new(&corpus_id, &scene_path),
                scene_text.clone(),
            );
            chunks.push(scene_chunk);
        }
    }

    Ok(chunks)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Split text into scenes at 2+ consecutive blank lines.
/// Merges short scenes with their successor and splits long scenes at sentence boundaries.
fn split_into_scenes(text: &str) -> Vec<String> {
    let raw_scenes = split_on_blank_lines(text, 2);

    let mut merged: Vec<String> = Vec::new();
    let mut pending = String::new();

    for scene in raw_scenes {
        let scene = scene.trim().to_string();
        if scene.is_empty() {
            continue;
        }

        if pending.is_empty() {
            pending = scene;
        } else if pending.len() < MIN_SCENE_CHARS {
            // Merge short pending into current.
            pending.push_str("\n\n");
            pending.push_str(&scene);
        } else {
            merged.push(pending);
            pending = scene;
        }
    }

    if !pending.is_empty() {
        merged.push(pending);
    }

    // Split any scene that's too long.
    let mut result = Vec::new();
    for scene in merged {
        if scene.len() <= MAX_SCENE_CHARS {
            result.push(scene);
        } else {
            result.extend(split_long_text(&scene));
        }
    }

    result
}

/// Split on N+ consecutive blank lines.
fn split_on_blank_lines(text: &str, min_blanks: usize) -> Vec<String> {
    let pattern = "\n".repeat(min_blanks + 1); // N blank lines = N+1 newlines.
    text.split(&pattern).map(|s| s.to_string()).collect()
}

/// Split a long text at sentence boundaries to keep under MAX_SCENE_CHARS.
fn split_long_text(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();

    for sentence in text.split_inclusive(['.', '!', '?']) {
        if current.len() + sentence.len() > MAX_SCENE_CHARS && !current.is_empty() {
            parts.push(current.trim().to_string());
            current = String::new();
        }
        current.push_str(sentence);
    }

    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }

    if parts.is_empty() {
        // Fallback: just truncate.
        parts.push(text[..MAX_SCENE_CHARS.min(text.len())].to_string());
    }

    parts
}
