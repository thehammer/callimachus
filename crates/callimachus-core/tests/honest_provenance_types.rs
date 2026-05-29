//! RED-phase behavioral tests for the `Provenance` tagged-union type (PR 1).
//!
//! `Provenance` is the single abstraction that passes, the walker, and the
//! storage layer use when describing *when* an artifact was derived. This file
//! tests the pure-Rust type API — no database required.
//!
//! # Coverage
//!
//! 1. `column_round_trip_concrete` — `Concrete` survives `to_columns` ->
//!    `from_columns` without change.
//! 2. `column_round_trip_range_predating` — `RangePredating` survives the same
//!    round-trip.
//! 3. `from_columns_errors_on_unknown_kind` — an unrecognised `kind` string
//!    returns `Err`.
//! 4. `accessors_concrete` — `kind_str`, `is_concrete`, `is_range_predating`,
//!    and `sha` all return the expected values for the `Concrete` arm.
//! 5. `accessors_range_predating` — same accessor contract for `RangePredating`.
//! 6. `refine_range_predating_becomes_concrete` — `RangePredating("c20").refine
//!    ("c10")` returns `Concrete("c10")`.
//! 7. `refine_concrete_is_noop` — `Concrete("c10").refine("c5")` returns
//!    `Concrete("c10")` unchanged (monotonicity guarantee).
//! 8. `is_valid_at_concrete_semantics` — `Concrete(c2)` is valid at c2 and c3
//!    (derived at-or-before query point) but not at c1 (derived after).
//! 9. `is_valid_at_range_predating_semantics` — `RangePredating(c2)` is valid at
//!    c1 and c2 (query point at-or-before upper bound) but not at c3.
//! 10. `serde_json_round_trip_concrete` — `Concrete` serialises to JSON and back.
//! 11. `serde_json_round_trip_range_predating` — `RangePredating` serialises to
//!     JSON and back.
//! 12. `from_columns_empty_kind_errors` — empty string is not a valid kind.

use callimachus_core::Provenance;

// ── helpers ───────────────────────────────────────────────────────────────────

/// A tiny linear-history oracle for commits c1 < c2 < c3.
/// `is_ancestor_or_equal(a, b)` returns true iff rank(a) <= rank(b).
fn rank(s: &str) -> i32 {
    match s {
        "c1" => 1,
        "c2" => 2,
        "c3" => 3,
        _ => -1,
    }
}

fn ancestor_or_equal(a: &str, b: &str) -> bool {
    rank(a) <= rank(b)
}

// ── Test 1: column round-trip — Concrete ─────────────────────────────────────

/// `Concrete(sha)` survives `to_columns` -> `from_columns` intact.
#[test]
fn column_round_trip_concrete() {
    let original = Provenance::concrete("deadbeef");
    let (kind, sha) = original.to_columns();
    let recovered = Provenance::from_columns(kind, sha)
        .expect("from_columns must succeed for a valid kind string");
    assert_eq!(original, recovered);
}

// ── Test 2: column round-trip — RangePredating ────────────────────────────────

/// `RangePredating(sha)` survives `to_columns` -> `from_columns` intact.
#[test]
fn column_round_trip_range_predating() {
    let original = Provenance::range_predating("cafebabe");
    let (kind, sha) = original.to_columns();
    let recovered = Provenance::from_columns(kind, sha)
        .expect("from_columns must succeed for a valid kind string");
    assert_eq!(original, recovered);
}

// ── Test 3: from_columns errors on an unknown kind string ─────────────────────

/// `from_columns` must return `Err` for a kind string that is neither
/// `"concrete"` nor `"range_predating"`.
#[test]
fn from_columns_errors_on_unknown_kind() {
    assert!(
        Provenance::from_columns("bogus_kind", "abc").is_err(),
        "from_columns must return Err for an unrecognised kind"
    );
}

// ── Test 4: accessors — Concrete arm ─────────────────────────────────────────

/// `kind_str`, `is_concrete`, `is_range_predating`, and `sha` return the
/// expected values for a `Concrete` provenance.
#[test]
fn accessors_concrete() {
    let p = Provenance::concrete("sha123");

    assert_eq!(p.kind_str(), "concrete");
    assert!(p.is_concrete(), "is_concrete must be true for Concrete");
    assert!(
        !p.is_range_predating(),
        "is_range_predating must be false for Concrete"
    );
    assert_eq!(p.sha(), "sha123");
}

// ── Test 5: accessors — RangePredating arm ────────────────────────────────────

