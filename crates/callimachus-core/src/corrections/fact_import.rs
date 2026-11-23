//! Import fact records (format 1) as scholia, anchoring them to entities by
//! address and materialising entities for facts that have none.

use crate::corrections::address::{
    apply_mask, canonical, cpu_spaces_from_config, entity_address, mask_from_config, parse_address,
};
use crate::corrections::engine::CorrectionsEngine;
use crate::corrections::facts::{FactRecord, confidence_score};
use crate::corrections::types::CorrectionKind;
use crate::error::{CalError, Result};
use crate::storage::StorageBackend;
use crate::types::edge::Edge;
use crate::types::entity::Entity;
use crate::types::location::Location;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Origin scope marking edges derived from facts.
pub const FACT_EDGE_SCOPE: &str = "fact";

/// Kinds that always get a materialised entity, anchored or not.
const ALWAYS_MATERIALISED: [&str; 4] = ["variant", "region", "constant", "platform"];

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub added: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub retracted: usize,
    pub rejected: Vec<String>,
    pub warnings: Vec<String>,
    pub anchored: usize,
    pub materialised: usize,
}

/// Id of the entity materialised for a fact.
pub fn fact_entity_id(corpus_id: &str, fact_id: &str) -> String {
    format!("{corpus_id}:fact.{fact_id}")
}

/// Import the facts in `paths` (facts files or index files) into `corpus_id`.
pub fn import(
    db: &dyn StorageBackend,
    corpus_id: &str,
    paths: &[PathBuf],
    prune: bool,
) -> Result<ImportReport> {
    let corpus = db.corpus_require(corpus_id)?;
    let mask = mask_from_config(&corpus.config);
    let cpu_spaces = cpu_spaces_from_config(&corpus.config);
    let mut report = ImportReport::default();

    // ── Read + validate ───────────────────────────────────────────────────
    let mut records: Vec<FactRecord> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    for (file, raw) in read_records(paths)? {
        let id_hint = raw
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        // Rejected ids still count as "present" so that --prune keeps them.
        seen_ids.insert(id_hint.clone());
        match FactRecord::from_json(&raw) {
            Ok(rec) => {
                if records.iter().any(|r| r.id == rec.id) {
                    report
                        .rejected
                        .push(format!("{file}: fact {id_hint}: duplicate id"));
                } else {
                    records.push(rec);
                }
            }
            Err(reason) => report
                .rejected
                .push(format!("{file}: fact {id_hint}: {reason}")),
        }
    }

    // ── Existing state ────────────────────────────────────────────────────
    let engine = CorrectionsEngine::load(db, corpus_id)?;
    let existing: HashMap<String, (FactRecord, Vec<String>)> = engine
        .facts_in_corpus(corpus_id)
        .into_iter()
        .map(|(f, a)| (f.id.clone(), (f.clone(), a.to_vec())))
        .collect();

    // ── Anchor + materialise ──────────────────────────────────────────────
    let entities = db.entity_list(corpus_id)?;
    let mut by_addr: HashMap<u64, Vec<&Entity>> = HashMap::new();
    for e in entities.iter().filter(|e| !e.kind.starts_with("fact_")) {
        if let Some(a) = entity_address(e) {
            by_addr.entry(apply_mask(a, mask)).or_default().push(e);
        }
    }

    for rec in &records {
        let anchors = anchors_for(rec, &by_addr, mask, cpu_spaces.as_deref());
        if !anchors.is_empty() {
            report.anchored += 1;
        }
        if anchors.is_empty() || ALWAYS_MATERIALISED.contains(&rec.kind.as_str()) {
            db.entity_upsert(&materialise(corpus_id, rec, mask, cpu_spaces.as_deref()))?;
            report.materialised += 1;
        }

        // ── Diff ──────────────────────────────────────────────────────────
        match existing.get(&rec.id) {
            Some((old, old_anchors)) if old == rec && *old_anchors == anchors => {
                report.unchanged += 1;
                continue;
            }
            Some(_) => report.changed += 1,
            None => report.added += 1,
        }
        db.correction_insert(
            Some(corpus_id),
            None,
            &CorrectionKind::Fact {
                fact: rec.clone(),
                retracted: false,
                anchors,
            },
        )?;
    }

    if prune {
        let mut stale: Vec<&String> = existing
            .keys()
            .filter(|id| !seen_ids.contains(*id))
            .collect();
        stale.sort();
        for id in stale {
            let (rec, anchors) = &existing[id];
            db.correction_insert(
                Some(corpus_id),
                None,
                &CorrectionKind::Fact {
                    fact: rec.clone(),
                    retracted: true,
                    anchors: anchors.clone(),
                },
            )?;
            report.retracted += 1;
        }
    }

    report.warnings = rebuild_variant_edges(db, corpus_id, mask)?;
    Ok(report)
}

