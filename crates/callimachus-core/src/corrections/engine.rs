use crate::corrections::facts::{FactRecord, confidence_score};
use crate::corrections::layouts::{LAYOUT_KIND, Layout};
use crate::corrections::types::{Correction, CorrectionKind};
use crate::error::Result;
use crate::storage::StorageBackend;
use crate::types::entity::Entity;

struct FactEntry<'a> {
    corpus_id: Option<&'a str>,
    fact: &'a FactRecord,
    retracted: bool,
    anchors: &'a [String],
}

/// Overlay engine that applies a sorted-by-`applied_at` list of corrections
/// to a snapshot of entities and summaries. Does not mutate the database.
pub struct CorrectionsEngine {
    corrections: Vec<Correction>,
}

impl CorrectionsEngine {
    /// Construct from a pre-loaded, `applied_at`-sorted slice of records.
    pub fn new(corrections: Vec<Correction>) -> Self {
        Self { corrections }
    }

    /// Load all corrections for a corpus from the database, already sorted ASC.
    /// Collection-scoped (EntityLink) corrections are excluded — they never have
    /// a corpus_id and are meaningless at the per-corpus level.
    pub fn load(db: &dyn StorageBackend, corpus_id: &str) -> Result<Self> {
        let corrections = db.correction_list(corpus_id)?;
        Ok(Self { corrections })
    }

    /// Load corrections for ALL corpora from the database, sorted by applied_at ASC.
    /// Used by the MCP server, which serves multiple corpora from one QueryService.
    pub fn load_all(db: &dyn StorageBackend) -> Result<Self> {
        // Filter out collection-scoped EntityLink corrections — they belong to
        // CollectionService, not the corpus-level engine.
        let corrections = db
            .correction_list_all()?
            .into_iter()
            .filter(|c| c.corpus_id.is_some())
            .collect();
        Ok(Self { corrections })
    }

    /// Load collection-scoped corrections for a collection. Returns the raw
    /// list; the EntityLinkIndex in CollectionService consumes these.
    pub fn load_for_collection(
        db: &dyn StorageBackend,
        collection_id: &str,
    ) -> Result<Vec<Correction>> {
        db.correction_list_for_collection(collection_id)
    }

