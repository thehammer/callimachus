# Tiered model selection — route entities to haiku/sonnet/opus by static signals

## Context

Callimachus currently runs every entity through a single LLM model (the
provider's `default_model`) for purpose, contract, and summary extraction.
A trivial getter and a complex auth orchestrator both get Sonnet. This
wastes money on simple entities and under-serves the genuinely complex
ones — exactly the entities (security-critical, high fan-in, panic-laden)
where Opus's marginal value over Sonnet is highest.

The static analysis layer (`ContractSignals` in
`callimachus-adapter-code/src/contracts.rs`) already produces the signals
needed to route entities to an appropriate tier before any LLM call is
made: `has_unsafe`, `is_fallible`, `panic_call_count`, `debt_markers`,
`is_mutating`, `body_lines`. Combined with graph signals (in-degree,
out-degree) from the edge store, those signals can drive a rule-based
router that targets Opus at the top ~5–10% of entities, Sonnet at the
middle 30–40%, and Haiku at the long tail. The result is better quality
on the entities that matter and lower total spend.

The design was deliberately rule-based rather than a weighted-sum
complexity score because rules are auditable, configurable per-corpus, and
produce a distribution you can reason about before spending money. The
feature request at `.claude/feature-requests/tiered-model-selection.md`
sketched the weighted-sum version; this plan supersedes it with the
rule-based design that emerged from later discussion.

This feature is **blocked on multi-model artifact storage**, which is
being planned in parallel at `docs/plans/multi-model-artifact-storage.md`.
That plan extends the artifact tables and idempotency checks (purpose,
contract, summary) to be keyed on `(entity_id, model)` rather than
`entity_id` alone, so a tiered re-run can fill in higher-tier artifacts
without colliding with lower-tier ones. **This branch must be based on
whichever branch multi-model artifact storage lands on** — either
`feature/multi-model-artifact-storage` while it's open or `origin/main`
after it merges. Do not start work on this plan against `main` until that
prerequisite is in place.

## Target

- **Repo:** callimachus
- **Branch:** `feature/tiered-model-selection`
- **Base:** `feature/multi-model-artifact-storage` (rebase to `origin/main`
  once that branch merges)

## Files to change

- `crates/callimachus-core/src/indexing/model_tier.rs` — **new file**.
  Houses `ModelTier` enum, `TierConfig` struct, `RoutingInputs` struct,
  and `ModelTierRouter` with the rule table. Pure logic, no I/O, fully
  unit-testable.
- `crates/callimachus-core/src/indexing/mod.rs` — register the new
  `model_tier` module (`pub mod model_tier;`).
- `crates/callimachus-core/src/storage/backend.rs:78-94` — add two new
  trait methods to the `// ── Edge ──` section:
  ```rust
  fn entity_in_degree(&self, corpus_id: &str, entity_id: &str) -> Result<u32>;
  fn entity_out_degree(&self, corpus_id: &str, entity_id: &str) -> Result<u32>;
  ```
- `crates/callimachus-core/src/storage/edge_store.rs` — implement the two
  new functions for SQLite (`SELECT COUNT(*) FROM edges WHERE corpus_id =
  ?1 AND to_entity_id = ?2` / `from_entity_id = ?2`). Wire them through
  `SqliteBackend` in `crates/callimachus-core/src/storage/sqlite.rs` to
  match the existing edge-store delegation pattern.
- `crates/callimachus-core/src/storage/postgres.rs:160-185` — add `todo!()`
  stubs for `entity_in_degree` and `entity_out_degree` matching the rest
  of the postgres stub style.
- `crates/callimachus-llm/src/anthropic.rs:17-45` — add a `with_model`
  method on `AnthropicApiProvider`:
  ```rust
  pub fn with_model(&self, model: impl Into<String>) -> AnthropicApiProvider
  ```
  The returned provider shares the same `reqwest::Client`, `api_key`,
  `base_url`, and `Arc<Mutex<ProviderUsage>>` (so cost/usage accounting
  remains unified across tiers) but overrides `default_model`. Implement
  by cloning struct fields — `reqwest::Client` is cheap to clone (it
  shares a connection pool internally), and `Arc` clones share storage.
  Document the usage-sharing behaviour in the doc comment.
- `crates/callimachus-llm/src/provider.rs` (or wherever the `LlmProvider`
  trait lives) — verify that the existing `CompletionRequest::model`
  field is honoured by all providers. The Anthropic provider already
  respects `req.model` at `anthropic.rs:97-100`. Confirm
  `ClaudeCodeProvider` and `DryRunProvider` do the same, or document that
  tier routing for those providers degrades to "use the default model"
  (acceptable; this feature only routes Anthropic API traffic in v1).
- `crates/callimachus-cli/src/config.rs` — extend `GlobalConfig` with a
  new `model_tiers: TierConfigToml` field (default-impl when section is
  absent). Re-export the deserialised struct from
  `callimachus-core::indexing::model_tier` to keep the source of truth in
  core; the CLI just deserialises into it. Wire the loaded `TierConfig`
  through `IndexOptions` so passes can see it.
- `crates/callimachus-core/src/indexing/pipeline.rs:18-50` — add
  `pub tier_config: TierConfig` to `IndexOptions` (default = disabled).
  Plumb it into each pass's `run(...)` call.
- `crates/callimachus-core/src/indexing/contract_pass.rs` — before the
  `extract_with_retry` call (around line 102), compute `RoutingInputs`
  (it already has `language` and source `content`; reuse the existing
  `analyze(language, &content, &entity.canonical_name)` from the code
  adapter to get `ContractSignals`, then look up in/out degrees). Call
  `router.route(&inputs)`, then `let llm_for_entity = match tier {
  Haiku => llm_haiku.as_ref(), Sonnet => llm_sonnet.as_ref(), Opus =>
  llm_opus.as_ref() };` and pass that into `extract_with_retry`. Tally
  per-tier counts in a `[u64; 3]` and emit one INFO log line at the end
  of the pass. Persist the chosen model on the `EntityContract.model`
  field (already populated from `llm.name()` — that call will now reflect
  the per-entity provider).
- `crates/callimachus-core/src/indexing/purpose_pass.rs` — same pattern.
  Purpose pass currently does not call the static analyser, so it needs
  to call `callimachus_adapter_code::contracts::analyze(...)` itself when
  the entity is a code entity. For non-code corpora (no signals
  available), the router falls back to the `default` tier from config.
- `crates/callimachus-core/src/indexing/summarize_pass.rs` — same
  pattern but routing is keyed on the *entity* a summary belongs to.
  Where summaries are over chunks (not entities), tier on a small set of
  derived signals: chunk line count, presence of debt markers, and child
  summary count. Document this divergence in a code comment.
- `crates/callimachus-core/src/indexing/pipeline.rs` (the construction
  site around line 76) — when `tier_config.enabled` is true, build three
  provider variants up-front from the base Anthropic provider via
  `with_model("claude-haiku-…")` etc., and pass `Arc<dyn LlmProvider>`
  for each to the passes. When disabled, pass the same single provider
  three times (cheap; just three `Arc::clone`s).

## Approach

1. **Land the prerequisite first.** Confirm
   `feature/multi-model-artifact-storage` is merged or rebase onto it.
   Verify that `purpose_get`, `contract_get`, `summary_get` now accept a
   `model` argument (or that the idempotency check considers model). If
   that work isn't done, stop — this plan cannot proceed safely without
   it.

2. **Create the router.** Write
   `crates/callimachus-core/src/indexing/model_tier.rs` containing:
   ```rust
   #[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
   pub enum ModelTier { Haiku, Sonnet, Opus }

   #[derive(Debug, Clone, Serialize, Deserialize)]
   pub struct TierConfig {
       pub enabled: bool,
       pub default: ModelTier,
       pub haiku_model: String,   // e.g. "claude-haiku-4-5"
       pub sonnet_model: String,  // e.g. "claude-sonnet-4-5"
       pub opus_model: String,    // e.g. "claude-opus-4-7"
       pub opus_rules: OpusThresholds,
       pub sonnet_rules: SonnetThresholds,
   }

   pub struct OpusThresholds {
       pub min_in_degree_fallible: u32,   // default 20
       pub min_panic_count_public: u32,   // default 3
       pub min_module_out_degree: u32,    // default 15
   }

   pub struct SonnetThresholds {
       pub min_in_degree: u32,   // default 10
       pub min_line_count: u32,  // default 150
   }

   pub struct RoutingInputs<'a> {
       pub signals: &'a ContractSignals,
       pub kind: &'a str,
       pub in_degree: u32,
       pub out_degree: u32,
   }

   pub struct ModelTierRouter<'a> { cfg: &'a TierConfig }
   impl<'a> ModelTierRouter<'a> {
       pub fn new(cfg: &'a TierConfig) -> Self { Self { cfg } }
       pub fn route(&self, inputs: &RoutingInputs) -> ModelTier { … }
       pub fn model_name(&self, tier: ModelTier) -> &str { … }
   }
   ```
   Routing rules (in this order — first match wins):
   ```
   if !cfg.enabled        → cfg.default
   if has_unsafe          → Opus
   if in_degree > opus_rules.min_in_degree_fallible
        && is_fallible    → Opus
   if is_public
        && panic_call_count > opus_rules.min_panic_count_public  → Opus
   if kind == "module"
        && out_degree > opus_rules.min_module_out_degree         → Opus
   if is_public && is_fallible                                   → Sonnet
   if is_mutating && is_fallible                                 → Sonnet
   if panic_call_count > 0                                       → Sonnet
   if !debt_markers.is_empty()                                   → Sonnet
   if kind == "class" || kind == "module"                        → Sonnet
   if in_degree > sonnet_rules.min_in_degree                     → Sonnet
   if body_lines > sonnet_rules.min_line_count                   → Sonnet
   otherwise                                                     → Haiku
   ```
   Provide `Default for TierConfig` that yields `enabled = false`,
   `default = Sonnet`, and the threshold defaults shown above. Provide
   `Default for OpusThresholds` and `Default for SonnetThresholds`.

3. **Add storage methods.** Add `entity_in_degree` and
   `entity_out_degree` to `StorageBackend`, implement on SQLite via
   `COUNT(*)` queries against the existing `edges` table, and stub on
   Postgres. Add a unit test in the edge store module that inserts a
   handful of edges and verifies the counts.

4. **Add `with_model` on the Anthropic provider.** Document that it
   shares usage accounting with the parent. Add a unit test that
   constructs three variants and verifies `req.model` is correctly
   substituted when each variant's `complete()` is called (use wiremock,
   mirroring the existing tests at `anthropic.rs:235-351`).

