use callimachus_core::types::Chunk;
use callimachus_llm::{CompletionRequest, LlmProvider};

/// Generate a summary at the given depth ("scene", "chapter", "corpus").
pub async fn summarize(
    chunk: &Chunk,
    llm: &dyn LlmProvider,
    depth: &str,
) -> anyhow::Result<Option<String>> {
    let prompt = build_prompt(chunk, depth);

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(512),
            chunk_id: Some(chunk.id.clone()),
        })
        .await
        .map_err(|e| anyhow::anyhow!("LLM error: {e}"))?;

    let text = resp.text.trim().to_string();
    if text.is_empty() || text == "[dry-run]" {
        // DryRunProvider returns "[dry-run]"; treat as valid stub summary.
        Ok(Some(text))
    } else {
        Ok(Some(text))
    }
}

fn build_prompt(chunk: &Chunk, depth: &str) -> String {
    match depth {
        "chapter" => format!(
            "You are summarizing a chapter of a book. Below are the summaries of its scenes.\n\
             Write a concise 2-4 sentence summary of the chapter as a whole.\n\n\
             Scene summaries:\n{}\n\nChapter summary:",
            chunk.content
        ),
        "corpus" => format!(
            "You are summarizing an entire book. Below are the summaries of its chapters.\n\
             Write a concise 3-5 sentence overview of the book as a whole.\n\n\
             Chapter summaries:\n{}\n\nBook summary:",
            chunk.content
        ),
        _ => format!(
            "Summarize the following passage in 1-3 sentences.\n\nPassage:\n{}\n\nSummary:",
            chunk.content
        ),
    }
}
