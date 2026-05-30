use crate::error::Result;
use crate::storage::{db::Database, history};
use crate::types::provenance::Provenance;
use crate::types::EntityPurpose;
use rusqlite::params;

pub fn upsert(db: &Database, p: &EntityPurpose) -> Result<()> {
    let conn = db.conn();
    let new_sha = p.provenance.as_ref().map(|pv| pv.sha());

    // Snapshot before overwrite.
    if let Some(sha) = new_sha {
        history::snapshot_if_version_changed_purpose(
            conn,
            &p.entity_id,
            &p.corpus_id,
            &p.model,
            Some(sha),
            sha,
        )?;
    }

    let prov_kind = p.provenance.as_ref().map(|pv| pv.kind_str());
    let prov_sha = p.provenance.as_ref().map(|pv| pv.sha());

    conn.execute(
        "INSERT INTO entity_purposes
         (entity_id, corpus_id, purpose, model, model_tier, generated_at, derived_at_kind, derived_at_sha)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, 'concrete'), COALESCE(?8, ''))
         ON CONFLICT(entity_id, model) DO UPDATE SET
             purpose          = excluded.purpose,
             model_tier       = excluded.model_tier,
             generated_at     = excluded.generated_at,
             derived_at_kind  = CASE WHEN excluded.derived_at_sha != '' THEN excluded.derived_at_kind ELSE entity_purposes.derived_at_kind END,
             derived_at_sha   = CASE WHEN excluded.derived_at_sha != '' THEN excluded.derived_at_sha  ELSE entity_purposes.derived_at_sha  END",
        params![
            p.entity_id,
            p.corpus_id,
            p.purpose,
            p.model,
            p.model_tier,
            p.generated_at,
            prov_kind,
            prov_sha,
        ],
    )?;
    Ok(())
}

/// Return the highest-tier artifact for the entity. When multiple models have
/// produced a purpose, opus > sonnet > haiku > unknown. Ties broken by
/// `generated_at DESC`.
pub fn get_best(db: &Database, corpus_id: &str, entity_id: &str) -> Result<Option<EntityPurpose>> {
    let mut stmt = db.conn().prepare(
        "SELECT entity_id, corpus_id, purpose, model, model_tier, generated_at, derived_at_kind, derived_at_sha
         FROM entity_purposes
         WHERE corpus_id = ?1 AND entity_id = ?2
         ORDER BY CASE model_tier
                    WHEN 'opus'    THEN 3
                    WHEN 'sonnet'  THEN 2
                    WHEN 'haiku'   THEN 1
                    ELSE 0
                  END DESC,
                  generated_at DESC
         LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![corpus_id, entity_id], row_to_purpose)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Return the artifact produced by an exact model name, or `None` if not found.
pub fn get_for_model(
    db: &Database,
    corpus_id: &str,
    entity_id: &str,
    model: &str,
) -> Result<Option<EntityPurpose>> {
    let mut stmt = db.conn().prepare(
        "SELECT entity_id, corpus_id, purpose, model, model_tier, generated_at, derived_at_kind, derived_at_sha
         FROM entity_purposes
         WHERE corpus_id = ?1 AND entity_id = ?2 AND model = ?3",
    )?;
    let mut rows = stmt.query_map(params![corpus_id, entity_id, model], row_to_purpose)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

pub fn list(db: &Database, corpus_id: &str) -> Result<Vec<EntityPurpose>> {
    let mut stmt = db.conn().prepare(
        "SELECT entity_id, corpus_id, purpose, model, model_tier, generated_at, derived_at_kind, derived_at_sha
         FROM entity_purposes WHERE corpus_id = ?1 ORDER BY entity_id ASC",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_purpose)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::CalError::from)
}

pub fn delete_for_entity(db: &Database, entity_id: &str) -> Result<()> {
    db.conn().execute(
        "DELETE FROM entity_purposes WHERE entity_id = ?1",
        params![entity_id],
    )?;
    Ok(())
}

fn row_to_purpose(row: &rusqlite::Row<'_>) -> rusqlite::Result<EntityPurpose> {
    // Column order: entity_id(0), corpus_id(1), purpose(2), model(3), model_tier(4),
    //               generated_at(5), derived_at_kind(6), derived_at_sha(7)
    let prov_kind: Option<String> = row.get(6)?;
    let prov_sha: Option<String> = row.get(7)?;
    let provenance = match (prov_kind.as_deref(), prov_sha.as_deref()) {
        (Some(k), Some(s)) if !s.is_empty() => Provenance::from_columns(k, s).ok(),
        _ => None,
    };
    Ok(EntityPurpose {
        entity_id: row.get(0)?,
        corpus_id: row.get(1)?,
        purpose: row.get(2)?,
        model: row.get(3)?,
        model_tier: row.get(4)?,
        generated_at: row.get(5)?,
        provenance,
    })
}