5. **Wire config.** Add a `[model_tiers]` section to the `GlobalConfig`
   loader. Sample:
   ```toml
   [model_tiers]
   enabled = true
   default = "sonnet"
   haiku_model  = "claude-haiku-4-5"
   sonnet_model = "claude-sonnet-4-5"
   opus_model   = "claude-opus-4-7"

   [model_tiers.opus_rules]
   min_in_degree_fallible = 20
   min_panic_count_public = 3
   min_module_out_degree  = 15

   [model_tiers.sonnet_rules]
   min_in_degree  = 10
   min_line_count = 150
   ```
   Add a `TierConfig` field to `IndexOptions::default()` (disabled).

6. **Wire passes.** In each of the three passes:
   - At the top of `run(...)`, build a `ModelTierRouter` from
     `opts.tier_config`.
   - Receive `Arc<dyn LlmProvider>` for each tier from the pipeline (or
     fall back to a single `llm` when disabled — the pipeline passes the
     same Arc three times in that case, so no code branching is needed
     in the passes themselves).
   - Before each LLM call, compute `ContractSignals`, in-degree,
     out-degree; call `router.route(...)`; select the matching provider.
   - Tally `tier_counts[tier as usize] += 1`.
   - Emit at INFO level at the end of the pass:
     `[contract] tier distribution: haiku=N sonnet=M opus=K`.
   - Emit at DEBUG level for each entity:
     `[contract] entity={id} tier={tier} kind={kind} in_deg={n} out_deg={n} fallible={bool} panics={n}`.

