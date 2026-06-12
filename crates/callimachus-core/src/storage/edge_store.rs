use crate::error::{CalError, Result};
use crate::storage::db::Database;
use crate::types::edge::Edge;
use crate::types::location::Location;
use rusqlite::params;

pub fn upsert(db: &Database, edge: &Edge) -> Result<()> {
    // Use a SELECT-guard so edges referencing entity IDs that don't exist in
    // the corpus (e.g. calls into external crates) are silently skipped rather
    // than causing a FK constraint violation.
    //
    // ON CONFLICT DO UPDATE overwrites `occurrence_count` (and `origin_scope`)
    // with the incoming value rather than adding to it.  This is correct and
    // idempotent because:
    //   (a) the cascade deletes a file's edges before re-extraction, so the
    //       incoming count from the extractor's aggregation step IS the
    //       authoritative per-file count for this reindex run;
    //   (b) if for some reason a conflict arises without a prior cascade
    //       (e.g. a clean file whose edges survived), overwriting with the
    //       freshly-aggregated count is still correct — it reflects the
    //       current state of the file.
    // We do NOT call a snapshot helper here — cascade.rs handles archiving
    // edges before deletion; `derived_at_version` is kept via COALESCE.
    db.conn().execute(
        "INSERT INTO edges
         (id, corpus_id, from_entity_id, to_entity_id, kind, location_uri, confidence,
          derived_at_version, occurrence_count, origin_scope)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10
         WHERE EXISTS (SELECT 1 FROM entities WHERE id = ?3)
           AND EXISTS (SELECT 1 FROM entities WHERE id = ?4)
         ON CONFLICT(id) DO UPDATE SET
           occurrence_count = excluded.occurrence_count,
           origin_scope     = excluded.origin_scope",
        params![
            edge.id,
            edge.corpus_id,
            edge.from_entity_id,
            edge.to_entity_id,
            edge.kind,
            edge.location.uri(),
            edge.confidence as f64,
            edge.derived_at_version,
            edge.occurrence_count as i64,
            edge.origin_scope,
        ],
    )?;
    Ok(())
}

pub fn get_for_entity(
    db: &Database,
    entity_id: &str,
    direction: EdgeDirection,
    kind: Option<&str>,
    limit: usize,
) -> Result<Vec<Edge>> {
    let (from_clause, to_clause) = match direction {
        EdgeDirection::Outbound => ("from_entity_id = ?1", "1=0"),
        EdgeDirection::Inbound => ("1=0", "to_entity_id = ?1"),
        EdgeDirection::Both => ("from_entity_id = ?1", "to_entity_id = ?1"),
    };

    // Build two separate queries to avoid passing mismatched parameter counts.
    if let Some(kind_val) = kind {
        let sql = format!(
            "SELECT id, corpus_id, from_entity_id, to_entity_id, kind, location_uri, confidence,
                    derived_at_version, occurrence_count, origin_scope
             FROM edges WHERE ({from_clause} OR {to_clause}) AND kind = ?3
             LIMIT ?2"
        );
        let mut stmt = db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![entity_id, limit as i64, kind_val], row_to_edge)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(CalError::from)
    } else {
        let sql = format!(
            "SELECT id, corpus_id, from_entity_id, to_entity_id, kind, location_uri, confidence,
                    derived_at_version, occurrence_count, origin_scope
             FROM edges WHERE ({from_clause} OR {to_clause})
             LIMIT ?2"
        );
        let mut stmt = db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![entity_id, limit as i64], row_to_edge)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(CalError::from)
    }
}

pub fn list(db: &Database, corpus_id: &str) -> Result<Vec<Edge>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, corpus_id, from_entity_id, to_entity_id, kind, location_uri, confidence,
                derived_at_version, occurrence_count, origin_scope
         FROM edges WHERE corpus_id = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_edge)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CalError::from)
}

pub fn count(db: &Database, corpus_id: &str) -> Result<u64> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM edges WHERE corpus_id = ?1",
        params![corpus_id],
        |r| r.get(0),
    )?;
    Ok(n as u64)
}

#[derive(Debug, Clone, Copy)]
pub enum EdgeDirection {
    Inbound,
    Outbound,
    Both,
}

