# Callimachus

## MCP Server

This project ships with a pre-built index of its own codebase at `data/callimachus.pinakes`.
When you open this project in Claude Code, the `callimachus` MCP server starts automatically
via `.mcp.json` and exposes the full entity graph, summaries, purposes, and contracts
for this codebase as queryable tools.

The `calli` binary must be on PATH. Build it first if needed:

```bash
cargo build --release
export PATH="$PWD/target/release:$PATH"
```

Or install it system-wide:

```bash
cargo install --path crates/callimachus-cli
```

## Working with the index

```bash
# Query the bundled demo index
calli --pinakes data/callimachus.pinakes inspect entities callimachus
calli --pinakes data/callimachus.pinakes inspect runs callimachus

# Index a new corpus
calli corpus add code "My Project" /path/to/project
calli index my-project
```

## Codebase notes

- Core indexing logic: `crates/callimachus-core/src/indexing/`
- Storage layer: `crates/callimachus-core/src/storage/`
- Adapter pattern: `crates/adapters/` — one crate per corpus kind (book, code, wiki)
- MCP tools: `crates/callimachus-mcp/src/tools.rs`
- CLI commands: `crates/callimachus-cli/src/commands/`

See `docs/codebase-analysis.md` for a structural analysis of this codebase
produced by indexing itself.

## Honest provenance model

Every artifact carries a [`Provenance`](crates/callimachus-core/src/types/provenance.rs)
tagged-union stamp that records *when* it was derived:

- **`Concrete(sha)`** — the artifact's substrate was proven touched at this SHA
  (the diff against the neighbour commit changed it).
- **`RangePredating(sha)`** — the artifact predates this SHA, but its exact
  most-recent-modification commit is not (yet) known. A one-sided upper bound.

Provenance only ever narrows: `RangePredating` can be refined to `Concrete`;
`Concrete` is already maximally specific and never widens. This monotonicity
invariant is enforced by `Provenance::refine` and `StorageBackend::refine_provenance`.

### Storage encoding

The `(derived_at_kind, derived_at_sha)` column pair on every head and history
table encodes the tagged union. Migration 013 introduced these columns.

Migration 015 completed the cleanup: the legacy `derived_at_version TEXT` and
`superseded_at_version TEXT` columns (from migration 012) were dropped from all
head and history tables. The uniqueness indexes on `*_history` tables, which
previously used `COALESCE(NULLIF(derived_at_sha,''), derived_at_version, '')`
as the key expression, were recreated as plain column-only indexes on
`(corpus_id, ..., derived_at_kind, derived_at_sha)`. All Rust types now carry
`provenance: Option<Provenance>` instead of `derived_at_version: Option<String>`.

### Writing provenance — the history_layer pattern

All provenance writes go through `crates/callimachus-core/src/indexing/history_layer.rs`.
The `history_layer::commit` function is the single writer: it snapshots the
current head row into `*_history` (if one exists), then upserts the head row
with the new provenance. This guarantees history tables are populated consistently
regardless of which walk path produced the artifact.

### Full design

See [`docs/plans/honest-provenance.md`](docs/plans/honest-provenance.md) for the
complete PRD, and [`docs/plans/honest-provenance-implementation.md`](docs/plans/honest-provenance-implementation.md)
for the phased implementation plan (PRs #36, #38, #39, #40).
