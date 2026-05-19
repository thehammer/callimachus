# Stage 0 — history-aware change detection for incremental indexing

## Context

The Callimachus indexing pipeline currently has no principled answer to "what
changed since the last run?" `chunk_pass.rs:64` decides idempotency by asking
the DB whether a chunk ID already exists. That works for *new* sources but
misses the most important case: a file whose contents changed produces the
same location URI and therefore the same chunk ID, and is silently skipped
unless `--full` is passed. There is no per-chunk content hash, no per-corpus
version anchor, and `corpus.last_indexed_at` is a wall-clock timestamp rather
than a content-version reference.

This wedge introduces **Stage 0 (`Pass::History`)** — a new first stage that
runs before `Pass::Chunk` and produces a `ChangeManifest` enumerating which
source paths are new or modified since the last successful run. All
downstream passes thread the manifest through and skip sources/chunks not
listed. The manifest is anchored by a *version reference*: a git commit SHA
when the corpus is a git repo, or a stable hash-of-hashes when it is not.
The version is written back to the corpus row only after the full pipeline
completes successfully, so partial failures replay correctly.

There is already a `crates/callimachus-core/src/indexing/change_detector.rs`
module (with `ChangeSet` / `ChangeStrategy` types) and a `reindex_pass.rs`
that the `watcher` and a `reindex` integration test consume. Stage 0 is the
in-pipeline successor to that ad-hoc detector path: the new manifest carries
strictly more information (anchored version, dirty set, deletions) and the
existing `ChangeSet`-based callers should migrate to it in a follow-up wedge.
**This PR does not delete `change_detector.rs` or `reindex_pass.rs`** — they
remain in place so the watcher keeps working; we only add Stage 0 alongside
them and rewire `chunk_pass` and downstream LLM passes to consult the new
manifest.

Stage 0 also captures cheap git commit metadata (message, author, SHA) for
each changed file at the same time it computes the diff, and writes it to
new `chunks.last_modified_*` columns. That is the only "history graph" work
in scope here — no commit entities, no co-change edges, no churn counts, no
hot/cold tiering. Those land in later wedges that build on these anchors.

PR #9 (PHP/TS/Vue contracts) just merged, so the code adapter already pulls
in `git2`. We reuse it.

## Target

- **Repo:** `callimachus` (`/Users/hammer/Code/callimachus`)
- **Branch:** `feature/stage-0-change-detection`
- **Base:** `origin/main`

## Files to change

### New files

- `crates/callimachus-core/src/indexing/history_pass.rs` — the Stage 0 runner; entry point `pub async fn run(db, corpus, adapter, opts) -> anyhow::Result<(ChangeManifest, PassStats)>`.
- `crates/callimachus-core/src/indexing/change_manifest.rs` — defines `ChangeManifest`, `ChangedSource`, `ChangeManifestExt` helpers, and unit tests for `is_dirty`.
- `crates/callimachus-core/migrations/009_change_manifest.sql` — adds `corpora.last_indexed_version TEXT`, `chunks.source_hash TEXT`, `chunks.introduced_at_version TEXT`, `chunks.last_modified_at_version TEXT`, `chunks.last_modified_commit_message TEXT`, `chunks.last_modified_author TEXT`. All nullable. Add an index on `chunks(corpus_id, source_hash)` for fast unchanged-source lookups.

### Modified files

- `crates/callimachus-core/src/storage/db.rs` — add `M::up(include_str!("../../migrations/009_change_manifest.sql"))` to the migrations vec (currently lines 8–15).
- `crates/callimachus-core/src/storage/backend.rs:27` — extend `StorageBackend` trait with:
  - `fn corpus_set_last_indexed_version(&self, id: &str, version: &str) -> Result<()>;`
  - `fn corpus_get_last_indexed_version(&self, id: &str) -> Result<Option<String>>;`
  - `fn chunk_set_source_hash(&self, chunk_id: &str, hash: &str) -> Result<()>;`
  - `fn chunk_set_history(&self, chunk_id: &str, version: &str, commit_message: Option<&str>, author: Option<&str>) -> Result<()>;`
  - `fn chunk_list_source_paths(&self, corpus_id: &str) -> Result<Vec<(String, String, String)>>;` — returns `(chunk_id, location_path, source_hash)` so Stage 0 can compare DB state to fresh adapter output.
