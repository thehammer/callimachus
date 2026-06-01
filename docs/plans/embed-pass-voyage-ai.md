# Wire the embed pass to a real embedding provider (Voyage AI)

## Problem

`calli index <corpus> --pass embed` and `--pass all` advertise that chunk
embeddings will be generated, but no embedder is ever constructed. Three call
sites pass `embedder: None` into the pipeline:

- `crates/callimachus-cli/src/commands/index.rs:72`
- `crates/callimachus-cli/src/commands/ingest.rs:88`
- `crates/callimachus-core/src/indexing/reindex_pass.rs:294`

The embed pass handles `None` by logging a warning and returning
`PassStats::default()`. So requesting embeddings is a **silent no-op**: the
`embeddings` table is permanently empty in every pinakes the CLI has ever
produced, and a user running `--pass all` believes they got vectors when they
got nothing. This is the documented bug
`.claude/bugs/open/embed-pass-unwired-in-cli.md`.

Everything *downstream* of the provider call is already built and tested: the
`embeddings` head + `embeddings_history` tables, provenance stamping, the
history-layer commit path, Layer-2 caching keyed on `(chunk.id, model)`,
idempotency (skip-if-already-embedded), and `--full` re-embed. The only missing
link is the provider: a configured `EmbeddingProvider` that turns chunk text
into a vector, plus the config and wiring to construct it.

## Audience

The operator running `calli index`/`ingest`/`reindex` from the CLI on their own
machine, indexing local code repositories. Concretely: a developer who edits
`config.toml` to turn embeddings on, sets a Voyage API key, runs `calli index
my-repo --pass all`, and expects `SELECT COUNT(*) FROM embeddings` to equal the
chunk count afterward. Today they get zero rows and no error. The same operator
must be able to turn the feature *off* (comment out a config block) and have the
tool behave exactly as it does today.

Cost context: embedding all local repos including full history is ~$27 one-time
with `voyage-code-3`; ongoing incremental cost is negligible. Cost is not a
gating concern for this audience.

## Success criteria

- With `[embedding]` configured (`enabled = true`, valid key, model
  `voyage-code-3`), `calli index <corpus> --pass embed` on a fresh corpus
  results in `COUNT(*) FROM embeddings WHERE corpus_id = <id>` equal to
  `COUNT(*) FROM chunks WHERE corpus_id = <id>`.
- Re-running the same command is a no-op (all chunks skipped, count unchanged) —
  the existing idempotency path still holds with a real provider.
- `--pass embed` or `--pass all` requested while embeddings are **not usably
  configured** (disabled, or enabled with a missing/empty key) produces a
  **clear, actionable error and a non-zero exit** before or at pass start — not
  a warning, not a silent skip, not an empty table.
- All three entry points — `index`, `ingest`, `reindex` — produce identical
  embedding behaviour for the same corpus and config. None of them silently
  drops embeddings.
- With `[embedding]` absent or `enabled = false`, every command that does *not*
  request the embed pass behaves byte-for-byte as it does today: no Voyage
  calls, no new error paths, no change to the default pass set, an empty
  `embeddings` table, and no downstream breakage from that emptiness.
- A user can fully back out by commenting out the `[embedding]` block in
  `config.toml`; no other change is required to return to today's behaviour.
- The provider abstraction is a named trait with a single concrete
  implementation (Voyage), shaped so a second provider could be added later
  without changing the pass or the wiring contract.

## In scope

- An `EmbeddingProvider` abstraction with one concrete implementation backed by
  Voyage AI (`voyage-code-3`). It must be async and must expose a **batch**
  entry point (embed many texts in one request), because Voyage accepts batched
  inputs and one-call-per-chunk is the wrong cost/latency shape for ~all-history
  corpora. A single-text convenience path may sit on top of the batch path.
- An `[embedding]` section in `GlobalConfig` (`crates/callimachus-cli/src/config.rs`)
  with the fields enumerated below.
- A builder/factory that turns the `[embedding]` config into an
  `Option<provider>` (or an error), with the loud-error contract below. This
  builder is the single shared construction path; `index`, `ingest`, and
  `reindex` all call it rather than each hand-rolling provider construction.
- Threading the constructed embedder through `reindex_pass::run` (which today
  hardcodes `embedder: None` internally) so reindex embeds too.
- Resolving the API key from an environment variable named in config
  (`api_key_env`), consistent with how secrets are handled elsewhere.
- Tests: config-to-provider construction (enabled+key, enabled+no-key,
  disabled); the loud-error path when embed is requested but unconfigured; an
  integration test asserting embeddings count equals chunk count after a real
  (or stubbed-provider) embed pass.

## Out of scope

- The semantic-search / similarity MCP query tool. This PRD gets vectors *into*
  the table correctly; reading them back for search is a separate piece of work.
- Embedding natural-language artifacts (purposes, contracts, summaries, themes).
  That is the separate feature `.claude/features/backlog/embed-nl-artifacts.md`,
  which depends on this one. This PRD embeds **raw code chunks only**.
