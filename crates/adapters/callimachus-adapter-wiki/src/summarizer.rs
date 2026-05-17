use callimachus_core::types::Chunk;
use callimachus_llm::{CompletionRequest, LlmProvider};

/// Summarize a single wiki section chunk.
pub async fn summarize_section(
    chunk: &Chunk,
    page_title: &str,
    section_heading: &str,
    llm: &dyn LlmProvider,
) -> anyhow::Result<String> {
    let prompt = format!(
        "You are summarizing a section of a wiki for a searchable index.\n\n\
         Page: {page_title}\n\
         Section: {section_heading}\n\n\
         <content>\n{content}\n</content>\n\n\
         Write a 1-2 sentence summary of this section. Focus on the key concept or information.\n\
         Return ONLY the summary text.",
        content = &chunk.content,
    );

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(200),
            chunk_id: Some(chunk.id.clone()),
        })
        .await?;

    Ok(resp.text.trim().to_string())
}

/// Summarize a wiki page from its section summaries.
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
        "You are summarizing a wiki page for a searchable index.\n\n\
         Page: {page_title}\n\n\
         Section summaries:\n{sections_text}\n\n\
         Write a 2-3 sentence summary of what this page covers.\n\
         Return ONLY the summary text.",
    );

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(300),
            chunk_id: None,
        })
        .await?;

    Ok(resp.text.trim().to_string())
}

/// Summarize an entire wiki corpus from page titles and their summaries.
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
        "You are summarizing a wiki (knowledge base) for a searchable index.\n\n\
         The wiki contains the following pages:\n{page_list}\n\n\
         Write a 3-5 sentence overview of what this wiki covers.\n\
         Return ONLY the summary text.",
    );

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(400),
            chunk_id: None,
        })
        .await?;

    Ok(resp.text.trim().to_string())
}

/// Extract page title and section heading from a chunk's location path and content.
pub fn chunk_metadata(chunk: &Chunk) -> (String, String) {
    // Page title: derive from path (strip "wiki/" prefix, replace hyphens/underscores).
    let path = &chunk.location.path;
    let page_part = path.split('#').next().unwrap_or(path);
    let page_title = page_part
        .trim_start_matches("wiki/")
        .replace(['-', '_'], " ");

    // Section heading: from the '#' fragment, or from the first heading in content.
    let section_heading = path
        .split('#')
        .nth(1)
        .map(|s| s.replace('-', " "))
        .or_else(|| {
            chunk
                .content
                .lines()
                .find(|l| l.starts_with("##"))
                .map(|l| l.trim_start_matches('#').trim().to_string())
        })
        .unwrap_or_else(|| "Introduction".to_string());

    (page_title, section_heading)
}