- `crates/callimachus-core/src/storage/corpus_store.rs` — implement `set_last_indexed_version` and `get_last_indexed_version`. Update the `SELECT id, name, kind, source, …` queries and `row_to_corpus` (around `corpus_store.rs:112`) so `last_indexed_version` is loaded into the new `Corpus` field.
- `crates/callimachus-core/src/storage/chunk_store.rs` — extend the chunk INSERT/upsert SQL to include the new columns and add `set_source_hash` / `set_history` / `list_source_paths` functions.
- `crates/callimachus-core/src/storage/sqlite.rs:78-83` — wire the new trait methods through to `corpus_store` and `chunk_store`.
- `crates/callimachus-core/src/storage/postgres.rs:60-118` — add stub `Err(anyhow!("not implemented"))` impls so the trait still compiles. (Postgres backend is currently entirely unimplemented stubs.)
- `crates/callimachus-core/src/types/pass.rs:6-25` — add `Pass::History` variant (first in enum), update `Display` and `FromStr` (`"history"`).
- `crates/callimachus-core/src/types/corpus.rs:37-66` — add `pub last_indexed_version: Option<String>` field (after `last_indexed_at`); default `None` in `Corpus::new`.
- `crates/callimachus-core/src/types/chunk.rs` — add `source_hash: Option<String>`, `introduced_at_version: Option<String>`, `last_modified_at_version: Option<String>` fields with `#[serde(default)]`. Leave `last_modified_commit_message` / `last_modified_author` in storage only (no need on the in-memory `Chunk` for this wedge — they are written by Stage 0 directly via `chunk_set_history`).
- `crates/callimachus-core/src/adapter/contract.rs:87` — extend the `SourceAdapter` trait with two new methods (default impls provided in the trait body):
  ```rust
  /// Version reference for the corpus's current state. Default: SHA-256 of
  /// the sorted (path, content-hash) pairs for every file under source_path.
  fn current_version(&self, source_path: &str) -> anyhow::Result<String> { /* default impl */ }

  /// Files that changed between two version refs. `from_version = None` ⇒
  /// return all source paths (first-run case). Default: compare per-file
  /// hashes from a JSON-encoded `from_version` manifest if available;
  /// otherwise return all paths.
  fn changed_sources(
      &self,
      source_path: &str,
      from_version: Option<&str>,
      to_version: &str,
  ) -> anyhow::Result<Vec<ChangedSource>> { /* default impl */ }
  ```
  Where `ChangedSource { path: String, kind: ChangeKind, commit_meta: Option<CommitMeta> }`, `ChangeKind ∈ {Added, Modified, Deleted}`, and `CommitMeta { sha, message, author, date }` is populated only by adapters that have it (CodeAdapter when on a git repo).
  Default `current_version` body: walk `source_path`, hash every file with SHA-256, sort by path, hash the concatenated `"{path}\0{hex_hash}\n"` lines, return the resulting hex digest with prefix `"v1-tree:"`. Default `changed_sources`: if `from_version` is `None` or doesn't parse as `"v1-tree:…"` ⇒ return every source path as `Added`; otherwise compute new per-file hashes and diff against the stored manifest. (For the default impl to be useful, store the per-file map under a sidecar key — for this wedge, the default just returns all paths when `from_version` differs from `to_version`, which is correct if wasteful. The CodeAdapter override is where the actual diff happens. Document this in the trait doc-comment.)
