# Honest provenance: refactor Callimachus's version model so artifacts stop lying about when they were produced

## Problem

Callimachus stamps every artifact it produces — chunks, entities, edges, purposes, contracts, summaries, themes, embeddings — with a single field, `derived_at_version`, set to the SHA we happened to be indexing at. That stamp is treated by the rest of the system as the artifact's provenance: "the canonical version of the corpus this artifact belongs to."

The stamp is dishonest. When we index at HEAD, every artifact gets stamped HEAD, including artifacts whose substrate — the source bytes the artifact is derived from — has not changed in fifty commits. The purpose computed for a function introduced in commit C1 and untouched since is stamped with the SHA of C52. The model knows the artifact exists; it does not know when its computation last became valid.

This lie is load-bearing. The reconstruction query "give me state-at-SHA-X" is implemented as exact-SHA-match: `WHERE derived_at_version = X`. To make that query return the right rows for old commits, the diff-based walker shipped in PR #34 has to *copy* unchanged artifacts forward at every commit, re-stamping each copy with the current SHA. On the real callimachus corpus (52 commits, ~2,000 entities), this denormalization costs **703 MB** of storage versus **76.8 MB** for an honest representation of the same information — a 9× multiplier paid in exchange for keeping the query model simple. The A/B comparison report (`/tmp/compare-A-vs-B.md`) shows the median per-SHA entity count is 1,964 under copy-forward versus 91 under supersession-only.

Three currently-open bugs are direct symptoms of the same underlying model defect, not independent issues:

- **`history-backfill-resume-stuck-on-partial-shas.md`** — the backfill walker can't resume from a partial state because it reconstructs its cursor from on-disk history, and on-disk history under copy-forward semantics is ambiguous about what's been processed.
- **`head-mode-theme-archival-missing.md`** — themes live at corpus granularity and don't fit inside the per-entity cascade that copy-forward archival relies on, so they silently fail to archive.
- **`embeddings-no-history-archival.md`** — embeddings were never given a history mirror at all, because the cost of one under the current model was prohibitive.

Each of these could be patched individually. But every patch cements the lie deeper into the system, and several cross-corpus features queued behind this work (`jira-pinakes-adapter`, `data-schema-pinakes-adapter`, `documentation-corpus-apple-appkit`) need provenance to compose meaningfully across pinakes — which it can't, when each pinakes is internally lying about when its artifacts were produced.

PR #34 just shipped. The walker it introduced is correct *under the broken model* but its `copy_unchanged_artifacts` machinery would be deleted entirely under the honest model. This is the right moment to fix the substrate before more code is built atop it.

## Audience

Three audiences feel this, in decreasing immediacy:

1. **The maintainers of Callimachus itself** — me, working in this repo, every time I touch the walker, the storage layer, or a new pass. The model defect leaks into every cross-cutting change. Adding a new artifact type currently requires deciding how to copy-forward it, how to cascade-archive it, and how to reconstruct it at an old SHA — three decisions that should not exist.

2. **Future adapter authors** — the people writing the `jira-pinakes-adapter`, `data-schema-pinakes-adapter`, and similar. They will be composing data from multiple pinakes whose provenance fields must mean the same thing. Today each pinakes' `derived_at_version` means "the SHA we last touched this row at," which is not composable across corpora that don't share a SHA-space.

3. **MCP consumers querying historical state** — agents and tools that ask "what did entity E look like at commit C?" or "when did purpose P stabilize?" Today these queries silently return wrong answers when the asked-about commit hasn't been backfilled with exact-SHA-match copies. The failure mode is invisible: a populated row at the wrong stamp looks indistinguishable from a correct one.

## Success criteria