    /// Apply all corrections in order to the provided entity list, mutating it
    /// in place: merges collapse two entities into one, renames update
    /// `canonical_name`, alias corrections add/remove aliases.
    pub fn apply_to_entities(&self, entities: &mut Vec<Entity>) {
        // --- Pass 1: build union-find (canonical_id resolution) ---
        // For each Merge, map the non-canonical id → canonical id.
        use std::collections::HashMap;
        let mut parent: HashMap<String, String> = HashMap::new();

        for corr in &self.corrections {
            if let CorrectionKind::Merge {
                entity_a_id,
                entity_b_id,
                canonical_id,
            } = &corr.kind
            {
                // Determine which id is absorbed (non-canonical).
                let absorbed = if canonical_id == entity_a_id {
                    entity_b_id.clone()
                } else {
                    entity_a_id.clone()
                };
                parent.insert(absorbed, canonical_id.clone());
            }
        }

        // Union-find path compression helper (operates on the parent map).
        fn find(parent: &HashMap<String, String>, id: &str) -> String {
            let mut cur = id.to_string();
            while let Some(next) = parent.get(&cur) {
                cur = next.clone();
            }
            cur
        }

        // --- Pass 2: collapse absorbed entities into canonical ones ---
        // Walk the entity list; for each entity whose resolved id differs from
        // its own id, it is absorbed. Accumulate their contributions into the
        // canonical entity.

        // First index: find all canonical-id entities.
        let mut canonical_idx: HashMap<String, usize> = HashMap::new();
        for (i, e) in entities.iter().enumerate() {
            let root = find(&parent, &e.id);
            // The canonical entity maps to itself.
            if root == e.id {
                canonical_idx.insert(e.id.clone(), i);
            }
        }

        // Collect absorbed entities and their contributions.
        let mut absorbed_contributions: HashMap<String, Vec<(String, Vec<String>, u32)>> =
            HashMap::new(); // canonical_id → [(name, aliases, count)]

        let mut to_remove: Vec<usize> = vec![];
        for (i, e) in entities.iter().enumerate() {
            let root = find(&parent, &e.id);
            if root != e.id {
                // This entity is absorbed.
                absorbed_contributions.entry(root).or_default().push((
                    e.canonical_name.clone(),
                    e.aliases.clone(),
                    e.appearance_count,
                ));
                to_remove.push(i);
            }
        }

        // Apply contributions to canonical entities.
        for (canonical_id, contributions) in &absorbed_contributions {
            if let Some(&idx) = canonical_idx.get(canonical_id) {
                let canon = &mut entities[idx];
                for (name, aliases, count) in contributions {
                    // Absorb the absorbed entity's canonical_name as an alias.
                    if !canon.aliases.contains(name) {
                        canon.aliases.push(name.clone());
                    }
                    // Absorb its aliases.
                    for alias in aliases {
                        if !canon.aliases.contains(alias) {
                            canon.aliases.push(alias.clone());
                        }
                    }
                    // Sum appearance counts.
                    canon.appearance_count += count;
                }
            }
        }

        // Remove absorbed entities (in reverse index order to preserve positions).
        to_remove.sort_unstable_by(|a, b| b.cmp(a));
        for idx in to_remove {
            entities.remove(idx);
        }

        // --- Pass 3: apply Rename corrections ---
        for corr in &self.corrections {
            if let CorrectionKind::Rename {
                entity_id,
                new_name,
            } = &corr.kind
            {
                // Rename applies to whatever the entity's current id resolves to.
                let target = find(&parent, entity_id);
                for e in entities.iter_mut() {
                    if e.id == target {
                        e.canonical_name = new_name.clone();
                    }
                }
            }
        }

        // --- Pass 4: apply Alias corrections in applied_at order ---
        for corr in &self.corrections {
            if let CorrectionKind::Alias {
                entity_id,
                add,
                remove,
            } = &corr.kind
            {
                let target = find(&parent, entity_id);
                for e in entities.iter_mut() {
                    if e.id == target {
                        for a in add {
                            if !e.aliases.contains(a) {
                                e.aliases.push(a.clone());
                            }
                        }
                        e.aliases.retain(|a| !remove.contains(a));
                    }
                }
            }
        }

        // --- Pass 5: anchored facts add their id as a virtual alias ---
        // A layout names one field per anchor (`obj.x`) instead of its own id.
        for entry in self.latest_facts() {
            if entry.retracted {
                continue;
            }
            let layout = Layout::of_fact(entry.fact);
            for anchor in entry.anchors {
                let (alias, confidence) = match (&layout, entry.corpus_id) {
                    (Some(l), Some(corpus)) => match l.field_for_anchor(corpus, anchor) {
                        // A layout field's confidence, else the layout's.
                        Some(f) => (
                            l.qualified(f),
                            Some(confidence_score(
                                f.confidence.as_deref().unwrap_or(&entry.fact.confidence),
                            )),
                        ),
                        None => continue,
                    },
                    _ => (entry.fact.id.clone(), None),
                };
                let target = find(&parent, anchor);
                for e in entities.iter_mut() {
                    if e.id == target {
                        if !e.aliases.contains(&alias) {
                            e.aliases.push(alias.clone());
                        }
                        if let Some(c) = confidence {
                            e.confidence = c;
                        }
                    }
                }
            }
        }
    }

    /// Latest non-retracted fact record per `(corpus, fact id)`.
    pub fn facts(&self) -> Vec<&FactRecord> {
        self.latest_facts()
            .into_iter()
            .filter(|e| !e.retracted)
            .map(|e| e.fact)
            .collect()
    }