- `crates/adapters/callimachus-adapter-code/src/adapter.rs` — override both methods. `current_version` uses `git2::Repository::open(source_path)` and returns `format!("git:{}", head_oid_full)`; falls back to the default tree-hash impl when the source is not a git repo. `changed_sources` uses `git2` `diff_tree_to_tree` between the two commit OIDs and emits `ChangedSource` entries with `commit_meta` populated from the HEAD commit. Add helper in `crates/adapters/callimachus-adapter-code/src/git.rs` for `diff_between(repo, from_sha, to_sha) -> Vec<ChangedSource>`.
- `crates/adapters/callimachus-adapter-book/src/adapter.rs` — no override needed, but add a one-line `// uses default current_version / changed_sources` comment where the trait impl block lives so future readers see the intent.
- `crates/adapters/callimachus-adapter-wiki/src/adapter.rs` — same as book.
- `crates/callimachus-core/src/indexing/mod.rs:2-15` — `pub mod change_manifest;` and `pub mod history_pass;`; re-export `ChangeManifest`, `ChangedSource`, `ChangeKind` from `change_manifest`.
- `crates/callimachus-core/src/lib.rs:20` — add `ChangeManifest` to the public re-export list.
- `crates/callimachus-core/src/indexing/pipeline.rs:17-53` — extend `IndexOptions` with `pub change_manifest: Option<ChangeManifest>` (default `None`). Add `Pass::History` to the default `passes` vec **before** `Pass::Chunk`.
- `crates/callimachus-core/src/indexing/pipeline.rs:75-208` — in `IndexPipeline::run`:
  1. Before the pass loop, capture a `manifest: Option<ChangeManifest>` local that is updated when `Pass::History` runs.
  2. Add a `Pass::History` match arm that calls `history_pass::run(...)`, stores the result in `manifest`, and threads a *clone* of it into `opts` for subsequent passes via a mutable `opts_with_manifest` clone (since `opts` is `&IndexOptions`, build a single `let mut opts_local = opts.clone();` outside the loop and update it after Stage 0).
  3. After the loop finishes successfully and `!opts.dry_run`, call `db.corpus_set_last_indexed_version(&corpus.id, &manifest.current_version)` if the manifest is `Some`. Do this in addition to the existing `corpus_set_last_indexed(now)` call at `pipeline.rs:201-204`.
  4. If a single-pass invocation (e.g. `passes == vec![Pass::Semantic]`) does *not* include `Pass::History`, synthesize an all-dirty manifest at the top of `run()` so downstream passes don't trip on a missing manifest. Document this behaviour in `IndexOptions` doc-comment: "When `passes` omits `Pass::History`, the pipeline treats every source as dirty."
- `crates/callimachus-core/src/indexing/chunk_pass.rs` — consume `opts.change_manifest`:
  1. After `adapter.discover` and before chunking each source, check `manifest.is_dirty(&source.path)`. If not dirty, increment `stats.skipped` and `continue`.
  2. After `chunk_upsert(&chunk)`, compute `source_hash = sha256(source_content)` once per source (hoist out of the per-chunk loop), call `db.chunk_set_source_hash(&chunk.id, &source_hash)`, and call `db.chunk_set_history(&chunk.id, &manifest.current_version, commit_meta.message, commit_meta.author)` when the manifest entry for this source carries `commit_meta`. For brand-new chunks (first time the chunk ID is seen) also set `introduced_at_version` — the simplest way is to extend `chunk_upsert` so it sets `introduced_at_version` only when the row is being inserted (use `INSERT … ON CONFLICT DO UPDATE SET … (no change to introduced_at_version)`).
- `crates/callimachus-core/src/indexing/structure_pass.rs`, `semantic_pass.rs`, `aliases_pass.rs`, `summarize_pass.rs`, `purpose_pass.rs`, `contract_pass.rs`, `theme_pass.rs`, `embed_pass.rs` — at the top of each `run`, when `opts.change_manifest` is `Some(m)` and `!m.all_dirty`, filter the work list to chunks whose underlying source file belongs to a dirty source. Concretely: load chunks from the DB as today, then `retain(|c| manifest.is_dirty_for_chunk(c))`. Add a helper on `ChangeManifest` that takes a `&Chunk` and decides — the rule is "the chunk's source file path is in `dirty_paths`, OR `all_dirty` is true." The helper must extract the file path from the chunk's location URI using this exact logic:
  1. Strip the `calli://` scheme prefix.
  2. Drop everything up to and including the first `/` (that segment is the corpus-id).
  3. Take everything before the first `#` (the fragment encodes the symbol anchor).
  
  Example: `calli://admin-portal/src/app/Services/VisitService.php#VisitService/part5` → file path is `src/app/Services/VisitService.php`. For book/wiki chunks the URI has no `#` fragment, so step 3 is a no-op and the result is the full relative path. In Rust the extraction is:
  ```rust
  fn file_path_from_uri<'a>(uri: &'a str) -> &'a str {
      let without_scheme = uri.strip_prefix("calli://").unwrap_or(uri);
      let without_corpus = without_scheme.splitn(2, '/').nth(1).unwrap_or(without_scheme);
      without_corpus.split('#').next().unwrap_or(without_corpus)
  }
  ```
  Use this verbatim (or as a private free function in `change_manifest.rs`) and call it inside `is_dirty_for_chunk`. Document the extraction rule and the example in the helper's doc-comment so future adapters can reason about it without reading the URI spec.
