# Make the history-walk backfill diff-based: copy unchanged artifacts instead of re-deriving every commit

## Context

The history-walk backfill in `crates/callimachus-core/src/indexing/history_walk.rs`
populates the `*_history` tables with the derived state of a code corpus at each
commit on HEAD's first-parent ancestry. Both walkers — `walk_history_forward`
(forward from a start commit to HEAD) and `walk_history_backward` (backward from
HEAD's parent down to `--from <sha>`) — currently build a
`ChangeManifest::all_dirty(version)` at **every** commit
(history_walk.rs:134 for forward, history_walk.rs:324 for backward). `all_dirty`
marks every source file dirty, which forces every artifact (chunks, entities,
edges, summaries, purposes, contracts, blocks, themes) to be re-derived from
scratch via LLM calls at every commit.

Measured cost on the callimachus corpus itself: 2,279 entities × ~50 commits ≈
18 minutes/commit, ~12–15 hours for a single full `--passes default,theme`
backfill. The LLM-heavy passes (purpose, contract, summarize) dominate. This
makes full-fidelity backfill impractical on real codebases. The cost was never
caught earlier because validation used theme-only backfill (themes are
corpus-level — one LLM call per commit regardless of corpus size, so the
per-entity multiplier is invisible).

Between any two adjacent commits only a handful of files change. The artifacts
for unchanged files are byte-for-byte identical to the neighbour commit's
already-computed artifacts. We can **copy** them instead of recomputing them,
eliminating the redundant LLM work while preserving exactly the same `*_history`
table contents.

This plan also serves a second purpose: it adds a convergence test that proves
forward, backward, and middle-out walks produce identical history tables —
encoding the direction-independence property that motivates the whole history
feature.

Related: there is a known SEPARATE bug — HEAD-mode theme upsert does not archive
superseded themes (`~/.claude/bug-reports/head-mode-theme-archival-missing.md`).
That is **out of scope** here; this plan must not conflict with its eventual fix.

## The exact-SHA-match invariant (verified — design around this, do NOT change it)

The history query layer uses **exact SHA match**, not interval/range semantics.
See `crates/callimachus-core/src/storage/sqlite.rs:242`
(`entity_list_at_version`) and the `VirtualHead` query layer
(`crates/callimachus-core/src/storage/virtual_head.rs`). Reconstruction of
"state at commit N" is:

```sql
SELECT ... FROM entities          WHERE corpus_id=?1 AND derived_at_version=?2
UNION ALL
SELECT ... FROM entities_history  WHERE corpus_id=?1 AND derived_at_version=?2
```

i.e. **every artifact row that should be visible at commit N must carry
`derived_at_version` EXACTLY equal to N**. `superseded_at_version` is NOT used
for reconstruction (it is used only for pruning; see
`crates/callimachus-core/src/storage/pruning.rs`). The VirtualHead doc comment
(virtual_head.rs lines ~7–16) explicitly rejects timestamp-range queries because
the backward walk's `superseded_at` wall-clock timestamps come out
reverse-ordered relative to commit chronology.

**Consequence:** for commit N to be queryable, N needs a COMPLETE set of artifact
rows stamped with its exact SHA. That is WHY `all_dirty` is currently used — it
is the brute-force way to guarantee a complete stamped set. **The fix must
preserve this invariant** (every processed commit ends with a complete,
exact-SHA-stamped artifact set, in head or history tables) while eliminating the
redundant LLM recomputation.

## The fix: copy-not-recompute

Instead of re-deriving everything at each commit, each walker step:

1. **Diffs** the current commit against the **adjacent already-processed commit**
   (the "neighbour") to get the set of changed source-file paths.
2. Builds a `ChangeManifest::from_changed(version, changed)` marking **only** the
   changed files dirty — NOT `all_dirty`.
3. Runs the existing pipeline path for the dirty files: cascade-invalidate, then
   re-derive normally. This is the only LLM cost.
4. **Copies** the neighbour's already-computed artifact rows for every entity
   whose source file is UNCHANGED, re-inserting them stamped with
   `derived_at_version = currentSHA`. **Zero LLM calls.**

Entity births/deaths fall out of the diff automatically: a file added at C10 is
simply absent from the unchanged set when stepping back to C9, so its artifacts
are correctly NOT copied backward; symmetric for deletions going forward.

### Reusing existing machinery for the diff

The code adapter already implements precise git2 diffing. `SourceAdapter::changed_sources(source_path, from_version, to_version)`
(`crates/callimachus-core/src/adapter/contract.rs:247`; CodeAdapter override at
`crates/adapters/callimachus-adapter-code/src/adapter.rs:655`) calls
`git::diff_between` → `repo.diff_tree_to_tree`
(`crates/adapters/callimachus-adapter-code/src/git.rs:148`) when both versions
are `"git:<oid>"` strings. It returns `Vec<ChangedSource>` with `Added /
Modified / Deleted` kinds and per-file commit metadata.

**Important:** `changed_sources` operates on the real repo at `corpus.source`
(the `.git` dir), reading commit trees by OID — NOT on the materialised temp
tree. So the walker calls it against the **original** corpus source path with the
two `git:<oid>` version strings, independent of the per-commit temp dir. Do not
re-implement git2 diffing in the walker — reuse `pipeline.adapter.changed_sources(...)`.

### Determining "which entities map to a changed file"

The cascade machinery already maps dirty source paths → affected chunks →
affected entities. `ChangeManifest::is_dirty_for_chunk`
(change_manifest.rs:150) extracts the source file path from a chunk's location
URI; `cascade::run` (cascade.rs) collects dirty chunk IDs and calls
`cascade_delete_dirty_subtree`, which resolves chunks → entities via
`entities_at_location`. The COPY step is the dual: entities whose
`first_location_uri` / `last_location_uri` resolve to an UNCHANGED file are the
ones to copy. The simplest, most robust formulation is set subtraction at the
**entity** level (see algorithm below), avoiding a second URI-parsing surface.

## New storage trait methods

Add to `StorageBackend` (`crates/callimachus-core/src/storage/backend.rs`) a
single cascading copy method plus per-artifact-type building blocks. Implement
fully for `SqliteBackend`; `PostgresBackend` may `unimplemented!()`/error (it is
a compile-only stub today — confirm and match the existing pattern); and
`BackfillStorageWrapper` must satisfy the trait (delegating to inner, since copy
writes go to `*_history` on the real backend).

```rust
/// Statistics returned by `copy_unchanged_artifacts`.
#[derive(Debug, Default, Clone)]
pub struct CopyStats {
    pub entities_copied: u64,
    pub edges_copied: u64,
    pub purposes_copied: u64,
    pub contracts_copied: u64,
    pub blocks_copied: u64,
    pub summaries_copied: u64,
    pub chunks_copied: u64,
}

/// Copy every artifact row for `entity_ids` that is stamped
/// `derived_at_version = from_version` into the corresponding `*_history`
/// table, re-stamped with `derived_at_version = to_version` and
/// `superseded_at_version = superseded`. Reads `from_version` rows from BOTH
/// the head tables and `*_history` (so a backward walk's first step, whose
/// neighbour is HEAD, reads from head tables, and later steps read from
/// history). Also copies the chunks and summaries associated with those
/// entities/their locations. Idempotent: re-inserts use `INSERT OR IGNORE`
/// keyed on the existing history PKs, so re-running a backfill never
/// duplicates rows.
///
/// All inserts run inside a single write transaction.
fn copy_unchanged_artifacts(
    &self,
    corpus_id: &str,
    from_version: &str,
    to_version: &str,
    superseded_at_version: &str,
    entity_ids: &[String],
) -> Result<CopyStats>;
```

Implementation notes for `SqliteBackend::copy_unchanged_artifacts`:

- Use `INSERT OR IGNORE INTO <artifact>_history (...) SELECT <cols-with-rewritten-version> FROM (<head> UNION ALL <history>) WHERE derived_at_version = from_version AND <key> IN (entity_ids)`.
  This mirrors the existing `*_history_insert` column lists in sqlite.rs:770–1088
  and the head→history `SELECT` shape already used by `archive_themes_for_corpus`
  (sqlite.rs:643). Rewrite `derived_at_version → to_version` and set
  `superseded_at_version` / `superseded_at` in the projection.
- Chunks: `chunks_history` is keyed on `(id, introduced_at_version)` and uses
  `introduced_at_version` as its version anchor (NOT `derived_at_version` — see
  the existing `chunk_history_insert` at sqlite.rs:770 and backfill tests at
  history_walk.rs:1113). Copy chunks for unchanged files by re-stamping
  `introduced_at_version`/`last_modified_at_version = to_version`. Resolve "which
  chunks belong to unchanged entities" via the chunk location URI vs. the dirty
  path set (reuse `ChangeManifest`), not via entity IDs.
- Edges: copy edges whose `from_entity_id` OR `to_entity_id` is in `entity_ids`
  and whose `derived_at_version = from_version`. Edge identity for convergence is
  `(from_entity_id, to_entity_id, kind)`.
- Purposes/contracts are keyed `(entity_id, model)`; blocks and summaries by
  `entity_id` / `target_id`. Copy all rows for the unchanged entity set at
  `from_version`.
- The `*_history` tables already de-duplicate via their PK + `INSERT OR IGNORE`
  (see every `*_history_insert`), so the copy is idempotency-safe by construction.

If a single cascading method proves awkward in TDD, the implementer MAY instead
add per-type methods (`copy_entity_history`, `copy_edges_for_entities`, …) and
have the walker orchestrate them — the trait surface is the implementer's choice
as long as the convergence tests pass and the head tables stay untouched.

## Per-direction algorithm

Both directions copy unchanged artifacts FROM THE NEIGHBOUR THEY JUST PROCESSED.
The version of the neighbour is the `from_version` argument to
`copy_unchanged_artifacts`; the current commit is `to_version`.

### Forward walk (`walk_history_forward`)

Process commits oldest → newest (existing order, history_walk.rs:114).

- **First step (root / `--from` start):** no neighbour. Derive everything fresh
  — `ChangeManifest::all_dirty(version)` for this ONE commit only (existing
  behaviour). No copy.
- **Step i>0 at commit Cᵢ, neighbour Cᵢ₋₁ (older, just processed):**
  1. `changed = adapter.changed_sources(&corpus.source, Some("git:Cᵢ₋₁"), "git:Cᵢ")`.
  2. `manifest = ChangeManifest::from_changed("git:Cᵢ", changed)`.
  3. `cascade::run(db, corpus, &manifest)` then `pipeline.run(...)` with that
     manifest — re-derives only the changed files (existing path).
  4. Compute the **unchanged entity set**: entities present at Cᵢ₋₁ whose source
     file is NOT dirty in `manifest`. Get the Cᵢ₋₁ entity set from
     `entity_list_at_version(corpus_id, "git:Cᵢ₋₁")`, filter out those whose
     location resolves to a dirty path (`manifest.is_dirty`), and also filter out
     entities at files Deleted in `changed` (they must not carry forward).
  5. `copy_unchanged_artifacts(corpus_id, "git:Cᵢ₋₁", "git:Cᵢ", superseded="git:Cᵢ", unchanged_ids)`.
     (Forward supersession of the OLDER row: when Cᵢ₋₁'s rows were written they
     had no successor yet; the forward pipeline's normal archive path stamps
     `superseded_at` as it goes. Match the supersession semantics the existing
     forward all_dirty path produces — verify against the current
     `walk_short_history_populates_history_tables` test expectations and keep them
     green.)
  6. `corpus_set_last_indexed_version("git:Cᵢ")` (existing, head tables — forward
     walk legitimately advances HEAD).

Forward walk writes through the **real** backend (not the wrapper); the final
commit (HEAD) lands in the head tables, intermediate commits in `*_history` via
the existing archive-on-supersede path. Preserve that: the copy step for forward
must land unchanged-entity rows in `*_history` stamped at the intermediate SHA,
exactly as `all_dirty` does today.

### Backward walk (`walk_history_backward`)

Process commits newest-older → oldest-older (existing order,
history_walk.rs:251–259). Writes go through `BackfillStorageWrapper` →
`*_history` only; head tables never touched.

- **First step (i=0), neighbour = HEAD:** the neighbour's rows live in the HEAD
  tables (`entities`, `chunks`, …), NOT in `*_history`. `copy_unchanged_artifacts`
  must read `from_version = "git:HEAD"` from the head tables (its UNION ALL over
  head + history handles this). Diff is
  `adapter.changed_sources(&corpus.source, Some("git:HEAD"), "git:Cᵢ")`.
- **Step i>0 at Cᵢ, neighbour = Cᵢ₊₁ (newer, just processed):** neighbour's rows
  are in `*_history` at `derived_at_version = "git:Cᵢ₊₁"`. Diff
  `Some("git:Cᵢ₊₁") → "git:Cᵢ"`.
- For each step:
  1. `changed = adapter.changed_sources(...)` as above.
  2. `manifest = ChangeManifest::from_changed("git:Cᵢ", changed)` (replaces the
     `all_dirty` at history_walk.rs:324). Keep `mode = HistoryBackfill`, the
     `VirtualHead` read_view, and the `BackfillStorageWrapper` exactly as today.
  3. Run the pipeline → re-derives only dirty files into `*_history` at Cᵢ.
  4. Unchanged entity set = entities at the neighbour (`entity_list_at_version`
     at the neighbour version) minus those on dirty paths, minus those at files
     Added in `changed` (an Added-going-newer file means it did NOT exist at the
     older Cᵢ, so do not copy it backward).
  5. `copy_unchanged_artifacts(corpus_id, neighbour_version, "git:Cᵢ", superseded=neighbour_version, unchanged_ids)`.
     The supersession target for a backward-copied row is the neighbour
     (next-newer) version — consistent with `BackfillSupersession`'s existing
     semantics (backfill.rs:294–299, history_walk.rs:294). When the copy writes
     through the wrapper, route its supersession through the same
     `BackfillSupersession` so `record_write_*` keeps the chain coherent for the
     next older step. (Simplest: have the wrapper's `copy_unchanged_artifacts`
     delegate to inner with `superseded = supersession.superseded_for_*` per row,
     then `record_write_*`. Confirm by reading backfill.rs:331–520.)

### Direction symmetry summary

| | Forward at Cᵢ | Backward at Cᵢ |
|---|---|---|
| neighbour | Cᵢ₋₁ (older) | Cᵢ₊₁ (newer); HEAD on first step |
| diff | `Cᵢ₋₁ → Cᵢ` | `Cᵢ₊₁ → Cᵢ` |
| Added files | new entities derived fresh | excluded from copy (didn't exist at Cᵢ) |
| Deleted files | excluded from copy (gone at Cᵢ) | entities derived fresh at Cᵢ |
| first step | root: all_dirty, no copy | neighbour=HEAD reads head tables |

## Theme policy

Themes are corpus-level (one LLM call per commit regardless of corpus size), so
the per-entity multiplier does not apply — but recomputing themes at a commit
where nothing changed is still wasted work and risks LLM nondeterminism in the
convergence test. Policy:

- **At least one file changed at Cᵢ:** re-run the theme pass once (existing
  `theme_pass::run`, one LLM call). It already short-circuits when the manifest
  reports no dirty sources (theme_pass.rs:35–42), so simply supplying a
  `from_changed` manifest with ≥1 dirty path triggers it; an empty manifest skips
  it. **No code change needed in theme_pass for the changed case.**
- **No files changed at Cᵢ (empty diff):** copy the neighbour's themes
  (and their theme-entity rows + `upheld_by`/`violated_by` edges) re-stamped to
  Cᵢ via `copy_unchanged_artifacts` (themes are part of the cascading copy:
  include `themes` and the `kind=theme` entity rows + theme edges in the copy
  set). Zero LLM calls.

Read `theme_pass.rs` to confirm the skip condition and that theme rows carry
`derived_at_version = manifest.current_version` (theme_pass.rs:69–101) so the
exact-SHA invariant holds for themes too. Do NOT touch the head-mode theme
archival path (the separate out-of-scope bug).

## Convergence-test fixture design (headline)

Build a small fixture git repo **inside the test** (extend the existing
`build_linear_repo` helper at history_walk.rs:584, or add a richer builder).
Requirements:

- ~6–10 `.txt` source files, ~10 commits, including: file **adds**, file
  **edits** (overwrite content), and at least one **rename** (git rename =
  delete old path + add new path in the tree; build it explicitly with the index
  like the `backfill_artifact_missing_from_head` test at history_walk.rs:1259).
- A deterministic adapter (reuse the in-test `SimpleAdapter` pattern) and the
  `DryRunProvider` LLM (`callimachus-llm` — `DryRunProvider::new()`, already used
  throughout these tests) so all derived artifacts are deterministic and the
  comparison is exact, modulo nothing.

The test runs THREE backfills of the same fixture into THREE fresh in-memory
SQLite DBs:

- **(a) forward** from root to HEAD (`walk_history_forward`).
- **(b) backward** from HEAD down to root (`walk_history_backward`, after a
  HEAD-only ingest to set `last_indexed_version`, as `setup_ingested_corpus`
  does at history_walk.rs:948).
- **(c) middle-out**: forward from some mid commit Cₖ to HEAD, then backward from
  Cₖ₋₁ to root. (If a single-call middle-out entry point does not exist, compose
  it from the two existing walkers; document the composition in the test.)

Then assert the deterministic content of the `*_history` tables (plus head
tables for the forward/HEAD state) is IDENTICAL across (a), (b), (c):

- **chunks**: set of `(id, introduced_at_version, content, source_hash)` rows.
- **entities**: set of `(id, derived_at_version, canonical_name, kind)` rows.
- **edges**: set of `(from_entity_id, to_entity_id, kind, derived_at_version)`.
- **per-SHA row sets**: for every commit SHA, the set of artifact rows stamped
  exactly that SHA (via `entity_list_at_version` and equivalent direct SQL for
  chunks/edges) must match across the three walks.

Comparison should normalise away non-deterministic columns (`superseded_at`
wall-clock timestamps, any `generated_at`/`created_at` timestamps, autogenerated
UUIDs where they exist — note theme `upheld_by`/`violated_by` edges use
`Uuid::new_v4()` at theme_pass.rs:113; either exclude theme edges from the
strict-id comparison or compare them by `(from,to,kind)` only). Document exactly
which columns are excluded and why.

## Redd-first test list

Write these as failing tests first, then implement until green.

1. **`copy_unchanged_artifacts` re-stamps version** — seed an entity (+purpose,
   contract, block, edge, summary, chunk) at `git:v1`; copy to `git:v2`; assert
   each `*_history` table has a row at `derived_at_version=git:v2` with identical
   content and `superseded_at_version` set as requested.
2. **copy reads from head tables** (backward first-step case) — seed at head
   `git:HEAD` only; `copy_unchanged_artifacts(from="git:HEAD", to="git:C")`;
   assert history rows appear at `git:C` and the head tables are unchanged.
3. **copy is idempotent** — call `copy_unchanged_artifacts` twice with identical
   args; assert no duplicate `*_history` rows (row counts unchanged after the
   second call).
4. **forward walk does diff-based work** — 3-commit linear repo, instrument the
   adapter to count `extract_with_llm`/`summarize` invocations; assert the second
   and third commits invoke the LLM only for the file(s) that changed, not all
   files. (Adapt the existing `walk_short_history_populates_history_tables` test.)
5. **forward walk history tables unchanged vs. old all_dirty behaviour** — the
   existing `walk_short_history_populates_history_tables`,
   `backfill_supersession_chain_correct`, `backfill_artifact_missing_from_head`,
   `backfill_head_untouched`, and `backfill_reverse_chronological_order` tests
   MUST stay green (they encode the row-set/supersession invariants).
6. **backward first step reads HEAD** — single-edit-between-HEAD-and-parent repo;
   backfill; assert the parent commit's unchanged entities are copied from the
   head tables and stamped at the parent SHA.
7. **rename across a commit** — file `a.txt` renamed to `b.txt` at Cᵢ; assert the
   old-path entity is absent at Cᵢ and the new-path entity is present, in both
   forward and backward walks.
8. **theme copy when nothing changed** — a commit whose diff is empty relative to
   its neighbour (e.g. a commit that only touches a non-source file) copies the
   neighbour's themes re-stamped, with zero theme-extraction LLM calls.
9. **CONVERGENCE (headline)** — forward == backward == middle-out, per the
   fixture design above.

## Approach (ordered implementation sequence)

1. **Storage trait + SQLite copy method.** Add `CopyStats` and
   `copy_unchanged_artifacts` to `backend.rs`. Implement in `sqlite.rs` using
   `INSERT OR IGNORE ... SELECT` per artifact type inside one `with_write_tx`.
   Stub/error in `postgres.rs` to match the existing stub pattern. Land tests
   1–3 first (Redd).
2. **BackfillStorageWrapper support.** Implement `copy_unchanged_artifacts` on
   the wrapper (backfill.rs) so it routes copy writes to `*_history` via the
   inner backend and threads supersession through `BackfillSupersession`
   (`superseded_for_*` + `record_write_*`). Keep head tables untouched.
3. **Forward walker.** Replace the `all_dirty` at history_walk.rs:134 with: first
   commit → `all_dirty` (unchanged); subsequent commits → `from_changed` manifest
   from `adapter.changed_sources(neighbour, current)` + a `copy_unchanged_artifacts`
   call for the unchanged entity set. Keep `corpus_set_last_indexed_version` and
   the HEAD-unchanged debug assertion. Land tests 4, 5, 7.
4. **Backward walker.** Replace the `all_dirty` at history_walk.rs:324 with the
   `from_changed` manifest + copy, handling the i=0 (neighbour=HEAD) case
   explicitly. Keep `mode = HistoryBackfill`, the `VirtualHead` read_view, and the
   wrapper. Land tests 6, 7 (backward direction).
5. **Theme policy.** Confirm the changed-case path already triggers theme_pass
   via a non-empty manifest; wire themes into `copy_unchanged_artifacts` for the
   no-change case. Land test 8.
6. **Convergence test.** Build the richer fixture (adds/edits/rename), run the
   three walks into three DBs, assert identical normalised history. Land test 9.
7. **Run the full suite** and `cargo clippy` clean.

## Acceptance criteria

- `cargo test -p callimachus-core` passes, including the new convergence test
  (forward == backward == middle-out, normalised).
- All pre-existing history/backfill tests in history_walk.rs and virtual_head.rs
  remain green (no regression to the exact-SHA-match query semantics).
- Forward and backward walkers no longer call `ChangeManifest::all_dirty` except
  for the forward walk's first (root/`--from`) commit. A test asserts
  diff-scoped LLM invocation (test 4).
- For every processed commit N, a complete exact-SHA-stamped artifact set exists
  for N (in head or history tables) — verified by the per-SHA row-set assertions
  in the convergence test.
- Backward backfill leaves the head tables and `corpora.last_indexed_version`
  untouched (existing `backfill_head_untouched` test stays green).
- Re-running a backfill produces no duplicate `*_history` rows (idempotency test 3).
- `cargo clippy --all-targets` is clean; no new `unwrap()` in non-test code paths
  beyond what the surrounding code already uses.
- The `*_history` schema and the `entity_list_at_version` / `VirtualHead`
  query SQL are UNCHANGED (diff touches walker + storage write methods only).
- PR body references this plan and explains the copy-not-recompute design in 2–3
  sentences.

## Out of scope

- Do NOT change the exact-SHA-match query semantics or the `*_history` schema.
  The fix is in the walker + new copy write methods only.
- Do NOT modify normal HEAD-mode incremental indexing (`calli index` /
  `calli reindex`) — it already does correct diff-based work. Reuse its
  machinery (`adapter.changed_sources`, `ChangeManifest`, `cascade`), don't fork
  it.
- Do NOT fix the separate HEAD-mode theme-archival bug
  (`~/.claude/bug-reports/head-mode-theme-archival-missing.md`). Just don't
  conflict with its eventual fix.
- Do NOT change the CLI surface (`calli history backfill`, `--from`, `--back`,
  `--passes`). Behaviour is identical; only the per-commit work shrinks.
- Do NOT implement `copy_unchanged_artifacts` fully on `PostgresBackend` (it is a
  compile-only stub today — match the existing unimplemented/error pattern).
- Do NOT add a real LLM provider to any test — all tests use `DryRunProvider` for
  determinism.
- No rename-detection heuristics beyond what git2's default diff reports
  (treat git tree add+delete as add+delete; the convergence test only requires
  the resulting row sets to match across directions).

```yaml
suggested_config:
  cody:
    model: opus
    effort: high
    rationale: "Correctness-critical change to fragile dual-direction history walker + new transactional storage methods; subtle SHA-stamping/supersession invariants."
  redd:
    model: sonnet
    effort: high
    rationale: "Convergence test is the proof of correctness; must construct a git fixture with rename and assert normalised three-way table equality."
  marty:
    model: sonnet
    effort: medium
    rationale: "Consolidate forward/backward shared copy-step logic into one helper after both directions work."
  perri:
    model: sonnet
    effort: high
    rationale: "Reviewer must verify the exact-SHA invariant and head-table-untouched guarantee are preserved; a missed bug corrupts all historical queries."
```