impl std::str::FromStr for EdgeDirection {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "inbound" => Ok(EdgeDirection::Inbound),
            "outbound" => Ok(EdgeDirection::Outbound),
            "both" => Ok(EdgeDirection::Both),
            other => Err(format!("unknown direction: {other}")),
        }
    }
}

/// Returns distinct location URIs for all edges involving `entity_id`.
pub fn location_uris_for_entity(db: &Database, entity_id: &str) -> Result<Vec<String>> {
    let mut stmt = db.conn().prepare(
        "SELECT DISTINCT location_uri FROM edges
         WHERE from_entity_id = ?1 OR to_entity_id = ?1",
    )?;
    let rows = stmt.query_map(params![entity_id], |row| row.get::<_, String>(0))?;
    let mut uris = Vec::new();
    for r in rows {
        uris.push(r.map_err(crate::error::CalError::from)?);
    }
    Ok(uris)
}

/// Returns entity IDs (from_entity_id and to_entity_id) for edges at `location_uri`.
pub fn entity_ids_at_location(db: &Database, location_uri: &str) -> Result<Vec<String>> {
    let mut stmt = db.conn().prepare(
        "SELECT DISTINCT from_entity_id FROM edges WHERE location_uri = ?1
         UNION
         SELECT DISTINCT to_entity_id FROM edges WHERE location_uri = ?1",
    )?;
    let rows = stmt.query_map(params![location_uri], |row| row.get::<_, String>(0))?;
    let mut ids = Vec::new();
    for r in rows {
        ids.push(r.map_err(crate::error::CalError::from)?);
    }
    Ok(ids)
}

/// Returns the number of edges pointing *into* `entity_id` within `corpus_id` (in-degree).
pub fn in_degree(db: &Database, corpus_id: &str, entity_id: &str) -> Result<u32> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM edges WHERE corpus_id = ?1 AND to_entity_id = ?2",
        rusqlite::params![corpus_id, entity_id],
        |r| r.get(0),
    )?;
    Ok(n as u32)
}

/// Returns the number of edges pointing *out of* `entity_id` within `corpus_id` (out-degree).
pub fn out_degree(db: &Database, corpus_id: &str, entity_id: &str) -> Result<u32> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM edges WHERE corpus_id = ?1 AND from_entity_id = ?2",
        rusqlite::params![corpus_id, entity_id],
        |r| r.get(0),
    )?;
    Ok(n as u32)
}

