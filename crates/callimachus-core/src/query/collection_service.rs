use crate::corrections::types::{Correction, CorrectionKind, EntityLinkKind};
use crate::error::Result;
use crate::query::service::QueryService;
use crate::query::types::{
    CollectionEntityMatch, CollectionEntityMeetInput, CollectionEntityMeetOutput,
    CollectionEntityRelation, CollectionEntityResolveInput, CollectionEntityResolveOutput,
    CollectionLocation, CollectionOverviewInput, CollectionOverviewOutput, CollectionSearchInput,
    CollectionSearchOutput, CollectionSearchResult, CorpusOverviewInput,
};
use crate::storage::StorageBackend;
use crate::types::{Collection, ToolResult};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

// ── EntityLinkIndex ───────────────────────────────────────────────────────────

/// In-memory index of cross-corpus entity links, built from a collection's corrections.
///
/// All links are stored bidirectionally. The `same_as_class` method computes a
/// transitive closure over `SameAs` links only.
type LinkMap = HashMap<(String, String), Vec<(String, String, EntityLinkKind)>>;

pub struct EntityLinkIndex {
    /// (corpus_id, entity_id) → Vec<(corpus_id, entity_id, kind)>
    links: LinkMap,
    /// All EntityLink corrections (for computing notes in relation output).
    raw: Vec<EntityLinkRaw>,
}

#[derive(Clone)]
struct EntityLinkRaw {
    corpus_a: String,
    entity_a: String,
    corpus_b: String,
    entity_b: String,
    #[allow(dead_code)]
    kind: EntityLinkKind,
    note: Option<String>,
}

impl EntityLinkIndex {
    /// Build from a slice of Correction records. Non-EntityLink corrections are silently ignored.
    pub fn from_corrections(corrections: &[Correction]) -> Self {
        let mut links: LinkMap = HashMap::new();
        let mut raw = Vec::new();

        for corr in corrections {
            if let CorrectionKind::EntityLink {
                corpus_a_id,
                entity_a_id,
                corpus_b_id,
                entity_b_id,
                kind,
                note,
            } = &corr.kind
            {
                // Skip degenerate self-links.
                if corpus_a_id == corpus_b_id && entity_a_id == entity_b_id {
                    continue;
                }

                let key_a = (corpus_a_id.clone(), entity_a_id.clone());
                let key_b = (corpus_b_id.clone(), entity_b_id.clone());

                // Store forward direction.
                links.entry(key_a.clone()).or_default().push((
                    corpus_b_id.clone(),
                    entity_b_id.clone(),
                    *kind,
                ));

                // Store reverse direction (all links are bidirectional).
                links.entry(key_b).or_default().push((
                    corpus_a_id.clone(),
                    entity_a_id.clone(),
                    *kind,
                ));

                raw.push(EntityLinkRaw {
                    corpus_a: corpus_a_id.clone(),
                    entity_a: entity_a_id.clone(),
                    corpus_b: corpus_b_id.clone(),
                    entity_b: entity_b_id.clone(),
                    kind: *kind,
                    note: note.clone(),
                });
            }
        }

        Self { links, raw }
    }

    /// Returns true when no links have been indexed.
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// All (corpus_id, entity_id, kind) directly linked to the given pair.
    /// If `kinds` is empty, returns all kinds.
    pub fn linked(
        &self,
        corpus_id: &str,
        entity_id: &str,
        kinds: &[EntityLinkKind],
    ) -> Vec<(String, String, EntityLinkKind)> {
        let key = (corpus_id.to_string(), entity_id.to_string());
        let Some(targets) = self.links.get(&key) else {
            return vec![];
        };

        targets
            .iter()
            .filter(|(tc, te, tk)| {
                // Never return the query pair itself (guard against double-storage bugs).
                if tc == corpus_id && te == entity_id {
                    return false;
                }
                kinds.is_empty() || kinds.contains(tk)
            })
            .cloned()
            .collect()
    }

    /// Transitive closure over SameAs links only.
    /// Returns the equivalence class including the given entity (always ≥ 1 element).
    pub fn same_as_class(&self, corpus_id: &str, entity_id: &str) -> HashSet<(String, String)> {
        let mut class = HashSet::new();
        let mut queue = vec![(corpus_id.to_string(), entity_id.to_string())];

        while let Some(pair) = queue.pop() {
            if !class.insert(pair.clone()) {
                continue; // already visited
            }
            for (tc, te, _kind) in self.linked(&pair.0, &pair.1, &[EntityLinkKind::SameAs]) {
                let next = (tc, te);
                if !class.contains(&next) {
                    queue.push(next);
                }
            }
        }

        class
    }

    /// Look up the raw note for a link from (corpus_a, entity_a) to (corpus_b, entity_b).
    fn note_for(
        &self,
        corpus_a: &str,
        entity_a: &str,
        corpus_b: &str,
        entity_b: &str,
    ) -> Option<String> {
        self.raw.iter().find_map(|r| {
            if (r.corpus_a == corpus_a
                && r.entity_a == entity_a
                && r.corpus_b == corpus_b
                && r.entity_b == entity_b)
                || (r.corpus_b == corpus_a
                    && r.entity_b == entity_a
                    && r.corpus_a == corpus_b
                    && r.entity_a == entity_b)
            {
                r.note.clone()
            } else {
                None
            }
        })
    }

