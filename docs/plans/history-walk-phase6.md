# Phase 6 — Virtual-head query layer for backward backfill

## Context

Callimachus' `calli history backfill` walks a corpus's git history backward,
re-running selected indexing passes against each commit's source tree so that
`*_history` mirror tables accumulate per-commit derived state. Five phases of
this work have landed on `main` (see PRs #27–#32, latest commit `ee410cc`),
giving us provenance columns (`derived_at_version`, `superseded_at_version`,
`superseded_at`), forward + backward walking, time-limited backfill, prune,
and `--passes <list>` filtering.

QA on Phase 5 surfaced a real architectural limit. Running
`calli history backfill webster --back 5 --passes theme` walks five commits
without error and the `--passes` filter correctly restricts to theme — but
theme produces **zero output** (`processed=0 skipped=1` at every iteration).
The temp-tree mechanism rewinds the **source tree** to each iteration's
commit, but **head tables stay frozen at HEAD**. Theme reads its inputs
(`entities`, `entity_contracts`, `summaries`) from head, sees the same state
on every iteration, and either skips entirely or stamps HEAD's themes with
the wrong SHA. Any pass that reads *derived* state rather than source files
has the same defect — confirmed for theme, likely for aliases and parts of
summarize.

Phase 6 closes the gap by introducing a **virtual-head query layer**: given a
target SHA during backfill, present the corpus's derived state AS-IT-WAS at
that SHA by overlaying `*_history` rows on top of head, filtered to
"active at target SHA." Derived-state-reading passes read through this layer
when `IndexMode::HistoryBackfill { .. }` is active; source-file-reading
passes (chunk, structure, semantic, purpose, contract) are unaffected.

## Target

- **Repo:** callimachus
- **Branch:** `feature/history-walk-phase6` (EXACT — do not append a topic suffix; Phase 2 hit `pr_url_branch_mismatch` because the branch was renamed with a suffix)
- **Base:** `origin/main` (at `ee410cc`)

## Files to change

- **NEW** `crates/callimachus-core/src/storage/virtual_head.rs` — `VirtualHead<'a>` struct, cutoff-timestamp resolver, per-table read methods.
- `crates/callimachus-core/src/storage/mod.rs` — `pub mod virtual_head;` plus re-export of `VirtualHead`.
- `crates/callimachus-core/src/storage/backfill.rs` — construct `VirtualHead` inside the backward walk and surface it via the read view.
- `crates/callimachus-core/src/indexing/pipeline.rs` — define `ReadView<'a>` (or equivalent) enum, plumb it through `IndexOptions` / per-pass entrypoints.
- `crates/callimachus-core/src/indexing/theme_pass.rs` — switch reads from `db.entity_list(..)` / `db.contract_list(..)` / `db.summary_list(..)` to `read_view.entity_list()` etc.
- `crates/callimachus-core/src/indexing/aliases_pass.rs` — same dispatch switch (best-effort; ship if mechanical).
- `crates/callimachus-core/src/indexing/summarize_pass.rs` — same dispatch switch on the head-reading code paths only (best-effort).
- `crates/callimachus-cli/src/commands/history.rs` — no signature change expected; verify it still compiles after pipeline changes.
- **NEW tests** colocated in `virtual_head.rs` (`#[cfg(test)] mod tests { .. }`) and in `theme_pass.rs` for the end-to-end case.

Read these files first to confirm exact symbol names and call sites before editing — names above are best-effort from the brief, not verified line-by-line.

## Approach

1. **Read the foundation.** Open `storage/history.rs`, `storage/backfill.rs`, `indexing/pipeline.rs`, and `indexing/theme_pass.rs`. Confirm the actual signatures of `entity_list`, `purpose_list`, `contract_list`, `summary_list`, `edge_list` on the storage backend, the shape of `IndexOptions` / `IndexMode`, and how `history_walk_backward` (or equivalent) constructs the per-iteration context today. Capture the artifact-key for each `*_history` table from the schema migrations (entities=`id`, edges=`id`, entity_purposes=`(entity_id, model)`, entity_contracts=`(entity_id, model)`, entity_blocks=`id`, summaries=`(target_kind, target_id, model)`, themes=`id`, chunks=`id`).

2. **Build `VirtualHead<'a>`.** New module `storage/virtual_head.rs`:
   - Struct holds `db: &'a dyn StorageBackend`, `corpus_id: String`, `target_sha: String`, `cutoff_timestamp: String` (resolved at construction).
   - `pub fn new(db, corpus_id, target_sha) -> Result<Self>`. Cutoff resolution: query any one of the `*_history` tables for a row with `superseded_at_version = target_sha`, take its `superseded_at`. If no such row exists (target SHA is current HEAD or never a supersession point), cutoff = "now" (i.e., `chrono::Utc::now().to_rfc3339()` or sentinel `9999-...` — pick whichever is consistent with how `superseded_at` is stored elsewhere; check `history.rs` for the convention).
   - Per-table read methods returning `Result<Vec<T>>`. Each implements the UNION ALL pattern:
     ```sql
     -- head rows still active at cutoff
     SELECT * FROM <table>
       WHERE corpus_id = ?
         AND generated_at <= ?cutoff      -- or created_at; per-table column
         AND <artifact_key> NOT IN (
           SELECT <artifact_key> FROM <table>_history
             WHERE corpus_id = ?
               AND superseded_at <= ?cutoff
         )
     UNION ALL
     -- history rows whose active window contains cutoff
     SELECT * FROM <table>_history
       WHERE corpus_id = ?
         AND generated_at <= ?cutoff      -- "derived by then"
         AND superseded_at > ?cutoff;     -- "not yet superseded at cutoff"
     ```
     Use timestamp comparison (`generated_at`, `superseded_at`), NOT SHA-string comparison — SHAs are alphabetical, not chronological.
   - Methods to implement, matching the storage-backend reads used by theme/aliases/summarize: `entity_list`, `purpose_list`, `contract_list`, `summary_list`, `edge_list`, and any others discovered in step 1 (e.g., `block_list`, `alias_list`). Skip ones no derived-state-reading pass consumes.

3. **Define `ReadView<'a>` in `indexing/pipeline.rs`.**
   ```rust
   pub enum ReadView<'a> {
       Head(&'a dyn StorageBackend),
       VirtualHead(VirtualHead<'a>),
   }
   impl<'a> ReadView<'a> {
       pub fn entity_list(&self, corpus_id: &str) -> Result<Vec<Entity>> { ... }
       // ...one per VirtualHead read method
   }
   ```
   Dispatch: `Head(db) => db.entity_list(corpus_id)`, `VirtualHead(vh) => vh.entity_list()` (corpus_id already embedded). Pass `ReadView<'_>` into each pass's entry function (or attach to `IndexOptions` if that's the existing plumbing pattern — confirm in step 1).

