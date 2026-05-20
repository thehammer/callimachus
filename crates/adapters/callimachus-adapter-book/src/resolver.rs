use callimachus_core::{adapter::EntityMerge, types::Entity};
use callimachus_llm::{CompletionRequest, LlmProvider};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct MergeRaw {
    keep: String,
    absorb: String,
    reason: String,
}

/// Ask the LLM to identify which entity names refer to the same entity.
pub async fn resolve_aliases(
    entities: &[Entity],
    llm: &dyn LlmProvider,
) -> anyhow::Result<Vec<EntityMerge>> {
    if entities.is_empty() {
        return Ok(vec![]);
    }

    let entity_list = entities
        .iter()
        .map(|e| {
            let aliases = if e.aliases.is_empty() {
                String::new()
            } else {
                format!(" (aliases: {})", e.aliases.join(", "))
            };
            format!("- {} [{}]{}", e.canonical_name, e.kind, aliases)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        r#"You are resolving entity aliases in a book index.

Below is a list of named entities. Identify pairs that refer to the same real-world entity
(e.g. "Eisenhorn" and "Gregor Eisenhorn" are the same character).

Return ONLY valid JSON — an array of merge instructions. For each duplicate, keep the
more canonical name and absorb the alias. Return an empty array if there are no duplicates.

[{{"keep": "canonical name", "absorb": "alias name", "reason": "brief reason"}}]

Entities:
{entity_list}

Merges:"#
    );

    let resp = llm
        .complete(CompletionRequest {
            prompt,
            model: None,
            max_tokens: Some(1024),
            chunk_id: None,
            kind: "entity".to_string(),
            pass: "aliases".to_string(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("LLM error: {e}"))?;

    let text = resp.text.trim();

    // DryRunProvider or empty response → no merges.
    if text == "[dry-run]" || text.is_empty() {
        return Ok(vec![]);
    }

    let json = extract_json_array(text);
    let raw: Vec<MergeRaw> = serde_json::from_str(&json).unwrap_or_default();

    let merges = raw
        .into_iter()
        .filter_map(|m| {
            let keep_entity = entities.iter().find(|e| e.canonical_name == m.keep)?;
            let absorb_entity = entities.iter().find(|e| e.canonical_name == m.absorb)?;
            Some(EntityMerge {
                keep_id: keep_entity.id.clone(),
                absorb_id: absorb_entity.id.clone(),
                reason: m.reason,
            })
        })
        .collect();

    Ok(merges)
}

fn extract_json_array(text: &str) -> String {
    // Strip markdown fences.
    let stripped = text
        .split("```json")
        .nth(1)
        .or_else(|| text.split("```").nth(1))
        .and_then(|s| s.split("```").next())
        .map(str::trim);

    if let Some(s) = stripped {
        return s.to_string();
    }

    let start = text.find('[').unwrap_or(0);
    let end = text.rfind(']').map(|i| i + 1).unwrap_or(text.len());
    text[start..end].to_string()
}
