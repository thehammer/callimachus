# PR 3 — Layer-2 cache + stable-sampling + file-shape hash

**Scope:** the third of 5 PRs implementing the honest-provenance refactor.
This PR makes the LLM-derived artifacts (purposes, contracts, summaries,
themes, embeddings) **content-cacheable** instead of re-derived on every
indexing pass, and adds opt-in stable-sampling for determinism within a
fixed file shape.

**Source plan:** `docs/plans/honest-provenance-implementation.md`.
**PRD:** `docs/plans/honest-provenance.md`.
**Predecessors (merged):** PR #36 (substrate), the PR-2 walker rewrite (just landed).

Read both planning docs before starting. This file scopes the implementation
work.

## Goal of PR 3

Stop re-deriving Layer-2 artifacts every time the cascade marks their
parent file dirty. The empirical finding that motivates this (toy
experiments, 2026-05-29):

- Six LLM purpose calls for `greet()` produced six different prose
  outputs across A, B, M, and three runs of the same fixture — despite
  `greet`'s bytes being byte-identical at C1 and unchanged at C2/C3.
- Each `calli index --pass all` against a "dirty" file triggers fresh
  re-derivation, even when the chunk's content hash is unchanged.
- At callimachus scale (2,279 entities × 50 commits), this is the
  difference between $200 and $5 in API spend on a backfill.

The PR 1 substrate created a `layer2_cache` table. This PR wires it
through the five Layer-2 passes and adds the surrounding-context-hash
that empirically gates cache validity (per the context-leak finding —
the same chunk content produces different purposes depending on what
else is in the file).

## Concrete deliverables

1. **`LlmProvider` trait extension** in `crates/callimachus-llm/src/`.
   - Add optional `temperature: Option<f32>` and `seed: Option<u64>`
     params to the chat/completion call surface.
   - `AnthropicApiProvider`: pass `temperature` through to the API
     request (Anthropic supports `temperature`; seed support varies).
   - `DryRunProvider`: honour `temperature=0` deterministically; record
     the seed in its outputs for test reproducibility.
   - OpenAI embed provider: pass through where the API supports it.

2. **Layer-2 cache plumbing** across the five passes:
   `crates/callimachus-core/src/indexing/{purpose,contract,summarize,theme,embed}_pass.rs`.
   - Before issuing an LLM call: compute the cache key from
     `(Layer-1 identity, file_shape_hash, model)`. The exact shape:
     - `entity_purposes` / `entity_contracts`: key is
       `(corpus_id, entity_id, file_shape_hash, model)`.
     - `summaries`: `(corpus_id, target_kind, target_id, file_shape_hash, model)`.
     - `themes`: `(corpus_id, theme_title_slug, corpus_entity_set_hash, model)`.
     - `embeddings`: `(corpus_id, chunk_id, model)` — chunks are
       content-addressable, no file-shape context needed.
   - Call `layer2_cache_get(key)`. If hit, use the cached value, skip
     the LLM call entirely.
   - If miss, make the LLM call, then `layer2_cache_put(key, value)`.

3. **`file_shape_hash` computation** in the structure pass
   (`crates/callimachus-core/src/indexing/structure_pass.rs`).
   - Defined as a stable hash of the entity-id-list at the file level:
     `sha256_hex(sort(entity_ids_in_file))`. The hash changes if any
     entity is added, removed, or renamed in the file. It does NOT
     change if entity bodies change without affecting structure (an
     edit to `greet()`'s body keeps the same entity-id-list).
   - Stored in `chunks.file_shape_hash` (column added in PR 1).
   - Also store `chunks.entity_id_list` (JSON array) for debuggability.

4. **`IndexOptions::stable_sampling: bool`** flag plumbed through the
   pipeline. When set:
   - All Layer-2 LLM calls pass `temperature=0`.
   - Calls also pass a deterministic seed derived from the cache key
     (e.g. first 8 bytes of `sha256(cache_key)` as u64).
   - Result: byte-identical Layer-2 outputs for identical inputs across
     runs, where the provider supports it. Combined with caching, this
     means a re-run of an unchanged corpus does zero LLM work.
   - Default: `false` (preserve current behaviour for users not opting
     in).

5. **CLI flag:** `calli index --stable-sampling` (and propagation to
   `calli ingest`, `calli reindex`, `calli history backfill`).

## Tests (Redd-first TDD)

- **Per-pass cache-hit-skips-LLM tests** (new, one per Layer-2 pass).
  - Use `DryRunProvider` extended with a call counter.
  - Run the pass once over a fixture corpus; assert the counter
    increments by `expected_call_count`.
  - Run the same pass over an identical corpus shape; assert the
    counter increments by 0 (every call is a cache hit).
  - Five tests total: one per pass.

- **Cache-miss-on-shape-change** (new): change the file shape
  (rename one entity), confirm the affected entity's purpose
  re-derives but unrelated entities' purposes stay cached.

- **Stable-sampling-byte-identical** (new): with
  `IndexOptions::stable_sampling = true`, run the same pass twice
  against identical inputs and a real provider (gated behind
  `#[ignore]` for cost). Assert byte-identical outputs.

- **Existing tests stay green.** All PR 1 + PR 2 tests continue to pass.

## Non-deliverables (explicitly defer)

- Embeddings history-table runtime archival — PR 4 wires it up; the
  table exists from PR 1.
- Tombstone-aware query reads (deaths affecting `entity_list_at_sha`) — PR 4.
- `calli history migrate-fresh` CLI — PR 4.
- Convergence test (full 4-path A/B/M/REF live-LLM) — PR 5.
- Legacy column cleanup — PR 5.

## Acceptance criteria

- `cargo build --release` succeeds.
- `cargo test --workspace` passes — including all five new
  cache-hit-skips-LLM tests and the cache-miss-on-shape-change test.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- The PR description documents:
  - The cache-hit-skips-LLM behaviour with at least one before/after
    LLM-call-count number (e.g. "running purpose pass twice against
    identical input goes from 50 calls to 0 calls").
  - The `--stable-sampling` flag and its default.
  - The `file_shape_hash` definition and why it's the right invalidation
    boundary (link the toy-experiment context-leak finding).

## Routing config

```yaml
suggested_config:
  cody:
    model: opus
    effort: high
    rationale: "Cache key plumbing across 5 passes + stable-sampling propagation. Stale-cache-hits would serve wrong artifacts; cache-key composition correctness matters."
  redd:
    model: sonnet
    effort: high
    rationale: "Cache-hit-skips-LLM is the proof of value. Five per-pass tests + one cache-miss-on-shape-change test + one stable-sampling-byte-identical test. DryRunProvider call counter is the linchpin."
  perri:
    model: sonnet
    effort: medium
    rationale: "Reviewer must verify cache key composition is correct per pass (especially summaries with depth and themes with entity-set-hash) and that stable-sampling default-false preserves existing behaviour."
  marty:
    model: sonnet
    effort: medium
    rationale: "Five passes share most cache plumbing; opportunity to extract a `cache_or_derive` helper after the first three land."
```

## References

- Full implementation plan: `docs/plans/honest-provenance-implementation.md`
- PRD: `docs/plans/honest-provenance.md`
- Toy fixture (context-leak experiment data): `/Users/hammer/Code/pinakes-toy/`
- The 18-sample context-leak finding writeup is in this session's transcript;
  short version: same chunk content + different surrounding file → different
  LLM purpose. Cache key must include file_shape_hash, not just chunk content.
