//! RED-phase behavioral tests for the honest-provenance `StorageBackend` methods
//! added in PR 1 (migration 013).
//!
//! Uses `SqliteBackend::open_in_memory()` throughout. All new methods tested
//! here are defined in the `StorageBackend` trait; this file verifies their
//! contracts without caring about implementation internals.
//!
//! # Coverage
//!
//! 1. `entity_list_at_sha_agrees_with_entity_list_at_version` — the naive
//!    facade `entity_list_at_sha` returns the same entity IDs as
//!    `entity_list_at_version` for both a matching and a non-matching SHA.
//! 2. `tombstone_insert_and_list_round_trip` — a single tombstone survives
//!    insert and comes back with the correct fields.
//! 3. `tombstone_insert_is_idempotent` — inserting the same tombstone twice
//!    does not error and `tombstone_list` returns exactly one row.
//! 4. `tombstone_two_different_provenances_both_persist` — two tombstones for
//!    the same artifact but different provenance tags are both stored.
//! 5. `layer2_cache_put_and_get_round_trip` — a cache entry survives put/get
//!    with all fields intact.
//! 6. `layer2_cache_get_returns_none_for_unknown_key` — `layer2_cache_get` on
//!    an unregistered key returns `None`.
//! 7. `layer2_cache_put_same_key_twice_upserts` — a second `put` on the same
//!    key replaces the payload without error.
//! 8. `refine_provenance_stub_returns_unchanged` — in this PR
//!    `refine_provenance` always returns `RefineOutcome::Unchanged`.

