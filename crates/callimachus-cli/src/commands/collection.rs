use anyhow::{Result, bail};
use callimachus_core::storage::StorageBackend;
use callimachus_core::storage::collection_store::slugify;
use callimachus_core::types::{Collection, CollectionKind, MemberType};

/// Sub-commands for `calli collection`.
#[derive(Debug)]
pub enum CollectionSubcommand {
    Add {
        name: String,
        kind: Option<String>,
    },
    List,
    Status {
        collection_id: String,
    },
    AddMember {
        collection_id: String,
        member: String,
        as_collection: bool,
    },
    RemoveMember {
        collection_id: String,
        member: String,
        as_collection: bool,
    },
    Remove {
        collection_id: String,
    },
}

pub fn run(sub: CollectionSubcommand, db: &dyn StorageBackend) -> Result<()> {
    match sub {
        CollectionSubcommand::Add { name, kind } => {
            let kind_str = kind.as_deref().unwrap_or("series");
            let kind_val = CollectionKind::from_str(kind_str);
            let id = slugify(&name);
            if id.is_empty() {
                bail!("collection name '{name}' produces an empty slug");
            }
            let collection = Collection::new(&id, &name, kind_val);
            db.collection_insert(&collection)?;
            println!("✓ Collection '{name}' (kind: {kind_str}) created with id: {id}");
        }

        CollectionSubcommand::List => {
            let collections = db.collection_list()?;
            if collections.is_empty() {
                println!("(no collections)");
                return Ok(());
            }
            // Resolve corpus counts.
            let rows: Vec<(String, String, String, usize, u64, u64)> = collections
                .into_iter()
                .map(|c| {
                    let member_count = c.members.len();
                    let corpus_ids = db.collection_resolve_corpus_ids(&c.id).unwrap_or_default();
                    let chunks: u64 = corpus_ids
                        .iter()
                        .map(|cid| db.chunk_count(cid).unwrap_or(0))
                        .sum();
                    let entities: u64 = corpus_ids
                        .iter()
                        .map(|cid| db.entity_count(cid).unwrap_or(0))
                        .sum();
                    (
                        c.id,
                        c.name,
                        c.kind.as_str().to_string(),
                        member_count,
                        chunks,
                        entities,
                    )
                })
                .collect();

            // Header.
            println!(
                "{:<30} {:<25} {:<12} {:>7} {:>8} {:>9}",
                "ID", "NAME", "KIND", "MEMBERS", "CHUNKS", "ENTITIES"
            );
            println!("{}", "-".repeat(96));
            for (id, name, kind, members, chunks, entities) in rows {
                println!(
                    "{:<30} {:<25} {:<12} {:>7} {:>8} {:>9}",
                    id, name, kind, members, chunks, entities
                );
            }
        }

        CollectionSubcommand::Status { collection_id } => {
            let collection = db.collection_require(&collection_id)?;
            println!("Collection: {} ({})", collection.name, collection.id);
            println!("Kind:       {}", collection.kind);
            println!("Created:    {}", collection.created_at);
            println!();
            print_collection_tree(db, &collection_id, 0)?;
        }

        CollectionSubcommand::AddMember {
            collection_id,
            member,
            as_collection,
        } => {
            // Validate that the collection exists.
            db.collection_require(&collection_id)?;

            let member_type = if as_collection {
                // Validate target collection exists.
                db.collection_require(&member)?;
                MemberType::Collection
            } else {
                // Validate target corpus exists.
                db.corpus_require(&member)?;
                MemberType::Corpus
            };

            db.collection_add_member(&collection_id, &member, member_type)?;
            let kind_str = if as_collection {
                "collection"
            } else {
                "corpus"
            };
            println!("✓ {kind_str} '{member}' added to collection '{collection_id}'");
        }

        CollectionSubcommand::RemoveMember {
            collection_id,
            member,
            as_collection,
        } => {
            db.collection_require(&collection_id)?;
            let member_type = if as_collection {
                MemberType::Collection
            } else {
                MemberType::Corpus
            };
            db.collection_remove_member(&collection_id, &member, member_type)?;
            let kind_str = if as_collection {
                "collection"
            } else {
                "corpus"
            };
            println!("✓ {kind_str} '{member}' removed from collection '{collection_id}'");
        }

        CollectionSubcommand::Remove { collection_id } => {
            if !db.collection_delete(&collection_id)? {
                bail!("collection '{collection_id}' not found");
            }
            println!("✓ Collection '{collection_id}' deleted");
        }
    }
    Ok(())
}