- A reader of any artifact row in storage can tell, from the row alone, whether the SHA stamp is **known-precise** (this SHA's diff is the most recent commit that could have affected this artifact's computation) or **known-uncertain** (this artifact predates this SHA, exact origin not yet refined). Today this distinction is not representable.
- Indexing a single SHA in isolation does not produce false-precision stamps. Artifacts untouched by that SHA's diff are tagged as predating it, not as having been derived at it.
- Backfilling an older commit refines uncertain stamps without copying rows. An artifact whose substrate was touched by the older commit's diff has its tag narrowed from "predates SHA_new" to "derived at SHA_old." Artifacts untouched by the older commit's diff have their range narrowed but no row is duplicated.
- The walker contains no `copy_unchanged_artifacts` code path. Walking a commit means processing its diff and updating affected tags; rows untouched by the diff are not read or written.
- Backfill resume requires no on-disk cursor reconstruction. The next commit to process is determined by the set of commits not yet processed, not by inspecting which rows have which stamps.
- Layer-2 (LLM-derived) artifacts are cached by `(Layer-1 identity, surrounding-context-hash, model)`. A purpose computed once for an entity in a given file shape is reused for every commit where that entity exists with that file shape, without re-deriving or re-stamping.
- Re-running the purpose pass on identical inputs produces identical output, byte-for-byte, when stable-sampling is enabled. The current ~25–40% residual LLM noise is eliminated as a cache-invalidation source.
- The three open bugs listed above close as side effects of this refactor, not as separate fixes.
- On the real callimachus corpus, a fully-backfilled pinakes is within ~2× of the 76.8 MB honest-representation lower bound established by the A/B comparison, not the 703 MB current model produces.

## In scope

### Pillar 1 — Honest provenance with range-aliases

`derived_at_version` becomes a tagged union, not a bare SHA string:

- **`Concrete(sha)`** — "this SHA's diff is the most recent commit that could have affected this artifact's computation." The artifact's known point of last-relevant-modification.
- **`RangePredating(sha)`** — "this artifact predates this SHA but I haven't refined exactly when it was introduced or last changed." Honest about uncertainty.

Refinement semantics:

- Indexing at a single SHA in isolation: for each Layer-1 artifact, if that SHA's diff touched the substrate the artifact derives from, tag `Concrete(SHA)`; otherwise tag `RangePredating(SHA)`. Layer-2 artifacts inherit their Layer-1 tag at production time but follow the cache-key rules in Pillar 2.
- Backfilling an older commit `SHA_old < SHA_new`: for each artifact currently tagged `RangePredating(SHA_new)`, if `SHA_old`'s diff touched its substrate, refine to `Concrete(SHA_old)`. Otherwise narrow the range: the artifact now `RangePredating(SHA_old)`. No row duplication, no copy-forward.
- At fully-indexed steady state (every commit in `[root..HEAD]` backfilled), every artifact has `Concrete(sha)`. Range-aliases evaporate.

Query semantics for "state at SHA X" change from exact-SHA-match to range-inclusion: an artifact is present at SHA X if its tag is `Concrete(Y)` with `Y ≤ X` or `RangePredating(Y)` with `Y ≥ X`, *and* no later supersession or death event has removed it. (See "Birth/death events" below.)

### Pillar 2 — Two-layer split with appropriate cache keys

Pipeline passes become provenance-agnostic transforms. A pass takes "the inputs for this state" and produces artifacts; provenance is owned by a separate history layer that the pass does not touch. This is the central architectural change.

**Layer 1 — source-derived.** Pure functions of `git_tree(SHA)`. Content-addressable.

- **Chunks** — identity = `(corpus, content_hash)`. Already content-addressed today.
- **Entities** — identity = `(corpus, canonical_name, kind)` *or* `(corpus, canonical_name, kind, content_hash)`. See open question.
- **Edges** — identity = `(corpus, from_entity_id, to_entity_id, kind)`. `location_uri` is **dropped from the identity key**: it is the source of the 9,647 orphan-edge divergence observed in the A/B comparison, where line-number shifts in unchanged code created spurious new edges. Location becomes a non-key attribute of the edge's current observation.

**Layer 2 — LLM-derived.** Functions over Layer 1 with LLM as side-effect. Includes `entity_purposes`, `entity_contracts`, `summaries`, `themes`, `embeddings`.

Cache key for Layer 2: **`(Layer-1 identity, surrounding-context-hash, model)`**, not `(identity, derived_at_version)`. A Layer-2 artifact computed for entity E with surrounding context C and model M is valid for *any* commit where E exists with C as its surrounding context. No re-stamping, no re-derivation, no copying.

The `surrounding-context-hash` component is non-negotiable and empirically grounded: the toy-repo experiments at `/Users/hammer/Code/pinakes-toy/` (18 LLM samples across 4 controlled contexts) demonstrated that the same chunk content produces *different* LLM purposes depending on what else is in the file. The drift is directional and reproducible, not sampling noise. Layer-2 cache keys that omit surrounding context would silently serve wrong-context results across commits.

