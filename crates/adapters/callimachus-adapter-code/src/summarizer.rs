use anyhow::Result;
use callimachus_core::types::Chunk;
use callimachus_llm::{CompletionRequest, LlmProvider};

use crate::extractor::ExtractedCodeStructure;

/// Generate a 1-3 sentence summary for a code chunk using the LLM.
///
/// Returns `None` for `kind == "file"` chunks (those are covered by their
/// symbol summaries) and for unknown/empty chunks.
pub async fn summarize_chunk(
    chunk: &Chunk,
    structure: &ExtractedCodeStructure,
    llm: &dyn LlmProvider,
) -> Result<Option<String>> {
    // Skip file-level chunks — they're summarized implicitly by their symbols.
    if chunk.kind == "file" {
        return Ok(None);
    }

    // Skip module-level chunks for now.
    if chunk.kind == "module" {
        return Ok(None);
    }

    // Determine language from the location path.
    let language = detect_language_from_chunk(chunk);
    let symbol_name = extract_symbol_from_location(&chunk.location.path);

    let doc_comment_section = structure
        .doc_comment
        .as_deref()
        .map(|d| format!("\nDoc comment:\n{d}\n"))
        .unwrap_or_default();

    let prompt = format!(
        r#"You are summarizing a code chunk for a searchable index.

Language: {language}
Symbol: {symbol_name}
Kind: {kind} (function|class|interface|module)
{doc_comment_section}
Code:
<code>
{content}
</code>

Write a 1-3 sentence summary of what this {kind} does. Focus on purpose and behavior,
not implementation details. If the code is self-explanatory from its name, a single
sentence is sufficient.

Return ONLY the summary text, no JSON, no preamble."#,
        language = language,
        symbol_name = symbol_name,
        kind = chunk.kind,
        content = &chunk.content[..chunk.content.floor_char_boundary(3000.min(chunk.content.len()))],
    );

    let req = CompletionRequest {
        prompt,
        model: Some("claude-haiku-4-5-20251001".to_string()),
        max_tokens: Some(200),
        chunk_id: Some(chunk.id.clone()),
    };

    let response = llm.complete(req).await?;
    let text = response.text.trim().to_string();

    if text.is_empty() || text == "[dry-run]" {
        Ok(Some(format!("[auto] {} `{}`", chunk.kind, symbol_name)))
    } else {
        Ok(Some(text))
    }
}

/// Generate a high-level repository overview from module-level doc strings.
pub async fn summarize_corpus(
    doc_comments: &[String],
    corpus_id: &str,
    llm: &dyn LlmProvider,
) -> Result<Option<String>> {
    if doc_comments.is_empty() {
        return Ok(None);
    }

    let combined = doc_comments.join("\n\n");
    let prompt = format!(
        r#"You are summarizing a software repository for a searchable index.

Repository: {corpus_id}

Top-level documentation strings from the codebase:
<docs>
{combined}
</docs>

Write a 3-5 sentence overview of what this repository does. Focus on the repository's
purpose and primary capabilities.

Return ONLY the summary text, no JSON, no preamble."#,
    );

    let req = CompletionRequest {
        prompt,
        model: None,
        max_tokens: Some(400),
        chunk_id: None,
    };

    let response = llm.complete(req).await?;
    let text = response.text.trim().to_string();

    if text.is_empty() || text == "[dry-run]" {
        Ok(Some(format!("[auto] Repository {corpus_id} index.")))
    } else {
        Ok(Some(text))
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn detect_language_from_chunk(chunk: &Chunk) -> String {
    // Pull extension from the location path.
    let path = chunk.location.path.split('#').next().unwrap_or("");
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        _ => "unknown",
    }
    .to_string()
}

fn extract_symbol_from_location(path: &str) -> String {
    if let Some(idx) = path.find('#') {
        path[idx + 1..].to_string()
    } else {
        // No symbol: use the filename.
        path.rsplit('/').next().unwrap_or(path).to_string()
    }
}