    /// Returns true if there is a raw (original-direction) link from
    /// (corpus_a, entity_a) to (corpus_b, entity_b). Does NOT match the reverse.
    fn is_forward_link(
        &self,
        corpus_a: &str,
        entity_a: &str,
        corpus_b: &str,
        entity_b: &str,
    ) -> bool {
        self.raw.iter().any(|r| {
            r.corpus_a == corpus_a
                && r.entity_a == entity_a
                && r.corpus_b == corpus_b
                && r.entity_b == entity_b
        })
    }
}

// ── CollectionService ─────────────────────────────────────────────────────────

/// Fan-out query service over a named collection of corpora.
///
/// One `QueryService` per resolved leaf corpus, plus an `EntityLinkIndex` built
/// from collection-scoped EntityLink corrections.
pub struct CollectionService {
    backend: Arc<dyn StorageBackend>,
    pub(crate) collection: Collection,
    pub(crate) corpus_services: HashMap<String, QueryService>,
    pub(crate) links: EntityLinkIndex,
    /// Resolved leaf corpus IDs in DFS order (deterministic).
    corpus_order: Vec<String>,
    /// Maps corpus_id → corpus name.
    corpus_names: HashMap<String, String>,
}

impl CollectionService {
    /// Load a collection by ID and open one `QueryService` per resolved leaf corpus.
    pub fn load(backend: Arc<dyn StorageBackend>, collection_id: &str) -> Result<Self> {
        let collection = backend.collection_require(collection_id)?;
        let corpus_order = backend.collection_resolve_corpus_ids(collection_id)?;

        let mut corpus_services = HashMap::new();
        let mut corpus_names = HashMap::new();

        for corpus_id in &corpus_order {
            if let Some(corpus) = backend.corpus_get(corpus_id)? {
                corpus_names.insert(corpus_id.clone(), corpus.name.clone());
                corpus_services.insert(corpus_id.clone(), QueryService::new(Arc::clone(&backend)));
            }
        }

        // Load collection-scoped corrections for the link index.
        let corrections = backend.correction_list_for_collection(collection_id)?;
        let links = EntityLinkIndex::from_corrections(&corrections);

        Ok(Self {
            backend,
            collection,
            corpus_services,
            links,
            corpus_order,
            corpus_names,
        })
    }

    // ── Test inspection helpers ─────────────────────────────────────────────

    /// Returns true when no member corpora were resolved (empty collection).
    pub fn corpus_services_is_empty(&self) -> bool {
        self.corpus_services.is_empty()
    }

    /// Returns true when the link index is empty.
    pub fn links_is_empty(&self) -> bool {
        self.links.is_empty()
    }

    // ── collection_search ───────────────────────────────────────────────────