// ── Reading files ─────────────────────────────────────────────────────────

/// Returns `(file name, raw record)` pairs. Follows index files.
fn read_records(paths: &[PathBuf]) -> Result<Vec<(String, serde_json::Value)>> {
    let mut out = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    for path in paths {
        let value = read_json(path)?;
        let listed = index_listing(&value);
        match listed {
            Some(names) => {
                let dir = path.parent().unwrap_or_else(|| Path::new("."));
                for name in names {
                    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
                        return Err(CalError::Other(format!(
                            "{}: refusing listed file name `{name}` (must be a plain file name)",
                            path.display()
                        )));
                    }
                    let file = dir.join(&name);
                    if visited.insert(file.clone()) {
                        collect_facts_file(&file, &name, &read_json(&file)?, &mut out)?;
                    }
                }
            }
            None => {
                if visited.insert(path.clone()) {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    collect_facts_file(path, &name, &value, &mut out)?;
                }
            }
        }
    }
    Ok(out)
}

fn read_json(path: &Path) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CalError::Other(format!("{}: {e}", path.display())))?;
    serde_json::from_str(&text)
        .map_err(|e| CalError::Other(format!("{}: invalid JSON: {e}", path.display())))
}

/// `Some(names)` when `value` is an index file: an object whose `facts` is a
/// non-empty list of strings.
fn index_listing(value: &serde_json::Value) -> Option<Vec<String>> {
    let items = value.get("facts")?.as_array()?;
    if items.is_empty() || !items.iter().all(|i| i.is_string()) {
        return None;
    }
    Some(
        items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect(),
    )
}

fn collect_facts_file(
    path: &Path,
    name: &str,
    value: &serde_json::Value,
    out: &mut Vec<(String, serde_json::Value)>,
) -> Result<()> {
    if value.get("format").and_then(|f| f.as_u64()) != Some(1) {
        return Err(CalError::Other(format!(
            "{}: not a facts file (expected `\"format\": 1`)",
            path.display()
        )));
    }
    let items = value
        .get("facts")
        .and_then(|f| f.as_array())
        .ok_or_else(|| CalError::Other(format!("{}: missing `facts` list", path.display())))?;
    out.extend(items.iter().map(|i| (name.to_string(), i.clone())));
    Ok(())
}

// ── Anchoring ─────────────────────────────────────────────────────────────

fn space_anchors(rec: &FactRecord, cpu_spaces: Option<&[String]>) -> bool {
    match (cpu_spaces, rec.space()) {
        (None, _) => true,
        (Some(spaces), Some(space)) => spaces.iter().any(|s| s == space),
        (Some(_), None) => false,
    }
}

fn anchors_for(
    rec: &FactRecord,
    by_addr: &HashMap<u64, Vec<&Entity>>,
    mask: Option<u64>,
    cpu_spaces: Option<&[String]>,
) -> Vec<String> {
    if !matches!(rec.kind.as_str(), "variable" | "table" | "routine")
        || !space_anchors(rec, cpu_spaces)
    {
        return vec![];
    }
    let Some(addr) = rec.address().and_then(parse_address) else {
        return vec![];
    };
    let mut hits: Vec<&Entity> = by_addr
        .get(&apply_mask(addr, mask))
        .cloned()
        .unwrap_or_default();
    if rec.kind == "routine" && hits.iter().any(|e| e.kind == "function") {
        hits.retain(|e| e.kind == "function");
    }
    let ids: BTreeSet<String> = hits.iter().map(|e| e.id.clone()).collect();
    ids.into_iter().collect()
}

