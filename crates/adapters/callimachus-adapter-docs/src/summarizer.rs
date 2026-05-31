/// Summarization for the docs adapter.
///
/// Mirrors the wiki summarizer structure. Supports `"section"` and `"page"` depths.
use callimachus_core::types::Chunk;
use callimachus_llm::{CompletionRequest, LlmProvider};

/// Extract page title and section heading from a docs chunk.
pub fn chunk_metadata(chunk: &Chunk) -> (String, String) {
    let path = &chunk.location.path;

    // Page part: docs/<Framework>/<Slug> — derive a human title from slug.
    let page_part = path.split('#').next().unwrap_or(path);
    let slug = page_part.rsplit('/').next().unwrap_or(page_part);
    let page_title = slug.to_string();

    // Section heading: from the '#' fragment.
    let section_heading = path
        .split('#')
        .nth(1)
        .map(|s| s.replace('-', " "))
        .unwrap_or_else(|| "Discussion".to_string());

    (page_title, section_heading)
}

/// Summarize a docs section chunk (Discussion prose).
pub async fn summarize_section(
    chunk: &Chunk,
    page_title: &str,
    section_heading: &str,
    llm: &dyn LlmProvider,
) -> anyhow::Result<String> {
    let prompt = format!(
        "You are summarizing a section of Apple developer documentation for a searchable index.\n\n\
         Symbol: {page_title}\n\
         Section: {section_heading}\n\n\
         <content>\n{content}\n</content>\n\n\
         Write a 1-2 sentence summary of this section. Focus on what the API does.\n\
         Return ONLY the summary text.",
        content = &chunk.content,
    );

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(200),
            chunk_id: Some(chunk.id.clone()),
            kind: "section".to_string(),
            pass: "summarize".to_string(),
        })
        .await?;

    Ok(resp.text.trim().to_string())
}

/// Summarize a docs page using its rendered page content directly.
///
/// Called from the `"page"` summarize arm; injects the full page markdown
/// so the LLM has substance to work with rather than an empty sections list.
pub async fn summarize_page_from_content(
    page_title: &str,
    content: &str,
    llm: &dyn LlmProvider,
) -> anyhow::Result<String> {
    let prompt = format!(
        "You are summarizing an Apple developer documentation page for a searchable index.\n\n\
         Symbol: {page_title}\n\n\
         Page content:\n{content}\n\n\
         Write a 2-3 sentence summary of what this API symbol does.\n\
         Return ONLY the summary text.",
    );

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(300),
            chunk_id: None,
            kind: "page".to_string(),
            pass: "summarize".to_string(),
        })
        .await?;

    Ok(resp.text.trim().to_string())
}

/// Summarize a docs page from its section summaries.
pub async fn summarize_page(
    page_title: &str,
    section_summaries: &[(String, String)], // (heading, summary)
    llm: &dyn LlmProvider,
) -> anyhow::Result<String> {
    let sections_text = section_summaries
        .iter()
        .map(|(h, s)| format!("- **{h}**: {s}"))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "You are summarizing an Apple developer documentation page for a searchable index.\n\n\
         Symbol: {page_title}\n\n\
         Section summaries:\n{sections_text}\n\n\
         Write a 2-3 sentence summary of what this API symbol does.\n\
         Return ONLY the summary text.",
    );

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(300),
            chunk_id: None,
            kind: "page".to_string(),
            pass: "summarize".to_string(),
        })
        .await?;

    Ok(resp.text.trim().to_string())
}

/// Summarize an entire docs corpus from page titles.
pub async fn summarize_corpus(
    pages: &[(String, Option<String>)], // (title, summary)
    llm: &dyn LlmProvider,
) -> anyhow::Result<String> {
    let page_list = pages
        .iter()
        .map(|(title, summary)| {
            if let Some(s) = summary {
                format!("- {title}: {s}")
            } else {
                format!("- {title}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "You are summarizing an Apple developer documentation corpus for a searchable index.\n\n\
         The corpus contains the following symbols:\n{page_list}\n\n\
         Write a 3-5 sentence overview of what frameworks and API surface this corpus covers.\n\
         Return ONLY the summary text.",
    );

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(400),
            chunk_id: None,
            kind: "corpus".to_string(),
            pass: "summarize".to_string(),
        })
        .await?;

    Ok(resp.text.trim().to_string())
}
