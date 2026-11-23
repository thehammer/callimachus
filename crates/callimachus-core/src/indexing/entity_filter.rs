//! Entity subsets for LLM passes.
//!
//! An [`EntityFilter`] is built from one or more spec strings (the repeatable
//! `--entities` flag). An entity is selected if it matches **any** clause:
//!
//! | Clause | Matches |
//! |--------|---------|
//! | `name:<glob>` | canonical name, case-insensitive, `*` and `?` wildcards |
//! | `kind:<kind>` | entity kind (case-insensitive) |
//! | `id:<id or name>` | exact entity id, or exact canonical name |
//! | `callers-of:<name>` | entities with a `calls` edge to the named entity |
//! | `callees-of:<name>` | entities the named entity has a `calls` edge to |
//! | `touches:<name>` | entities with a `reads`/`writes`/`uses_table` edge to it |
//! | `@<path>` | a file with one clause (or bare name) per line; `#` comments |
//!
//! A bare word (no `key:` prefix) is shorthand for `id:<word>`.
//!
//! Graph clauses need the corpus graph. They are resolved into id sets once
//! (see [`EntityFilter::resolved`]); until then they match nothing.

use std::collections::HashSet;

use crate::{
    storage::{EdgeDirection, StorageBackend},
    types::Entity,
};

/// Edge kinds that count as "touching" an entity for `touches:<name>`.
/// Kept as a const so later slices can extend it.
pub const TOUCH_EDGE_KINDS: &[&str] = &["reads", "writes", "uses_table"];

const CALL_EDGE_KIND: &str = "calls";
const EDGE_LIMIT: usize = i32::MAX as usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clause {
    /// Lower-cased glob over the canonical name.
    Name(String),
    Kind(String),
    /// Entity id or canonical name.
    Id(String),
    CallersOf(GraphTarget),
    CalleesOf(GraphTarget),
    Touches(GraphTarget),
}

/// A graph clause: the named target plus, once resolved, the matching ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphTarget {
    pub name: String,
    pub ids: Option<HashSet<String>>,
}

impl GraphTarget {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ids: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EntityFilter {
    /// Every entity is selected.
    #[default]
    All,
    Spec(Vec<Clause>),
}

impl EntityFilter {
    /// Parse spec strings (each a clause or `@file`) into a filter. No specs
    /// means [`EntityFilter::All`].
    pub fn parse<S: AsRef<str>>(specs: &[S]) -> Result<Self, String> {
        let mut clauses = Vec::new();
        for spec in specs {
            let spec = spec.as_ref().trim();
            if spec.is_empty() {
                continue;
            }
            if let Some(path) = spec.strip_prefix('@') {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| format!("cannot read entity list '{path}': {e}"))?;
                for line in text.lines() {
                    let line = line.split('#').next().unwrap_or("").trim();
                    if !line.is_empty() {
                        clauses.push(parse_clause(line)?);
                    }
                }
            } else {
                clauses.push(parse_clause(spec)?);
            }
        }
        if clauses.is_empty() {
            Ok(Self::All)
        } else {
            Ok(Self::Spec(clauses))
        }
    }

    /// Parse and resolve against the corpus graph in one step.
    pub fn build<S: AsRef<str>>(
        specs: &[S],
        corpus_id: &str,
        graph: &dyn StorageBackend,
    ) -> anyhow::Result<Self> {
        Self::parse(specs)
            .map_err(|e| anyhow::anyhow!(e))?
            .resolved(corpus_id, graph)
    }

