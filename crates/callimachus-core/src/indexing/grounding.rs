//! Grounding: a block of verified facts about one entity, put into the LLM
//! passes' prompts together with field-annotated source. No LLM is involved;
//! everything comes from the fact base (see `docs/facts.md`).

use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::adapter::SourceAdapter;
use crate::corrections::address::{self, apply_mask, entity_address, mask_from_config, parse_address};
use crate::corrections::facts::FactRecord;
use crate::corrections::layouts;
use crate::corrections::types::{CorrectionKind, EntityLinkKind};
use crate::corrections::CorrectionsEngine;
use crate::error::Result;
use crate::query::collection_service::EntityLinkIndex;
use crate::storage::{EdgeDirection, StorageBackend};
use crate::types::{Corpus, Entity};

/// First line of every non-empty block.
pub const HEADER: &str = "Verified context from the project's fact base (prefer it over guesses; if the code contradicts a fact, say so):";

/// Most fact lines in one block.
pub const MAX_LINES: usize = 40;
/// Most characters in one block (header and omitted line included).
pub const MAX_CHARS: usize = 2_500;

/// Room kept for the trailing `- … N more omitted` line.
const OMITTED_RESERVE: usize = 32;
const EDGE_LIMIT: usize = 500;

/// How much of the fact base reaches the prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundingMode {
    /// Nothing: prompts are exactly what they were before facts existed.
    None,
    /// The verified-context block.
    Facts,
    /// The block plus source with struct fields named after layouts.
    Full,
}

impl GroundingMode {
    pub fn name(self) -> &'static str {
        match self {
            GroundingMode::None => "none",
            GroundingMode::Facts => "facts",
            GroundingMode::Full => "full",
        }
    }
}

impl FromStr for GroundingMode {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Ok(GroundingMode::None),
            "facts" => Ok(GroundingMode::Facts),
            "full" => Ok(GroundingMode::Full),
            other => Err(format!(
                "unknown grounding mode `{other}` (expected none, facts or full)"
            )),
        }
    }
}

/// The grounding of one entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grounding {
    /// Header plus fact lines; empty when nothing is known.
    pub block: String,
    /// Source with fields named, only when it differs from the original.
    pub annotated_content: Option<String>,
    /// sha256 of `block` + `annotated_content` + the mode name.
    pub hash: String,
}

impl Grounding {
    /// True when the prompt would be unchanged: no block and no annotation.
    pub fn is_empty(&self) -> bool {
        self.block.is_empty() && self.annotated_content.is_none()
    }

    /// The block as the adapters want it (`None` when empty).
    pub fn block_opt(&self) -> Option<&str> {
        (!self.block.is_empty()).then_some(self.block.as_str())
    }

    /// The content to put in the prompt: annotated when available.
    pub fn content_or<'a>(&'a self, content: &'a str) -> &'a str {
        self.annotated_content.as_deref().unwrap_or(content)
    }
}

struct RegionFact {
    space: String,
    start: u64,
    end: u64,
    mask: Option<u64>,
    confidence: String,
}

struct ConstantFact {
    value: String,
    number: Option<u64>,
    summary: String,
    confidence: String,
}

struct Reference {
    corpus_id: String,
    engine: CorrectionsEngine,
}

/// Everything `build` needs, loaded once per pass.
pub struct GroundingContext {
    db: Arc<dyn StorageBackend>,
    adapter: Arc<dyn SourceAdapter>,
    corpus_id: String,
    engine: CorrectionsEngine,
    references: Vec<Reference>,
    links: Vec<EntityLinkIndex>,
    mask: Option<u64>,
    regions: Vec<RegionFact>,
    constants: Vec<ConstantFact>,
    /// The mode this pass runs in.
    pub mode: GroundingMode,
    with_context: AtomicU64,
    lines: AtomicU64,
}

