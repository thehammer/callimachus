//! Link game-corpus entities to reference-corpus entities that share an address.

use crate::corrections::address::{
    apply_mask, canonical, cpu_spaces_from_config, entity_address, is_reference, mask_from_config,
    parse_address,
};
use crate::corrections::engine::CorrectionsEngine;
use crate::corrections::types::{CorrectionKind, EntityLinkKind};
use crate::error::Result;
use crate::storage::StorageBackend;
use std::collections::{HashMap, HashSet};

/// For every non-reference corpus G and reference corpus R in `collection_id`,
/// append a `references` link from each G entity with an address to the R
/// fact entities (or fact-anchored entities) at the same masked address.
/// Returns the number of links added; existing links are not duplicated.
pub fn link_addresses(db: &dyn StorageBackend, collection_id: &str) -> Result<usize> {
    let corpus_ids = db.collection_resolve_corpus_ids(collection_id)?;
    let mut games = Vec::new();
    let mut references = Vec::new();
    for id in &corpus_ids {
        if let Some(c) = db.corpus_get(id)? {
            if is_reference(&c.config) {
                references.push(c);
            } else {
                games.push(c);
            }
        }
    }

    // Existing endpoints, in either direction.
    let mut existing: HashSet<(String, String, String, String)> = HashSet::new();
    for c in db.correction_list_for_collection(collection_id)? {
        if let CorrectionKind::EntityLink {
            corpus_a_id,
            entity_a_id,
            corpus_b_id,
            entity_b_id,
            ..
        } = c.kind
        {
            existing.insert((
                corpus_b_id.clone(),
                entity_b_id.clone(),
                corpus_a_id.clone(),
                entity_a_id.clone(),
            ));
            existing.insert((corpus_a_id, entity_a_id, corpus_b_id, entity_b_id));
        }
    }

    // Reference side: masked address → (entity ids).
    let mut ref_targets: Vec<(String, HashMap<u64, Vec<String>>)> = Vec::new();
    for r in &references {
        let mask = mask_from_config(&r.config);
        let spaces = cpu_spaces_from_config(&r.config);
        let mut map: HashMap<u64, Vec<String>> = HashMap::new();
        for e in db.entity_list(&r.id)? {
            if e.kind.starts_with("fact_")
                && let Some(a) = entity_address(&e)
            {
                map.entry(apply_mask(a, mask)).or_default().push(e.id);
            }
        }
        let engine = CorrectionsEngine::load(db, &r.id)?;
        for (fact, anchors) in engine.facts_in_corpus(&r.id) {
            let anchors_here = spaces
                .as_ref()
                .is_none_or(|s| fact.space().is_some_and(|sp| s.iter().any(|x| x == sp)));
            if anchors.is_empty() || !anchors_here {
                continue;
            }
            if let Some(a) = fact.address().and_then(parse_address) {
                let list = map.entry(apply_mask(a, mask)).or_default();
                for id in anchors {
                    if !list.contains(id) {
                        list.push(id.clone());
                    }
                }
            }
        }
        ref_targets.push((r.id.clone(), map));
    }

    let mut added = 0;
    for g in &games {
        let g_mask = mask_from_config(&g.config);
        for e in db.entity_list(&g.id)? {
            if e.kind.starts_with("fact_") {
                continue;
            }
            let Some(addr) = entity_address(&e).map(|a| apply_mask(a, g_mask)) else {
                continue;
            };
            for (r_id, map) in &ref_targets {
                for target in map.get(&addr).into_iter().flatten() {
                    let key = (g.id.clone(), e.id.clone(), r_id.clone(), target.clone());
                    if !existing.insert(key.clone()) {
                        continue;
                    }
                    existing.insert((key.2.clone(), key.3.clone(), key.0.clone(), key.1.clone()));
                    db.correction_insert(
                        None,
                        Some(collection_id),
                        &CorrectionKind::EntityLink {
                            corpus_a_id: g.id.clone(),
                            entity_a_id: e.id.clone(),
                            corpus_b_id: r_id.clone(),
                            entity_b_id: target.clone(),
                            kind: EntityLinkKind::References,
                            note: Some(format!("address {}", canonical(addr))),
                        },
                    )?;
                    added += 1;
                }
            }
        }
    }
    Ok(added)
}