    /// Fan-out keyword search across all corpora in the collection.
    /// Results are merged and sorted by (relevance DESC, corpus_id ASC, location_uri ASC).
    pub fn collection_search(
        &self,
        input: CollectionSearchInput,
    ) -> ToolResult<CollectionSearchOutput> {
        let limit = input.limit.unwrap_or(20) as usize;
        let mut all: Vec<CollectionSearchResult> = Vec::new();

        for corpus_id in &self.corpus_order {
            let corpus_name = self
                .corpus_names
                .get(corpus_id)
                .cloned()
                .unwrap_or_else(|| corpus_id.clone());

            let fts_results = match self.backend.fts_search(corpus_id, &input.query, limit) {
                Ok(r) => r,
                Err(_) => continue,
            };

            for r in fts_results {
                let location = match crate::types::Location::parse(&r.location_uri) {
                    Ok(loc) => loc,
                    Err(_) => continue,
                };
                all.push(CollectionSearchResult {
                    corpus_id: corpus_id.clone(),
                    corpus_name: corpus_name.clone(),
                    location,
                    snippet: r.snippet,
                    relevance: r.rank as f32,
                });
            }
        }

        // Sort: relevance DESC, corpus_id ASC, location_uri ASC.
        all.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.corpus_id.cmp(&b.corpus_id))
                .then_with(|| a.location.uri.cmp(&b.location.uri))
        });
        all.truncate(limit);

        ToolResult::ok(CollectionSearchOutput { results: all })
    }

    // ── collection_overview ─────────────────────────────────────────────────

    pub fn collection_overview(
        &self,
        _input: CollectionOverviewInput,
    ) -> ToolResult<CollectionOverviewOutput> {
        let mut corpora = Vec::new();
        let mut total_chunks = 0u64;
        let mut total_entities = 0u64;

        for corpus_id in &self.corpus_order {
            let chunks = self.backend.chunk_count(corpus_id).unwrap_or(0);
            let entities = self.backend.entity_count(corpus_id).unwrap_or(0);
            total_chunks += chunks;
            total_entities += entities;

            // Build a lightweight overview via the per-corpus service.
            if let Some(svc) = self.corpus_services.get(corpus_id) {
                let result = svc.corpus_overview(CorpusOverviewInput {
                    corpus_id: corpus_id.clone(),
                });
                if let ToolResult::Ok(ok) = result {
                    corpora.push(ok.data);
                }
            }
        }

        // Nested collection children (direct member_type=collection).
        let direct_members = self
            .backend
            .collection_direct_members(&self.collection.id)
            .unwrap_or_default();
        let mut nested_collections = Vec::new();
        for m in direct_members {
            if m.member_type == crate::types::MemberType::Collection {
                let Ok(Some(c)) = self.backend.collection_get(&m.member_id) else {
                    continue;
                };
                nested_collections.push(c);
            }
        }

        // Link counts by kind.
        let mut cross_corpus_links_by_kind: BTreeMap<String, u64> = BTreeMap::new();
        let corrections = self
            .backend
            .correction_list_for_collection(&self.collection.id)
            .unwrap_or_default();
        for corr in &corrections {
            if let CorrectionKind::EntityLink { kind, .. } = &corr.kind {
                *cross_corpus_links_by_kind
                    .entry(kind.as_str().to_string())
                    .or_insert(0) += 1;
            }
        }

        ToolResult::ok(CollectionOverviewOutput {
            collection: self.collection.clone(),
            corpora,
            nested_collections,
            total_chunks,
            total_entities,
            cross_corpus_links_by_kind,
        })
    }

    // ── collection_entity_resolve ───────────────────────────────────────────

    /// Resolve an entity name across all corpora in the collection.
    ///
    /// Returns `matches` (direct name hits, with SameAs metadata) and `related`
    /// (entities reached via non-SameAs links from the matches).
    ///
    /// Identity rule: only SameAs folds entities together. Non-SameAs linked
    /// entities that are also direct name hits are moved to `related`.
    pub fn collection_entity_resolve(
        &self,
        input: CollectionEntityResolveInput,
    ) -> ToolResult<CollectionEntityResolveOutput> {
        // Gather all direct name hits across member corpora.
        let mut candidates: Vec<(String, crate::types::Entity)> = Vec::new();
        for corpus_id in &self.corpus_order {
            let found = self
                .backend
                .entity_find_by_name(corpus_id, &input.name)
                .unwrap_or_default();
            for e in found {
                candidates.push((corpus_id.clone(), e));
            }
        }

        if candidates.is_empty() {
            return ToolResult::ok(CollectionEntityResolveOutput {
                matches: vec![],
                related: vec![],
            });
        }

        // Build a set of candidate (corpus_id, entity_id) keys.
        let candidate_keys: HashSet<(String, String)> = candidates
            .iter()
            .map(|(c, e)| (c.clone(), e.id.clone()))
            .collect();

        // Determine which candidates are targets of non-SameAs links from other candidates.
        // Such entities get demoted from `matches` to `related`.
        let mut demoted: HashSet<(String, String)> = HashSet::new();
        let mut related: Vec<CollectionEntityRelation> = Vec::new();

        for (corpus_id, entity) in &candidates {
            let non_same_as_links = self.links.linked(
                corpus_id,
                &entity.id,
                &[
                    EntityLinkKind::Implements,
                    EntityLinkKind::Exemplifies,
                    EntityLinkKind::References,
                    EntityLinkKind::Contrasts,
                ],
            );

            for (tc, te, kind) in &non_same_as_links {
                // Only demote when this candidate is the A-side (source) of the
                // original directed link. The index stores links bidirectionally,
                // so we must check direction before demoting to avoid demoting the
                // source entity when we encounter the reverse edge.
                let is_forward = self.links.is_forward_link(corpus_id, &entity.id, tc, te);
                if is_forward && candidate_keys.contains(&(tc.clone(), te.clone())) {
                    demoted.insert((tc.clone(), te.clone()));
                }
                // Only emit a relation entry for the forward direction to avoid duplicates.
                if is_forward {
                    let note = self.links.note_for(corpus_id, &entity.id, tc, te);
                    related.push(CollectionEntityRelation {
                        from: (corpus_id.clone(), entity.id.clone()),
                        to: (tc.clone(), te.clone()),
                        kind: *kind,
                        note,
                    });
                }
            }
        }

        // Build matches: candidates not demoted.
        let matches: Vec<CollectionEntityMatch> = candidates
            .into_iter()
            .filter(|(c, e)| !demoted.contains(&(c.clone(), e.id.clone())))
            .map(|(corpus_id, entity)| {
                // Compute SameAs class excluding self.
                let class = self.links.same_as_class(&corpus_id, &entity.id);
                let same_as: Vec<(String, String)> = class
                    .into_iter()
                    .filter(|(c, e_id)| !(c == &corpus_id && e_id == &entity.id))
                    .collect();

                let corpus_name = self
                    .corpus_names
                    .get(&corpus_id)
                    .cloned()
                    .unwrap_or_else(|| corpus_id.clone());

                CollectionEntityMatch {
                    corpus_id,
                    corpus_name,
                    entity,
                    same_as,
                }
            })
            .collect();

        ToolResult::ok(CollectionEntityResolveOutput { matches, related })
    }

    // ── collection_entity_meet ──────────────────────────────────────────────

    /// Find all locations where entities A and B (or their SameAs equivalents) co-occur.
    pub fn collection_entity_meet(
        &self,
        input: CollectionEntityMeetInput,
    ) -> ToolResult<CollectionEntityMeetOutput> {
        // Gather entity IDs for name_a across all corpora.
        let entity_a_ids = self.collect_entity_ids_by_name(&input.entity_a);
        let entity_b_ids = self.collect_entity_ids_by_name(&input.entity_b);

        if entity_a_ids.is_empty() || entity_b_ids.is_empty() {
            return ToolResult::ok(CollectionEntityMeetOutput {
                first_co_occurrence: None,
                all: vec![],
                count: 0,
            });
        }

        // Expand both sets via SameAs closure.
        let expanded_a = self.expand_via_same_as(&entity_a_ids);
        let expanded_b = self.expand_via_same_as(&entity_b_ids);

        // Collect edge location URIs for all entities in expanded_a.
        let mut locs_a: HashSet<String> = HashSet::new();
        for (_, eid) in &expanded_a {
            for uri in self
                .backend
                .edge_location_uris_for_entity(eid)
                .unwrap_or_default()
            {
                locs_a.insert(uri);
            }
        }

        // Collect edge location URIs for all entities in expanded_b.
        let mut locs_b: HashSet<String> = HashSet::new();
        for (_, eid) in &expanded_b {
            for uri in self
                .backend
                .edge_location_uris_for_entity(eid)
                .unwrap_or_default()
            {
                locs_b.insert(uri);
            }
        }

        // Intersect.
        let mut common: Vec<String> = locs_a.into_iter().filter(|l| locs_b.contains(l)).collect();
        common.sort_unstable_by(|a, b| {
            // Sort by (corpus order index, uri).
            let a_order = self.corpus_index_for_uri(a);
            let b_order = self.corpus_index_for_uri(b);
            a_order.cmp(&b_order).then_with(|| a.cmp(b))
        });

        let count = common.len() as u64;
        let all: Vec<CollectionLocation> = common
            .iter()
            .filter_map(|uri| {
                let loc = crate::types::Location::parse(uri).ok()?;
                let corpus_name = self
                    .corpus_names
                    .get(&loc.corpus_id)
                    .cloned()
                    .unwrap_or_else(|| loc.corpus_id.clone());
                Some(CollectionLocation {
                    corpus_id: loc.corpus_id.clone(),
                    corpus_name,
                    location: loc,
                })
            })
            .collect();

        let first_co_occurrence = all.first().cloned();

        ToolResult::ok(CollectionEntityMeetOutput {
            first_co_occurrence,
            all,
            count,
        })
    }

    // ── private helpers ─────────────────────────────────────────────────────

    /// Collect all (corpus_id, entity_id) pairs matching `name` across all corpora.
    fn collect_entity_ids_by_name(&self, name: &str) -> Vec<(String, String)> {
        let mut result = Vec::new();
        for corpus_id in &self.corpus_order {
            let found = self
                .backend
                .entity_find_by_name(corpus_id, name)
                .unwrap_or_default();
            for e in found {
                result.push((corpus_id.clone(), e.id));
            }
        }
        result
    }

    /// Expand a set of (corpus_id, entity_id) pairs via SameAs transitive closure.
    fn expand_via_same_as(&self, pairs: &[(String, String)]) -> HashSet<(String, String)> {
        let mut result = HashSet::new();
        for (c, e) in pairs {
            for pair in self.links.same_as_class(c, e) {
                result.insert(pair);
            }
        }
        result
    }

    /// Return the DFS corpus index for a location URI, or usize::MAX if not found.
    fn corpus_index_for_uri(&self, uri: &str) -> usize {
        if let Ok(loc) = crate::types::Location::parse(uri) {
            self.corpus_order
                .iter()
                .position(|id| id == &loc.corpus_id)
                .unwrap_or(usize::MAX)
        } else {
            usize::MAX
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    use crate::corrections::types::{Correction, CorrectionKind, EntityLinkKind};
    use crate::query::collection_service::{CollectionService, EntityLinkIndex};
    use crate::query::types::{
        CollectionEntityMeetInput, CollectionEntityResolveInput, CollectionOverviewInput,
        CollectionSearchInput, SearchMode,
    };
    use crate::storage::SqliteBackend;
    use crate::storage::backend::StorageBackend;
    use crate::types::ToolResult;
    use crate::types::{Chunk, Collection, CollectionKind, Corpus, Entity, Location, MemberType};
    use std::sync::Arc;

    // ── test infrastructure ──────────────────────────────────────────────────

    fn make_backend() -> Arc<dyn StorageBackend> {
        Arc::new(SqliteBackend::open_in_memory().unwrap())
    }

    fn seed_corpus(db: &dyn StorageBackend, id: &str, name: &str) -> Corpus {
        let c = Corpus::new(id.into(), name.into(), "book".into(), "/tmp".into());
        db.corpus_insert(&c).unwrap();
        c
    }

    fn seed_entity(db: &dyn StorageBackend, id: &str, corpus_id: &str, name: &str) -> Entity {
        let e = Entity::new(id.into(), corpus_id.into(), name.into(), "character".into());
        db.entity_upsert(&e).unwrap();
        e
    }

    fn seed_chunk(db: &dyn StorageBackend, corpus_id: &str, path: &str) -> Chunk {
        let loc = Location::new(corpus_id, path);
        let chunk = Chunk::new(
            corpus_id.into(),
            None,
            "chapter".into(),
            loc,
            format!("content of {corpus_id}/{path}"),
        );
        db.chunk_upsert(&chunk).unwrap();
        chunk
    }

    fn make_collection(db: &dyn StorageBackend, id: &str, name: &str) -> Collection {
        let c = Collection::new(id, name, CollectionKind::Series);
        db.collection_insert(&c).unwrap();
        c
    }

    fn make_entity_link_correction(
        collection_id: &str,
        corpus_a: &str,
        entity_a: &str,
        corpus_b: &str,
        entity_b: &str,
        kind: EntityLinkKind,
    ) -> Correction {
        Correction {
            id: uuid::Uuid::new_v4().to_string(),
            corpus_id: None,
            collection_id: Some(collection_id.into()),
            kind: CorrectionKind::EntityLink {
                corpus_a_id: corpus_a.into(),
                entity_a_id: entity_a.into(),
                corpus_b_id: corpus_b.into(),
                entity_b_id: entity_b.into(),
                kind,
                note: None,
            },
            applied_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    // ── EntityLinkIndex tests ────────────────────────────────────────────────

    // An index built from no corrections has no links.
    // linked() returns empty, same_as_class() returns the singleton set containing
    // only the queried pair itself.
    #[test]
    fn entity_link_index_empty() {
        let index = EntityLinkIndex::from_corrections(&[]);
        let linked = index.linked("corpus_a", "entity_a", &[]);
        assert!(
            linked.is_empty(),
            "expected no links from an empty index, got: {linked:?}"
        );

        let class = index.same_as_class("corpus_a", "entity_a");
        assert_eq!(
            class.len(),
            1,
            "same_as_class on empty index should return singleton"
        );
        assert!(class.contains(&("corpus_a".to_string(), "entity_a".to_string())));
    }

    // A SameAs correction A→B means linked(A) yields B and linked(B) yields A —
    // the relationship is bidirectional regardless of how it was stored.
    #[test]
    fn entity_link_index_same_as_linked_is_bidirectional() {
        let corr = make_entity_link_correction(
            "col1",
            "corpus_a",
            "ea",
            "corpus_b",
            "eb",
            EntityLinkKind::SameAs,
        );
        let index = EntityLinkIndex::from_corrections(&[corr]);

        let fwd = index.linked("corpus_a", "ea", &[]);
        assert_eq!(fwd.len(), 1, "expected 1 forward link");
        assert_eq!(fwd[0].0, "corpus_b");
        assert_eq!(fwd[0].1, "eb");
        assert_eq!(fwd[0].2, EntityLinkKind::SameAs);

        let rev = index.linked("corpus_b", "eb", &[]);
        assert_eq!(rev.len(), 1, "expected 1 reverse link");
        assert_eq!(rev[0].0, "corpus_a");
        assert_eq!(rev[0].1, "ea");
        assert_eq!(rev[0].2, EntityLinkKind::SameAs);
    }

    // SameAs links form an equivalence class: both sides are in each other's class.
    #[test]
    fn entity_link_index_same_as_class() {
        let corr = make_entity_link_correction(
            "col1",
            "corpus_a",
            "ea",
            "corpus_b",
            "eb",
            EntityLinkKind::SameAs,
        );
        let index = EntityLinkIndex::from_corrections(&[corr]);

        let class = index.same_as_class("corpus_a", "ea");
        assert_eq!(class.len(), 2);
        assert!(class.contains(&("corpus_a".to_string(), "ea".to_string())));
        assert!(class.contains(&("corpus_b".to_string(), "eb".to_string())));

        // same_as_class is symmetric: asking from the other end gives the same set.
        let class_b = index.same_as_class("corpus_b", "eb");
        assert_eq!(class, class_b);
    }

    // SameAs transitivity: A↔B and B↔C means A, B, and C are all in the same class.
    #[test]
    fn entity_link_index_transitive_same_as() {
        let ab = make_entity_link_correction(
            "col1",
            "corpus_a",
            "ea",
            "corpus_b",
            "eb",
            EntityLinkKind::SameAs,
        );
        let bc = make_entity_link_correction(
            "col1",
            "corpus_b",
            "eb",
            "corpus_c",
            "ec",
            EntityLinkKind::SameAs,
        );
        let index = EntityLinkIndex::from_corrections(&[ab, bc]);

        let class = index.same_as_class("corpus_a", "ea");
        assert_eq!(
            class.len(),
            3,
            "transitive closure should contain all three: {class:?}"
        );
        assert!(class.contains(&("corpus_a".to_string(), "ea".to_string())));
        assert!(class.contains(&("corpus_b".to_string(), "eb".to_string())));
        assert!(class.contains(&("corpus_c".to_string(), "ec".to_string())));
    }

    // Implements links do NOT extend the SameAs equivalence class.
    // Only SameAs participates in the identity closure.
    #[test]
    fn entity_link_index_implements_not_in_same_as_class() {
        let corr = make_entity_link_correction(
            "col1",
            "corpus_a",
            "ea",
            "corpus_b",
            "eb",
            EntityLinkKind::Implements,
        );
        let index = EntityLinkIndex::from_corrections(&[corr]);

        let class = index.same_as_class("corpus_a", "ea");
        assert_eq!(
            class.len(),
            1,
            "Implements link should not extend SameAs class"
        );
        assert!(class.contains(&("corpus_a".to_string(), "ea".to_string())));
    }

    // When an entity has both SameAs and Implements links, filtering by kind
    // returns only the requested kind.
    #[test]
    fn entity_link_index_linked_filter_by_kind() {
        let same_as = make_entity_link_correction(
            "col1",
            "corpus_a",
            "ea",
            "corpus_b",
            "eb",
            EntityLinkKind::SameAs,
        );
        let implements = make_entity_link_correction(
            "col1",
            "corpus_a",
            "ea",
            "corpus_c",
            "ec",
            EntityLinkKind::Implements,
        );
        let index = EntityLinkIndex::from_corrections(&[same_as, implements]);

        let all = index.linked("corpus_a", "ea", &[]);
        assert_eq!(all.len(), 2, "unfiltered should return both links");

        let only_implements = index.linked("corpus_a", "ea", &[EntityLinkKind::Implements]);
        assert_eq!(only_implements.len(), 1);
        assert_eq!(only_implements[0].2, EntityLinkKind::Implements);

        let only_same_as = index.linked("corpus_a", "ea", &[EntityLinkKind::SameAs]);
        assert_eq!(only_same_as.len(), 1);
        assert_eq!(only_same_as[0].2, EntityLinkKind::SameAs);
    }

    // linked() must never return the query pair itself, even if a degenerate
    // self-link correction (A→A) was inserted.
    #[test]
    fn entity_link_index_no_self_in_linked() {
        let self_link = make_entity_link_correction(
            "col1",
            "corpus_a",
            "ea",
            "corpus_a",
            "ea",
            EntityLinkKind::SameAs,
        );
        let index = EntityLinkIndex::from_corrections(&[self_link]);

        let linked = index.linked("corpus_a", "ea", &[]);
        assert!(
            linked.is_empty(),
            "self-link should not appear in linked() results, got: {linked:?}"
        );
    }

    // Non-EntityLink corrections (Rename, Merge, etc.) are silently ignored
    // and do not cause a panic or incorrect behaviour.
    #[test]
    fn entity_link_index_ignores_non_entity_link_corrections() {
        let rename = Correction {
            id: uuid::Uuid::new_v4().to_string(),
            corpus_id: Some("corpus_a".into()),
            collection_id: None,
            kind: CorrectionKind::Rename {
                entity_id: "ea".into(),
                new_name: "Eisenhorn the Inquisitor".into(),
            },
            applied_at: "2026-01-01T00:00:00Z".into(),
        };
        let index = EntityLinkIndex::from_corrections(&[rename]);
        let linked = index.linked("corpus_a", "ea", &[]);
        assert!(
            linked.is_empty(),
            "rename correction should produce no links"
        );
    }

    // ── CollectionService::load tests ────────────────────────────────────────

    // Loading a collection that does not exist returns an error.
    #[test]
    fn collection_service_load_not_found() {
        let backend = make_backend();
        let result = CollectionService::load(Arc::clone(&backend), "nonexistent");
        assert!(result.is_err(), "expected Err for missing collection");
    }

    // Loading a collection with no members results in an empty corpus_services map
    // and an empty link index.
    #[test]
    fn collection_service_load_empty_collection() {
        let backend = make_backend();
        make_collection(backend.as_ref(), "col1", "Empty Collection");

        let svc = CollectionService::load(Arc::clone(&backend), "col1")
            .expect("load should succeed for existing empty collection");

        assert!(
            svc.corpus_services_is_empty(),
            "empty collection should have no per-corpus services"
        );
        assert!(
            svc.links_is_empty(),
            "empty collection should have an empty link index"
        );
    }

    // ── CollectionService search tests ──────────────────────────────────────

    // Searching for a term that does not appear in any member corpus returns
    // an empty results list — not an error.
    #[test]
    fn collection_service_search_no_results() {
        let backend = make_backend();
        seed_corpus(backend.as_ref(), "c1", "First Book");
        seed_corpus(backend.as_ref(), "c2", "Second Book");
        seed_chunk(backend.as_ref(), "c1", "ch/1");
        seed_chunk(backend.as_ref(), "c2", "ch/1");
        backend.fts_rebuild("c1").unwrap();
        backend.fts_rebuild("c2").unwrap();

        make_collection(backend.as_ref(), "col1", "My Series");
        backend
            .collection_add_member("col1", "c1", MemberType::Corpus)
            .unwrap();
        backend
            .collection_add_member("col1", "c2", MemberType::Corpus)
            .unwrap();

        let svc = CollectionService::load(Arc::clone(&backend), "col1").unwrap();
        let result = svc.collection_search(CollectionSearchInput {
            collection_id: "col1".into(),
            query: "zznothere".into(),
            mode: SearchMode::Keyword,
            limit: Some(10),
        });

        let ToolResult::Ok(out) = result else {
            panic!("expected Ok from search, got error");
        };
        assert!(
            out.data.results.is_empty(),
            "expected no results for unmatched query, got: {:?}",
            out.data.results
        );
    }

    // ── CollectionService overview tests ────────────────────────────────────

    // Overview aggregates chunk counts across all corpora in the collection.
    #[test]
    fn collection_overview_counts_chunks_and_entities() {
        let backend = make_backend();
        seed_corpus(backend.as_ref(), "c1", "First Book");
        seed_corpus(backend.as_ref(), "c2", "Second Book");
        seed_chunk(backend.as_ref(), "c1", "ch/1");
        seed_chunk(backend.as_ref(), "c1", "ch/2");
        seed_chunk(backend.as_ref(), "c2", "ch/1");
        seed_chunk(backend.as_ref(), "c2", "ch/2");

        make_collection(backend.as_ref(), "col1", "My Series");
        backend
            .collection_add_member("col1", "c1", MemberType::Corpus)
            .unwrap();
        backend
            .collection_add_member("col1", "c2", MemberType::Corpus)
            .unwrap();

        let svc = CollectionService::load(Arc::clone(&backend), "col1").unwrap();
        let result = svc.collection_overview(CollectionOverviewInput {
            collection_id: "col1".into(),
        });

        let ToolResult::Ok(out) = result else {
            panic!("expected Ok from overview");
        };
        assert_eq!(
            out.data.total_chunks, 4,
            "expected 4 total chunks across both corpora"
        );
        assert_eq!(
            out.data.total_entities, 0,
            "expected 0 entities (none seeded)"
        );
    }

    // ── CollectionService entity resolve tests ───────────────────────────────

    // When no EntityLink corrections exist, resolving an entity name that appears
    // in multiple corpora returns one match per corpus.
    #[test]
    fn collection_entity_resolve_no_links() {
        let backend = make_backend();
        seed_corpus(backend.as_ref(), "c1", "Eisenhorn");
        seed_corpus(backend.as_ref(), "c2", "Ravenor");
        seed_entity(backend.as_ref(), "e1-eisenhorn", "c1", "Eisenhorn");
        seed_entity(backend.as_ref(), "e2-eisenhorn", "c2", "Eisenhorn");

        make_collection(backend.as_ref(), "col1", "Abnett Series");
        backend
            .collection_add_member("col1", "c1", MemberType::Corpus)
            .unwrap();
        backend
            .collection_add_member("col1", "c2", MemberType::Corpus)
            .unwrap();

        let svc = CollectionService::load(Arc::clone(&backend), "col1").unwrap();
        let result = svc.collection_entity_resolve(CollectionEntityResolveInput {
            collection_id: "col1".into(),
            name: "Eisenhorn".into(),
        });

        let ToolResult::Ok(out) = result else {
            panic!("expected Ok from entity resolve");
        };
        assert_eq!(
            out.data.matches.len(),
            2,
            "expected one match per corpus, got: {}",
            out.data.matches.len()
        );
        assert!(
            out.data.related.is_empty(),
            "expected no related entries when no corrections exist"
        );
        for m in &out.data.matches {
            assert!(
                m.same_as.is_empty(),
                "expected empty same_as list without corrections, corpus: {}",
                m.corpus_id
            );
        }
    }

    // When two corpora both have an "Eisenhorn" entity linked by SameAs,
    // resolve returns 2 matches and each match's same_as list includes the other.
    #[test]
    fn collection_entity_resolve_with_same_as_populates_same_as_list() {
        let backend = make_backend();
        seed_corpus(backend.as_ref(), "c1", "Eisenhorn");
        seed_corpus(backend.as_ref(), "c2", "Ravenor");
        seed_entity(backend.as_ref(), "e1", "c1", "Eisenhorn");
        seed_entity(backend.as_ref(), "e2", "c2", "Eisenhorn");

        make_collection(backend.as_ref(), "col1", "Abnett Series");
        backend
            .collection_add_member("col1", "c1", MemberType::Corpus)
            .unwrap();
        backend
            .collection_add_member("col1", "c2", MemberType::Corpus)
            .unwrap();

        // Insert the SameAs correction through the backend.
        backend
            .correction_insert(
                None,
                Some("col1"),
                &CorrectionKind::EntityLink {
                    corpus_a_id: "c1".into(),
                    entity_a_id: "e1".into(),
                    corpus_b_id: "c2".into(),
                    entity_b_id: "e2".into(),
                    kind: EntityLinkKind::SameAs,
                    note: None,
                },
            )
            .unwrap();

        let svc = CollectionService::load(Arc::clone(&backend), "col1").unwrap();
        let result = svc.collection_entity_resolve(CollectionEntityResolveInput {
            collection_id: "col1".into(),
            name: "Eisenhorn".into(),
        });

        let ToolResult::Ok(out) = result else {
            panic!("expected Ok from entity resolve with same_as");
        };
        assert_eq!(out.data.matches.len(), 2);

        let c1_match = out
            .data
            .matches
            .iter()
            .find(|m| m.corpus_id == "c1")
            .unwrap();
        assert!(
            c1_match
                .same_as
                .iter()
                .any(|(cid, eid)| cid == "c2" && eid == "e2"),
            "c1 match should list (c2, e2) in same_as"
        );

        let c2_match = out
            .data
            .matches
            .iter()
            .find(|m| m.corpus_id == "c2")
            .unwrap();
        assert!(
            c2_match
                .same_as
                .iter()
                .any(|(cid, eid)| cid == "c1" && eid == "e1"),
            "c2 match should list (c1, e1) in same_as"
        );
    }

    // When entity A (corpus_a) has an Implements link to entity B (corpus_b),
    // resolving by A's name returns 1 match (corpus_a only) and 1 related entry.
    #[test]
    fn collection_entity_resolve_implements_yields_related_not_match() {
        let backend = make_backend();
        seed_corpus(backend.as_ref(), "c1", "Pattern Book");
        seed_corpus(backend.as_ref(), "c2", "Example Book");
        seed_entity(backend.as_ref(), "ea", "c1", "CommandPattern");
        seed_entity(backend.as_ref(), "eb", "c2", "CommandPattern");

        make_collection(backend.as_ref(), "col1", "Design Patterns");
        backend
            .collection_add_member("col1", "c1", MemberType::Corpus)
            .unwrap();
        backend
            .collection_add_member("col1", "c2", MemberType::Corpus)
            .unwrap();

        backend
            .correction_insert(
                None,
                Some("col1"),
                &CorrectionKind::EntityLink {
                    corpus_a_id: "c1".into(),
                    entity_a_id: "ea".into(),
                    corpus_b_id: "c2".into(),
                    entity_b_id: "eb".into(),
                    kind: EntityLinkKind::Implements,
                    note: None,
                },
            )
            .unwrap();

        let svc = CollectionService::load(Arc::clone(&backend), "col1").unwrap();
        let result = svc.collection_entity_resolve(CollectionEntityResolveInput {
            collection_id: "col1".into(),
            name: "CommandPattern".into(),
        });

        let ToolResult::Ok(out) = result else {
            panic!("expected Ok from entity resolve with implements");
        };
        assert_eq!(
            out.data.matches.len(),
            1,
            "Implements link should not produce a second match"
        );
        assert_eq!(
            out.data.related.len(),
            1,
            "Implements link should appear in related"
        );
        assert_eq!(out.data.related[0].kind, EntityLinkKind::Implements);
    }

    // ── CollectionService entity meet tests ─────────────────────────────────

    // Two entities in the same corpus that share an edge co-occurrence should
    // be found by collection_entity_meet.
    #[test]
    fn collection_entity_meet_same_corpus() {
        let backend = make_backend();
        seed_corpus(backend.as_ref(), "c1", "The Book");
        let chunk = seed_chunk(backend.as_ref(), "c1", "ch/1");
        seed_entity(backend.as_ref(), "e1", "c1", "Eisenhorn");
        seed_entity(backend.as_ref(), "e2", "c1", "Ravenor");

        use crate::types::Edge;
        let edge1 = Edge::new(
            "ed1".into(),
            "c1".into(),
            "e1".into(),
            "e2".into(),
            "appears_with".into(),
            chunk.location.clone(),
        );
        backend.edge_upsert(&edge1).unwrap();

        make_collection(backend.as_ref(), "col1", "Abnett");
        backend
            .collection_add_member("col1", "c1", MemberType::Corpus)
            .unwrap();

        let svc = CollectionService::load(Arc::clone(&backend), "col1").unwrap();
        let result = svc.collection_entity_meet(CollectionEntityMeetInput {
            collection_id: "col1".into(),
            entity_a: "Eisenhorn".into(),
            entity_b: "Ravenor".into(),
        });

        let ToolResult::Ok(out) = result else {
            panic!("expected Ok from entity meet, same corpus");
        };
        assert!(out.data.count > 0, "expected at least one co-occurrence");
        assert!(
            out.data.first_co_occurrence.is_some(),
            "expected a first_co_occurrence location"
        );
        let loc = out.data.first_co_occurrence.unwrap();
        assert_eq!(loc.corpus_id, "c1");
    }

    // Entity "Eisenhorn" exists in both corpus_a and corpus_b under different IDs
    // but linked via SameAs. "Ravenor" is in corpus_b and shares an edge with
    // corpus_b's Eisenhorn. meet("Eisenhorn", "Ravenor") should find that via
    // SameAs expansion.
    #[test]
    fn collection_entity_meet_cross_corpus_via_same_as() {
        let backend = make_backend();
        seed_corpus(backend.as_ref(), "c1", "Eisenhorn Book");
        seed_corpus(backend.as_ref(), "c2", "Ravenor Book");

        seed_entity(backend.as_ref(), "e-c1-eisenhorn", "c1", "Eisenhorn");
        seed_entity(backend.as_ref(), "e-c2-eisenhorn", "c2", "Eisenhorn");
        seed_entity(backend.as_ref(), "e-c2-ravenor", "c2", "Ravenor");

        let shared_chunk = seed_chunk(backend.as_ref(), "c2", "ch/crossover");
        use crate::types::Edge;
        let edge = Edge::new(
            "ed-cross".into(),
            "c2".into(),
            "e-c2-eisenhorn".into(),
            "e-c2-ravenor".into(),
            "appears_with".into(),
            shared_chunk.location.clone(),
        );
        backend.edge_upsert(&edge).unwrap();

        // Create collection FIRST, then insert the correction (FK constraint requires collection to exist).
        make_collection(backend.as_ref(), "col1", "Abnett Collection");
        backend
            .collection_add_member("col1", "c1", MemberType::Corpus)
            .unwrap();
        backend
            .collection_add_member("col1", "c2", MemberType::Corpus)
            .unwrap();

        // SameAs link: c1's Eisenhorn ≡ c2's Eisenhorn.
        backend
            .correction_insert(
                None,
                Some("col1"),
                &CorrectionKind::EntityLink {
                    corpus_a_id: "c1".into(),
                    entity_a_id: "e-c1-eisenhorn".into(),
                    corpus_b_id: "c2".into(),
                    entity_b_id: "e-c2-eisenhorn".into(),
                    kind: EntityLinkKind::SameAs,
                    note: None,
                },
            )
            .unwrap();

        let svc = CollectionService::load(Arc::clone(&backend), "col1").unwrap();
        let result = svc.collection_entity_meet(CollectionEntityMeetInput {
            collection_id: "col1".into(),
            entity_a: "Eisenhorn".into(),
            entity_b: "Ravenor".into(),
        });

        let ToolResult::Ok(out) = result else {
            panic!("expected Ok from cross-corpus entity meet via SameAs expansion");
        };
        assert!(
            out.data.count > 0,
            "expected co-occurrence found via SameAs expansion across corpora"
        );
        let all_corpus_ids: Vec<_> = out.data.all.iter().map(|l| &l.corpus_id).collect();
        assert!(
            all_corpus_ids.iter().any(|id| id.as_str() == "c2"),
            "co-occurrence should be in c2: {all_corpus_ids:?}"
        );
    }
}