4. **Wire `IndexMode::HistoryBackfill` to construct `VirtualHead`.** In `storage/backfill.rs` (or wherever the backward walk calls into the indexing pipeline per iteration), at the start of each iteration after the temp tree is rewound: construct `VirtualHead::new(db, corpus_id, iteration_sha)?` and pass it as `ReadView::VirtualHead(..)` to the passes. All other `IndexMode::*` variants pass `ReadView::Head(db)`.

5. **Migrate theme_pass.** In `indexing/theme_pass.rs`, replace every head-table read with the equivalent `read_view.<method>(..)` call. The signature change is mechanical; confirm callers still compile.

6. **Migrate aliases_pass and summarize_pass (best-effort).** Same mechanical switch. If summarize has paths that legitimately read source files (file content) versus derived state (entity descriptions), only switch the derived-state reads. If either turns out to be non-mechanical, leave it for a fast-follow and note it in the PR body.

7. **Verify the headline use case manually.** After implementation:
   ```bash
   cargo build --release
   ./target/release/calli history backfill webster --back 5 --passes theme
   ```
   Expect: `processed > 0` per iteration; query `themes_history` and confirm 5 distinct `superseded_at_version` values with distinct theme content.

8. **Write tests** (see Tests section).

9. **Run the full check matrix** (see Acceptance criteria).