- `crates/callimachus-core/src/indexing/pipeline.rs` tests at lines 211–414 — extend the `FakeAdapter` impl with `current_version` (return a static string) and `changed_sources` (return all paths). Update the `all_passes_complete_without_error` test to expect **8** run-log entries instead of 7 (add history). Update `chunk_pass_is_idempotent` to assert the manifest reports zero dirty paths on the second run.
- `crates/callimachus-cli/src/commands/index.rs` — no CLI surface changes. Confirm `IndexOptions { change_manifest: None, .. }` still compiles wherever the CLI constructs options. If `--pass history` should be expressible on the CLI (it should, for symmetry), wire `"history" => Pass::History` through whatever pass-name parser the CLI uses; grep for the existing parser and add the variant there too.

### Untouched but read-during-implementation

- `crates/callimachus-core/src/indexing/change_detector.rs` (the legacy `ChangeSet`-based detector). **Not deleted.** Stage 0 is implemented alongside it; reconciliation is a follow-up wedge.
- `crates/callimachus-core/src/indexing/reindex_pass.rs` and `watcher.rs` — unchanged.

## Approach

1. **Schema first.** Write `migrations/009_change_manifest.sql` adding the six new nullable columns plus an index on `chunks(corpus_id, source_hash)`. Add it to the `MIGRATIONS` vec in `storage/db.rs`. Run `cargo test -p callimachus-core` to confirm existing tests still pass with the migration applied to fresh in-memory DBs.

2. **Types.** Add `last_indexed_version: Option<String>` to `Corpus` and `source_hash` / `introduced_at_version` / `last_modified_at_version: Option<String>` to `Chunk`. Add `Pass::History` variant and string conversions. Bump nothing else — these are additive.

3. **Storage layer.** Implement the new SQLite functions in `corpus_store.rs` and `chunk_store.rs`. Update `row_to_corpus` and the chunk row mapper. Add stubs to `postgres.rs` so the trait still compiles. Add unit tests in `storage/sqlite.rs`'s existing test module for `corpus_set/get_last_indexed_version` and `chunk_set_source_hash` round-trip.

4. **`ChangeManifest` and `SourceAdapter` extensions.** Create `change_manifest.rs` with the struct and helper methods (`is_dirty`, `is_dirty_for_chunk`, `all_dirty()`, `empty()`). Extend `SourceAdapter` with default `current_version` (SHA-256 hash-of-hashes) and default `changed_sources` (returns all paths when version differs, else empty). Unit-test the defaults against a temp directory.

5. **Code adapter overrides.** In `callimachus-adapter-code/src/adapter.rs`, override `current_version` to return `format!("git:{full_oid}")` when `git2::Repository::open` succeeds. Override `changed_sources` to call a new helper in `git.rs` that uses `Repository::diff_tree_to_tree` between two commit OIDs. Populate `ChangedSource.commit_meta` from the *to-version* commit (its message/author). Add unit tests in `git.rs` modelled on the existing `git_strategy_selected_when_since_is_ref_and_git_exists` test in `change_detector.rs:447`.

6. **`Pass::History` runner.** Write `history_pass.rs`. Signature: `pub async fn run(db, corpus, adapter, opts) -> anyhow::Result<(ChangeManifest, PassStats)>`. Body:
   - Read `corpus.last_indexed_version` (the field is already on the in-memory `Corpus` after step 2).
   - Compute `current = adapter.current_version(&corpus.source)?`.
   - If `last == Some(current)` and `!opts.full` ⇒ return `ChangeManifest::empty(current)`.
   - If `last.is_none() || opts.full` ⇒ return `ChangeManifest::all_dirty(current)`.
   - Otherwise: `let changed = adapter.changed_sources(&corpus.source, last.as_deref(), &current)?;` and assemble a `ChangeManifest { dirty_paths: changed.iter().map(|c| c.path).collect(), all_dirty: false, current_version: current, commit_metadata: changed.into_iter().filter_map(...).collect() }`.
   - `stats.processed = manifest.dirty_paths.len() as u64`.

