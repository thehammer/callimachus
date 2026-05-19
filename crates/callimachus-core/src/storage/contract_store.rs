use crate::error::Result;
use crate::storage::db::Database;
use crate::types::EntityContract;
use rusqlite::params;

pub fn upsert(db: &Database, c: &EntityContract) -> Result<()> {
    let debt_json = serde_json::to_string(&c.debt_markers).unwrap_or_else(|_| "[]".into());
    let assumptions_json = serde_json::to_string(&c.assumptions).unwrap_or_else(|_| "[]".into());
    let risks_json = serde_json::to_string(&c.risks).unwrap_or_else(|_| "[]".into());

    db.conn().execute(
        "INSERT OR REPLACE INTO entity_contracts
         (entity_id, corpus_id,
          is_public, is_must_use, is_deprecated, is_fallible, is_nullable,
          is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete,
          panic_call_count, debt_markers, assumptions, risks,
          intent_gap, caller_notes, model, model_tier, generated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
        params![
            c.entity_id,
            c.corpus_id,
            c.is_public as i64,
            c.is_must_use as i64,
            c.is_deprecated as i64,
            c.is_fallible as i64,
            c.is_nullable as i64,
            c.is_mutating as i64,
            c.is_diverging as i64,
            c.has_panic_risk as i64,
            c.has_unsafe as i64,
            c.is_incomplete as i64,
            c.panic_call_count,
            debt_json,
            assumptions_json,
            risks_json,
            c.intent_gap,
            c.caller_notes,
            c.model,
            c.model_tier,
            c.generated_at,
        ],
    )?;
    Ok(())
}

/// Return the highest-tier contract for the entity. opus > sonnet > haiku > unknown.
/// Ties broken by `generated_at DESC`.
pub fn get_best(db: &Database, corpus_id: &str, entity_id: &str) -> Result<Option<EntityContract>> {
    let mut stmt = db.conn().prepare(
        "SELECT entity_id, corpus_id,
                is_public, is_must_use, is_deprecated, is_fallible, is_nullable,
                is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete,
                panic_call_count, debt_markers, assumptions, risks,
                intent_gap, caller_notes, model, model_tier, generated_at
         FROM entity_contracts
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
    let mut rows = stmt.query_map(params![corpus_id, entity_id], row_to_contract)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Return the contract produced by an exact model name, or `None` if not found.
pub fn get_for_model(
    db: &Database,
    corpus_id: &str,
    entity_id: &str,
    model: &str,
) -> Result<Option<EntityContract>> {
    let mut stmt = db.conn().prepare(
        "SELECT entity_id, corpus_id,
                is_public, is_must_use, is_deprecated, is_fallible, is_nullable,
                is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete,
                panic_call_count, debt_markers, assumptions, risks,
                intent_gap, caller_notes, model, model_tier, generated_at
         FROM entity_contracts
         WHERE corpus_id = ?1 AND entity_id = ?2 AND model = ?3",
    )?;
    let mut rows = stmt.query_map(params![corpus_id, entity_id, model], row_to_contract)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// All contract rows for the corpus (multiple per entity when multi-model).
pub fn list(db: &Database, corpus_id: &str) -> Result<Vec<EntityContract>> {
    let mut stmt = db.conn().prepare(
        "SELECT entity_id, corpus_id,
                is_public, is_must_use, is_deprecated, is_fallible, is_nullable,
                is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete,
                panic_call_count, debt_markers, assumptions, risks,
                intent_gap, caller_notes, model, model_tier, generated_at
         FROM entity_contracts WHERE corpus_id = ?1 ORDER BY entity_id ASC",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_contract)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::CalError::from)
}

/// One best-tier contract per entity. Useful when callers need exactly one row
/// per entity (e.g. debt analysis, graph queries).
pub fn list_best_per_entity(db: &Database, corpus_id: &str) -> Result<Vec<EntityContract>> {
    let mut stmt = db.conn().prepare(
        "SELECT entity_id, corpus_id,
                is_public, is_must_use, is_deprecated, is_fallible, is_nullable,
                is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete,
                panic_call_count, debt_markers, assumptions, risks,
                intent_gap, caller_notes, model, model_tier, generated_at
         FROM entity_contracts c1
         WHERE corpus_id = ?1
           AND generated_at = (
               SELECT MAX(c2.generated_at) FROM entity_contracts c2
               WHERE c2.entity_id   = c1.entity_id
                 AND c2.corpus_id   = c1.corpus_id
                 AND CASE c2.model_tier
                       WHEN 'opus'   THEN 3
                       WHEN 'sonnet' THEN 2
                       WHEN 'haiku'  THEN 1
                       ELSE 0
                     END = (
                       SELECT MAX(CASE c3.model_tier
                                    WHEN 'opus'   THEN 3
                                    WHEN 'sonnet' THEN 2
                                    WHEN 'haiku'  THEN 1
                                    ELSE 0
                                  END)
                       FROM entity_contracts c3
                       WHERE c3.entity_id = c1.entity_id
                         AND c3.corpus_id = c1.corpus_id
                     )
           )
         ORDER BY entity_id ASC",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_contract)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::CalError::from)
}

/// Return contracts that show signs of inconsistency or technical debt, one
/// best-tier row per entity.
pub fn list_with_inconsistencies(db: &Database, corpus_id: &str) -> Result<Vec<EntityContract>> {
    let mut stmt = db.conn().prepare(
        "SELECT entity_id, corpus_id,
                is_public, is_must_use, is_deprecated, is_fallible, is_nullable,
                is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete,
                panic_call_count, debt_markers, assumptions, risks,
                intent_gap, caller_notes, model, model_tier, generated_at
         FROM entity_contracts c1
         WHERE corpus_id = ?1
           AND (is_incomplete = 1
                OR intent_gap IS NOT NULL
                OR (is_fallible = 0 AND length(risks) > 2))
           AND generated_at = (
               SELECT MAX(c2.generated_at) FROM entity_contracts c2
               WHERE c2.entity_id   = c1.entity_id
                 AND c2.corpus_id   = c1.corpus_id
                 AND CASE c2.model_tier
                       WHEN 'opus'   THEN 3
                       WHEN 'sonnet' THEN 2
                       WHEN 'haiku'  THEN 1
                       ELSE 0
                     END = (
                       SELECT MAX(CASE c3.model_tier
                                    WHEN 'opus'   THEN 3
                                    WHEN 'sonnet' THEN 2
                                    WHEN 'haiku'  THEN 1
                                    ELSE 0
                                  END)
                       FROM entity_contracts c3
                       WHERE c3.entity_id = c1.entity_id
                         AND c3.corpus_id = c1.corpus_id
                     )
           )
         ORDER BY entity_id ASC",
    )?;
    let rows = stmt.query_map(params![corpus_id], row_to_contract)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::CalError::from)
}

fn row_to_contract(row: &rusqlite::Row<'_>) -> rusqlite::Result<EntityContract> {
    let debt_str: String = row.get(13)?;
    let assumptions_str: String = row.get(14)?;
    let risks_str: String = row.get(15)?;

    let debt_markers: Vec<String> = serde_json::from_str(&debt_str).unwrap_or_default();
    let assumptions: Vec<String> = serde_json::from_str(&assumptions_str).unwrap_or_default();
    let risks: Vec<String> = serde_json::from_str(&risks_str).unwrap_or_default();

    Ok(EntityContract {
        entity_id: row.get(0)?,
        corpus_id: row.get(1)?,
        is_public: row.get::<_, i64>(2)? != 0,
        is_must_use: row.get::<_, i64>(3)? != 0,
        is_deprecated: row.get::<_, i64>(4)? != 0,
        is_fallible: row.get::<_, i64>(5)? != 0,
        is_nullable: row.get::<_, i64>(6)? != 0,
        is_mutating: row.get::<_, i64>(7)? != 0,
        is_diverging: row.get::<_, i64>(8)? != 0,
        has_panic_risk: row.get::<_, i64>(9)? != 0,
        has_unsafe: row.get::<_, i64>(10)? != 0,
        is_incomplete: row.get::<_, i64>(11)? != 0,
        panic_call_count: row.get(12)?,
        debt_markers,
        assumptions,
        risks,
        intent_gap: row.get(16)?,
        caller_notes: row.get(17)?,
        model: row.get(18)?,
        model_tier: row.get(19)?,
        generated_at: row.get(20)?,
    })
}
