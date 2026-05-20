use callimachus_core::{
    adapter::ExtractedSemantic,
    types::{Chunk, Edge, Entity, Location},
};
use callimachus_llm::{CompletionRequest, LlmProvider};
use serde::Deserialize;

/// JSON shape we expect from the LLM.
#[derive(Debug, Deserialize)]
struct ExtractResponse {
    #[serde(default)]
    entities: Vec<EntityRaw>,
    #[serde(default)]
    edges: Vec<EdgeRaw>,
    #[serde(default)]
    summary_text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EntityRaw {
    name: String,
    kind: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EdgeRaw {
    from: String,
    to: String,
    kind: String,
}

pub async fn extract(chunk: &Chunk, llm: &dyn LlmProvider) -> anyhow::Result<ExtractedSemantic> {
    let prompt = build_extraction_prompt(&chunk.content);

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(2048),
            chunk_id: Some(chunk.id.clone()),
            kind: "book".to_string(),
            pass: "semantic".to_string(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("LLM error: {e}"))?;

    parse_extraction_response(&resp.text, chunk)
}

fn build_extraction_prompt(content: &str) -> String {
    format!(
        r#"You are indexing a passage from a book for a searchable database.

Extract the following from the passage below:
1. Named entities (characters, places, organizations, objects) with their kind and a brief description.
2. Relationships (edges) between entities observed in this passage.
3. A 1-3 sentence summary of what happens in this passage.

Return ONLY valid JSON in this exact shape:
{{
  "entities": [{{"name": "...", "kind": "character|place|organization|object", "description": "..."}}],
  "edges": [{{"from": "...", "to": "...", "kind": "meets|located_in|allied_with|mentions"}}],
  "summary_text": "..."
}}

Passage:
<passage>
{content}
</passage>"#
    )
}

fn parse_extraction_response(text: &str, chunk: &Chunk) -> anyhow::Result<ExtractedSemantic> {
    // Extract JSON from the response — the LLM may wrap it in markdown fences.
    let json_str = extract_json(text);

    let raw: ExtractResponse = serde_json::from_str(&json_str)
        .map_err(|e| anyhow::anyhow!("failed to parse LLM JSON: {e}\nRaw: {json_str}"))?;

    let entities: Vec<Entity> = raw
        .entities
        .iter()
        .map(|e| {
            let id = entity_id(&chunk.corpus_id, &e.name);
            Entity {
                id,
                corpus_id: chunk.corpus_id.clone(),
                canonical_name: e.name.clone(),
                kind: e.kind.clone(),
                abstract_kind: String::new(),
                aliases: vec![],
                description: e.description.clone(),
                first_location: Some(chunk.location.clone()),
                last_location: Some(chunk.location.clone()),
                appearance_count: 1,
                confidence: 0.8,
            }
        })
        .collect();

    let edges: Vec<Edge> = raw
        .edges
        .iter()
        .filter_map(|e| {
            let from_id = entities
                .iter()
                .find(|en| en.canonical_name.to_lowercase() == e.from.to_lowercase())
                .map(|en| en.id.clone())?;
            let to_id = entities
                .iter()
                .find(|en| en.canonical_name.to_lowercase() == e.to.to_lowercase())
                .map(|en| en.id.clone())?;

            let edge_id = format!("{}-{}-{}", from_id, e.kind, to_id);
            Some(Edge::new(
                edge_id,
                chunk.corpus_id.clone(),
                from_id,
                to_id,
                e.kind.clone(),
                Location::new(&chunk.corpus_id, &chunk.location.path),
            ))
        })
        .collect();

    Ok(ExtractedSemantic {
        entities,
        edges,
        summary_text: raw.summary_text,
    })
}

/// Extract the first JSON object from potentially fenced text.
fn extract_json(text: &str) -> String {
    // Try to find a JSON block between ```json ... ``` or ``` ... ```.
    let stripped = text
        .split("```json")
        .nth(1)
        .or_else(|| text.split("```").nth(1))
        .and_then(|s| s.split("```").next())
        .map(str::trim);

    if let Some(s) = stripped {
        return s.to_string();
    }

    // Fall back: find the first { ... } balanced block.
    let start = text.find('{').unwrap_or(0);
    let end = text.rfind('}').map(|i| i + 1).unwrap_or(text.len());
    text[start..end].to_string()
}

/// Deterministic entity ID: lowercase, spaces replaced with underscores.
pub fn entity_id(corpus_id: &str, name: &str) -> String {
    let slug = name
        .to_lowercase()
        .replace(' ', "_")
        .replace(['\'', '"', ',', '.'], "");
    format!("{corpus_id}:{slug}")
}
