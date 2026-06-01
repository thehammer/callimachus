# Wire the embed pass to a real Voyage AI embedding provider

## Context

`calli index <corpus> --pass embed` and `--pass all` advertise that chunk
embeddings will be generated, but **no embedder is ever constructed**. Three call
sites hardcode `embedder: None`, so requesting embeddings is a silent no-op: the
`embeddings` table is permanently empty in every pinakes the CLI has ever
produced, and a user running `--pass all` believes they got vectors when they got
nothing. This is the documented bug
`.claude/bugs/open/embed-pass-unwired-in-cli.md`.

Everything *downstream* of the provider call is already built and tested: the
`embeddings` head + `embeddings_history` tables, provenance stamping, the
history-layer commit path, Layer-2 caching keyed on `(chunk.id, model)`,
idempotency (skip-if-already-embedded), and `--full` re-embed. The only missing
link is the provider: a configured embedder that turns chunk text into a vector,
plus the config and wiring to construct it.

This plan implements a standalone `EmbeddingProvider` trait with a single concrete
implementation backed by Voyage AI (`voyage-code-3`), an `[embedding]` config
section, a `build_embedding_provider` factory in `callimachus-llm` (mirroring
`resolve::build`), and the wiring to thread the embedder through `index`,
`ingest`, and `reindex`. It also makes "embed requested but not usably configured"
a loud, fail-fast error at command setup instead of a silent skip.

The full PRD is at `docs/plans/embed-pass-voyage-ai.md`. Its open questions are
resolved as constraints in this plan (see Approach).

## Target

- **Repo:** callimachus
- **Branch:** `fix/embed-pass-voyage-ai`
- **Base:** `origin/main`

## suggested_config

```yaml
effort: low  # downgrade: mechanical wiring + one new HTTP provider following the existing OpenAiEmbeddingProvider precedent; no architectural ambiguity left open — every decision is pinned in this plan. The diff is wide (many embedder: None sites) but shallow.
model: sonnet
```

## Key facts confirmed by reading the code

- The embed pass (`embed_pass.rs`) currently takes `Option<Arc<dyn LlmProvider>>`
  and silently returns `PassStats::default()` when it is `None`
  (`embed_pass.rs:30-39`) **or** when `supports_embeddings()` is false
  (`embed_pass.rs:41-48`). Both traps must be eliminated for the requested-embed
  case.
- `IndexPipeline.embedder` is typed `Option<Arc<dyn LlmProvider>>`
  (`pipeline.rs:319`). The pipeline passes it to `embed_pass::run` at
  `pipeline.rs:497`.
- The `embedder: None` literal appears at ~20 sites (`pipeline.rs`, `history_walk.rs`,
  plus the three CLI commands). Because `None` infers its type from the field, only
  the **named** type annotations need editing when the field type changes:
  `pipeline.rs:319` (struct field), `embed_pass.rs:27` (function param), and the
  two test bindings `embed_pass.rs:169` and `embed_pass.rs:186`.
- `history_walk.rs:449` copies the field with `pipeline.embedder.clone()` — no edit
  needed; it follows the field type automatically.
- `pipeline.rs:340` wraps the embedder with `StableSamplingProvider::wrap`, which
  only accepts `Arc<dyn LlmProvider>`. Once the embedder is a different trait, this
  wrap must be removed (Voyage embeddings are deterministic for a fixed model;
  stable-sampling is a no-op for them).
- `query/service.rs:18` also holds an `embedder: Option<Arc<dyn LlmProvider>>`,
  but it is the semantic-search **read** path, which is explicitly out of scope.
  It is `None` everywhere today. **Do not touch it.**
- `callimachus-core` already depends on `callimachus-llm`
  (`callimachus-core/src/.../Cargo.toml`), so the new trait can live in
  `callimachus-llm` and be referenced from core.
