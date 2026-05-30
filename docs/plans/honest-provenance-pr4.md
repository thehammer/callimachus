# PR 4 — Embeddings history + tombstones + CLI `migrate-fresh`

**Scope:** the fourth of 5 PRs implementing the honest-provenance refactor.
After PR 3's Layer-2 cache lands, this PR wires up the remaining runtime
behaviour: embeddings get full provenance + history via the history layer,
tombstones are written and consulted on reads, and a CLI command exists for
the fresh-start migration path.

**Source plan:** `docs/plans/honest-provenance-implementation.md`.
**PRD:** `docs/plans/honest-provenance.md`.
**Predecessors (merged):** PR #36 (substrate), PR #38 (history layer + walker rewrite), the PR-3 cache/stable-sampling PR (just landed).

Read both planning docs before starting. This file scopes the implementation
work.

## Goal of PR 4

1. **Wire embed_pass through `history_layer`** so embeddings get the same
   provenance + supersession treatment as other artifacts. Resolves the
   fourth open bug (`embeddings-no-history-archival`).
2. **Make tombstones real at write-time** — the history layer writes
   tombstone rows when entities/chunks/edges are removed by a diff, not
   just when artifacts are superseded.
3. **Make tombstones real at read-time** — the `entity_list_at_sha` (and
   sibling) queries consult tombstones to exclude entities that died
   before the target SHA. Today this lookup ignores tombstones.
4. **Ship `calli history migrate-fresh`** — the documented migration
   path. Wipes existing pinakes content (preserving the corpora row +
   `last_indexed_version` snapshot, optionally) and prompts the user to
   rebuild from scratch.

## Concrete deliverables

1. **Embed pass through history layer:**
   `crates/callimachus-core/src/indexing/embed_pass.rs`.

   - When a chunk gets a new content_hash (or is freshly introduced),
     the embed pass requests an embedding from the provider.
   - The result is written via `history_layer::commit_embedding(...)`
     (new method) which stamps `Provenance` and inserts a row in
     `embeddings`. Prior embeddings for the same `(chunk_id, model)`
     pair are moved to `embeddings_history` with appropriate
     `superseded_at_sha`.
   - Combined with PR 3's `layer2_cache_get` consultation, the actual
     embedding provider should rarely be called for unchanged chunks.

2. **Tombstone writes in `history_layer`:**
   `crates/callimachus-core/src/indexing/history_layer.rs`.

   - When `commit(...)` runs and the diff includes entity/chunk/edge
     deletions, write `artifact_tombstones` rows with
     `(kind, corpus_id, id, died_at_sha)`.
   - One row per death event per artifact. Multiple deaths over a
     corpus's history accumulate.

3. **Tombstone-aware reads:**
   `crates/callimachus-core/src/storage/sqlite.rs` and `postgres.rs`.

   - Refactor `entity_list_at_sha` (and `chunk_list_at_sha`,
     `edge_list_at_sha` where present) to exclude artifacts whose
     tombstone `died_at_sha` is an ancestor of the target SHA.
   - Helper: `is_tombstoned_at(corpus_id, id, kind, target_sha)`
     checks whether a tombstone exists with `died_at_sha` that is
     an ancestor of `target_sha`. The ancestor check needs git
     access; the storage backend can take an optional `AncestryReader`
     trait that wraps git2 lookups.
   - For corpora without an attached git repo (book/wiki adapters):
     fall back to literal SHA equality (or a configurable resolver).
     Document the behaviour.

4. **`calli history migrate-fresh` subcommand:**
   `crates/callimachus-cli/src/commands/history.rs` (or wherever
   `calli history *` lives).

   - Wipes all `*_history` rows + corpus head-table rows + layer2_cache
     entries + tombstones for the named corpus.
   - Resets `corpora.backfill_cursor = NULL` and
     `corpora.last_indexed_version = NULL`.
   - Preserves the `corpora` row itself (so the corpus stays registered).
   - Prints a confirmation banner reminding the user that the next
     `calli index` will be a from-scratch rebuild against current HEAD.
   - Accept `--corpus <id>` (required) and `--yes` to skip the
     "are you sure?" prompt.

## Tests (Redd-first TDD)

- **Embedding supersession test** (new):
  - Build a fixture with a chunk at C1, modify its content at C2.
  - Run embed pass at C1, then at C2.
  - Assert `embeddings` head row at C2 has the C2 vector; `embeddings_history`
    has the C1 vector with `superseded_at_sha = C2`.

- **Tombstone write test** (new):
  - Fixture with entity at C1, removed at C2 (delete the function).
  - Walk forward C1 → C2; assert `artifact_tombstones` has a row
    `(kind="entity", id=<entity_id>, died_at_sha = C2)`.

- **Tombstone-aware read test** (new):
  - Same fixture as above.
  - Query `entity_list_at_sha(C1)` — entity should appear.
  - Query `entity_list_at_sha(C2)` — entity should NOT appear.
  - Query `entity_list_at_sha(C3)` where C3 is descendant of C2 —
    entity should NOT appear (tombstone is ancestral).

- **`migrate-fresh` round-trip test** (new):
  - Build a pinakes with corpus + some data + some history.
  - Run `migrate-fresh --corpus X --yes`.
  - Assert: all `*_history` empty for X, head tables empty for X,
    corpora row preserved for X, `backfill_cursor = NULL`,
    `last_indexed_version = NULL`.
  - Run `calli index X` afterwards — should succeed and rebuild fresh.

- **Existing tests stay green.** Particularly the PR 2 regression tests
  and the PR 3 cache-hit tests.

## Non-deliverables (explicitly defer)

- Convergence test (full 4-path A/B/M/REF with live LLM) — PR 5.
- Dropping `derived_at_version TEXT` columns — PR 5.
- Documentation pass (CLAUDE.md updates, README updates) — PR 5.

## Acceptance criteria

- **Run `cargo fmt` and `cargo clippy --workspace --all-targets -- -D warnings` before opening the PR.** PR 1 and PR 3 both hit CI failures on un-fmt'd new test files; don't make it three in a row.
- `cargo build --release` succeeds.
- `cargo test --workspace` passes — including the four new tests above.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `calli history migrate-fresh --help` works.
- The PR description documents:
  - The embed pass is now history-aware (resolves the fourth bug).
  - Tombstones are written by the walker/cascade and consulted by reads.
  - The `migrate-fresh` workflow with an example invocation.

## Routing config

```yaml
suggested_config:
  cody:
    model: opus
    effort: high
    rationale: "Three runtime wires (embeddings, tombstone writes, tombstone reads) + a destructive CLI command. Tombstone-aware reads need ancestry checks via git2, which adds non-trivial cross-module coupling."
  redd:
    model: sonnet
    effort: high
    rationale: "Four new tests cover the runtime behaviours. The migrate-fresh test is destructive and must verify nothing escapes the wipe; the tombstone-aware-read test needs a git fixture with a deletion."
  perri:
    model: sonnet
    effort: medium
    rationale: "Reviewer must verify migrate-fresh preserves the corpora row, embed_pass routes through history_layer (not direct storage writes), and tombstone reads use ancestry not literal equality."
  marty:
    model: sonnet
    effort: medium
    rationale: "Optional consolidation: the three new `_list_at_sha` tombstone-aware queries share a pattern that could become a helper."
```

## References

- Full implementation plan: `docs/plans/honest-provenance-implementation.md`
- PRD: `docs/plans/honest-provenance.md`
- Bug resolved by this PR: `.claude/bugs/open/embeddings-no-history-archival.md`
- PR 2's `history_layer` module is the substrate for this PR's `commit_embedding` and tombstone writes.