### Pillar 3 — Stable-sampling for Layer 2 determinism

Within a fixed context, residual LLM sampling noise is ~25–40% (same prompt, minor wording variants). To make Layer 2 caching lossless rather than lossy-but-acceptable, LLM-driven passes must support pinning `temperature=0` and a fixed `seed` where the provider supports it.

This is opt-in at the pass level, not always-on, because:

- Not all providers honor `seed`.
- Some passes (e.g. summarization) may want diversity in early development.
- The decision to commit to a single canonical answer per `(input, model)` is a product choice that should be explicit.

When enabled, the property "re-running the pass on identical inputs produces identical output" becomes a hard invariant the cache can rely on, converting "first-run output is canonical forever" from a pragmatic tradeoff into a non-tradeoff.

### Decision: purpose-pass prompt scope

The purpose pass today receives entity context including surrounding file content. The trade-off space:

- **Chunk-only** — maximal cacheability (any commit where the chunk content matches reuses the cache), lowest richness. "Used by the conversational greeting system" disappears; only the function's own body informs the output.
- **File-shape** — current behavior. Stable per file shape (cache key includes file structure). Richer; preserves cross-function references within a file.
- **Broader (corpus-wide)** — richest, least stable. Almost any commit invalidates the cache because the corpus context is nearly always changing.

**Decision: file-shape, as the default.** It is the cacheable middle ground and the empirical sweet spot from the context-leak experiments: stable within a commit, refines cleanly across commits when the file's shape changes, and preserves the qualitative richness users value in purpose outputs. The pass remains configurable for experimentation, but the production default is file-shape and the cache key incorporates a hash of the file's *shape* (the ordered list of entity identities present in the file, not their bodies).

### Decision: 0-state as explicit empty boundary

State 0 is the conceptual empty state before the root commit. It is never indexed, never represented as a row in storage. Its only role is to make the root commit's diff a normal diff (against empty) rather than a special case in the walker. Documenting this explicitly avoids the recurring confusion of "why doesn't index 0 exist?" and prevents accidental attempts to materialize it.

### Decision: birth/death events via tombstones

Entities and chunks are introduced and removed over a corpus's lifetime. Two options:

- **Tombstones** — explicit rows marking `removed_at_sha`. Query-cheap (single index lookup), storage-cheap (one row per death), survives rebase-style operations cleanly.
- **Absence-by-rebuild** — death inferred from "not present at SHA X" by re-reading the tree. Theoretically cleaner (no extra state), but query-expensive and requires the git tree to be present at query time.

**Decision: tombstones.** Pragmatism wins. A death event is a small row, and the alternative makes every existence query depend on git availability. Tombstone rows carry the same provenance tag as any other artifact: `Concrete(sha)` for "this SHA's diff removed it," `RangePredating(sha)` for "removed somewhere before this SHA but not yet refined."

### Decision: theme cadence

Themes are corpus-level — they summarize the whole corpus state, not individual entities. They don't fit the Layer-2 cache key cleanly because their "surrounding context" is the entire corpus, which changes at every commit. Two options:

- **Re-derive per commit** — one LLM call per commit, cheap relative to the per-entity passes. Themes have honest `Concrete(sha)` provenance at every commit.
- **Re-derive on substantive change** — skip commits whose diff is "small enough." Requires a threshold which is a tunable, which is a footgun.

**Decision: re-derive per commit.** Themes are cheap (one call) and the resulting honesty is worth more than the saved calls. The existing theme archival bug (`head-mode-theme-archival-missing.md`) is resolved by this rule: themes have a per-commit `Concrete(sha)` lineage by construction, no separate archival cascade needed.

### Decision: migration is fresh-start

Existing pinakes carry a mix of honest concrete stamps and copy-forward lies. A heuristic to distinguish them after the fact would be brittle. **Migration is fresh-start: a new pinakes schema version, wipe and rebuild from the corpus.** For the callimachus corpus this is hours, not days. For larger corpora it is a one-time cost paid once.

The CLI grows a `migrate` (or equivalently named) command that recognizes the old schema, walks the user through the rebuild, and preserves the corpus configuration so re-indexing is one command, not a re-setup.

## Out of scope

