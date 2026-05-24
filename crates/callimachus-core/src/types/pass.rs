use serde::{Deserialize, Serialize};

/// Indexing passes, run in order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pass {
    /// History-aware change detection (Stage 0). Runs before Chunk.
    /// Produces a ChangeManifest that downstream passes use to skip
    /// unchanged sources. When omitted, all sources are treated as dirty.
    History,
    /// Stream chunks from the adapter and content-address them.
    Chunk,
    /// Parser-driven structural extraction (no LLM).
    Structure,
    /// LLM-driven entity/edge/event extraction.
    Semantic,
    /// LLM-driven alias / entity-deduplication pass (runs after Semantic).
    Aliases,
    /// LLM-driven summarization (bottom-up: scene → chapter → corpus).
    Summarize,
    /// Embedding generation (optional, off by default).
    Embed,
    /// LLM-driven purpose extraction (why an entity exists).
    Purpose,
    /// Static + LLM-driven contract analysis (signals, risks, assumptions).
    Contract,
    /// Corpus-level architectural theme detection (opt-in).
    Theme,
}

impl std::fmt::Display for Pass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pass::History => write!(f, "history"),
            Pass::Chunk => write!(f, "chunk"),
            Pass::Structure => write!(f, "structure"),
            Pass::Semantic => write!(f, "semantic"),
            Pass::Aliases => write!(f, "aliases"),
            Pass::Summarize => write!(f, "summarize"),
            Pass::Embed => write!(f, "embed"),
            Pass::Purpose => write!(f, "purpose"),
            Pass::Contract => write!(f, "contract"),
            Pass::Theme => write!(f, "theme"),
        }
    }
}

impl std::str::FromStr for Pass {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "history" => Ok(Pass::History),
            "chunk" => Ok(Pass::Chunk),
            "structure" => Ok(Pass::Structure),
            "semantic" => Ok(Pass::Semantic),
            "aliases" => Ok(Pass::Aliases),
            "summarize" => Ok(Pass::Summarize),
            "embed" => Ok(Pass::Embed),
            "purpose" => Ok(Pass::Purpose),
            "contract" => Ok(Pass::Contract),
            "theme" => Ok(Pass::Theme),
            other => Err(format!("unknown pass: {other}")),
        }
    }
}

/// Default pass list used by `parse_passes_list` for the `default` token and by
/// [`crate::indexing::pipeline::IndexOptions::default`] for the standard eight-pass run.
///
/// **Keep in sync** with `IndexOptions::default()` in `crates/callimachus-core/src/indexing/pipeline.rs`.
const DEFAULT_PASSES: &[Pass] = &[
    Pass::History,
    Pass::Chunk,
    Pass::Structure,
    Pass::Semantic,
    Pass::Aliases,
    Pass::Summarize,
    Pass::Purpose,
    Pass::Contract,
];

/// Parse a comma-separated `--passes` value into a deduplicated, ordered
/// `Vec<Pass>`.
///
/// The literal token `default` expands to the current default pass list:
/// History, Chunk, Structure, Semantic, Aliases, Summarize, Purpose,
/// Contract. (Theme and Embed are opt-in.)
///
/// Returns an `Err` for:
/// * an empty input string (after trimming)
/// * an unknown pass name (the error message lists all valid names)
///
/// Order in the input does NOT determine execution order — the pipeline's
/// internal pass ordering is preserved at runtime. Duplicates are removed
/// silently.
pub fn parse_passes_list(input: &str) -> Result<Vec<Pass>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("--passes requires at least one pass name".to_string());
    }

    let mut result: Vec<Pass> = Vec::new();

    for token in trimmed.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if token == "default" {
            for p in DEFAULT_PASSES {
                if !result.contains(p) {
                    result.push(p.clone());
                }
            }
        } else {
            let pass = token.parse::<Pass>().map_err(|_| {
                format!(
                    "unknown pass: '{token}' (valid: history, chunk, structure, semantic, \
                     aliases, summarize, purpose, contract, theme, embed, default)"
                )
            })?;
            if !result.contains(&pass) {
                result.push(pass);
            }
        }
    }

    if result.is_empty() {
        return Err("--passes requires at least one pass name".to_string());
    }

    Ok(result)
}

/// Status of an indexing run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunStatus::Running => write!(f, "running"),
            RunStatus::Completed => write!(f, "completed"),
            RunStatus::Failed => write!(f, "failed"),
            RunStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::str::FromStr for RunStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(RunStatus::Running),
            "completed" => Ok(RunStatus::Completed),
            "failed" => Ok(RunStatus::Failed),
            "cancelled" => Ok(RunStatus::Cancelled),
            other => Err(format!("unknown run status: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Pass, parse_passes_list};

    #[test]
    fn single_pass() {
        let result = parse_passes_list("chunk").unwrap();
        assert_eq!(result, vec![Pass::Chunk]);
    }

    #[test]
    fn default_expansion() {
        let result = parse_passes_list("default").unwrap();
        assert_eq!(
            result,
            vec![
                Pass::History,
                Pass::Chunk,
                Pass::Structure,
                Pass::Semantic,
                Pass::Aliases,
                Pass::Summarize,
                Pass::Purpose,
                Pass::Contract,
            ]
        );
    }

    #[test]
    fn default_plus_extra() {
        let result = parse_passes_list("default,theme").unwrap();
        assert_eq!(result.len(), 9, "default(8) + theme = 9");
        assert!(result.contains(&Pass::Theme));
        assert!(result.contains(&Pass::History));
    }

    #[test]
    fn comma_with_whitespace() {
        let result = parse_passes_list(" chunk , structure ").unwrap();
        assert_eq!(result, vec![Pass::Chunk, Pass::Structure]);
    }

    #[test]
    fn duplicate_dedup() {
        let result = parse_passes_list("chunk,chunk").unwrap();
        assert_eq!(result, vec![Pass::Chunk]);
    }

    #[test]
    fn unknown_pass_errors() {
        let err = parse_passes_list("bogus").unwrap_err();
        assert!(err.contains("unknown pass: 'bogus'"));
        assert!(err.contains("valid:"));
    }

    #[test]
    fn empty_list_errors() {
        let err = parse_passes_list("").unwrap_err();
        assert!(err.contains("--passes requires at least one pass name"));
    }

    #[test]
    fn default_with_dup() {
        // "default,chunk" should deduplicate chunk (it's already in default).
        let result = parse_passes_list("default,chunk").unwrap();
        assert_eq!(result.len(), 8, "chunk already in default; still 8 passes");
        // chunk appears exactly once
        assert_eq!(result.iter().filter(|p| **p == Pass::Chunk).count(), 1);
    }
}