/// Recursively print the resolved corpus tree for a collection, indented.
fn print_collection_tree(db: &dyn StorageBackend, collection_id: &str, depth: usize) -> Result<()> {
    let indent = "  ".repeat(depth);
    let members = db.collection_direct_members(collection_id)?;
    for m in members {
        match m.member_type {
            MemberType::Corpus => {
                let corpus = db.corpus_get(&m.member_id)?;
                let name = corpus.as_ref().map(|c| c.name.as_str()).unwrap_or("?");
                let chunks = corpus
                    .as_ref()
                    .map(|c| db.chunk_count(&c.id).unwrap_or(0))
                    .unwrap_or(0);
                let entities = corpus
                    .as_ref()
                    .map(|c| db.entity_count(&c.id).unwrap_or(0))
                    .unwrap_or(0);
                println!(
                    "{indent}  corpus  {:<30}  {name}  ({chunks} chunks, {entities} entities)",
                    m.member_id
                );
            }
            MemberType::Collection => {
                let col = db.collection_get(&m.member_id)?;
                let name = col.as_ref().map(|c| c.name.as_str()).unwrap_or("?");
                println!("{indent}  collection  {:<28}  {name}", m.member_id);
                print_collection_tree(db, &m.member_id, depth + 1)?;
            }
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use callimachus_core::storage::SqliteBackend;
    use callimachus_core::types::Corpus;

    fn setup() -> SqliteBackend {
        SqliteBackend::open_in_memory().unwrap()
    }

    fn seed_corpus(db: &dyn StorageBackend, id: &str) {
        let c = Corpus::new(
            id.into(),
            format!("Corpus {id}"),
            "book".into(),
            "/tmp".into(),
        );
        db.corpus_insert(&c).unwrap();
    }

    #[test]
    fn add_creates_collection_with_slug() {
        let db = setup();
        run(
            CollectionSubcommand::Add {
                name: "Eisenhorn Trilogy".into(),
                kind: Some("series".into()),
            },
            &db,
        )
        .unwrap();

        let cols = db.collection_list().unwrap();
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].id, "eisenhorn-trilogy");
        assert_eq!(cols[0].kind.as_str(), "series");
    }

    #[test]
    fn add_member_unknown_corpus_errors() {
        let db = setup();
        run(
            CollectionSubcommand::Add {
                name: "My Col".into(),
                kind: None,
            },
            &db,
        )
        .unwrap();

        let err = run(
            CollectionSubcommand::AddMember {
                collection_id: "my-col".into(),
                member: "nonexistent-corpus".into(),
                as_collection: false,
            },
            &db,
        )
        .unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("not found") || msg.contains("nonexistent"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn add_member_nested_collection() {
        let db = setup();
        run(
            CollectionSubcommand::Add {
                name: "Outer".into(),
                kind: None,
            },
            &db,
        )
        .unwrap();
        run(
            CollectionSubcommand::Add {
                name: "Inner".into(),
                kind: None,
            },
            &db,
        )
        .unwrap();

        run(
            CollectionSubcommand::AddMember {
                collection_id: "outer".into(),
                member: "inner".into(),
                as_collection: true,
            },
            &db,
        )
        .unwrap();

        let members = db.collection_direct_members("outer").unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].member_type, MemberType::Collection);
    }

    #[test]
    fn remove_deletes_collection_leaves_corpora() {
        let db = setup();
        seed_corpus(&db, "book1");
        run(
            CollectionSubcommand::Add {
                name: "My Series".into(),
                kind: None,
            },
            &db,
        )
        .unwrap();
        run(
            CollectionSubcommand::AddMember {
                collection_id: "my-series".into(),
                member: "book1".into(),
                as_collection: false,
            },
            &db,
        )
        .unwrap();

        run(
            CollectionSubcommand::Remove {
                collection_id: "my-series".into(),
            },
            &db,
        )
        .unwrap();

        // Collection is gone.
        assert!(db.collection_get("my-series").unwrap().is_none());
        // Corpus still exists.
        assert!(db.corpus_get("book1").unwrap().is_some());
    }
}
