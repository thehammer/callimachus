use anyhow::Result;
use callimachus_core::storage::StorageBackend;

// ── Linkage types ─────────────────────────────────────────────────────────────

#[derive(Debug)]
enum Confidence {
    High,
    Medium,
}

impl Confidence {
    fn label(&self) -> &'static str {
        match self {
            Confidence::High => "HIGH",
            Confidence::Medium => "MEDIUM",
        }
    }
}

#[derive(Debug)]
struct Candidate {
    confidence: Confidence,
    id_a: String,
    name_a: String,
    id_b: String,
    name_b: String,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run `calli link candidates <corpus_a> <corpus_b>`.
pub fn run_candidates(corpus_a: &str, corpus_b: &str, db: &dyn StorageBackend) -> Result<()> {
    let entities_a = db
        .entity_list(corpus_a)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let entities_b = db
        .entity_list(corpus_b)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if entities_a.is_empty() || entities_b.is_empty() {
        eprintln!(
            "warning: one or both corpora have no entities ({} entities in {corpus_a}, {} in {corpus_b})",
            entities_a.len(),
            entities_b.len()
        );
        return Ok(());
    }

    // Normalize entity names.
    let norm_a: Vec<(String, &str, &str)> = entities_a
        .iter()
        .map(|e| {
            (
                normalize(&e.canonical_name),
                e.id.as_str(),
                e.canonical_name.as_str(),
            )
        })
        .collect();
    let norm_b: Vec<(String, &str, &str)> = entities_b
        .iter()
        .map(|e| {
            (
                normalize(&e.canonical_name),
                e.id.as_str(),
                e.canonical_name.as_str(),
            )
        })
        .collect();

    let mut candidates: Vec<Candidate> = Vec::new();

    for (na, id_a, name_a) in &norm_a {
        for (nb, id_b, name_b) in &norm_b {
            if na == nb {
                // Exact normalized match → HIGH.
                candidates.push(Candidate {
                    confidence: Confidence::High,
                    id_a: id_a.to_string(),
                    name_a: name_a.to_string(),
                    id_b: id_b.to_string(),
                    name_b: name_b.to_string(),
                });
            } else {
                // Same abstract_kind + Levenshtein ≤ 2 → MEDIUM.
                let ea = entities_a
                    .iter()
                    .find(|e| e.id == *id_a)
                    .expect("entity in norm_a must be in entities_a");
                let eb = entities_b
                    .iter()
                    .find(|e| e.id == *id_b)
                    .expect("entity in norm_b must be in entities_b");

                let kinds_match =
                    !ea.abstract_kind.is_empty() && ea.abstract_kind == eb.abstract_kind;

                if kinds_match && levenshtein(na, nb) <= 2 {
                    candidates.push(Candidate {
                        confidence: Confidence::Medium,
                        id_a: id_a.to_string(),
                        name_a: name_a.to_string(),
                        id_b: id_b.to_string(),
                        name_b: name_b.to_string(),
                    });
                }
            }
        }
    }

    if candidates.is_empty() {
        println!("No link candidates found between {corpus_a} and {corpus_b}.");
        return Ok(());
    }

    println!(
        "# Link candidates: {corpus_a} ↔ {corpus_b}  ({} candidates)\n",
        candidates.len()
    );

    for c in &candidates {
        println!("# {} — {} ↔ {}", c.confidence.label(), c.name_a, c.name_b);
        println!(
            "calli correct {corpus_a} entity_link --corpus-a {corpus_a} --entity-a {} --corpus-b {corpus_b} --entity-b {} --kind same_as --collection <collection_id>",
            c.id_a, c.id_b
        );
        println!();
    }

    Ok(())
}

// ── Normalization ─────────────────────────────────────────────────────────────

/// Lowercase, trim, collapse whitespace, strip ASCII punctuation.
pub fn normalize(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ── Levenshtein (no external crate) ──────────────────────────────────────────

/// Classic dynamic-programming Levenshtein edit distance.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    // Optimization: bail early when the length difference already exceeds threshold.
    if m.abs_diff(n) > 2 {
        return m.abs_diff(n);
    }

    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for (i, row) in dp.iter_mut().enumerate().take(m + 1) {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate().take(n + 1) {
        *cell = j;
    }

    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }

    dp[m][n]
}

// ── CLI subcommand enum ───────────────────────────────────────────────────────

pub enum LinkSubcommand {
    Candidates { corpus_a: String, corpus_b: String },
}

pub fn run(sub: LinkSubcommand, db: &dyn StorageBackend) -> Result<()> {
    match sub {
        LinkSubcommand::Candidates { corpus_a, corpus_b } => {
            run_candidates(&corpus_a, &corpus_b, db)
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use callimachus_core::{
        storage::SqliteBackend,
        types::{Corpus, Entity},
    };

    fn make_db_with_corpora() -> SqliteBackend {
        let db = SqliteBackend::open_in_memory().unwrap();
        let c1 = Corpus::new("c1".into(), "C1".into(), "book".into(), "/tmp/c1".into());
        let c2 = Corpus::new("c2".into(), "C2".into(), "code".into(), "/tmp/c2".into());
        db.corpus_insert(&c1).unwrap();
        db.corpus_insert(&c2).unwrap();
        db
    }

    fn make_entity(corpus: &str, id: &str, name: &str, abstract_kind: &str) -> Entity {
        let mut e = Entity::new(id.into(), corpus.into(), name.into(), "function".into());
        e.abstract_kind = abstract_kind.into();
        e
    }

    #[test]
    fn levenshtein_exact() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn levenshtein_one_edit() {
        assert_eq!(levenshtein("hello", "helo"), 1);
    }

    #[test]
    fn levenshtein_two_edits() {
        assert_eq!(levenshtein("sunday", "saturday"), 3); // > 2
        assert_eq!(levenshtein("cat", "cut"), 1);
        assert_eq!(levenshtein("cat", "cot"), 1);
        assert_eq!(levenshtein("ca", "cb"), 1);
    }

    #[test]
    fn normalize_strips_punctuation() {
        assert_eq!(normalize("Hello, World!"), "hello world");
        assert_eq!(normalize("  foo   BAR  "), "foo bar");
        assert_eq!(normalize("foo-bar"), "foobar");
    }

    #[test]
    fn candidates_empty_corpora() {
        let db = make_db_with_corpora();
        // No entities — should not panic.
        run_candidates("c1", "c2", &db).unwrap();
    }

    #[test]
    fn candidates_exact_match() {
        let db = make_db_with_corpora();

        let ea = make_entity("c1", "c1:alpha", "Alpha", "process");
        let eb = make_entity("c2", "c2:alpha", "Alpha", "process");
        db.entity_upsert(&ea).unwrap();
        db.entity_upsert(&eb).unwrap();

        // Collect candidates without printing.
        let entities_a = db.entity_list("c1").unwrap();
        let entities_b = db.entity_list("c2").unwrap();

        let norm_a: Vec<(String, &str, &str)> = entities_a
            .iter()
            .map(|e| {
                (
                    normalize(&e.canonical_name),
                    e.id.as_str(),
                    e.canonical_name.as_str(),
                )
            })
            .collect();
        let norm_b: Vec<(String, &str, &str)> = entities_b
            .iter()
            .map(|e| {
                (
                    normalize(&e.canonical_name),
                    e.id.as_str(),
                    e.canonical_name.as_str(),
                )
            })
            .collect();

        let exact: Vec<_> = norm_a
            .iter()
            .flat_map(|(na, _, _)| norm_b.iter().filter(move |(nb, _, _)| na == nb))
            .collect();

        assert_eq!(exact.len(), 1);
    }

    #[test]
    fn candidates_medium_match() {
        // "alpha" vs "alphas" — edit distance 1, same abstract_kind.
        assert_eq!(levenshtein("alpha", "alphas"), 1);
        assert!(levenshtein("alpha", "alphas") <= 2);
    }
}