impl GroundingContext {
    /// Load the corpus's facts, the reference corpora of every collection it
    /// belongs to, and the links between them. `requested` of `None` applies the
    /// default rule: `Full` when the corpus has any fact or reference link,
    /// otherwise `None`.
    pub fn load(
        db: Arc<dyn StorageBackend>,
        corpus: &Corpus,
        adapter: Arc<dyn SourceAdapter>,
        requested: Option<GroundingMode>,
    ) -> Result<Self> {
        let corpus_id = corpus.id.clone();
        if requested == Some(GroundingMode::None) {
            return Ok(Self::empty(db, adapter, corpus_id));
        }
        let engine = CorrectionsEngine::load(db.as_ref(), &corpus_id)?;
        let mut references: Vec<Reference> = Vec::new();
        let mut links = Vec::new();
        let mut has_reference_link = false;
        for collection in db.collection_list()? {
            let members = db.collection_resolve_corpus_ids(&collection.id)?;
            if !members.contains(&corpus_id) {
                continue;
            }
            let corrections = db.correction_list_for_collection(&collection.id)?;
            has_reference_link |= corrections.iter().any(|c| {
                matches!(&c.kind, CorrectionKind::EntityLink { corpus_a_id, corpus_b_id, kind, .. }
                    if *kind == EntityLinkKind::References
                        && (*corpus_a_id == corpus_id || *corpus_b_id == corpus_id))
            });
            links.push(EntityLinkIndex::from_corrections(&corrections));
            for member in members {
                if member == corpus_id || references.iter().any(|r| r.corpus_id == member) {
                    continue;
                }
                let is_reference = db
                    .corpus_get(&member)?
                    .is_some_and(|c| address::is_reference(&c.config));
                if is_reference {
                    references.push(Reference {
                        engine: CorrectionsEngine::load(db.as_ref(), &member)?,
                        corpus_id: member,
                    });
                }
            }
        }
        let mode = requested.unwrap_or_else(|| {
            if !engine.facts_in_corpus(&corpus_id).is_empty() || has_reference_link {
                GroundingMode::Full
            } else {
                GroundingMode::None
            }
        });
        if mode == GroundingMode::None {
            return Ok(Self::empty(db, adapter, corpus_id));
        }

        let mask = mask_from_config(&corpus.config);
        let mut regions = Vec::new();
        let mut constants = Vec::new();
        let mut collect = |engine: &CorrectionsEngine, id: &str, mask: Option<u64>| {
            for (fact, _) in engine.facts_in_corpus(id) {
                match fact.kind.as_str() {
                    "region" => {
                        let (Some(start), Some(end)) = (
                            fact.data.get("start").and_then(|v| v.as_str()).and_then(parse_address),
                            fact.data.get("end").and_then(|v| v.as_str()).and_then(parse_address),
                        ) else {
                            continue;
                        };
                        regions.push(RegionFact {
                            space: fact.space().unwrap_or_default().to_string(),
                            start,
                            end,
                            mask,
                            confidence: fact.confidence.clone(),
                        });
                    }
                    "constant" => {
                        if let Some(value) = fact.data.get("value") {
                            let text = match value {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            constants.push(ConstantFact {
                                number: parse_number(&text),
                                value: text,
                                summary: fact.summary.clone(),
                                confidence: fact.confidence.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        };
        collect(&engine, &corpus_id, mask);
        for r in &references {
            let ref_mask = db
                .corpus_get(&r.corpus_id)?
                .and_then(|c| mask_from_config(&c.config));
            collect(&r.engine, &r.corpus_id, ref_mask);
        }

        Ok(Self {
            db,
            adapter,
            corpus_id,
            engine,
            references,
            links,
            mask,
            regions,
            constants,
            mode,
            with_context: AtomicU64::new(0),
            lines: AtomicU64::new(0),
        })
    }

    fn empty(db: Arc<dyn StorageBackend>, adapter: Arc<dyn SourceAdapter>, corpus_id: String) -> Self {
        Self {
            db,
            adapter,
            engine: CorrectionsEngine::new(vec![]),
            corpus_id,
            references: vec![],
            links: vec![],
            mask: None,
            regions: vec![],
            constants: vec![],
            mode: GroundingMode::None,
            with_context: AtomicU64::new(0),
            lines: AtomicU64::new(0),
        }
    }

    /// Ground one entity in this context's mode and count it for [`log_line`](Self::log_line).
    pub fn build_for(&self, entity: &Entity, content: &str) -> Grounding {
        let g = build(self, entity, content, self.mode);
        if !g.block.is_empty() {
            self.with_context.fetch_add(1, Ordering::Relaxed);
            // Fact lines only: the header and an omitted line are not facts.
            let n = g.block.lines().filter(|l| l.starts_with("- ")).count();
            self.lines.fetch_add(n as u64, Ordering::Relaxed);
        }
        g
    }

    /// `[purpose] grounding=full entities_with_context=N avg_lines=M`
    pub fn log_line(&self, pass: &str) -> String {
        let n = self.with_context.load(Ordering::Relaxed);
        let lines = self.lines.load(Ordering::Relaxed);
        let avg = if n == 0 { 0.0 } else { lines as f64 / n as f64 };
        format!(
            "[{pass}] grounding={} entities_with_context={n} avg_lines={avg:.1}",
            self.mode.name()
        )
    }
}

/// Ground `entity` (whose source is `content`) in `mode`.
pub fn build(
    ctx: &GroundingContext,
    entity: &Entity,
    content: &str,
    mode: GroundingMode,
) -> Grounding {
    let (block, annotated_content) = match mode {
        GroundingMode::None => (String::new(), None),
        GroundingMode::Facts => (render(&collect_lines(ctx, entity, content)), None),
        GroundingMode::Full => (
            render(&collect_lines(ctx, entity, content)),
            annotate(ctx, entity, content),
        ),
    };
    let mut hasher = Sha256::new();
    hasher.update(block.as_bytes());
    hasher.update(annotated_content.as_deref().unwrap_or("").as_bytes());
    hasher.update(mode.name().as_bytes());
    Grounding {
        block,
        annotated_content,
        hash: hex::encode(hasher.finalize()),
    }
}

fn annotate(ctx: &GroundingContext, entity: &Entity, content: &str) -> Option<String> {
    let language = entity
        .first_location
        .as_ref()
        .and_then(|l| ctx.db.chunk_get_by_uri(&l.uri()).ok().flatten())
        .and_then(|c| c.language)?;
    let fields = layouts::field_names_for(&ctx.engine, &entity.canonical_name);
    if fields.is_empty() {
        return None;
    }
    let annotated = ctx.adapter.annotate_source(content, &language, &fields);
    (annotated != content).then_some(annotated)
}

/// Lines in priority order, deduplicated, not yet capped.
fn collect_lines(ctx: &GroundingContext, entity: &Entity, content: &str) -> Vec<String> {
    let mut own = Vec::new(); // 1
    let mut targets = Vec::new(); // 2
    let mut fields = Vec::new(); // 3
    let mut callees = Vec::new(); // 4
    let mut regions = Vec::new(); // 5

    for (fact, _) in ctx.engine.facts_for_entity(&entity.id) {
        own.push(format!(
            "- {}: {} {}",
            entity.canonical_name,
            fact.summary,
            confidence_suffix(&[], &fact.confidence)
        ));
    }

    for (kind, verb) in [("reads", "reads"), ("writes", "writes"), ("uses_table", "uses table")] {
        let edges = ctx
            .db
            .edge_get_for_entity(&entity.id, EdgeDirection::Outbound, Some(kind), EDGE_LIMIT)
            .unwrap_or_default();
        for edge in edges {
            let Ok(Some(target)) = ctx.db.entity_get_by_id(&edge.to_entity_id) else {
                continue;
            };
            let name = &target.canonical_name;
            let mut described = false;
            for (fact, _) in ctx.engine.facts_for_entity(&target.id) {
                described = true;
                if fact.kind == "field" {
                    fields.push(field_line(verb, name, &fact));
                } else {
                    targets.push(format!(
                        "- {verb} {name} = {} {}",
                        fact.summary,
                        confidence_suffix(&[], &fact.confidence)
                    ));
                }
            }
            if reference_lines(ctx, verb, &target, &mut targets) {
                described = true;
            }
            if !described && let Some(line) = region_line(ctx, verb, &target) {
                regions.push(line);
            }
        }
    }

    callee_lines(ctx, entity, &mut callees);

    let mut constants = Vec::new(); // 6
    constant_lines(ctx, content, &mut constants);

    let mut seen = HashSet::new();
    own.into_iter()
        .chain(targets)
        .chain(fields)
        .chain(callees)
        .chain(regions)
        .chain(constants)
        .filter(|l| seen.insert(l.clone()))
        .collect()
}

/// `(s16, confirmed)`, `(confirmed)`, `(reference, meaning open)`, `(meaning open)`.
fn confidence_suffix(qualifiers: &[&str], confidence: &str) -> String {
    let label = match confidence {
        "unknown" => "meaning open",
        other => other,
    };
    let mut parts: Vec<&str> = qualifiers.to_vec();
    parts.push(label);
    format!("({})", parts.join(", "))
}

fn field_line(verb: &str, target_name: &str, fact: &FactRecord) -> String {
    let layout = fact.data.get("layout").and_then(|v| v.as_str()).unwrap_or("");
    let field = fact.id.rsplit('.').next().unwrap_or("");
    let qualified = format!("{layout}.{field}");
    let ty = fact.data.get("type").and_then(|v| v.as_str());
    let described = if fact.summary == qualified {
        qualified
    } else {
        format!("{qualified}: {}", fact.summary)
    };
    let suffix = match ty {
        Some(t) if !t.is_empty() => confidence_suffix(&[t], &fact.confidence),
        _ => confidence_suffix(&[], &fact.confidence),
    };
    format!("- {verb} {target_name} = {described} {suffix}")
}

/// Reference-corpus entities linked to `target`, with the variants reaching
/// them through one `affects` edge. Returns whether any line was added.
fn reference_lines(ctx: &GroundingContext, verb: &str, target: &Entity, out: &mut Vec<String>) -> bool {
    let mut added = false;
    let mut seen: HashSet<String> = HashSet::new();
    for index in &ctx.links {
        for (other_corpus, other_entity, _) in
            index.linked(&ctx.corpus_id, &target.id, &[EntityLinkKind::References])
        {
            let Some(reference) = ctx.references.iter().find(|r| r.corpus_id == other_corpus) else {
                continue;
            };
            if !seen.insert(other_entity.clone()) {
                continue;
            }
            let Ok(Some(re)) = ctx.db.entity_get_by_id(&other_entity) else {
                continue;
            };
            let anchored = reference.engine.facts_for_entity(&re.id);
            let summary = anchored
                .first()
                .map(|(f, _)| f.summary.clone())
                .or_else(|| re.description.clone());
            let Some(summary) = summary else { continue };
            let mut line = format!(
                "- {verb} {} = {summary} {}",
                target.canonical_name,
                confidence_suffix(&["reference"], &fact_confidence(reference, &re))
            );
            let edges = ctx
                .db
                .edge_get_for_entity(&re.id, EdgeDirection::Both, Some("affects"), 100)
                .unwrap_or_default();
            let mut variants: Vec<String> = Vec::new();
            for edge in edges {
                let other = if edge.from_entity_id == re.id {
                    &edge.to_entity_id
                } else {
                    &edge.from_entity_id
                };
                if let Ok(Some(v)) = ctx.db.entity_get_by_id(other)
                    && v.kind == "fact_variant"
                    && let Some(d) = v.description.as_deref()
                {
                    let text = format!("Variant: {d} {}", confidence_suffix(&[], &fact_confidence(reference, &v)));
                    if !variants.contains(&text) {
                        variants.push(text);
                    }
                }
            }
            // The variants go between the reference summary and the line's end,
            // so the line still ends with a confidence.
            for v in variants {
                line.push_str(". ");
                line.push_str(&v);
            }
            out.push(line);
            added = true;
        }
    }
    added
}

/// Confidence label of the fact a reference-corpus entity stands for.
fn fact_confidence(reference: &Reference, entity: &Entity) -> String {
    reference
        .engine
        .facts_in_corpus(&reference.corpus_id)
        .into_iter()
        .find(|(f, anchors)| {
            (entity.kind.starts_with("fact_") && f.id == entity.canonical_name)
                || anchors.contains(&entity.id)
        })
        .map(|(f, _)| f.confidence.clone())
        .unwrap_or_else(|| "confirmed".to_string())
}

fn region_line(ctx: &GroundingContext, verb: &str, target: &Entity) -> Option<String> {
    let address = entity_address(target)?;
    ctx.regions
        .iter()
        .find(|r| {
            let a = apply_mask(address, r.mask.or(ctx.mask));
            (r.start..=r.end).contains(&a)
        })
        .map(|r| {
            format!(
                "- {verb} {}: in {} region \"{}\" {}",
                target.canonical_name,
                r.space.to_uppercase(),
                r.space,
                confidence_suffix(&[], &r.confidence)
            )
        })
}

/// Direct callees with a routine fact. A name shared by several candidates is
/// listed once as ambiguous, never with one candidate's fact.
fn callee_lines(ctx: &GroundingContext, entity: &Entity, out: &mut Vec<String>) {
    let edges = ctx
        .db
        .edge_get_for_entity(&entity.id, EdgeDirection::Outbound, Some("calls"), EDGE_LIMIT)
        .unwrap_or_default();
    let mut callees: Vec<Entity> = Vec::new();
    for edge in edges {
        if callees.iter().any(|c| c.id == edge.to_entity_id) {
            continue;
        }
        if let Ok(Some(c)) = ctx.db.entity_get_by_id(&edge.to_entity_id) {
            callees.push(c);
        }
    }
    let mut done: HashSet<String> = HashSet::new();
    for callee in &callees {
        if !done.insert(callee.canonical_name.clone()) {
            continue;
        }
        let candidates: Vec<&Entity> = callees
            .iter()
            .filter(|c| c.canonical_name == callee.canonical_name)
            .collect();
        let routine = |e: &Entity| {
            ctx.engine
                .facts_for_entity(&e.id)
                .into_iter()
                .map(|(f, _)| f)
                .find(|f| f.kind == "routine")
        };
        if candidates.len() > 1 {
            if candidates.iter().any(|c| routine(c).is_some()) {
                out.push(format!(
                    "- calls {} (ambiguous: {} candidates)",
                    callee.canonical_name,
                    candidates.len()
                ));
            }
        } else if let Some(fact) = routine(callee) {
            out.push(format!(
                "- calls {} = {} {}",
                callee.canonical_name,
                fact.summary,
                confidence_suffix(&[], &fact.confidence)
            ));
        }
    }
}

fn constant_lines(ctx: &GroundingContext, content: &str, out: &mut Vec<String>) {
    if ctx.constants.is_empty() {
        return;
    }
    let tokens: Vec<String> = content
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect();
    let numbers: HashSet<u64> = tokens.iter().filter_map(|t| parse_number(t)).collect();
    for c in &ctx.constants {
        let lower = c.value.trim().to_ascii_lowercase();
        let hit = tokens.contains(&lower)
            || c.number.is_some_and(|n| n > 9 && numbers.contains(&n));
        if hit {
            out.push(format!(
                "- constant {} = {} {}",
                c.value,
                c.summary,
                confidence_suffix(&[], &c.confidence)
            ));
        }
    }
}

/// `0x2c0` (any case, optional `u`/`l` suffix) or a decimal number.
fn parse_number(token: &str) -> Option<u64> {
    let t = token
        .trim()
        .trim_end_matches(|c: char| matches!(c, 'u' | 'U' | 'l' | 'L'));
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => t.parse().ok(),
    }
}

/// Header plus lines, capped; empty when there are no lines.
fn render(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = String::from(HEADER);
    let mut kept = 0;
    for line in lines {
        if kept >= MAX_LINES || out.len() + 1 + line.len() + OMITTED_RESERVE > MAX_CHARS {
            break;
        }
        out.push('\n');
        out.push_str(line);
        kept += 1;
    }
    if kept < lines.len() {
        out.push_str(&format!("\n- … {} more omitted", lines.len() - kept));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_caps_lines_and_marks_the_rest() {
        let lines: Vec<String> = (0..60).map(|i| format!("- fact {i}")).collect();
        let block = render(&lines);
        assert_eq!(block.lines().count(), 1 + MAX_LINES + 1);
        assert!(block.ends_with("- … 20 more omitted"));
        assert!(block.len() < MAX_CHARS);
    }

    #[test]
    fn render_caps_chars() {
        let lines: Vec<String> = (0..10).map(|i| format!("- {i} {}", "x".repeat(400))).collect();
        let block = render(&lines);
        assert!(block.len() < MAX_CHARS, "{}", block.len());
        assert!(block.contains("more omitted"));
    }

    #[test]
    fn numbers_match_across_case_and_padding() {
        assert_eq!(parse_number("0x2C0"), parse_number("0x02c0"));
        assert_eq!(parse_number("0x2c0u"), Some(0x2c0));
    }

    #[test]
    fn unknown_confidence_is_meaning_open() {
        assert_eq!(confidence_suffix(&[], "unknown"), "(meaning open)");
        assert_eq!(confidence_suffix(&["s16"], "confirmed"), "(s16, confirmed)");
    }
}