7. **Update the pipeline.** When `tier_config.enabled`, downcast the
   pipeline's base provider to `AnthropicApiProvider` to call
   `with_model`. If the downcast fails (e.g. user is on `ClaudeCode` or
   `DryRun`), log a WARN one-liner and fall back to passing the same
   provider Arc for all three tiers — the feature degrades gracefully
   for non-Anthropic providers without erroring. Encapsulate this in a
   small helper `fn build_tier_providers(base: Arc<dyn LlmProvider>,
   cfg: &TierConfig) -> [Arc<dyn LlmProvider>; 3]`.

8. **Tests.**
   - **Unit (router):** one test per rule branch — at minimum:
     `unsafe → opus`, `in_degree 21 fallible → opus`,
     `public with 4 panics → opus`, `module out_degree 16 → opus`,
     `public fallible → sonnet`, `mutating fallible → sonnet`,
     `panic_call_count 1 → sonnet`, `non-empty debt_markers → sonnet`,
     `class kind → sonnet`, `in_degree 11 → sonnet`,
     `body_lines 200 → sonnet`, `simple getter → haiku`,
     `disabled config → default`.
   - **Unit (provider):** `with_model` substitutes the model in the
     wire request and shares usage with parent.
   - **Unit (storage):** in/out-degree counts on a small fixture.
   - **Integration:** a test in `pipeline.rs` (alongside the existing
     pipeline tests near line 224) that runs a mini corpus through the
     pipeline with a mock `LlmProvider` that records the model field
     per call. Insert two entities — one trivial getter, one with
     `has_unsafe` — and assert the getter routed to haiku and the
     unsafe one routed to opus. Use the `DryRunProvider` pattern or
     extend an existing test helper.