    /// Latest non-retracted facts of one corpus, with their anchor entity ids.
    pub fn facts_in_corpus(&self, corpus_id: &str) -> Vec<(&FactRecord, &[String])> {
        self.latest_facts()
            .into_iter()
            .filter(|e| !e.retracted && e.corpus_id == Some(corpus_id))
            .map(|e| (e.fact, e.anchors))
            .collect()
    }

    /// Latest non-retracted facts anchored to `entity_id`, with all their anchors.
    ///
    /// A `layout` fact is returned as a synthetic `field` fact for the one field
    /// the entity stands for (`layout.obj.x`).
    pub fn facts_for_entity(&self, entity_id: &str) -> Vec<(FactRecord, Vec<String>)> {
        self.latest_facts()
            .into_iter()
            .filter(|e| !e.retracted && e.anchors.iter().any(|a| a == entity_id))
            .filter_map(|e| {
                let anchors = e.anchors.to_vec();
                let Some(layout) = Layout::of_fact(e.fact) else {
                    return Some((e.fact.clone(), anchors));
                };
                let field = layout.field_for_anchor(e.corpus_id?, entity_id)?;
                let qualified = layout.qualified(field);
                let synthetic = FactRecord {
                    id: format!("{}.{}", e.fact.id, field.name),
                    kind: "field".to_string(),
                    topic: e.fact.topic.clone(),
                    summary: field.summary.clone().unwrap_or(qualified),
                    data: serde_json::json!({
                        "layout": layout.name,
                        "offset": field.offset,
                        "type": field.ty,
                    }),
                    confidence: field
                        .confidence
                        .clone()
                        .unwrap_or_else(|| e.fact.confidence.clone()),
                    evidence: e.fact.evidence.clone(),
                    notes: e.fact.notes.clone(),
                };
                Some((synthetic, anchors))
            })
            .collect()
    }

    /// Latest non-retracted `layout` facts, optionally of one corpus.
    pub(crate) fn layout_facts(&self, corpus_id: Option<&str>) -> Vec<(&FactRecord, &[String])> {
        self.latest_facts()
            .into_iter()
            .filter(|e| {
                !e.retracted
                    && e.fact.kind == LAYOUT_KIND
                    && corpus_id.is_none_or(|c| e.corpus_id == Some(c))
            })
            .map(|e| (e.fact, e.anchors))
            .collect()
    }

    /// Anchor entity ids of a non-retracted fact in a corpus, by fact id.
    pub fn anchors_for_fact(&self, corpus_id: &str, fact_id: &str) -> Vec<String> {
        self.latest_facts()
            .into_iter()
            .find(|e| !e.retracted && e.corpus_id == Some(corpus_id) && e.fact.id == fact_id)
            .map(|e| e.anchors.to_vec())
            .unwrap_or_default()
    }

    /// Anchor entity ids named by a layout alias such as `obj.x`, one per base
    /// that has an entity.
    pub fn anchors_for_layout_field(&self, corpus_id: &str, qualified: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (fact, anchors) in self.layout_facts(Some(corpus_id)) {
            let Some(layout) = Layout::of_fact(fact) else {
                continue;
            };
            out.extend(
                anchors
                    .iter()
                    .filter(|a| {
                        layout
                            .field_for_anchor(corpus_id, a)
                            .is_some_and(|f| layout.qualified(f) == qualified)
                    })
                    .cloned(),
            );
        }
        out
    }

