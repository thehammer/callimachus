/// LLM prompts for capture adapter semantic and summary passes.
use callimachus_core::types::Chunk;
use callimachus_llm::{CompletionRequest, LlmProvider};

/// Ask the LLM to describe an endpoint given its chunk content (signature,
/// request/response samples, auth clues from header keys, status codes).
///
/// Returns 1–3 sentences describing:
/// - What the endpoint does.
/// - The request schema (method, parameters, body shape).
/// - The response schema / notable fields.
/// - Auth requirements inferred from header key names / status codes.
pub async fn describe_endpoint(chunk: &Chunk, llm: &dyn LlmProvider) -> anyhow::Result<String> {
    let prompt = format!(
        "You are building a searchable index of a REST API captured from network traffic.\n\n\
         Analyze the following endpoint observation and produce a concise description.\n\n\
         <endpoint>\n{content}\n</endpoint>\n\n\
         Write 1-3 sentences that explain:\n\
         1. What this endpoint does (its purpose in the application).\n\
         2. The request shape: method, path parameters, relevant query params, body structure.\n\
         3. The response schema: key fields, data types.\n\
         4. Authentication/authorization requirements inferred from the request header keys or HTTP status codes.\n\n\
         Focus on what an engineer would need to know to call this endpoint or understand its contract.\n\
         Return ONLY the description text.",
        content = &chunk.content,
    );

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(600),
            chunk_id: Some(chunk.id.clone()),
            kind: "endpoint".to_string(),
            pass: "semantic".to_string(),
            ..Default::default()
        })
        .await?;

    Ok(resp.text.trim().to_string())
}

/// Summarize an endpoint in 1–2 sentences (for the summarize pass).
pub async fn summarize_endpoint(chunk: &Chunk, llm: &dyn LlmProvider) -> anyhow::Result<String> {
    let prompt = format!(
        "Summarize this REST API endpoint in 1-2 sentences for a searchable index.\n\n\
         <endpoint>\n{content}\n</endpoint>\n\n\
         Focus on what the endpoint does and what data it returns.\n\
         Return ONLY the summary text.",
        content = &chunk.content,
    );

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(200),
            chunk_id: Some(chunk.id.clone()),
            kind: "endpoint".to_string(),
            pass: "summarize".to_string(),
            ..Default::default()
        })
        .await?;

    Ok(resp.text.trim().to_string())
}

/// Summarize an entire API capture corpus from endpoint-level summaries.
///
/// The `chunk.content` for a corpus-level summary chunk contains pre-aggregated
/// child summaries provided by the summarize pass.
pub async fn summarize_corpus(chunk: &Chunk, llm: &dyn LlmProvider) -> anyhow::Result<String> {
    let prompt = format!(
        "You are summarizing a REST API for a searchable index.\n\n\
         The following endpoint summaries were captured from network traffic:\n\n\
         <endpoints>\n{content}\n</endpoints>\n\n\
         Write a 3-5 sentence overview of:\n\
         - What application or service this API belongs to.\n\
         - The main functional areas covered by the endpoints.\n\
         - Authentication patterns observed.\n\
         - Any notable patterns (versioning, entity IDs, path conventions).\n\n\
         Return ONLY the summary text.",
        content = &chunk.content,
    );

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(500),
            chunk_id: None,
            kind: "corpus".to_string(),
            pass: "summarize".to_string(),
            ..Default::default()
        })
        .await?;

    Ok(resp.text.trim().to_string())
}