    pub fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }

    /// Resolve every not-yet-resolved graph clause into an id set. Already
    /// resolved clauses are left alone, so this is cheap to call repeatedly.
    pub fn resolved(self, corpus_id: &str, graph: &dyn StorageBackend) -> anyhow::Result<Self> {
        let Self::Spec(clauses) = self else {
            return Ok(self);
        };
        let mut entities: Option<Vec<Entity>> = None;
        let mut out = Vec::with_capacity(clauses.len());
        for clause in clauses {
            out.push(match clause {
                Clause::CallersOf(t) if t.ids.is_none() => Clause::CallersOf(resolve_target(
                    t,
                    corpus_id,
                    graph,
                    &mut entities,
                    EdgeDirection::Inbound,
                    &[CALL_EDGE_KIND],
                )?),
                Clause::CalleesOf(t) if t.ids.is_none() => Clause::CalleesOf(resolve_target(
                    t,
                    corpus_id,
                    graph,
                    &mut entities,
                    EdgeDirection::Outbound,
                    &[CALL_EDGE_KIND],
                )?),
                Clause::Touches(t) if t.ids.is_none() => Clause::Touches(resolve_target(
                    t,
                    corpus_id,
                    graph,
                    &mut entities,
                    EdgeDirection::Inbound,
                    TOUCH_EDGE_KINDS,
                )?),
                other => other,
            });
        }
        Ok(Self::Spec(out))
    }

    /// Whether `e` is selected. `graph` is accepted for symmetry with the
    /// resolve step; matching itself uses only the resolved id sets.
    pub fn matches(&self, e: &Entity, _graph: &dyn StorageBackend) -> bool {
        match self {
            Self::All => true,
            Self::Spec(clauses) => clauses.iter().any(|c| clause_matches(c, e)),
        }
    }

    /// Whether any entity whose first location is `uri` is selected.
    pub fn matches_location(&self, corpus_id: &str, uri: &str, graph: &dyn StorageBackend) -> bool {
        if self.is_all() {
            return true;
        }
        graph
            .entities_at_location(corpus_id, uri)
            .map(|es| {
                es.iter().any(|e| {
                    e.first_location.as_ref().is_some_and(|l| l.uri() == uri)
                        && self.matches(e, graph)
                })
            })
            .unwrap_or(false)
    }
}

fn parse_clause(s: &str) -> Result<Clause, String> {
    let Some((key, value)) = s.split_once(':') else {
        return Ok(Clause::Id(s.to_string()));
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("empty value in entity clause '{s}'"));
    }
    match key.trim() {
        "name" => Ok(Clause::Name(value.to_lowercase())),
        "kind" => Ok(Clause::Kind(value.to_lowercase())),
        "id" => Ok(Clause::Id(value.to_string())),
        "callers-of" => Ok(Clause::CallersOf(GraphTarget::new(value))),
        "callees-of" => Ok(Clause::CalleesOf(GraphTarget::new(value))),
        "touches" => Ok(Clause::Touches(GraphTarget::new(value))),
        other => Err(format!(
            "unknown entity clause '{other}:' (expected name:, kind:, id:, callers-of:, \
             callees-of:, touches: or @file); write ids as id:<id>"
        )),
    }
}

fn clause_matches(c: &Clause, e: &Entity) -> bool {
    match c {
        Clause::Name(glob) => glob_match(glob, &e.canonical_name.to_lowercase()),
        Clause::Kind(k) => e.kind.eq_ignore_ascii_case(k),
        Clause::Id(v) => e.id == *v || e.canonical_name.eq_ignore_ascii_case(v),
        Clause::CallersOf(t) | Clause::CalleesOf(t) | Clause::Touches(t) => {
            t.ids.as_ref().is_some_and(|ids| ids.contains(&e.id))
        }
    }
}

fn resolve_target(
    mut target: GraphTarget,
    corpus_id: &str,
    graph: &dyn StorageBackend,
    entities: &mut Option<Vec<Entity>>,
    direction: EdgeDirection,
    edge_kinds: &[&str],
) -> anyhow::Result<GraphTarget> {
    if entities.is_none() {
        *entities = Some(graph.entity_list(corpus_id)?);
    }
    let named: Vec<&Entity> = entities
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|e| e.id == target.name || e.canonical_name.eq_ignore_ascii_case(&target.name))
        .collect();

    let mut ids = HashSet::new();
    for n in named {
        for kind in edge_kinds {
            for edge in graph.edge_get_for_entity(&n.id, direction, Some(kind), EDGE_LIMIT)? {
                // `direction` is relative to `n`: inbound edges give us the
                // `from` side, outbound edges the `to` side.
                ids.insert(match direction {
                    EdgeDirection::Inbound => edge.from_entity_id,
                    _ => edge.to_entity_id,
                });
            }
        }
    }
    target.ids = Some(ids);
    Ok(target)
}