- Specific schema column names, indexes, foreign-key shapes, migration SQL — Archie's job.
- The exact diff algorithm in the walker. The PRD specifies what the walker must know after processing each commit; not how it computes the diff.
- Performance benchmarks beyond the existing A/B comparison. Once Archie's plan is in hand, benchmarks belong in the implementation PRs.
- The cross-corpus query design — how `jira-pinakes-adapter` and friends compose provenance across pinakes that don't share a SHA-space. That is a separate, larger PRD that depends on this one landing first.
- A change to the LLM provider abstraction beyond exposing `temperature` and `seed` parameters. Multi-model artifact storage (`docs/plans/multi-model-artifact-storage.md`) remains its own work.
- A general "history compaction" or "prune old range-aliases" feature. At steady state range-aliases evaporate naturally; there is nothing to compact.

## Risks and unknowns

- **Range-alias refinement is monotonic only if commits are processed in time-order.** Out-of-order backfill (e.g. backfilling C10 after C20) is the common case and the model handles it: refinement always narrows the tag, never widens it. But the walker's bookkeeping must ensure no path takes a `Concrete(C20)` tag and rewinds it to `RangePredating(C10)`. The PRD asserts the invariant; the plan must enforce it.
- **Edge identity without `location_uri` could collide** when the same source-target-kind triple appears at multiple call sites in the same entity (e.g. two calls from function A to function B). The A/B comparison did not surface this as a problem because such duplicates were already being deduplicated upstream, but the plan should confirm before committing to the new key.
- **Stable-sampling provider support varies.** OpenAI honors `seed` best-effort, Anthropic exposes `temperature=0` but no seed, local providers vary. The PRD does not promise byte-identical output across providers — only within a `(provider, model, seed)` triple where supported.
- **Tombstones plus range-aliases can produce queries with two sources of uncertainty.** "Is entity E present at SHA X" can answer "yes (Concrete), but maybe-dead (RangePredating tombstone)." The plan must specify whether such answers are surfaced to the caller or resolved internally before responding.
- **File-shape hash for purpose-pass cache key needs a precise definition.** "Ordered list of entity identities in the file" is the starting point but renames, moves, and content changes all affect it differently. The plan must pin the hash function.

## Open questions

These are the decisions the PRD deliberately defers to the design loop with Archie. Each is a question, not a position.

1. **Entity identity: name-only or name+content?** Under name-only, a function whose body changes is the same entity with new content; renames create a new entity and orphan the old purpose. Under name+content, every body change creates a new entity-version; renames preserve content-keyed identity. The choice has cascading effects on Layer 2 cache hit rate, rename handling, and what "the purpose of entity E" means across history. Recommendation to discuss: name-only at the storage layer, with a separate rename-detection pass that emits `rename` edges — but this is exactly the kind of choice that should be made with Archie's input on implementation feasibility.

2. **Cache invalidation for Layer 2 on entity rename.** Direct consequence of question 1. Under name+content identity the purpose survives; under name-only it is lost. Is "lost purpose on rename" acceptable, or does the rename-detection pass need to propagate purposes forward?

3. **Tombstone lifecycle in fully-indexed corpora.** Once every commit in `[root..HEAD]` is backfilled and every tag is `Concrete`, do tombstones stay forever (audit trail, useful for "when was X removed?") or can they be compacted into a single "alive-between-X-and-Y" range on the live row? The product answer depends on whether death-event queries are a first-class use case.

4. **Should `RangePredating` carry a lower bound when one is known?** The model as specified is one-sided: "predates SHA Y, no further refinement." If we backfill C10 and C20 but not C15, an artifact untouched by both could honestly be tagged "in `[C10, C20]`" — a two-sided range. This is strictly more information than the one-sided form. The cost is a wider tag and more bookkeeping; the benefit is sharper queries. Recommendation to discuss: start one-sided, add lower bound later if queries demand it.

5. **What does `theme` provenance mean in a fresh-start migration that backfills out of order?** Themes are re-derived per commit, so they always carry `Concrete(sha)` — but if backfill processes commits non-monotonically, a theme at C10 might be derived after a theme at C20. Is this a problem? Probably not — themes are independent per commit — but the plan should confirm there's no implicit ordering dependency.

---

**File:** `/Users/hammer/Code/callimachus/docs/plans/honest-provenance.md`