fn row_to_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<Edge> {
    // Column order: id(0), corpus_id(1), from_entity_id(2), to_entity_id(3),
    //               kind(4), location_uri(5), confidence(6), derived_at_version(7),
    //               occurrence_count(8), origin_scope(9)
    let uri: String = row.get(5)?;
    let location = Location::parse(&uri).unwrap_or_else(|_| Location {
        corpus_id: String::new(),
        path: uri.clone(),
    });
    Ok(Edge {
        id: row.get(0)?,
        corpus_id: row.get(1)?,
        from_entity_id: row.get(2)?,
        to_entity_id: row.get(3)?,
        kind: row.get(4)?,
        location,
        confidence: row.get::<_, f64>(6)? as f32,
        derived_at_version: row.get(7)?,
        occurrence_count: row.get::<_, i64>(8)? as u32,
        origin_scope: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use crate::storage::{SqliteBackend, StorageBackend};
    use crate::types::{Corpus, Edge, Entity, Location};

    fn setup() -> (SqliteBackend, String) {
        let db = SqliteBackend::open_in_memory().unwrap();
        let corpus_id = "test-corpus";
        let corpus = Corpus::new(
            corpus_id.to_string(),
            "Test".to_string(),
            "fake".to_string(),
            "/tmp".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();
        (db, corpus_id.to_string())
    }

    fn entity(corpus_id: &str, id: &str) -> Entity {
        Entity::new(
            id.to_string(),
            corpus_id.to_string(),
            id.to_string(),
            "function".to_string(),
        )
    }

    fn edge(corpus_id: &str, id: &str, from: &str, to: &str) -> Edge {
        Edge::new(
            id.to_string(),
            corpus_id.to_string(),
            from.to_string(),
            to.to_string(),
            "calls".to_string(),
            Location::new(corpus_id, "src/main.rs"),
        )
    }

    // ── Test D: upsert idempotency — overwrite, not add ─────────────────────

    /// Upserting the same edge twice must leave exactly one row whose
    /// `occurrence_count` equals the second write's value, not the sum.
    /// This proves the ON CONFLICT overwrite semantics are correct for
    /// incremental reindex (the extractor's aggregation step produces the
    /// authoritative per-file count, and storage must not double it).
    #[test]
    fn upsert_overwrites_occurrence_count_rather_than_accumulating() {
        let (db, corpus_id) = setup();

        let a = entity(&corpus_id, "entity-a");
        let b = entity(&corpus_id, "entity-b");
        db.entity_upsert(&a).unwrap();
        db.entity_upsert(&b).unwrap();

        let mut e = edge(&corpus_id, "edge-ab", "entity-a", "entity-b");
        e.occurrence_count = 5;
        db.edge_upsert(&e).unwrap();

        // Upsert the same edge again with the same count.
        db.edge_upsert(&e).unwrap();

        let guard = db.db_for_test();
        let edges = super::list(&guard, &corpus_id).unwrap();
        drop(guard);
        let matching: Vec<_> = edges.iter().filter(|x| x.id == "edge-ab").collect();

        assert_eq!(
            matching.len(),
            1,
            "upsert of same edge id must produce exactly one row"
        );
        assert_eq!(
            matching[0].occurrence_count, 5,
            "occurrence_count must remain 5 after a second upsert of the same value, \
             not accumulate to 10"
        );
    }

    // ── Test E: round-trip of occurrence_count and origin_scope ─────────────

    /// `occurrence_count` and `origin_scope` must survive a write-then-read
    /// cycle through the storage layer without corruption.
    #[test]
    fn new_fields_round_trip_through_storage() {
        let (db, corpus_id) = setup();

        let a = entity(&corpus_id, "entity-a");
        let b = entity(&corpus_id, "entity-b");
        db.entity_upsert(&a).unwrap();
        db.entity_upsert(&b).unwrap();

        let mut e = edge(&corpus_id, "edge-ab", "entity-a", "entity-b");
        e.occurrence_count = 3;
        e.origin_scope = "test".to_string();
        db.edge_upsert(&e).unwrap();

        let guard = db.db_for_test();
        let stored =
            super::get_for_entity(&guard, "entity-a", super::EdgeDirection::Outbound, None, 10)
                .unwrap();
        drop(guard);

        let found = stored
            .iter()
            .find(|x| x.id == "edge-ab")
            .expect("edge-ab should be retrievable via get_for_entity");

        assert_eq!(
            found.occurrence_count, 3,
            "occurrence_count must round-trip through storage"
        );
        assert_eq!(
            found.origin_scope, "test",
            "origin_scope must round-trip through storage"
        );
    }

    #[test]
    fn in_out_degree_counts() {
        let (db, corpus_id) = setup();

        // Insert three entities: A, B, C.
        let a = entity(&corpus_id, "entity-a");
        let b = entity(&corpus_id, "entity-b");
        let c = entity(&corpus_id, "entity-c");
        db.entity_upsert(&a).unwrap();
        db.entity_upsert(&b).unwrap();
        db.entity_upsert(&c).unwrap();

        // Insert edges A→B and A→C.
        db.edge_upsert(&edge(&corpus_id, "edge-ab", "entity-a", "entity-b"))
            .unwrap();
        db.edge_upsert(&edge(&corpus_id, "edge-ac", "entity-a", "entity-c"))
            .unwrap();

        // B has one inbound edge (from A).
        assert_eq!(db.entity_in_degree(&corpus_id, "entity-b").unwrap(), 1);
        // A has no inbound edges.
        assert_eq!(db.entity_in_degree(&corpus_id, "entity-a").unwrap(), 0);
        // A has two outbound edges (to B and C).
        assert_eq!(db.entity_out_degree(&corpus_id, "entity-a").unwrap(), 2);
        // C has no outbound edges.
        assert_eq!(db.entity_out_degree(&corpus_id, "entity-c").unwrap(), 0);
    }
}