- Multi-provider support. The trait must *allow* a future second provider, but
  only the Voyage implementation is built here. No provider auto-detection, no
  provider-selection UX beyond a `provider` config field that currently accepts
  one value.
- A new storage migration. The `embeddings` / `embeddings_history` schema,
  provenance columns, history archival, and Layer-2 cache are already present
  and sufficient. If the design discovers a genuine schema gap, that is an open
  question to raise — not an assumed deliverable.
- Changing the default pass set. Embed stays opt-in via `--pass embed` /
  `--pass all`, exactly as today.
- Dimension reduction, quantization, or `sqlite-vec` storage. Vectors are stored
  as today (f32 little-endian blob via the existing store).

## Risks and unknowns

- **Existing trait overlap.** `embed` and `supports_embeddings` already live on
  the `LlmProvider` trait, and `OpenAiEmbeddingProvider` already implements them
  by panicking on `complete`. The design must decide whether the Voyage provider
  is (a) a fresh standalone `EmbeddingProvider` trait that the pass depends on
  instead of `LlmProvider`, or (b) another `LlmProvider` impl following the
  OpenAI-embed precedent. Option (b) is the smaller diff (the pass already takes
  `Option<Arc<dyn LlmProvider>>`); option (a) is the cleaner abstraction the bug
  doc and brief gesture at. This is the central design decision for Archie — see
  Open questions. Either way the *pass-facing* contract is `embed`/batch +
  `supports_embeddings`.
- **The `supports_embeddings()` second silent trap.** The pass *also* silently
  skips when `embedder.supports_embeddings()` is false
  (`embed_pass.rs:41`). Whatever the wiring builds for an enabled config must
  return a provider for which this is true; and if the builder ever yields a
  provider that can't embed, that must surface as a loud error too, not a skip.
  The error contract below covers the *unconfigured* case; the design must
  ensure a *misconfigured-but-enabled* provider can't fall through to this
  warning-and-skip.
- **Voyage dimensions vs. config.** `voyage-code-3` has a native dimensionality
  (and Voyage supports requesting reduced dimensions). The `dimensions` stored
  on each row is currently derived from the returned vector length, not from
  config. If a `dimensions` config field is offered, the design must decide
  whether it *requests* a dimensionality from Voyage or merely *asserts/validates*
  the returned length — a mismatch between configured and actual dimensions
  should fail loudly, never write a wrong-width vector.
- **Batch sizing and partial failure.** A batched call can partially fail or hit
  rate limits. The pass currently treats a per-chunk embed error as
  `stats.failed += 1` and continues. With batching, the design must define what
  "one failed item in a batch of N" does to `PassStats` and whether the run as a
  whole should still exit success. The success criterion is "count equals chunk
  count"; persistent partial failure should be visible, not silently absorbed.
- **Model name written to storage.** `StoredEmbedding` records `model` from
  `embedder.name()`, and the Layer-2 cache key uses the same `name()`. The
  Voyage provider's `name()` must be the real model identifier
  (e.g. `voyage-code-3`), not a generic label, or cache hits and future
  cross-model queries will be keyed wrong.

## Open questions

- New `EmbeddingProvider` trait, or another `LlmProvider` impl following
  `OpenAiEmbeddingProvider`? (Risk #1.) Which keeps the embed pass signature and
  the three wiring sites simplest while still leaving room for a second
  provider?
- Config field set. Proposed: `enabled` (bool), `provider` (string, currently
  only `"voyage"`), `model` (string, default `voyage-code-3`), `api_key_env`
  (string, the env var name to read the key from), `batch_size` (optional int).
  Is a `dimensions` field worth its weight given the validate-vs-request
  ambiguity in Risk #3, or should dimensions be left provider-native and merely
  recorded? Direct inline `api_key` in config, or env-var-only? (Brief leans to
  `api_key_env`; confirm whether an inline key is also allowed for parity with
  `llm.api_key`.)
- Exact loud-error wording and trigger point. When `--pass embed`/`all` is
  requested and embeddings are not usably configured, should the error fire at
  command setup (before any other pass runs) or at embed-pass entry? Setup-time
  fails fast and wastes no LLM spend on the other passes; pass-entry lets the
  non-embed passes complete first. Recommend setup-time fail-fast for `--pass
  embed` (embed is the only requested pass) and a decision for `--pass all`
  (where other passes are also requested). Archie to choose.
- Does the loud-error contract apply only when embed is *explicitly requested*?
  Confirm: with embed **not** requested, a missing/disabled `[embedding]` config
  must never error — it is the normal, fully-supported off state.
- Should the builder live in the CLI (`commands` / a shared `config`→provider
  helper) or in `callimachus-llm` alongside `resolve::build`? The completion
  provider uses `resolve_provider` (CLI) + `build_provider` (llm crate); mirror
  that split for embeddings, or consolidate?