/// `kind_str`, `is_concrete`, `is_range_predating`, and `sha` return the
/// expected values for a `RangePredating` provenance.
#[test]
fn accessors_range_predating() {
    let p = Provenance::range_predating("sha456");

    assert_eq!(p.kind_str(), "range_predating");
    assert!(
        !p.is_concrete(),
        "is_concrete must be false for RangePredating"
    );
    assert!(
        p.is_range_predating(),
        "is_range_predating must be true for RangePredating"
    );
    assert_eq!(p.sha(), "sha456");
}

// ── Test 6: refinement — RangePredating narrows to Concrete ──────────────────

/// `RangePredating("c20").refine("c10")` must return `Concrete("c10")`.
/// Refinement collapses the upper-bound tag to a proven point.
#[test]
fn refine_range_predating_becomes_concrete() {
    let p = Provenance::range_predating("c20");
    let refined = p.refine("c10");
    assert_eq!(
        refined,
        Provenance::concrete("c10"),
        "RangePredating must refine to Concrete(observed_sha)"
    );
}

// ── Test 7: refinement — Concrete is monotonically stable ────────────────────

/// `Concrete("c10").refine("c5")` must return `Concrete("c10")` unchanged.
/// Provenance never widens — a Concrete tag cannot be overwritten.
#[test]
fn refine_concrete_is_noop() {
    let p = Provenance::concrete("c10");
    let after = p.clone().refine("c5");
    assert_eq!(
        after, p,
        "Concrete provenance must not change when refined (monotonicity)"
    );
}

// ── Test 8: is_valid_at — Concrete semantics ─────────────────────────────────

/// `Concrete(c2)` is valid at c2 (equal) and c3 (descendant of c2), but NOT
/// at c1 (which predates c2 — the artifact didn't exist yet).
#[test]
fn is_valid_at_concrete_semantics() {
    let p = Provenance::concrete("c2");

    assert!(
        p.is_valid_at("c2", ancestor_or_equal),
        "Concrete(c2) must be valid at its own SHA"
    );
    assert!(
        p.is_valid_at("c3", ancestor_or_equal),
        "Concrete(c2) must be valid at a descendant (c3)"
    );
    assert!(
        !p.is_valid_at("c1", ancestor_or_equal),
        "Concrete(c2) must NOT be valid at an ancestor (c1)"
    );
}

// ── Test 9: is_valid_at — RangePredating semantics ────────────────────────────

/// `RangePredating(c2)` asserts the artifact predates c2. It is valid at c1
/// (which is before the upper bound) and c2 (equal), but NOT at c3 (which is
/// strictly after the upper bound).
#[test]
fn is_valid_at_range_predating_semantics() {
    let p = Provenance::range_predating("c2");

    assert!(
        p.is_valid_at("c1", ancestor_or_equal),
        "RangePredating(c2) must be valid at c1 (query before upper bound)"
    );
    assert!(
        p.is_valid_at("c2", ancestor_or_equal),
        "RangePredating(c2) must be valid at its own upper-bound SHA"
    );
    assert!(
        !p.is_valid_at("c3", ancestor_or_equal),
        "RangePredating(c2) must NOT be valid at c3 (query after upper bound)"
    );
}

// ── Test 10: serde round-trip — Concrete ─────────────────────────────────────

/// `Concrete` survives `serde_json::to_string` -> `serde_json::from_str`
/// without data loss.
#[test]
fn serde_json_round_trip_concrete() {
    let original = Provenance::concrete("abc123");
    let json = serde_json::to_string(&original).expect("serialize must succeed");
    let recovered: Provenance = serde_json::from_str(&json).expect("deserialize must succeed");
    assert_eq!(
        original, recovered,
        "Concrete must survive a JSON round-trip"
    );
}

// ── Test 11: serde round-trip — RangePredating ────────────────────────────────

/// `RangePredating` survives `serde_json::to_string` -> `serde_json::from_str`
/// without data loss.
#[test]
fn serde_json_round_trip_range_predating() {
    let original = Provenance::range_predating("def456");
    let json = serde_json::to_string(&original).expect("serialize must succeed");
    let recovered: Provenance = serde_json::from_str(&json).expect("deserialize must succeed");
    assert_eq!(
        original, recovered,
        "RangePredating must survive a JSON round-trip"
    );
}

// ── Test 12: from_columns rejects empty kind ─────────────────────────────────

/// An empty kind string is not a valid `derived_at_kind` value; `from_columns`
/// must return `Err`.
#[test]
fn from_columns_empty_kind_errors() {
    assert!(
        Provenance::from_columns("", "somereally").is_err(),
        "from_columns must return Err for an empty kind string"
    );
}