7. **Pipeline wiring.** In `pipeline.rs::run`, take `opts: IndexOptions` by *value* (or clone at the top — currently `&opts` is borrowed; switch the loop to read `&opts_local` so we can update `opts_local.change_manifest` after Stage 0). Add the `Pass::History` arm: run it, stash the manifest on `opts_local`, then continue the loop. If the user passed `passes` without `Pass::History`, synthesise `ChangeManifest::all_dirty("synthetic")` so downstream passes don't crash. After the loop, write `last_indexed_version` only if `opts.passes.contains(&Pass::History)` and not dry-run.

8. **Chunk pass — source-hash and history columns.** Update `chunk_pass.rs` per the modified-files spec above: filter sources by manifest before chunking, hash source content once per source, set `source_hash` and `introduced_at_version` / `last_modified_at_version` via the new storage calls. Make sure the per-chunk `chunk_upsert` and the post-upsert `chunk_set_source_hash` happen atomically enough that a crash between them does not leave a chunk without a hash on a happy path (acceptable: next run treats it as dirty via the manifest). Use the existing `db.chunk_has` short-circuit only when the source is not dirty *and* the hash matches — but since we already skipped non-dirty sources in step 1 of the pass, this is moot inside the loop.

9. **Downstream passes — manifest filter.** For each of `structure_pass.rs`, `semantic_pass.rs`, `aliases_pass.rs`, `summarize_pass.rs`, `purpose_pass.rs`, `contract_pass.rs`, `theme_pass.rs`, `embed_pass.rs`: after the existing chunk-list fetch, insert a `chunks.retain(|c| manifest.as_ref().map(|m| m.is_dirty_for_chunk(c)).unwrap_or(true));`. The `is_dirty_for_chunk` helper on `ChangeManifest` must extract the source file path from the chunk's location URI by: (a) stripping the `calli://` scheme prefix, (b) dropping everything up to and including the first `/` to remove the corpus-id segment, (c) taking everything before the first `#` to discard the symbol anchor. Example: `calli://admin-portal/src/app/Services/VisitService.php#VisitService/part5` → `src/app/Services/VisitService.php`. For book/wiki chunks (no `#` fragment) step (c) is a no-op. The extraction should be a private free function in `change_manifest.rs` — `fn file_path_from_uri(uri: &str) -> &str` — with this body:
   ```rust
   let without_scheme = uri.strip_prefix("calli://").unwrap_or(uri);
   let without_corpus = without_scheme.splitn(2, '/').nth(1).unwrap_or(without_scheme);
   without_corpus.split('#').next().unwrap_or(without_corpus)
   ```
   Document the extraction rule and the example in the `is_dirty_for_chunk` doc-comment so future adapters don't have to reverse-engineer the URI format.

10. **Tests.** Add the unit + integration tests called out in the Tests section below. Update the existing pipeline tests at `pipeline.rs:329` to account for the new `history` run-log entry.

11. **Manual smoke.** Re-index `data/callimachus.db` against the live repo: first run should produce a non-null `last_indexed_version` (a real git SHA, since this repo is a git repo); second invocation of `calli index callimachus` should report zero chunks processed in chunk/structure/semantic passes.

12. **Document.** Add a paragraph to `docs/architecture.md` describing Stage 0 and the version-anchor model. One paragraph, no diagrams — diagrams are a follow-up.

## Acceptance criteria

### Functional (behaviour the executor must achieve)