9. **Run the dogfood corpus.** After the code lands, run
   `calli --db data/callimachus.db index callimachus` against this very
   repo and inspect the tier distribution log line. Expect roughly
   haiku ≫ sonnet > opus, with opus being a small minority (target
   under 15%). If opus is firing on most entities, the thresholds are
   wrong — tune `opus_rules` upward and re-run. Record the resulting
   distribution in the PR body for reviewer context.

10. **Format and lint.** `cargo fmt --all` and `cargo clippy --all-targets
    --all-features -- -D warnings`. Run `cargo test --workspace`.

## Acceptance criteria

- `crates/callimachus-core/src/indexing/model_tier.rs` exists and
  contains `ModelTier`, `TierConfig`, `RoutingInputs`, and
  `ModelTierRouter` as described.
- All router branches from the rule table are covered by unit tests, and
  `cargo test -p callimachus-core model_tier` is green.
- `StorageBackend::entity_in_degree` and `entity_out_degree` exist on
  the trait, are implemented on SQLite, and are stubbed on Postgres.
- `AnthropicApiProvider::with_model` exists, shares the underlying
  `reqwest::Client` and usage `Arc<Mutex<…>>`, and is covered by at
  least one wiremock test asserting the right model goes on the wire.
- A `[model_tiers]` section in `config.toml` is read by `GlobalConfig`
  and flows through `IndexOptions` into all three passes.
- Each pass (purpose, contract, summarize) selects per-entity providers
  via the router when `enabled = true`, and falls back to a single
  provider when `enabled = false`.
- Each pass emits one INFO-level tier distribution summary at the end
  of the pass (format: `[pass] tier distribution: haiku=N sonnet=M opus=K`).
- Integration test: mock provider records the model per entity; a
  high-complexity fixture entity (has_unsafe) routes to opus, a trivial
  fixture entity routes to haiku.
- `cargo fmt --all -- --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, and `cargo test --workspace` all pass.
- PR body includes the tier distribution from a dogfood index run
  against the callimachus corpus.
- For non-Anthropic providers (ClaudeCode, DryRun), the pipeline emits
  a WARN log and uses the single provider for all entities; indexing
  still completes successfully.

## Out of scope

- Changing the artifact storage schema or idempotency rules — that's the
  prerequisite plan's job and must already be done.
- Empirical calibration of thresholds beyond a single dogfood run.
  Threshold tuning across multiple corpora is follow-up work.
- Tier routing for embedding calls. This plan only routes the
  text-completion passes.
- Pre-run cost projection / "show me what this would cost" CLI
  command — useful but separate.
- Per-corpus tier overrides (the `TierConfig` is global for now). If
  corpora need different rules later, that's a config-layering change
  best done as its own plan.
- Support for non-Anthropic providers' tier routing. The feature
  degrades gracefully for `ClaudeCodeProvider` and `DryRunProvider`
  (single provider for all entities) but does not actively route them.

```yaml
suggested_config:
  cody:
    model: sonnet
    effort: high
    rationale: "Multi-crate change spanning storage trait, LLM provider, three indexing passes, config plumbing, and integration tests; correctness matters for downstream cost."
  redd:
    model: sonnet
    effort: high
    rationale: "Many test surfaces (router rule table, provider variant, storage counts, end-to-end routing); coverage discipline directly validates the tier rules."
  marty:
    model: sonnet
    effort: medium
    rationale: "Standard refactor pass — consolidate the per-pass signals/router/tier-counts boilerplate and the build_tier_providers helper if duplication emerges."
  perri:
    model: sonnet
    effort: high
    rationale: "Routing logic touches cost-sensitive code paths; missed bugs in the rule table or the provider sharing model cause real-money regressions."
```
