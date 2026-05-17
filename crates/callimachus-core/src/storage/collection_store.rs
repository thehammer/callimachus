use crate::error::{CalError, Result};
use crate::storage::db::Database;
use crate::types::{Collection, CollectionKind, CollectionMember, MemberType};
use rusqlite::params;
pub fn insert(db: &Database, collection: &Collection) -> Result<()> {
    db.conn().execute(
        "INSERT INTO collections (id, name, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            collection.id,
            collection.name,
            collection.kind.as_str(),
            collection.created_at
        ],
    )?;
    Ok(())
}

pub fn list(db: &Database) -> Result<Vec<Collection>> {
    let ids: Vec<String> = {
        let mut stmt = db
            .conn()
            .prepare("SELECT id FROM collections ORDER BY created_at ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(CalError::from)?
    };
    ids.into_iter().map(|id| require(db, &id)).collect()
}

pub fn get(db: &Database, id: &str) -> Result<Option<Collection>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT id, name, kind, created_at FROM collections WHERE id = ?1")?;
    let mut rows = stmt.query_map(params![id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    match rows.next() {
        None => Ok(None),
        Some(r) => {
            let (cid, name, kind_str, created_at) = r.map_err(CalError::from)?;
            let members = direct_members(db, &cid)?;
            Ok(Some(Collection {
                id: cid,
                name,
                kind: CollectionKind::from_str(&kind_str),
                created_at,
                members,
            }))
        }
    }
}

pub fn require(db: &Database, id: &str) -> Result<Collection> {
    get(db, id)?.ok_or_else(|| CalError::NotFound(format!("collection '{id}'")))
}

pub fn add_member(
    db: &Database,
    collection_id: &str,
    member_id: &str,
    member_type: MemberType,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    db.conn().execute(
        "INSERT OR IGNORE INTO collection_members
             (collection_id, member_id, member_type, added_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![collection_id, member_id, member_type.as_str(), now],
    )?;
    Ok(())
}

pub fn remove_member(
    db: &Database,
    collection_id: &str,
    member_id: &str,
    member_type: MemberType,
) -> Result<()> {
    db.conn().execute(
        "DELETE FROM collection_members
         WHERE collection_id = ?1 AND member_id = ?2 AND member_type = ?3",
        params![collection_id, member_id, member_type.as_str()],
    )?;
    Ok(())
}

pub fn delete(db: &Database, id: &str) -> Result<bool> {
    let n = db
        .conn()
        .execute("DELETE FROM collections WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

pub fn direct_members(db: &Database, collection_id: &str) -> Result<Vec<CollectionMember>> {
    let mut stmt = db.conn().prepare(
        "SELECT member_id, member_type FROM collection_members
         WHERE collection_id = ?1 ORDER BY member_id ASC",
    )?;
    let rows = stmt.query_map(params![collection_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut members = Vec::new();
    for row in rows {
        let (member_id, member_type_str) = row.map_err(CalError::from)?;
        let member_type = MemberType::from_str(&member_type_str)
            .ok_or_else(|| CalError::Other(format!("unknown member_type: {member_type_str}")))?;
        members.push(CollectionMember {
            member_id,
            member_type,
        });
    }
    Ok(members)
}

/// Recursively resolve to the flat, deduplicated set of corpus ids reachable
/// from `collection_id`, expanding nested collections.
///
/// Traversal order: DFS with alphabetical ordering of members at each level.
/// Cycles are detected by tracking the in-progress stack; back-edges are
/// logged as warnings and skipped.
pub fn resolve_corpus_ids(db: &Database, collection_id: &str) -> Result<Vec<String>> {
    let mut result: Vec<String> = Vec::new();
    let mut seen_corpora: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut in_stack: std::collections::HashSet<String> = std::collections::HashSet::new();

    resolve_recursive(
        db,
        collection_id,
        &mut result,
        &mut seen_corpora,
        &mut in_stack,
    )?;
    Ok(result)
}

fn resolve_recursive(
    db: &Database,
    collection_id: &str,
    result: &mut Vec<String>,
    seen_corpora: &mut std::collections::HashSet<String>,
    in_stack: &mut std::collections::HashSet<String>,
) -> Result<()> {
    if in_stack.contains(collection_id) {
        tracing::warn!(
            collection_id,
            "cycle detected in collection membership; skipping back-edge"
        );
        return Ok(());
    }

    in_stack.insert(collection_id.to_string());

    let members = direct_members(db, collection_id)?;
    // Members are already returned alphabetically by member_id from direct_members.
    for member in members {
        match member.member_type {
            MemberType::Corpus => {
                if seen_corpora.insert(member.member_id.clone()) {
                    result.push(member.member_id);
                }
            }
            MemberType::Collection => {
                resolve_recursive(db, &member.member_id, result, seen_corpora, in_stack)?;
            }
        }
    }

    in_stack.remove(collection_id);
    Ok(())
}

/// Generate a URL-safe slug from a display name.
pub fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Database, corpus_store};
    use crate::types::{Collection, CollectionKind, Corpus};

    fn setup() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn seed_corpus(db: &Database, id: &str) {
        let corpus = Corpus::new(
            id.into(),
            format!("Corpus {id}"),
            "book".into(),
            "/tmp".into(),
        );
        corpus_store::insert(db, &corpus).unwrap();
    }

    fn make_collection(id: &str, name: &str) -> Collection {
        Collection::new(id, name, CollectionKind::Series)
    }

    #[test]
    fn insert_and_list() {
        let db = setup();
        insert(&db, &make_collection("c1", "My Collection")).unwrap();
        let cols = list(&db).unwrap();
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].id, "c1");
        assert!(cols[0].members.is_empty());
    }

    #[test]
    fn add_and_remove_corpus_member() {
        let db = setup();
        insert(&db, &make_collection("c1", "Col")).unwrap();
        seed_corpus(&db, "book1");

        add_member(&db, "c1", "book1", MemberType::Corpus).unwrap();
        let col = get(&db, "c1").unwrap().unwrap();
        assert_eq!(col.members.len(), 1);
        assert_eq!(col.members[0].member_id, "book1");
        assert_eq!(col.members[0].member_type, MemberType::Corpus);

        remove_member(&db, "c1", "book1", MemberType::Corpus).unwrap();
        let col2 = get(&db, "c1").unwrap().unwrap();
        assert!(col2.members.is_empty());
    }

    #[test]
    fn add_nested_collection_member() {
        let db = setup();
        insert(&db, &make_collection("outer", "Outer")).unwrap();
        insert(&db, &make_collection("inner", "Inner")).unwrap();

        add_member(&db, "outer", "inner", MemberType::Collection).unwrap();
        let ms = direct_members(&db, "outer").unwrap();
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].member_type, MemberType::Collection);
    }

    #[test]
    fn resolve_corpus_ids_flat() {
        let db = setup();
        insert(&db, &make_collection("c1", "Col")).unwrap();
        seed_corpus(&db, "b1");
        seed_corpus(&db, "b2");
        add_member(&db, "c1", "b1", MemberType::Corpus).unwrap();
        add_member(&db, "c1", "b2", MemberType::Corpus).unwrap();

        let ids = resolve_corpus_ids(&db, "c1").unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"b1".to_string()));
        assert!(ids.contains(&"b2".to_string()));
    }

    #[test]
    fn resolve_corpus_ids_nested() {
        let db = setup();
        insert(&db, &make_collection("outer", "Outer")).unwrap();
        insert(&db, &make_collection("inner", "Inner")).unwrap();
        seed_corpus(&db, "b1");
        seed_corpus(&db, "b2");
        seed_corpus(&db, "b3");

        add_member(&db, "inner", "b1", MemberType::Corpus).unwrap();
        add_member(&db, "inner", "b2", MemberType::Corpus).unwrap();
        add_member(&db, "outer", "inner", MemberType::Collection).unwrap();
        add_member(&db, "outer", "b3", MemberType::Corpus).unwrap();

        let ids = resolve_corpus_ids(&db, "outer").unwrap();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&"b1".to_string()));
        assert!(ids.contains(&"b2".to_string()));
        assert!(ids.contains(&"b3".to_string()));
    }

    #[test]
    fn resolve_corpus_ids_cycle_no_loop() {
        let db = setup();
        insert(&db, &make_collection("a", "A")).unwrap();
        insert(&db, &make_collection("b", "B")).unwrap();
        seed_corpus(&db, "x");

        // a → b → a (cycle). b also has corpus x.
        add_member(&db, "a", "b", MemberType::Collection).unwrap();
        add_member(&db, "b", "a", MemberType::Collection).unwrap();
        add_member(&db, "b", "x", MemberType::Corpus).unwrap();

        // Should not loop; should return x.
        let ids = resolve_corpus_ids(&db, "a").unwrap();
        assert!(ids.contains(&"x".to_string()));
    }

    #[test]
    fn delete_cascades_members() {
        let db = setup();
        insert(&db, &make_collection("c1", "Col")).unwrap();
        seed_corpus(&db, "b1");
        add_member(&db, "c1", "b1", MemberType::Corpus).unwrap();

        assert!(delete(&db, "c1").unwrap());
        assert!(get(&db, "c1").unwrap().is_none());
        // Members cascade-deleted via FK.
        let ms = direct_members(&db, "c1").unwrap();
        assert!(ms.is_empty());
    }

    #[test]
    fn require_not_found() {
        let db = setup();
        let err = require(&db, "nope").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Eisenhorn Trilogy"), "eisenhorn-trilogy");
        assert_eq!(slugify("Black Library 40K"), "black-library-40k");
        assert_eq!(slugify("GOF Design Patterns"), "gof-design-patterns");
    }
}