- A fresh corpus indexed for the first time writes a non-null `corpus.last_indexed_version` after the pipeline completes successfully (and only then).
- Running `calli index <corpus>` a second time, with no source changes, produces **zero processed chunks in chunk, structure, semantic, aliases, summarize, purpose, and contract passes** (all skipped via the manifest). The `history` pass run-log entry still appears.
- Modifying exactly one source file and re-running indexes only that file's chunks; all other chunks remain untouched (verifiable by chunk `last_modified_at_version` staying at the previous value).
- Passing `--full` bypasses the manifest filter: every source is treated as dirty and every chunk is re-upserted, even when `last_indexed_version` matches `current_version`.
- For a code corpus that is a git repo, `corpus.last_indexed_version` after a successful run starts with `"git:"` and equals the HEAD commit's full OID.
- For a code corpus that is not a git repo (or a book or wiki corpus), `corpus.last_indexed_version` starts with `"v1-tree:"`.
- Chunks produced from a git-backed code corpus have `last_modified_commit_message` and `last_modified_author` populated for any chunk whose source file appeared in the diff of the run that touched it.
- If the pipeline errors mid-run (e.g. an LLM pass fails after chunk pass succeeds), `corpus.last_indexed_version` is **not** updated; the next run replays all dirty sources.
- A single-pass invocation like `calli index --pass semantic <corpus>` runs `Pass::History` for the manifest but treats the manifest as all-dirty inside the semantic pass (matching the historical behaviour of `--pass`).

### Non-functional (architectural / quality)

- `cargo test --workspace` passes.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- `cargo fmt --check` passes.
- No public API of `StorageBackend`, `SourceAdapter`, `IndexOptions`, `IndexResult`, `Corpus`, or `Chunk` is removed or has signatures changed in a breaking way. Additions only.
- The `change_detector.rs` / `reindex_pass.rs` / `watcher.rs` modules still compile and their existing tests still pass — Stage 0 lives **alongside** them in this wedge.
- The new SQLite migration applies cleanly to an empty DB and to a DB previously migrated through `008_kind_taxonomy.sql` (covered by adding a test that opens `data/callimachus.db` read-only in a tempfile-copy and runs the migrator).
- New trait methods on `SourceAdapter` have non-trivial default impls so the book and wiki adapters need no override and continue to compile without changes.
- PR description references the wedge title and includes a screenshot or paste of two consecutive `calli index` runs against `data/callimachus.db`, with the second showing zero processed chunks in downstream passes.
- Commit messages are scoped (`feat:`, `refactor:`, etc.) consistent with the recent history (see `ef3441b`, `33bd29c`).
- PR title: `feat: Stage 0 — history-aware change detection for incremental indexing`.

## Out of scope

- **Do not delete or rewrite** `change_detector.rs`, `reindex_pass.rs`, or `watcher.rs`. Reconciling the legacy `ChangeSet` path with `ChangeManifest` is a separate follow-up wedge — call it out in the PR description but do not do it here.
- **Do not** implement a commit-entity graph, co-change edges, or churn counts. The new `chunks.last_modified_commit_message` and `last_modified_author` columns are the only history surface.
- **Do not** implement hot/cold tiering, archive policies, or any retention sweeping.
- **Do not** add new corpus kinds or new adapters.
- **Do not** modify the LLM prompt logic, summarization templates, or contract extraction prompts.
- **Do not** change CLI flag surface beyond accepting `history` as a value of `--pass`. No new flags.
- **Do not** implement Postgres support for the new storage methods beyond stub `Err` returns — Postgres backend is unimplemented across the board (see `storage/postgres.rs:60+`).
- **Do not** change `corpus.pipeline_version` semantics. `last_indexed_version` is a new, independent column.
- **Do not** bump the workspace `pipeline_version` constant — that's reserved for changes that invalidate prior LLM output.

```yaml
suggested_config:
  cody:
    model: sonnet
    effort: high
    rationale: "Multi-crate Rust change touching trait surface, schema migration, pipeline orchestration, and every downstream pass; correctness across passes is load-bearing."
  redd:
    model: sonnet
    effort: high
    rationale: "Wedge defines new idempotency contract — incremental skip behaviour, version-write-only-on-success, and migration compatibility all need targeted tests."
  marty:
    model: sonnet
    effort: medium
    rationale: "Refactor pass should consolidate the duplicated manifest-filter snippet across seven downstream passes into a shared helper."
  perri:
    model: sonnet
    effort: high
    rationale: "Reviewer must catch subtle correctness issues — partial-failure rollback semantics, manifest filtering for code vs book locations, and trait default-impl behaviour."
```