fn materialise(
    corpus_id: &str,
    rec: &FactRecord,
    mask: Option<u64>,
    cpu_spaces: Option<&[String]>,
) -> Entity {
    let mut e = Entity::new(
        fact_entity_id(corpus_id, &rec.id),
        corpus_id.to_string(),
        rec.id.clone(),
        format!("fact_{}", rec.kind),
    );
    e.description = Some(rec.summary.clone());
    e.confidence = confidence_score(&rec.confidence);
    if matches!(rec.kind.as_str(), "variable" | "table" | "routine")
        && space_anchors(rec, cpu_spaces)
        && let Some(addr) = rec.address().and_then(parse_address)
    {
        e.aliases.push(canonical(apply_mask(addr, mask)));
    }
    e
}

// ── Variant edges ─────────────────────────────────────────────────────────

/// Delete and recreate this corpus's fact-derived `affects` edges from the
/// current (post-import) set of facts.
fn rebuild_variant_edges(
    db: &dyn StorageBackend,
    corpus_id: &str,
    mask: Option<u64>,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    db.edge_delete_by_scope(corpus_id, "affects", FACT_EDGE_SCOPE)?;

    let engine = CorrectionsEngine::load(db, corpus_id)?;
    let facts = engine.facts_in_corpus(corpus_id);
    let entities = db.entity_list(corpus_id)?;
    let mut by_addr: HashMap<u64, Vec<&Entity>> = HashMap::new();
    for e in &entities {
        if let Some(a) = entity_address(e) {
            by_addr.entry(apply_mask(a, mask)).or_default().push(e);
        }
    }
    let by_fact: HashMap<&str, (&FactRecord, &[String])> = facts
        .iter()
        .map(|(f, a)| (f.id.as_str(), (*f, *a)))
        .collect();

    for (variant, _) in facts.iter().filter(|(f, _)| f.kind == "variant") {
        let from = fact_entity_id(corpus_id, &variant.id);
        let affects = variant
            .data
            .get("affects")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|i| i.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut targets: BTreeMap<String, f32> = BTreeMap::new();
        let own = confidence_score(&variant.confidence);
        for item in affects {
            let mut resolved = false;
            if let Some((rec, anchors)) = by_fact.get(item) {
                resolved = true;
                let conf = own.min(confidence_score(&rec.confidence));
                let mut add = |id: String| {
                    targets.insert(id, conf);
                };
                if anchors.is_empty() || ALWAYS_MATERIALISED.contains(&rec.kind.as_str()) {
                    add(fact_entity_id(corpus_id, &rec.id));
                }
                anchors.iter().cloned().for_each(&mut add);
            } else if let Some(addr) = parse_address(item)
                && let Some(hits) = by_addr.get(&apply_mask(addr, mask))
            {
                resolved = !hits.is_empty();
                for e in hits {
                    targets.insert(e.id.clone(), own);
                }
            }
            if !resolved {
                warnings.push(format!(
                    "variant {}: `affects` target `{item}` does not resolve to a fact or address",
                    variant.id
                ));
            }
        }
        targets.remove(&from);
        for (to, conf) in targets {
            let mut edge = Edge::new(
                format!("{corpus_id}:edge:fact:affects:{}:{to}", variant.id),
                corpus_id.to_string(),
                from.clone(),
                to,
                "affects".to_string(),
                Location::new(corpus_id, format!("facts/{}", variant.id)),
            );
            edge.confidence = conf;
            edge.origin_scope = FACT_EDGE_SCOPE.to_string();
            db.edge_upsert(&edge)?;
        }
    }
    Ok(warnings)
}