/// Case-sensitive glob over already-lowercased text; `*` any run, `?` one char.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::{
        storage::SqliteBackend,
        types::{Corpus, Edge, Location},
    };

    const C: &str = "c";

    fn ent(db: &SqliteBackend, name: &str, kind: &str) -> Entity {
        let mut e = Entity::new(format!("{C}:{name}"), C.into(), name.into(), kind.into());
        e.first_location = Some(Location::new(C, format!("src/{name}.c")));
        db.entity_upsert(&e).unwrap();
        e
    }

    fn edge(db: &SqliteBackend, from: &str, to: &str, kind: &str) {
        db.edge_upsert(&Edge {
            id: format!("{from}-{kind}-{to}"),
            corpus_id: C.into(),
            from_entity_id: format!("{C}:{from}"),
            to_entity_id: format!("{C}:{to}"),
            kind: kind.into(),
            location: Location::new(C, ""),
            confidence: 1.0,
            ..Default::default()
        })
        .unwrap();
    }

    /// `fun_a` calls `fun_b`; `fun_b` writes `DAT_00001000`; `fun_c` reads it.
    fn db() -> SqliteBackend {
        let db = SqliteBackend::open_in_memory().unwrap();
        db.corpus_insert(&Corpus::new(
            C.into(),
            "c".into(),
            "code".into(),
            "/tmp".into(),
        ))
        .unwrap();
        for n in ["fun_a", "fun_b", "fun_c"] {
            ent(&db, n, "function");
        }
        ent(&db, "DAT_00001000", "variable");
        edge(&db, "fun_a", "fun_b", "calls");
        edge(&db, "fun_b", "DAT_00001000", "writes");
        edge(&db, "fun_c", "DAT_00001000", "reads");
        db
    }

    fn names(db: &SqliteBackend, f: &EntityFilter) -> Vec<String> {
        let mut v: Vec<String> = db
            .entity_list(C)
            .unwrap()
            .into_iter()
            .filter(|e| f.matches(e, db))
            .map(|e| e.canonical_name)
            .collect();
        v.sort();
        v
    }

    fn build(db: &SqliteBackend, specs: &[&str]) -> EntityFilter {
        EntityFilter::build(specs, C, db).unwrap()
    }

    #[test]
    fn no_specs_is_all() {
        let db = db();
        assert!(build(&db, &[]).is_all());
        assert_eq!(names(&db, &build(&db, &[])).len(), 4);
    }

    #[test]
    fn name_glob_is_case_insensitive() {
        let db = db();
        assert_eq!(
            names(&db, &build(&db, &["name:FUN_?"])),
            ["fun_a", "fun_b", "fun_c"]
        );
        assert_eq!(names(&db, &build(&db, &["name:dat_*"])), ["DAT_00001000"]);
        assert_eq!(names(&db, &build(&db, &["name:*_b"])), ["fun_b"]);
    }

    #[test]
    fn kind_and_id_clauses() {
        let db = db();
        assert_eq!(
            names(&db, &build(&db, &["kind:variable"])),
            ["DAT_00001000"]
        );
        assert_eq!(names(&db, &build(&db, &["id:c:fun_a"])), ["fun_a"]);
        assert_eq!(names(&db, &build(&db, &["id:fun_c"])), ["fun_c"]);
        assert_eq!(names(&db, &build(&db, &["fun_b"])), ["fun_b"]);
    }

    #[test]
    fn graph_clauses() {
        let db = db();
        assert_eq!(names(&db, &build(&db, &["callers-of:fun_b"])), ["fun_a"]);
        assert_eq!(names(&db, &build(&db, &["callees-of:fun_a"])), ["fun_b"]);
        assert_eq!(
            names(&db, &build(&db, &["touches:DAT_00001000"])),
            ["fun_b", "fun_c"]
        );
    }

    #[test]
    fn clauses_or_together() {
        let db = db();
        let f = build(
            &db,
            &["name:fun_a", "callers-of:DAT_00001000", "kind:variable"],
        );
        // callers-of follows only `calls` edges, so the variable clause adds DAT.
        assert_eq!(names(&db, &f), ["DAT_00001000", "fun_a"]);
    }

    #[test]
    fn unresolved_graph_clause_matches_nothing_until_resolved() {
        let db = db();
        let f = EntityFilter::parse(&["callers-of:fun_b"]).unwrap();
        assert!(names(&db, &f).is_empty());
        assert_eq!(names(&db, &f.resolved(C, &db).unwrap()), ["fun_a"]);
    }

    #[test]
    fn file_spec_with_comments() {
        let db = db();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "# the subset\nfun_a   # bare name\n\nkind:variable").unwrap();
        let spec = format!("@{}", file.path().display());
        assert_eq!(
            names(&db, &build(&db, &[spec.as_str()])),
            ["DAT_00001000", "fun_a"]
        );
    }

    #[test]
    fn bad_specs_error() {
        assert!(EntityFilter::parse(&["bogus:x"]).is_err());
        assert!(EntityFilter::parse(&["name:"]).is_err());
        assert!(EntityFilter::parse(&["@/nonexistent/list.txt"]).is_err());
    }

    #[test]
    fn location_match_uses_first_location() {
        let db = db();
        let f = build(&db, &["name:fun_a"]);
        assert!(f.matches_location(C, &Location::new(C, "src/fun_a.c").uri(), &db));
        assert!(!f.matches_location(C, &Location::new(C, "src/fun_b.c").uri(), &db));
    }
}