- `StoredEmbedding::new` (`embedding_store.rs:26`) already derives `dimensions`
  from `vector.len()`. No `dimensions` config field is added (PRD decision #4).
- HTTP provider test precedent: `wiremock = "0.6"` is already a dev-dependency of
  `callimachus-llm` (used by `anthropic.rs` tests).
- `LlmError` (`error.rs`) has an `Other(String)` variant suitable for embedding
  errors; reuse it.

## Files to change

### New files

- `crates/callimachus-llm/src/embedding.rs` — the `EmbeddingProvider` trait
  definition (see Approach step 1).
- `crates/callimachus-llm/src/voyage.rs` — `VoyageEmbeddingProvider`, the concrete
  Voyage AI implementation (Approach step 2).

### `crates/callimachus-llm/src/lib.rs`

- Add `mod embedding;`, `mod voyage;`.
- Re-export `pub use embedding::EmbeddingProvider;`,
  `pub use voyage::VoyageEmbeddingProvider;`.
- Add `pub use resolve::build_embedding_provider;` (new factory, Approach step 4).

### `crates/callimachus-llm/src/resolve.rs`

- Add `EmbeddingProviderConfig` struct and `build_embedding_provider` function
  (Approach step 4). This mirrors the existing `ProviderConfig` / `build` split.

### `crates/callimachus-cli/src/config.rs:27-32`

- Add an `EmbeddingConfig` struct and an `embedding: Option<EmbeddingConfig>` field
  on `GlobalConfig` (Approach step 3). Mirror `LlmConfig`'s `Default`/serde shape.

### `crates/callimachus-core/src/indexing/embed_pass.rs`

- `:3` — change `use callimachus_llm::LlmProvider;` to
  `use callimachus_llm::EmbeddingProvider;` (keep `LlmProvider` import only if a
  test still needs it — it does not after step 6).
- `:27` — change param type to `embedder: Option<Arc<dyn EmbeddingProvider>>`.
- `:81` and `:109` — `embedder.name()` and the `embed` call now resolve against the
  new trait (no textual change if the method names match — they do).
- `:87` — `embedder.embed(&chunk.content).await` stays; the trait keeps an `embed`
  method (Approach step 1). Optionally switch the per-chunk loop to the batch path
  (Approach step 5) — this is the recommended shape but the single-text path is
  acceptable for a first cut. **Decision: keep the per-chunk `embed` loop for this
  PR** to minimize blast radius; the batch method exists on the trait for a future
  optimization. Update the doc-comment at `:18-21` to drop the "logs a warning"
  framing for the unconfigured case if the pass keeps the `None` guard (it should
  — see step 6).
- `:169`, `:186` (tests) — change the test embedder bindings from
  `Arc<dyn LlmProvider> = Arc::new(DryRunProvider::new())` to a new
  `StubEmbeddingProvider` (Approach step 7). Remove the
  `use callimachus_llm::DryRunProvider;` test import.

### `crates/callimachus-core/src/indexing/pipeline.rs`

- `:319` — change field type to
  `pub embedder: Option<Arc<dyn EmbeddingProvider>>`.
- `:3` (or wherever `LlmProvider` is imported) — add
  `use callimachus_llm::EmbeddingProvider;`.
- `:332-341` — remove the `embedder = embedder.map(StableSamplingProvider::wrap);`
  line (the wrap only accepts `LlmProvider`). Keep the tier wraps. Add a one-line
  comment explaining Voyage embeddings are deterministic so stable-sampling is a
  no-op for them.
- All `embedder: None` sites in this file (`:698, :731, :764, :850, :866, :911`)
  need **no change** — `None` infers the new type.

### `crates/callimachus-core/src/indexing/reindex_pass.rs`

- `:35-42` — add an `embedder: Option<Arc<dyn EmbeddingProvider>>` parameter to
  `run`'s signature.
- `:294` (the test `full_index` helper's `IndexPipeline { … embedder: None }`) —
  no change needed (`None` infers).
- After the existing downstream passes (after `:125`, before alias resolution at
  `:127`), add an embed step: call `embed_pass::run(db.as_ref(), corpus,
  embedder.clone(), opts).await?` so reindex embeds too. Import
  `crate::indexing::embed_pass` at the top.
- Update the four in-module test call sites of `run` (`:314, :345, :374, :413`) to
  pass `None` for the new parameter.

### `crates/callimachus-cli/src/commands/index.rs`

- `:13` — add `build_embedding_provider` to the `callimachus_llm` import.
- `:68-73` — replace `embedder: None, // TODO …` with a call to the shared builder
  (Approach step 6): construct `Option<Arc<dyn EmbeddingProvider>>` from
  `config.embedding`, **after** the fail-fast check.
- After `resolve_passes` at `:50` — add the fail-fast guard (Approach step 6):
  if the resolved pass list contains `Pass::Embed` and embeddings are not usably
  configured, `bail!` with an actionable message before the pipeline runs.

### `crates/callimachus-cli/src/commands/ingest.rs`

- `:19` / `:21` — import `build_embedding_provider`.
- `:84-89` — replace `embedder: None` with the builder call.
- After the pass list is resolved (`:58-70`) — add the same fail-fast guard keyed
  on whether `pass_list` contains `Pass::Embed`.

### `crates/callimachus-cli/src/commands/reindex.rs`

- `:10` / `:12` — import `build_embedding_provider`.
- `:82-94` — build the embedder and pass it as the new `embedder` argument to
  `reindex_pass::run`. Add the fail-fast guard: reindex always runs the downstream
  passes including (now) embed, so if `[embedding]` is configured-enabled it must
  be usable; if it is enabled-but-broken, fail loudly. Reindex does not take a
  `--pass` flag, so the trigger is "`[embedding].enabled == true`" — see step 6 for
  the exact rule.

### Tests (new / extended)

- `crates/callimachus-cli/src/config.rs` `#[cfg(test)]` — config construction +
  builder tests (Approach step 8).
- `crates/callimachus-llm/src/voyage.rs` `#[cfg(test)]` — wiremock test of request
  shape and response parsing (Approach step 8).
- `crates/callimachus-core/src/indexing/embed_pass.rs` `#[cfg(test)]` — replace
  `DryRunProvider` usage with a `StubEmbeddingProvider` and keep the existing three
  tests green; add a `count(embeddings) == count(chunks)` assertion (already
  present as `embedding_count == 3`).
- A loud-error test in `crates/callimachus-cli/src/commands/index.rs`
  `#[cfg(test)]` (Approach step 8).

## Approach

### 1. Define the `EmbeddingProvider` trait (`callimachus-llm/src/embedding.rs`)

```rust
use std::sync::Arc;
use crate::error::Result;

/// An embedding provider turns text into dense float vectors.
///
/// Implementations must be cheap to clone behind an `Arc` and safe to call
/// concurrently. The batch entry point is the primary one; `embed` is a
/// single-text convenience that defaults to a one-element batch.
#[async_trait::async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed a batch of texts in a single call. Returns one vector per input,
    /// in input order. Errors if the provider cannot satisfy the whole batch.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Embed a single text. Default impl delegates to `embed_batch`.
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed_batch(&[text.to_string()]).await?;
        out.pop()
            .ok_or_else(|| crate::error::LlmError::Other(
                "embedding provider returned no vector for single input".into(),
            ))
    }

    /// The model identifier written to storage and used as the Layer-2 cache
    /// key (e.g. `"voyage-code-3"`). Must be the real model name, not a label.
    fn name(&self) -> &str;
}

/// Blanket impl so `Box<dyn EmbeddingProvider>` works where the trait is required.
#[async_trait::async_trait]
impl EmbeddingProvider for Box<dyn EmbeddingProvider> {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        (**self).embed_batch(texts).await
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        (**self).embed(text).await
    }
    fn name(&self) -> &str {
        (**self).name()
    }
}
```

Note: there is **no** `supports_embeddings()` method. Possessing an
`EmbeddingProvider` *is* the capability — the old `supports_embeddings` silent
trap (`embed_pass.rs:41`) is eliminated by construction.

### 2. Implement `VoyageEmbeddingProvider` (`callimachus-llm/src/voyage.rs`)

Follow `openai_embed.rs` as the structural template (Client + usage Mutex).

- Constants: `const VOYAGE_EMBED_URL: &str = "https://api.voyageai.com/v1/embeddings";`
  and `const DEFAULT_MODEL: &str = "voyage-code-3";`.
- Struct fields: `api_key: String`, `model: String`, `client: reqwest::Client`,
  `usage: Arc<Mutex<ProviderUsage>>` (reuse `ProviderUsage` from `provider.rs`),
  and `input_type: &'static str` set to `"document"` (Voyage distinguishes
  `document` vs `query` input types; chunks being indexed are documents).
- `pub fn new(api_key: String, model: Option<String>) -> Self`.

**Voyage HTTP API shape** (POST, bearer auth):

Request body:
```json
{
  "model": "voyage-code-3",
  "input": ["text one", "text two"],
  "input_type": "document"
}
```

Response body:
```json
{
  "object": "list",
  "data": [
    { "object": "embedding", "embedding": [0.0, 0.1, ...], "index": 0 },
    { "object": "embedding", "embedding": [0.0, 0.2, ...], "index": 1 }
  ],
  "model": "voyage-code-3",
  "usage": { "total_tokens": 123 }
}
```

`embed_batch` implementation:
1. POST to `VOYAGE_EMBED_URL` with `.bearer_auth(&self.api_key).json(&body)`.
2. On non-success status: read body text, return
   `LlmError::Other(format!("Voyage embeddings returned {status}: {body}"))`.
3. Parse with serde structs (`VoyageResponse { data: Vec<VoyageObject>, usage:
   Option<VoyageUsage> }`, `VoyageObject { embedding: Vec<f32>, index: usize }`).
4. **Sort `data` by `index`** before extracting vectors — do not assume the API
   returns them in input order.
5. Assert `data.len() == texts.len()`; if not, return a loud `LlmError::Other`
   describing the count mismatch (covers the partial-failure risk).
6. Accumulate `usage.total_tokens` into `self.usage` and bump `calls`.
7. Return `Vec<Vec<f32>>` in input order.

`name(&self) -> &str { &self.model }` — returns the real model id so storage and
the Layer-2 cache key are correct.

### 3. Add `[embedding]` config (`callimachus-cli/src/config.rs`)

Add to `GlobalConfig` (after `model_tiers`, around line 15):

```rust
    #[serde(default)]
    pub embedding: Option<EmbeddingConfig>,
```

New struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EmbeddingConfig {
    /// Master switch. When false (or the whole [embedding] block is absent),
    /// embeddings are off and requesting --pass embed/all errors loudly.
    #[serde(default)]
    pub enabled: bool,
    /// Provider id. Currently only "voyage" is accepted.
    #[serde(default)]
    pub provider: Option<String>,
    /// Model name. Defaults to voyage-code-3 when absent.
    #[serde(default)]
    pub model: Option<String>,
    /// Inline API key. Lower precedence than api_key_env.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Name of the environment variable holding the API key.
    /// Takes precedence over api_key when both are present (PRD decision #2).
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Optional batch size hint (reserved; the per-chunk loop ignores it for
    /// this PR). Kept so a future batch path needs no config change.
    #[serde(default)]
    pub batch_size: Option<usize>,
}
```

Example config block (for the PR description / docs):
```toml
[embedding]
enabled = true
provider = "voyage"
model = "voyage-code-3"
api_key_env = "VOYAGE_API_KEY"
```

### 4. Add the builder (`callimachus-llm/src/resolve.rs`)

Mirror `ProviderConfig` / `build`. The CLI translates `EmbeddingConfig` →
`EmbeddingProviderConfig` (so the llm crate stays unaware of CLI types), then calls
the builder. Define in the llm crate:

```rust
#[derive(Debug, Default)]
pub struct EmbeddingProviderConfig {
    pub enabled: bool,
    pub provider: Option<String>,   // None or "voyage"
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
}

/// Build an embedding provider from config.
///
/// Returns:
/// - `Ok(None)` when embeddings are disabled (enabled == false). This is the
///   normal, fully-supported off state.
/// - `Ok(Some(provider))` when enabled and a key resolves.
/// - `Err(..)` when enabled but misconfigured (unknown provider, or no key
///   resolvable from api_key_env / api_key). Loud, actionable message.
pub fn build_embedding_provider(
    cfg: EmbeddingProviderConfig,
) -> Result<Option<Arc<dyn EmbeddingProvider>>> {
    if !cfg.enabled {
        return Ok(None);
    }
    let provider = cfg.provider.as_deref().unwrap_or("voyage");
    if provider != "voyage" {
        return Err(LlmError::Other(format!(
            "unknown embedding provider '{provider}'; only 'voyage' is supported"
        )));
    }
    // api_key_env takes precedence (PRD decision #2).
    let key = cfg
        .api_key_env
        .as_deref()
        .and_then(|name| std::env::var(name).ok())
        .or(cfg.api_key.clone())
        .ok_or_else(|| LlmError::Other(
            "embedding enabled but no API key found: set api_key_env to the name \
             of an environment variable holding your Voyage key (e.g. \
             VOYAGE_API_KEY), or set api_key inline".into(),
        ))?;
    let provider = VoyageEmbeddingProvider::new(key, cfg.model.clone());
    Ok(Some(Arc::new(provider)))
}
```

Export it from `lib.rs`.

### 5. (Deferred) batch path

The trait exposes `embed_batch`, but for this PR the embed pass keeps its existing
per-chunk `embedder.embed(&chunk.content)` loop (Layer-2 cache + idempotency live
in that loop and are non-trivial to preserve under batching). Document this in the
embed_pass doc-comment as a known future optimization. The `batch_size` config
field is plumbed but unused. This keeps the diff shallow while leaving the batch
shape available — satisfies the PRD's "must expose a batch entry point" by having
it on the trait + concrete provider, even though the pass calls it one-at-a-time.

### 6. Fail-fast wiring + loud error (the three commands)

**Rule for "usably configured":** `build_embedding_provider` returns
`Ok(Some(_))`. `Ok(None)` means disabled. `Err(_)` means enabled-but-broken.

**`index.rs` and `ingest.rs`** (embed is opt-in via pass selection):
1. Resolve the pass list first (already done).
2. Compute `let embed_requested = passes.contains(&Pass::Embed);`.
3. Build the embedder config from `config.embedding` and call
   `build_embedding_provider`.
4. Decision matrix (PRD decision #1 — fail-fast always, at command setup, before
   any pass runs):
   - `embed_requested && build → Err(e)` ⇒ `bail!("embeddings requested via --pass
     but not usable: {e}")`. Fail before the pipeline runs.
   - `embed_requested && build → Ok(None)` (disabled) ⇒ `bail!("--pass embed/all
     requested but [embedding] is disabled or absent in config; set [embedding]
     enabled = true with a Voyage api_key_env")`.
   - `embed_requested && Ok(Some(p))` ⇒ pass `Some(p)` into the pipeline.
   - `!embed_requested` ⇒ pass `None` into the pipeline regardless of config (never
     error in the off path — PRD success criterion). Do **not** even call the
     builder, or call it and discard, but never surface its error when embed is not
     requested.
5. Replace `embedder: None` in the `IndexPipeline { … }` construction with the
   resolved `Option<Arc<dyn EmbeddingProvider>>`.

**`reindex.rs`** (no `--pass` flag; reindex now always runs an embed step):
- The trigger is `config.embedding.as_ref().map_or(false, |e| e.enabled)`.
- If embeddings are **enabled**, call `build_embedding_provider`; on `Err`,
  `bail!` (enabled-but-broken is loud). On `Ok(Some(p))`, pass `Some(p)`.
- If embeddings are **disabled/absent**, pass `None` to `reindex_pass::run` — the
  embed step inside reindex_pass then no-ops via the `None` guard. This preserves
  byte-for-byte today's reindex behaviour when embeddings are off.

**`embed_pass.rs` guards:** keep the `None` guard (`:30-39`) — it is now only
reachable from the disabled/not-requested path, which is legitimate. **Remove** the
`supports_embeddings()` guard (`:41-48`) entirely — the new trait has no such
method and possessing the provider is the capability. The loud-error contract is
enforced at command setup (step 6 above), not inside the pass.

### 7. Test stub

Add a `StubEmbeddingProvider` in the embed_pass test module (and reuse the same
pattern in CLI/integration tests). It returns a fixed-width deterministic vector,
mirroring `DryRunProvider::embed`:

```rust
struct StubEmbeddingProvider;
#[async_trait::async_trait]
impl EmbeddingProvider for StubEmbeddingProvider {
    async fn embed_batch(&self, texts: &[String]) -> callimachus_llm::error::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| { let mut v = vec![0.0f32; 8]; v[0] = 1.0; v }).collect())
    }
    fn name(&self) -> &str { "voyage-code-3" }
}
```

(If `callimachus_llm::error::Result` is not re-exported, return
`anyhow::Result`-compatible via the crate's `Result` alias — confirm the export and
use the public path. `LlmError` and its `Result` alias are in
`callimachus-llm/src/error.rs`; export them from `lib.rs` if not already, or have
the trait's `Result` be `std::result::Result<_, LlmError>` which is already public
via the trait signature.)

### 8. Tests to write

**config.rs construction (unit, `callimachus-cli/src/config.rs`):**
- `embedding_disabled_yields_none`: `EmbeddingConfig { enabled: false, .. }` →
  `build_embedding_provider` returns `Ok(None)`.
- `embedding_enabled_with_key_yields_provider`: set a temp env var, `api_key_env`
  points at it, `enabled: true` → `Ok(Some)` and `provider.name() == "voyage-code-3"`.
- `embedding_enabled_without_key_errors`: `enabled: true`, no `api_key`, no
  resolvable `api_key_env` → `Err` whose message mentions a key.
- `api_key_env_takes_precedence_over_inline`: both set, env var present → key from
  env is used (assert by pointing env var at a sentinel and inline at another; the
  provider has no public key getter, so instead assert this at the
  `build_embedding_provider` level by checking it does not error and, if needed, add
  a `#[cfg(test)]` accessor — prefer asserting precedence logic directly in a small
  helper test).
- `unknown_provider_errors`: `provider: Some("openai")`, `enabled: true` → `Err`
  mentioning 'voyage'.

**voyage.rs HTTP (unit, wiremock):**
- Mount a `wiremock` server returning a canned 2-element response for a 2-element
  input; assert `embed_batch` returns two 3-dim vectors in `index` order (include
  an out-of-order `index` in the mock to prove the sort).
- Assert a non-2xx status yields an `Err` containing the status and body.
- Override the URL: add a `#[cfg(test)]` constructor or a private `with_base_url`
  so the test points at the wiremock server (follow how `anthropic.rs` tests inject
  the base URL — check that file for the pattern and mirror it).

**embed_pass.rs (existing tests, adapted):**
- Keep `embeds_three_chunks`, `embed_pass_is_idempotent`,
  `embed_pass_with_no_embedder_skips_gracefully`, swapping `DryRunProvider` for
  `StubEmbeddingProvider`. The count assertion `embedding_count == 3` already
  encodes `count(embeddings) == count(chunks)`.

**index.rs loud-error (integration-ish, `callimachus-cli/src/commands/index.rs`):**
- A test that invokes `super::run(..., Some("embed"), ...)` with a corpus present
  and a `GlobalConfig` whose `[embedding]` is absent/disabled, asserting the result
  is `Err` and the message mentions `embed` and config. Use the existing in-memory
  `SqliteBackend` test harness in that module.

**reindex embed wiring (optional, `reindex_pass.rs`):**
- Extend one existing reindex test to pass `Some(StubEmbeddingProvider)` and assert
  `db.embedding_count(&corpus.id) > 0` after reindex; pass `None` in the others
  (already required by the signature change).

### 9. Resolve the bug doc

Run:
```bash
~/.claude/bin/doc-mgr move /Users/hammer/Code/callimachus/.claude/bugs/open/embed-pass-unwired-in-cli.md resolved
```
Commit the move alongside the code change.

## Acceptance criteria

- `cargo build` and `cargo clippy --all-targets` pass with no new warnings.
- `cargo test -p callimachus-llm -p callimachus-core -p callimachus-cli` passes.
- The new `EmbeddingProvider` trait exists in `callimachus-llm` with `embed_batch`,
  `embed`, and `name`, and `embed_pass::run` depends on
  `Option<Arc<dyn EmbeddingProvider>>` — **not** `Arc<dyn LlmProvider>`.
- `build_embedding_provider` lives in `callimachus-llm` (`resolve.rs`) and is
  re-exported from `lib.rs`; the three commands call it rather than hand-rolling
  provider construction.
- With `[embedding]` disabled/absent and embed **not** requested, behaviour is
  unchanged: no Voyage calls, no errors, empty `embeddings` table (the existing
  `embed_pass_with_no_embedder_skips_gracefully` test still passes).
- `--pass embed` (or `--pass all`) requested while embeddings are disabled or
  enabled-but-keyless produces a non-zero exit with an actionable error **before**
  the pipeline runs (covered by the new index.rs loud-error test).
- The `supports_embeddings()` silent-skip guard at the old `embed_pass.rs:41-48` is
  removed.
- `reindex_pass::run` accepts an embedder parameter and runs an embed step; all its
  in-module tests compile and pass.
- The bug doc has been moved to `.claude/bugs/resolved/`.
- PR body references the bug doc and notes that the semantic-search read path is
  intentionally out of scope.

## Out of scope

- The semantic-search / similarity MCP query tool and `query/service.rs`. Do **not**
  change its `embedder` field type. Reading vectors back is separate work.
- Embedding natural-language artifacts (purposes, contracts, summaries, themes).
  Raw code chunks only.
- Multi-provider support beyond a `provider` field that currently only accepts
  `"voyage"`. No auto-detection, no second concrete provider.
- Any new storage migration. The `embeddings` / `embeddings_history` schema,
  provenance columns, history archival, and Layer-2 cache are present and
  sufficient. If a genuine schema gap surfaces, stop and raise it — do not invent a
  migration.
- A `dimensions` config field. Record the returned vector length via
  `StoredEmbedding::new` (PRD decision #4). Do not add dimension request/validate
  config.
- Converting the embed pass to true batched calls. The trait and provider expose
  `embed_batch`, but the pass keeps its per-chunk loop this PR.
- Removing or repurposing `OpenAiEmbeddingProvider` / the `embed` +
  `supports_embeddings` methods on `LlmProvider`. Leave them as-is; they are unused
  by the new path but removing them is a separate cleanup.
- Changing the default pass set. Embed stays opt-in via `--pass embed` / `--pass
  all` for `index`/`ingest`. Reindex runs embed only when `[embedding].enabled`.

## Suggested config

```yaml
suggested_config:
  cody:  { model: sonnet, effort: medium, rationale: "Wide diff across multiple crates (new trait, new HTTP provider, config struct, 3 wiring sites, test stub) — medium effort appropriate despite each piece being mechanical." }
  redd:  { skip: true, rationale: "Tests are fully specified in the plan; Cody writes them inline." }
  marty: { skip: true, rationale: "No refactoring scope — purely additive feature." }
  perri: { model: sonnet, effort: medium, rationale: "Cross-crate change with a silent-failure trap being removed; worth a thorough review pass." }
```