    /// Latest record (retracted or not) per `(corpus, fact id)`, by `applied_at`.
    fn latest_facts(&self) -> Vec<FactEntry<'_>> {
        use std::collections::HashMap;
        let mut idx: HashMap<(Option<&str>, &str), usize> = HashMap::new();
        let mut out: Vec<FactEntry<'_>> = Vec::new();
        // Corrections are sorted applied_at ASC: later entries replace earlier.
        for corr in &self.corrections {
            if let CorrectionKind::Fact {
                fact,
                retracted,
                anchors,
            } = &corr.kind
            {
                let entry = FactEntry {
                    corpus_id: corr.corpus_id.as_deref(),
                    fact,
                    retracted: *retracted,
                    anchors,
                };
                match idx.get(&(corr.corpus_id.as_deref(), fact.id.as_str())) {
                    Some(&i) => out[i] = entry,
                    None => {
                        idx.insert((corr.corpus_id.as_deref(), fact.id.as_str()), out.len());
                        out.push(entry);
                    }
                }
            }
        }
        out
    }

    /// Return the most-recently-applied `EditSummary` text for the given
    /// `(target_kind, target_id)` pair, or `None` if no matching correction exists.
    pub fn override_summary(&self, target_kind: &str, target_id: &str) -> Option<&str> {
        // Corrections are sorted applied_at ASC; iterate in reverse to get latest.
        self.corrections.iter().rev().find_map(|corr| {
            if let CorrectionKind::EditSummary {
                target_kind: tk,
                target_id: tid,
                text,
            } = &corr.kind
            {
                if tk == target_kind && tid == target_id {
                    Some(text.as_str())
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    /// Resolve an entity ID through any Merge corrections (union-find style).
    /// Returns the canonical entity ID; if no merge applies, returns the input.
    pub fn resolve_entity_id<'a>(&'a self, entity_id: &'a str) -> &'a str {
        // Build the parent map and follow the chain. We iterate the corrections
        // vector rather than caching to avoid extra allocation for the common case.
        let mut cur: &str = entity_id;
        // Safety limit: avoid infinite loops from contradictory corrections.
        for _ in 0..self.corrections.len() + 1 {
            let next = self.corrections.iter().find_map(|corr| {
                if let CorrectionKind::Merge {
                    entity_a_id,
                    entity_b_id,
                    canonical_id,
                } = &corr.kind
                {
                    let absorbed = if canonical_id == entity_a_id {
                        entity_b_id.as_str()
                    } else {
                        entity_a_id.as_str()
                    };
                    if cur == absorbed {
                        return Some(canonical_id.as_str());
                    }
                }
                None
            });
            match next {
                Some(parent) => cur = parent,
                None => break,
            }
        }
        cur
    }

    /// All correction records in `applied_at` ASC order.
    pub fn all(&self) -> &[Correction] {
        &self.corrections
    }
}

// ---------------------------------------------------------------------------
// Helpers for building test fixtures
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_helpers {
    use crate::corrections::types::{Correction, CorrectionKind};
    use crate::types::entity::Entity;

    pub fn make_record(id: &str, corpus_id: &str, kind: CorrectionKind) -> Correction {
        Correction {
            id: id.to_string(),
            corpus_id: Some(corpus_id.to_string()),
            collection_id: None,
            kind,
            applied_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    pub fn make_record_at(
        id: &str,
        corpus_id: &str,
        kind: CorrectionKind,
        applied_at: &str,
    ) -> Correction {
        Correction {
            id: id.to_string(),
            corpus_id: Some(corpus_id.to_string()),
            collection_id: None,
            kind,
            applied_at: applied_at.to_string(),
        }
    }

    pub fn make_entity(
        id: &str,
        corpus_id: &str,
        name: &str,
        kind: &str,
        aliases: Vec<&str>,
        count: u32,
        confidence: f32,
    ) -> Entity {
        Entity {
            id: id.to_string(),
            corpus_id: corpus_id.to_string(),
            canonical_name: name.to_string(),
            kind: kind.to_string(),
            abstract_kind: String::new(),
            aliases: aliases.into_iter().map(str::to_string).collect(),
            description: None,
            first_location: None,
            last_location: None,
            appearance_count: count,
            confidence,
            provenance: None,
            start_line: None,
            end_line: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::test_helpers::*;
    use super::*;
    use crate::corrections::types::CorrectionKind;

    const CORPUS: &str = "test-corpus";

    // -----------------------------------------------------------------------
    // Merge
    // -----------------------------------------------------------------------

    /// After a Merge correction keeping A, apply_to_entities collapses B into A:
    /// - only one entity remains
    /// - its id and canonical_name come from A
    /// - its aliases include B's canonical_name and all of B's aliases
    /// - its appearance_count is the sum of A's and B's counts
    #[test]
    fn test_merge_apply_entities() {
        let kind = CorrectionKind::Merge {
            entity_a_id: "a".to_string(),
            entity_b_id: "b".to_string(),
            canonical_id: "a".to_string(),
        };
        let engine = CorrectionsEngine::new(vec![make_record("c1", CORPUS, kind)]);

        let mut entities = vec![
            make_entity("a", CORPUS, "Eisenhorn", "character", vec![], 30, 0.9),
            make_entity(
                "b",
                CORPUS,
                "the Inquisitor",
                "character",
                vec!["Gregor"],
                17,
                0.7,
            ),
        ];

        engine.apply_to_entities(&mut entities);

        assert_eq!(
            entities.len(),
            1,
            "B should be merged into A, leaving one entity"
        );
        let a = &entities[0];
        assert_eq!(a.id, "a");
        assert_eq!(a.canonical_name, "Eisenhorn");
        assert_eq!(a.appearance_count, 47, "counts should sum: 30 + 17 = 47");
        assert!(
            a.aliases.contains(&"the Inquisitor".to_string()),
            "A's aliases should include B's canonical_name"
        );
        assert!(
            a.aliases.contains(&"Gregor".to_string()),
            "A's aliases should include B's prior aliases"
        );
    }

    /// resolve_entity_id follows Merge chains: B → A, A → A.
    #[test]
    fn test_merge_resolve_entity_id() {
        let kind = CorrectionKind::Merge {
            entity_a_id: "a".to_string(),
            entity_b_id: "b".to_string(),
            canonical_id: "a".to_string(),
        };
        let engine = CorrectionsEngine::new(vec![make_record("c1", CORPUS, kind)]);

        assert_eq!(engine.resolve_entity_id("b"), "a", "B should resolve to A");
        assert_eq!(
            engine.resolve_entity_id("a"),
            "a",
            "A should resolve to itself"
        );
    }

    // -----------------------------------------------------------------------
    // Rename
    // -----------------------------------------------------------------------

    /// A Rename correction updates the entity's canonical_name.
    #[test]
    fn test_rename() {
        let kind = CorrectionKind::Rename {
            entity_id: "a".to_string(),
            new_name: "NewName".to_string(),
        };
        let engine = CorrectionsEngine::new(vec![make_record("c1", CORPUS, kind)]);

        let mut entities = vec![make_entity(
            "a",
            CORPUS,
            "OldName",
            "character",
            vec![],
            10,
            0.8,
        )];

        engine.apply_to_entities(&mut entities);

        assert_eq!(entities[0].canonical_name, "NewName");
    }

    // -----------------------------------------------------------------------
    // Alias add / remove
    // -----------------------------------------------------------------------

    /// An Alias correction with add=["bar"] appends "bar" to existing aliases.
    #[test]
    fn test_alias_add() {
        let kind = CorrectionKind::Alias {
            entity_id: "a".to_string(),
            add: vec!["bar".to_string()],
            remove: vec![],
        };
        let engine = CorrectionsEngine::new(vec![make_record("c1", CORPUS, kind)]);

        let mut entities = vec![make_entity(
            "a",
            CORPUS,
            "Alpha",
            "character",
            vec!["foo"],
            5,
            0.8,
        )];

        engine.apply_to_entities(&mut entities);

        let aliases = &entities[0].aliases;
        assert!(
            aliases.contains(&"foo".to_string()),
            "existing alias retained"
        );
        assert!(aliases.contains(&"bar".to_string()), "new alias added");
    }

    /// An Alias correction with remove=["foo"] drops "foo" from existing aliases.
    #[test]
    fn test_alias_remove() {
        let kind = CorrectionKind::Alias {
            entity_id: "a".to_string(),
            add: vec![],
            remove: vec!["foo".to_string()],
        };
        let engine = CorrectionsEngine::new(vec![make_record("c1", CORPUS, kind)]);

        let mut entities = vec![make_entity(
            "a",
            CORPUS,
            "Alpha",
            "character",
            vec!["foo", "bar"],
            5,
            0.8,
        )];

        engine.apply_to_entities(&mut entities);

        let aliases = &entities[0].aliases;
        assert!(
            !aliases.contains(&"foo".to_string()),
            "foo should be removed"
        );
        assert!(aliases.contains(&"bar".to_string()), "bar should remain");
    }

    // -----------------------------------------------------------------------
    // EditSummary
    // -----------------------------------------------------------------------

    /// override_summary returns the correction text for a matching (kind, id) pair.
    #[test]
    fn test_edit_summary_match() {
        let kind = CorrectionKind::EditSummary {
            target_kind: "chunk".to_string(),
            target_id: "calli://xenos/ch/3".to_string(),
            text: "operator text".to_string(),
        };
        let engine = CorrectionsEngine::new(vec![make_record("c1", CORPUS, kind)]);

        let result = engine.override_summary("chunk", "calli://xenos/ch/3");
        assert_eq!(result, Some("operator text"));
    }

    /// override_summary returns None when no correction matches the target.
    #[test]
    fn test_edit_summary_no_match() {
        let kind = CorrectionKind::EditSummary {
            target_kind: "chunk".to_string(),
            target_id: "calli://xenos/ch/3".to_string(),
            text: "operator text".to_string(),
        };
        let engine = CorrectionsEngine::new(vec![make_record("c1", CORPUS, kind)]);

        let result = engine.override_summary("chunk", "calli://xenos/ch/99");
        assert_eq!(result, None);
    }

    // -----------------------------------------------------------------------
    // Chained corrections
    // -----------------------------------------------------------------------

    /// Merge A←B then rename A: final entity has the new name, B's aliases, summed count.
    #[test]
    fn test_chained_merge_then_rename() {
        let merge = CorrectionKind::Merge {
            entity_a_id: "a".to_string(),
            entity_b_id: "b".to_string(),
            canonical_id: "a".to_string(),
        };
        let rename = CorrectionKind::Rename {
            entity_id: "a".to_string(),
            new_name: "NewName".to_string(),
        };
        let engine = CorrectionsEngine::new(vec![
            make_record_at("c1", CORPUS, merge, "2026-01-01T00:00:00Z"),
            make_record_at("c2", CORPUS, rename, "2026-01-02T00:00:00Z"),
        ]);

        let mut entities = vec![
            make_entity("a", CORPUS, "OldNameA", "character", vec![], 20, 0.9),
            make_entity(
                "b",
                CORPUS,
                "OldNameB",
                "character",
                vec!["alias-b"],
                10,
                0.7,
            ),
        ];

        engine.apply_to_entities(&mut entities);

        assert_eq!(entities.len(), 1, "B merged into A");
        let a = &entities[0];
        assert_eq!(a.id, "a");
        assert_eq!(a.canonical_name, "NewName", "rename applied after merge");
        assert_eq!(a.appearance_count, 30, "counts summed");
        assert!(
            a.aliases.contains(&"OldNameB".to_string()),
            "B's name absorbed as alias"
        );
        assert!(
            a.aliases.contains(&"alias-b".to_string()),
            "B's aliases absorbed"
        );
    }

    // -----------------------------------------------------------------------
    // Empty corrections
    // -----------------------------------------------------------------------

    /// With no corrections, apply_to_entities is a no-op.
    #[test]
    fn test_empty_corrections() {
        let engine = CorrectionsEngine::new(vec![]);

        let mut entities = vec![
            make_entity("a", CORPUS, "Alpha", "character", vec![], 5, 0.8),
            make_entity("b", CORPUS, "Beta", "character", vec![], 3, 0.7),
        ];
        let before_ids: Vec<String> = entities.iter().map(|e| e.id.clone()).collect();

        engine.apply_to_entities(&mut entities);

        assert_eq!(entities.len(), 2, "entity count unchanged");
        let after_ids: Vec<String> = entities.iter().map(|e| e.id.clone()).collect();
        assert_eq!(before_ids, after_ids, "entity order/ids unchanged");
    }
}