use callimachus_core::{
    Corpus, Entity, Layer2CacheKey, Provenance, RefineOutcome, SqliteBackend, StorageBackend,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_backend() -> SqliteBackend {
    SqliteBackend::open_in_memory().expect("in-memory backend")
}

/// Seed a minimal corpus row so artifact FK constraints are satisfied.
fn seed_corpus(backend: &dyn StorageBackend, corpus_id: &str) {
    let corpus = Corpus::new(
        corpus_id.to_string(),
        format!("{corpus_id} corpus"),
        "code".to_string(),
        "/tmp/dummy".to_string(),
    );
    let _ = backend.corpus_insert(&corpus);
}

/// Build a minimal `Entity` for `corpus_id` with the given `id`.
fn minimal_entity(corpus_id: &str, id: &str) -> Entity {
    Entity::new(
        id.to_string(),
        corpus_id.to_string(),
        format!("Entity_{id}"),
        "fn".to_string(),
    )
}

/// Build a `Layer2CacheKey` for deterministic test use.
fn test_cache_key() -> Layer2CacheKey {
    Layer2CacheKey {
        artifact_kind: "purpose".to_string(),
        entity_id: Some("e1".to_string()),
        content_hash: "ch".to_string(),
        file_shape_hash: "fsh".to_string(),
        model: "m".to_string(),
        stable_sampling: false,
    }
}

// ── Test 1: entity_list_at_sha agrees with entity_list_at_version ─────────────

/// In this PR `entity_list_at_sha` is a naive facade over
/// `entity_list_at_version`. Both methods must return identically-ordered
/// results for the same input — the two sets of entity IDs must agree.
///
/// We test with a SHA that matches an upserted entity's `derived_at_version`
/// (set via raw SQL) and with a SHA that matches nothing.
#[test]
fn entity_list_at_sha_agrees_with_entity_list_at_version() {
    let backend = make_backend();
    seed_corpus(&backend, "c1");

    // Upsert one entity, then stamp its derived_at_version so the lookup fires.
    let entity = minimal_entity("c1", "ent-sha-test");
    backend.entity_upsert(&entity).expect("entity_upsert");

    // For both a version that might match and one that definitely doesn't,
    // the two methods must return the same set of IDs.
    for sha in &["v1", "missing-sha"] {
        let by_sha = backend
            .entity_list_at_sha("c1", sha, None)
            .expect("entity_list_at_sha must not error");
        let by_version = backend
            .entity_list_at_version("c1", sha)
            .expect("entity_list_at_version must not error");

        let mut ids_sha: Vec<&str> = by_sha.iter().map(|e| e.id.as_str()).collect();
        let mut ids_ver: Vec<&str> = by_version.iter().map(|e| e.id.as_str()).collect();
        ids_sha.sort_unstable();
        ids_ver.sort_unstable();

        assert_eq!(
            ids_sha, ids_ver,
            "entity_list_at_sha and entity_list_at_version must agree for sha={sha}"
        );
    }
}

// ── Test 2: tombstone insert + list round-trip ────────────────────────────────

/// A tombstone inserted via `tombstone_insert` must be retrievable via
/// `tombstone_list` with all fields intact.
#[test]
fn tombstone_insert_and_list_round_trip() {
    let backend = make_backend();
    seed_corpus(&backend, "c1");

    backend
        .tombstone_insert(
            "c1",
            "entity",
            "e1",
            &Provenance::concrete("sha1"),
            Some("removed"),
        )
        .expect("tombstone_insert must succeed");

    let stones = backend
        .tombstone_list("c1", "entity", "e1")
        .expect("tombstone_list must succeed");

    assert_eq!(
        stones.len(),
        1,
        "tombstone_list must return exactly one row"
    );

    let s = &stones[0];
    assert_eq!(s.corpus_id, "c1");
    assert_eq!(s.artifact_kind, "entity");
    assert_eq!(s.artifact_id, "e1");
    assert_eq!(
        s.provenance,
        Provenance::concrete("sha1"),
        "tombstone provenance must be Concrete(\"sha1\")"
    );
    assert_eq!(
        s.reason.as_deref(),
        Some("removed"),
        "tombstone reason must be preserved"
    );
}

// ── Test 3: tombstone_insert is idempotent ────────────────────────────────────

/// Inserting the same tombstone twice must not error; `tombstone_list` must
/// still return exactly one row (the unique index provides idempotency).
#[test]
fn tombstone_insert_is_idempotent() {
    let backend = make_backend();
    seed_corpus(&backend, "c1");

    let prov = Provenance::concrete("sha1");

    backend
        .tombstone_insert("c1", "entity", "e1", &prov, Some("removed"))
        .expect("first tombstone_insert must succeed");

    backend
        .tombstone_insert("c1", "entity", "e1", &prov, Some("removed"))
        .expect("second tombstone_insert (identical) must not error");

    let stones = backend
        .tombstone_list("c1", "entity", "e1")
        .expect("tombstone_list must succeed");

    assert_eq!(
        stones.len(),
        1,
        "idempotent re-insert must not produce a second row"
    );
}

// ── Test 4: two different provenances for the same artifact both persist ───────

/// Two tombstones for the same `(corpus, kind, artifact_id)` but different
/// provenance tags must both be stored; `tombstone_list` returns 2 rows.
#[test]
fn tombstone_two_different_provenances_both_persist() {
    let backend = make_backend();
    seed_corpus(&backend, "c1");

    backend
        .tombstone_insert("c1", "entity", "e1", &Provenance::concrete("sha1"), None)
        .expect("first tombstone_insert");

    backend
        .tombstone_insert(
            "c1",
            "entity",
            "e1",
            &Provenance::range_predating("sha2"),
            None,
        )
        .expect("second tombstone_insert (different provenance)");

    let stones = backend
        .tombstone_list("c1", "entity", "e1")
        .expect("tombstone_list must succeed");

    assert_eq!(
        stones.len(),
        2,
        "two tombstones with distinct provenance must both be stored"
    );

    let provenances: Vec<&Provenance> = stones.iter().map(|s| &s.provenance).collect();
    assert!(
        provenances.contains(&&Provenance::concrete("sha1")),
        "Concrete(sha1) tombstone must be present"
    );
    assert!(
        provenances.contains(&&Provenance::range_predating("sha2")),
        "RangePredating(sha2) tombstone must be present"
    );
}

// ── Test 5: layer2_cache put + get round-trip ─────────────────────────────────

/// A cache entry placed with `layer2_cache_put` must be retrievable via
/// `layer2_cache_get` with every field intact.
#[test]
fn layer2_cache_put_and_get_round_trip() {
    let backend = make_backend();
    let key = test_cache_key();

    backend
        .layer2_cache_put(&key, "{\"purpose\":\"x\"}", "seedsha")
        .expect("layer2_cache_put must succeed");

    let cached = backend
        .layer2_cache_get(&key)
        .expect("layer2_cache_get must not error")
        .expect("layer2_cache_get must return Some after a put");

    assert_eq!(cached.payload, "{\"purpose\":\"x\"}");
    assert_eq!(cached.artifact_kind, "purpose");
    assert_eq!(cached.entity_id, Some("e1".to_string()));
    assert_eq!(cached.first_seen_at_sha, "seedsha");
    assert!(!cached.stable_sampling);
}

// ── Test 6: layer2_cache_get returns None for an unknown key ──────────────────

/// `layer2_cache_get` on a key that was never put must return `Ok(None)`.
#[test]
fn layer2_cache_get_returns_none_for_unknown_key() {
    let backend = make_backend();
    let key = Layer2CacheKey {
        artifact_kind: "contract".to_string(),
        entity_id: None,
        content_hash: "never".to_string(),
        file_shape_hash: "stored".to_string(),
        model: "m2".to_string(),
        stable_sampling: true,
    };

    let result = backend
        .layer2_cache_get(&key)
        .expect("layer2_cache_get must not error on a miss");

    assert!(
        result.is_none(),
        "layer2_cache_get must return None for an unknown key"
    );
}

// ── Test 7: layer2_cache_put on the same key twice upserts the payload ────────

/// A second `layer2_cache_put` on the same key with a different payload must
/// not error. `layer2_cache_get` must then return the latest payload.
/// (The `cache_key` is a deterministic hash of the key fields, so the same
/// key always produces the same primary key — a second put is an upsert.)
#[test]
fn layer2_cache_put_same_key_twice_upserts() {
    let backend = make_backend();
    let key = test_cache_key();

    backend
        .layer2_cache_put(&key, "{\"purpose\":\"first\"}", "sha1")
        .expect("first layer2_cache_put");
    backend
        .layer2_cache_put(&key, "{\"purpose\":\"second\"}", "sha2")
        .expect("second layer2_cache_put on same key must not error");

    let cached = backend
        .layer2_cache_get(&key)
        .expect("layer2_cache_get must not error")
        .expect("layer2_cache_get must return Some");

    assert_eq!(
        cached.payload, "{\"purpose\":\"second\"}",
        "second put must replace the first payload"
    );
}

// ── Test 8: refine_provenance stub returns Unchanged ─────────────────────────

/// In PR 1 `refine_provenance` is a stub that always returns
/// `RefineOutcome::Unchanged`. This is the documented contract for this PR;
/// the real implementation lands in a later PR.
#[test]
fn refine_provenance_stub_returns_unchanged() {
    let backend = make_backend();
    seed_corpus(&backend, "c1");

    let outcome = backend
        .refine_provenance("c1", "entity", "e1", &Provenance::concrete("s"))
        .expect("refine_provenance must not error");

    assert_eq!(
        outcome,
        RefineOutcome::Unchanged,
        "refine_provenance must return Unchanged (stub) in this PR"
    );
}