## Tests

Add to `virtual_head.rs`:

- `active_at_includes_unsuperseded_head_rows` — fixture with one entity in head, `generated_at = T_A`; construct VirtualHead at cutoff `T_B > T_A`; expect entity returned.
- `active_at_excludes_superseded_rows` — entity has a history row with `superseded_at = T_B`; VirtualHead at cutoff `T_A < T_B` returns the history-version data, not head; VirtualHead at cutoff `T_C > T_B` returns head.
- `active_at_returns_history_version_when_superseded_after_target` — history row with `generated_at = T_A`, `superseded_at = T_C`; query at cutoff `T_B` (between A and C); expect history-row data, not head.
- `cutoff_timestamp_resolution` — seed a `*_history` row with `superseded_at_version = 'git:abc'` and `superseded_at = '2026-01-01T00:00:00Z'`; `VirtualHead::new(.., "abc")` resolves cutoff to that timestamp.
- `cutoff_when_target_is_current_head` — VirtualHead constructed with a SHA that never appears as `superseded_at_version` in any history table; cutoff resolves to "now" sentinel; all rows returned are head.

Add to `theme_pass.rs` (or a sibling integration test file):

- `backfill_produces_themes_per_commit` — fixture corpus with 3 commits, each having materially different entity state in head + history; run backfill with `--back 3 --passes theme`; assert `themes_history` has 3 distinct `superseded_at_version` values and the theme content differs between rows.

Confirm no existing tests regress. Pay special attention to single-snapshot `calli index <corpus> --pass theme` tests — they must still pass unchanged (the `ReadView::Head` path is the default).

## Acceptance criteria

- `cargo build --workspace` succeeds.
- `cargo test --workspace` passes, including all new tests above.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo fmt --all --check` clean.
- **Theme backfill produces real output:** on a fixture with N commits,
  `calli history backfill <corpus> --back N --passes theme` populates
  `themes_history` with N distinct `superseded_at_version` values and the
  theme content per commit reflects that commit's entity state (not HEAD's).
- **No regression on the head path:** `calli index <corpus> --pass theme`
  produces identical output to its pre-change behaviour on a single-snapshot
  corpus.
- Branch name is exactly `feature/history-walk-phase6` — no topic suffix.
- PR body cites this plan and the Phase 5 QA finding that motivated it.

## Out of scope

- Cross-corpus virtual head.
- Caching virtual-head results between iterations (each iteration's target SHA differs, so no reuse).
- Exposing virtual-head reads outside backfill mode — current callers all want head-direct.
- Persisting cutoff-timestamp resolution in `corpora` for fast lookup (per-construction resolution is cheap).
- Migrating any pass beyond theme + aliases + summarize. If aliases/summarize migration proves non-mechanical, leave for a fast-follow and note in PR body; theme is the must-ship.

```yaml
suggested_config:
  cody:
    model: sonnet
    effort: high
    rationale: "Correctness-critical SQL on the cutoff-timestamp predicate; subtle UNION ALL semantics across many *_history tables."
  redd:
    model: sonnet
    effort: high
    rationale: "Temporal semantics need exhaustive boundary coverage (active-at-cutoff edges, supersession windows); tests are the contract."
  marty:
    model: sonnet
    effort: medium
    rationale: "Standard refactor pass to consolidate ReadView dispatch patterns across the migrated passes."
  perri:
    model: sonnet
    effort: high
    rationale: "Reviewer must catch any pass still reading head-direct during backfill mode; silent failures here are easy to miss."
```
