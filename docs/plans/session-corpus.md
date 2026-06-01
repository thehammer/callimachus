# Add a `sessions` corpus kind to Callimachus

## Context

Callimachus turns a corpus into a queryable entity graph with summaries,
themes, and honest provenance. Today it ships three corpus kinds — `code`,
`book`, `wiki` — each implemented as a `SourceAdapter`. This plan adds a
fourth: `sessions`, which indexes Claude Code session transcripts (the JSONL
files under `~/.claude/projects/`) as an *evolutionary corpus whose axis is
time rather than git history*.

The motivation and scope are set by the vision doc at
`docs/visions/sessions-corpus.md` — read it for the "why". In one line: a
developer's session history is a temporal corpus of the same essential shape
the indexer already handles (append-only, uniquely-identified, timestamped
units that accumulate and whose meaning refines as more arrive), and pointing
the existing machinery at it produces a *semantic memory of one's own work*
that beats grep.

This is **v1**: earn trust on solid ground (structural facts + per-session
problem/decision extraction + on-demand semantic search and recall), and
explicitly defer the speculative parts (cross-session theme emergence,
proactive session-start surfacing). The product decisions below are settled by
the operator and are **not** open for re-litigation in implementation:

- **Global corpus, project-faceted.** One `sessions` corpus for all projects.
  `cwd` is a per-entity metadata facet used to scope queries, not a corpus
  boundary. Cross-project intersection is a first-class value.
- **On-demand v1, proactive deferred.** No session-start surfacing in v1. Ship
  search and decision recall; earn proactive later.
- **Privacy is not a v1 constraint.** All session content is fair game.
- **Cross-machine sharing is not v1**, but must not be architected out (no
  machine-specific assumptions baked into IDs or schema).
- **Scholia are in v1.** The existing `apply_scholion` MCP tool, applied to
  session entities, is the feedback/correction mechanism that makes extraction
  trustworthy over time. No new scholia code is required — session entities are
  ordinary entities, so `apply_scholion` already works on them. v1 must verify
  this and document it, not build it.

### Architectural findings carried forward (already validated against the code)

These were established by a prior read of the codebase. They are load-bearing
assumptions for this plan; the executor should sanity-check them but not
re-derive them.

- **Adapter registration is trivial.** A new adapter is a new workspace crate
  plus a 4-line addition to `build_adapter` in
  `crates/callimachus-cli/src/commands/index.rs:100-107`.
- **Entity kinds are free-form strings** (`Entity.kind: String`,
  `crates/callimachus-core/src/types/entity.rs:11`). New kinds (`Session`,
  `Problem`, `Decision`, `FileTouched`) need **zero** schema changes. The only
  schema-adjacent work is an optional row in the `kind_taxonomy` table
  (migration `008_kind_taxonomy.sql`) to map concrete kinds → abstract kinds.
- **`Corpus.config: serde_json::Value`** (`types/corpus.rs:43`) is the home for
  session-root configuration. No new column needed.
- **The storage/provenance layer is already adapter-agnostic.**
  `AncestryReader` is a trait (`storage/ancestry.rs`); book/wiki pass `None` and
  fall back to literal-equality presence. `derived_at_sha` is an opaque string
  (`types/provenance.rs`) — a session ID is a legal SHA value.
- **The git-bound `history_walk.rs` is bypassed entirely in v1.** v1 does not
  walk a DAG. Each new session file enters as an `Added` source through the
  normal incremental pipeline (`ChangeManifest`), and the adapter overrides
  `changed_sources`/`current_version` so it never touches git2. SessionAncestry
  (the continuation/fork DAG) is explicitly **v2**.
- **Layer-2 cache** (`indexing/layer2_cache.rs`, `Layer2CacheKey` in
  `types/provenance.rs`) keys expensive LLM artifacts on
  `(artifact_kind, entity_id, content_hash, file_shape_hash, model,
  stable_sampling)`. This is the mechanism that bounds re-indexing cost and
  defends against LLM-nondeterminism phantom history. It must be used from day
  one for all Layer-2 session extraction.

### The biggest non-architectural risk

**LLM non-determinism creates phantom history.** If the same session is
re-interpreted and the model returns slightly different problems/decisions, the
corpus accretes spurious "changes." Defend with **stable sampling +
content-hash caching from day one** (see Extraction design). This is not
optional polish; it is the difference between a trustworthy corpus and noise.

## Target

- **Repo:** callimachus (`/Users/hammer/Code/callimachus`)
- **Branch:** `feature/session-corpus-v1`
- **Base:** `origin/main`

## Scope

### In scope (v1)

1. A new `callimachus-adapter-sessions` crate implementing `SourceAdapter`.
2. Discovery + chunking of session JSONL files into one chunk per session
   (see Open Question A for the unit decision and its v1 resolution).
3. **Layer-1 structural extraction** (no LLM): files-touched, repos/projects
   involved (from `cwd`/`gitBranch`), PR links, time span, message counts,
   `isSidechain`/agent-setting flags, parent/continuation hints recorded as
   metadata (edges deferred — see below).
4. **Layer-2 LLM extraction** (cached, stable-sampled): per-session *problems
   addressed* and *decisions reached*, each as an entity with a description and
   confidence.
5. Entity taxonomy: `Session`, `Problem`, `Decision`, `FileTouched`,
   `Project` (concrete kinds), with `kind_taxonomy` rows mapping to abstract
   kinds.
6. Per-session honest provenance: `Concrete(session_id)` on every entity the
   session produces.
7. Three MCP tools: `session_search`, `decision_history`, `file_history`.
8. Incremental indexing: a newly-completed session file is absorbed without a
   full reindex; already-indexed sessions are skipped.
9. Embeddings over session entities/chunks for semantic search (reuse existing
   `Pass::Embed`).
10. Verify and document that `apply_scholion` works on session entities.

### Explicitly deferred (NOT v1)

- Cross-session **theme emergence** and theme timelines (`Pass::Theme` for
  sessions). v1 may leave `extract_themes` as the default no-op.
- **Proactive** session-start surfacing.
- **SessionAncestry**: the continuation/fork DAG as graph edges. v1 records
  parent/continuation *hints* as entity metadata only; it does not build the
  cross-session edge graph or refine provenance along it.
- **Cross-session entity resolution / alias merging.** v1 does **not** merge
  `Problem`/`Decision` entities across sessions (accept duplicates).
  `resolve_aliases` returns empty. v1 measures the duplication rate so a future
  PR can decide whether resolution is worth it.
- **Composition with the code corpus** (linking a decision to code entities).
- Cross-machine pinakes sync.
- Any verbatim-transcript-browser surface (the existing `sessions` skill keeps
  that job).

## New files to create

### Crate: `crates/adapters/callimachus-adapter-sessions/`

- `Cargo.toml` — model on
  `crates/adapters/callimachus-adapter-code/Cargo.toml`. Dependencies:
  `callimachus-core`, `callimachus-llm`, `anyhow`, `serde`, `serde_json`,
  `async-trait`, `tokio`, `tracing`, `chrono`, `sha2`, `hex`. **Do not** depend
  on `git2`, `tree-sitter`, or `walkdir`-for-source (a small `walkdir` for
  discovery is fine but the conservative default discovery already walks files).
- `src/lib.rs` — `pub use adapter::SessionsAdapter;` and module declarations.
- `src/adapter.rs` — `SessionsAdapter` implementing `SourceAdapter`
  (`crates/callimachus-core/src/adapter/contract.rs:115`). Mirror the shape of
  `crates/adapters/callimachus-adapter-book/src/adapter.rs`.
- `src/parser.rs` — JSONL parsing into a typed `ParsedSession` (see types
  below). This is the only place that knows the raw JSONL record schema.
- `src/structure.rs` — Layer-1 structural extraction (Session/FileTouched/
  Project/PR entities + intra-session edges, no LLM).
- `src/extractor.rs` — Layer-2 LLM extraction of `Problem` and `Decision`
  entities (mirrors `callimachus-adapter-book/src/extractor.rs`).
- `src/summarizer.rs` — per-session summary (mirrors book summarizer).
- `tests/sessions_adapter.rs` — integration tests using a checked-in fixture
  JSONL file (see Acceptance criteria). Use `ProviderConfig::DryRun` for any
  pipeline test so no real LLM is called.
- `tests/fixtures/sample-session.jsonl` — a small (~30-line), hand-trimmed
  real-shaped session transcript covering: `user`, `assistant` (with a
  `tool_use` block), `attachment`, `pr-link`, `agent-setting`, `last-prompt`,
  `queue-operation`. Derive it by trimming a real file under
  `~/.claude/projects/` and redacting nothing (privacy is not a constraint) —
  but keep it small and committed so tests are hermetic.

### Key types (in `src/parser.rs`)

```rust
/// One parsed session file. The unit of the corpus (v1: one file = one session).
pub struct ParsedSession {
    pub session_id: String,          // from any record's sessionId
    pub cwd: Option<String>,         // last-seen cwd (project association facet)
    pub git_branch: Option<String>,  // last-seen gitBranch
    pub started_at: Option<String>,  // earliest timestamp (rfc3339)
    pub ended_at: Option<String>,    // latest timestamp
    pub first_user_prompt: Option<String>, // first user message text
    pub message_count: usize,        // user + assistant records
    pub is_sidechain: bool,          // any record with isSidechain=true
    pub agent_settings: Vec<String>, // distinct agentSetting values
    pub parent_session_ids: Vec<String>, // continuation hints (metadata only in v1)
    pub files_touched: Vec<String>,  // paths from Read/Edit/Write/Bash tool_use args
    pub pr_links: Vec<PrLink>,       // {pr_number, pr_repository, pr_url}
    pub transcript_text: String,     // concatenated user+assistant prose, for the chunk body
}
```

## Files to modify

- `Cargo.toml:2-11` (workspace `members`) — add
  `"crates/adapters/callimachus-adapter-sessions"` to the array.
- `crates/callimachus-cli/src/commands/index.rs:100-107` — add the registration
  arm to `build_adapter`:
  ```rust
  "sessions" => Ok(Arc::new(SessionsAdapter::new())),
  ```
  and the corresponding `use callimachus_adapter_sessions::SessionsAdapter;` at
  the top (`index.rs:4-6` import block).
- `crates/callimachus-cli/Cargo.toml` — add
  `callimachus-adapter-sessions = { path = "../adapters/callimachus-adapter-sessions" }`
  to `[dependencies]` (mirror the existing book/code/wiki adapter deps there).
- `crates/callimachus-core/migrations/` — add a new migration
  `015_session_kinds.sql` (next number after `014`) that inserts `kind_taxonomy`
  rows for the sessions corpus. **Do not** renumber existing migrations.
  ```sql
  INSERT INTO kind_taxonomy VALUES
      ('Session',     'sessions', 'event'),
      ('Problem',     'sessions', 'concept'),
      ('Decision',    'sessions', 'concept'),
      ('FileTouched', 'sessions', 'artifact'),
      ('Project',     'sessions', 'place');
  ```
  (Abstract-kind values: reuse existing taxonomy vocabulary if present —
  inspect current rows in `008_kind_taxonomy.sql` and any later additions; if
  `event`/`artifact` are not already in use, prefer existing terms like
  `concept`/`component`/`place` rather than inventing new ones. The exact
  abstract kinds are not load-bearing for v1 — keep them consistent and don't
  break `list_abstract_kinds`.)
- `crates/callimachus-core/src/storage/db.rs:8-25` — register the new migration
  in the `migrations()` vec:
  ```rust
  M::up(include_str!("../../migrations/015_session_kinds.sql")),
  ```
- `crates/callimachus-mcp/src/tools.rs` — add three `ToolDesc` entries to
  `TOOL_LIST` (the `Lazy<Vec<ToolDesc>>` at `tools.rs:13`). Update the doc
  comment count at `tools.rs:12` ("All 27 Callimachus tools…").
- `crates/callimachus-mcp/src/dispatch.rs` — add three match arms in `dispatch`
  (`dispatch.rs:25`) routing to the new query-service methods.
- `crates/callimachus-core/src/query/service.rs` — add three methods:
  `session_search`, `decision_history`, `file_history` (see MCP tools below).
  Their input/output types go in the query types module alongside
  `SearchInput`/`SearchOutput` (find where those are defined — same crate — and
  follow the pattern).

## Approach

1. **Scaffold the crate.** Create
   `crates/adapters/callimachus-adapter-sessions/` with `Cargo.toml` and a
   `SessionsAdapter` whose methods initially mirror `BookAdapter`
   (`callimachus-adapter-book/src/adapter.rs`). Add it to the workspace members
   and to the CLI's `build_adapter`. Confirm it compiles and that
   `calli corpus add sessions "Sessions" <path>` followed by a dry-run index
   selects the adapter (mirror the `code_corpus_selects_code_adapter` test in
   `index.rs:260`).

2. **Override the version/change-detection seam so it never touches git.** In
   `SessionsAdapter`, keep the default `current_version` (hash-of-hashes over
   the source tree is fine for v1 — it is content-stable and git-free) **or**
   override it to a cheaper signal if the default proves slow over 15k files
   (e.g. a hash over `(path, mtime, size)` tuples). Keep the default
   `changed_sources` (conservative `Added`-on-diff). The net effect: new/changed
   session files surface as `Added`/`Modified`; `AncestryReader` is never
   needed; the pipeline runs `Pass::History` harmlessly (it just computes the
   manifest). **Do not** add `Pass::History`'s git walk — `SessionsAdapter` has
   no git handle and must not acquire one.

3. **Discover sessions.** `discover(source)` walks `source` for `*.jsonl`
   files and returns one `DiscoveredSource { path, kind: "session", meta }`
   per file. `source` is the directory configured on the corpus (default
   `~/.claude/projects`, but read it from `corpus.source` — the operator sets it
   at `corpus add` time). Skip files that fail to parse rather than aborting the
   run (transcripts are messy — degrade gracefully, log a warning).

4. **Parse + chunk.** `parser::parse(path)` reads the JSONL line-by-line into a
   `ParsedSession` (tolerating unknown record types and malformed lines).
   `chunk(source)` produces **one chunk per session** (v1 unit decision — see
   Open Question A), `kind = "session"`, with:
   - `content` = a normalized transcript text (concatenated user + assistant
     prose, tool calls summarized as `[tool: Bash] <cmd first line>` etc.,
     attachments and bookkeeping records dropped). This content is what
     `Chunk::new` content-hashes (`types/chunk.rs:54`), so it must be
     **deterministic** given the file — sort nothing that has meaningful order,
     but do not include timestamps or volatile fields in the body.
   - `location` = `Location::new(corpus_id, format!("session/{session_id}"))`.
     `format_location` returns `chunk.location.path`; `parse_location` mirrors
     book's implementation (`callimachus-adapter-book/src/adapter.rs:113`).
   - The `ParsedSession` is stashed in `source.meta` (or re-parsed in
     `extract_structure`) so structural extraction has the typed facts.

5. **Layer-1 structural extraction.** `extract_structure(chunk)` returns an
   `ExtractedStructure` with:
   - A `Session` entity (`canonical_name = session_id`, description = first user
     prompt truncated, with `cwd`, `git_branch`, time span, message count,
     `is_sidechain`, `agent_settings`, `parent_session_ids` recorded in the
     description or — preferred — as structured fields if the entity model
     allows; otherwise encode as a compact prefix in `description`). The
     `Session` entity is the structural anchor.
   - A `Project` entity per distinct `cwd` (canonical_name = cwd, deduped within
     the session). Edge `Session --touched_project--> Project`.
   - A `FileTouched` entity per distinct file path (canonical_name = path).
     Edge `Session --touched_file--> FileTouched`. **This is the substrate for
     `file_history`.** These are deterministic facts — no LLM, no
     nondeterminism.
   - One `pr_link` recorded as an entity or edge (`Session --opened_pr--> PR`),
     canonical_name = pr_url.
   - Set `derived_at_version` is handled by the pipeline; the per-entity
     `Concrete(session_id)` provenance is established because the chunk enters
     as `Added` at `current_version` — **but note**: the manifest's
     `current_version` is a tree-hash, not the session_id. See step 7 for how to
     make provenance carry the session_id.

6. **Layer-2 LLM extraction (cached + stable-sampled).** `extract_with_llm(chunk,
   llm)` runs only on `kind == "session"` chunks. It prompts the model to return
   structured JSON: a list of *problems addressed* and a list of *decisions
   reached*, each `{title, statement, confidence}`, with an explicit instruction
   to **distinguish what was decided from what was considered and dropped**
   (the central interpretive risk). Map each to a `Problem` / `Decision` entity
   and a `Session --addressed--> Problem` / `Session --decided--> Decision`
   edge. **Before** calling the LLM, build a `Layer2CacheKey`
   (`types/provenance.rs:Layer2CacheKey`) with `artifact_kind =
   "session_extract"`, `content_hash = chunk.id`, `file_shape_hash =
   chunk.file_shape_hash`, `model`, and the run's `stable_sampling` flag; look
   it up via the existing `layer2_cache` path the other passes use (study
   `indexing/purpose_pass.rs` / `embed_pass.rs` for the lookup/store pattern).
   On hit, deserialize the cached payload and skip the call. This is the
   phantom-history defense: identical session content + model ⇒ identical
   extraction, no spurious change.

7. **Make provenance carry the session_id.** The honest-provenance goal is that
   a session entity reads as `Concrete(session_id)`, not
   `Concrete(<tree-hash>)`. Investigate where the chunk-pass / structure-pass
   stamps `derived_at_version` (`indexing/structure_pass.rs:69-78`,
   `semantic_pass.rs:128`) and how it flows to the
   `(derived_at_kind, derived_at_sha)` columns. The cleanest v1 approach that
   stays inside the existing seams: have the adapter expose the session_id as
   the per-source version by overriding `changed_sources` to return one
   `ChangedSource` per session whose `commit_meta.sha = session_id`, and confirm
   the structure/semantic passes propagate `commit_meta` → provenance for that
   source. If that propagation does not exist for non-git corpora today
   (likely — `commit_meta` is "only populated for git-backed code corpora" per
   `change_manifest.rs:35`), then **v1 falls back to the honest, correct-but-
   coarse behavior**: entities are stamped `Concrete(<current_version>)` and the
   *session_id is recorded on the `Session` entity and in the chunk location*.
   Recall queries sort/attribute by the session's own timestamp + id, not by the
   provenance SHA. **Do the smaller thing**: do not refactor the provenance
   propagation path in v1 unless a one-line change makes `commit_meta.sha` flow
   through. Document which path was taken. (See Open Question B — this is the
   "what does `Concrete(session_id)` mean" question; v1's answer is "it means
   the Session entity's id field and timestamp; the provenance column may be
   coarser and that's acceptable because v1 has no DAG to refine against.")

8. **Summary + embeddings.** Implement `summarize` (per-session one-paragraph
   summary, cached via Layer-2 like book's). Embeddings come for free from the
   existing `Pass::Embed` over chunks/entities; ensure the MCP server can be run
   with an embedder so `session_search` does semantic search (see step 10). No
   sessions-specific embed code needed.

9. **Skip theme + alias passes for v1.** Leave `extract_themes` and
   `resolve_aliases` as the trait defaults / empty. Do not register
   `Pass::Theme` as part of the default sessions index. (The CLI `index`
   command's default passes — `index.rs:148-158` — already exclude `Theme`;
   confirm a default `calli index <sessions-corpus>` run does not invoke theme
   extraction.)

10. **MCP tools.** Add to `QueryService` (`query/service.rs`) and wire through
    `tools.rs` + `dispatch.rs`:

    - **`session_search`** — semantic + FTS over session entities/chunks.
      Input: `{ corpus_id, query, cwd?, limit? }`. `cwd` is an optional facet
      filter (substring/exact match on the `Project`/`Session` cwd metadata).
      Implement as a thin wrapper over the existing `search` path
      (`service.rs:125`) with mode defaulting to `hybrid`, plus a post-filter on
      `cwd`. Returns ranked sessions with snippet + session_id + timestamp.
    - **`decision_history`** — Input: `{ corpus_id, topic, cwd?, limit? }`.
      Semantic search restricted to `kind = "Decision"` entities matching
      `topic`, returned **timestamp-sorted, newest first**, each with its
      statement, confidence, owning session_id, and date. (This is the degraded
      v1 of "what did I decide about X, and when" — no ancestry needed; ordering
      is by the session's own timestamp.)
    - **`file_history`** — Input: `{ corpus_id, path, limit? }`. Looks up the
      `FileTouched` entity by canonical_name == path (exact, then suffix match),
      returns every `Session` with a `touched_file` edge to it, newest first,
      each with session_id, date, cwd, and the session's problem/decision
      summaries. This is pure Layer-1 graph traversal — deterministic and
      reliable, the highest-confidence tool.

11. **Verify scholia.** Add a test (or a documented manual check) that
    `apply_scholion` (`tools.rs:377`, dispatch arm) accepts a session
    `Decision` entity id and that `list_scholia` returns it. No new code expected
    — this is a verification + a sentence in the adapter's module doc / the
    project CLAUDE.md.

12. **Cost / cadence decision (all-history vs. from-now).** See the dedicated
    section below — implement the recommendation: **from-now-forward by
    default, all-history opt-in.**

## The "all history vs. from now" decision

~15,600 session JSONL files exist today under `~/.claude/projects/` (verified).
Indexing every one through a Layer-2 LLM pass is expensive and front-loads all
cost before any value is proven. The Layer-2 content-hash cache bounds
*re-indexing* cost (a re-run is nearly free) but not the *first* pass.

**Recommendation, to implement in v1:** *from-now-forward by default,
all-history opt-in.*

- The corpus's `source` directory is `~/.claude/projects`. Add a corpus
  `config` field (in `Corpus.config` JSON) `indexed_after`: an optional RFC3339
  timestamp. `discover` skips session files whose `started_at` (earliest
  record timestamp) is before `indexed_after`. Default `indexed_after` = the
  corpus `created_at` (i.e. "from now forward").
- To index all history, the operator sets `indexed_after` to null/epoch in the
  corpus config (documented in the adapter module doc and CLAUDE.md). This makes
  the expensive choice explicit and reversible.
- Incremental runs pick up only sessions newer than the last indexed version
  via the normal `ChangeManifest` path, so steady-state cost is one Layer-2
  extraction per new completed session — bounded and cheap.
- The Layer-2 cache guarantees that re-running the index (e.g. after an adapter
  version bump) does not re-pay for unchanged sessions.

This keeps v1 affordable, proves value on recent sessions first, and leaves the
all-history corpus one config edit away — consistent with the operator's "let's
see what the indexing shows us" posture.

## MCP tools — signatures

```jsonc
// session_search
{ "corpus_id": "string", "query": "string",
  "cwd": "string?",            // optional project facet (path substring)
  "mode": "keyword|semantic|hybrid", // default "hybrid"
  "limit": "integer?" }        // default 20
// → [{ session_id, cwd, started_at, snippet, score }]

// decision_history
{ "corpus_id": "string", "topic": "string",
  "cwd": "string?", "limit": "integer?" }   // default 20
// → [{ decision: {title, statement, confidence}, session_id, date, cwd }]
//   sorted newest-first by session timestamp

// file_history
{ "corpus_id": "string", "path": "string", "limit": "integer?" }
// → [{ session_id, date, cwd, problems: [...], decisions: [...] }]
//   sorted newest-first
```

## Open questions for implementation

These two are genuinely open (the operator left them to design). v1 must pick
the pragmatic answer and **document the choice in the adapter's module doc**:

**A. Session unit: file vs. logical thread.** A single JSONL file is the obvious
unit, but sessions fork (Mother-spawned children, agent sub-sessions,
resumptions linked by `parentUuid` / continuation). **v1 decision: the file is
the unit.** One JSONL file → one `Session` chunk/entity, keyed by `session_id`.
Continuation/parent relationships are recorded as *metadata hints* on the
`Session` entity (`parent_session_ids`) but are **not** stitched into a
cross-file logical thread. This is the analog of honest-provenance's "name-only"
identity choice — the cheaper, well-defined option that we can refine later.
Rationale: the logical-thread unit requires the SessionAncestry DAG, which is
explicitly v2; building it now would block v1 on the hardest part. The
`parent_session_ids` metadata is captured now precisely so v2 can build the DAG
without re-indexing.

**B. What `Concrete(session_id)` means for an entity (and a future theme).** In
the code corpus, `Concrete(sha)` means "this commit's diff touched the bytes
this artifact derives from." For a session entity, the analog is "this session
is the substrate this artifact was extracted from." **v1 decision:** a
`Session`/`Problem`/`Decision`/`FileTouched` entity's *origin session* is
recorded authoritatively as the `Session` entity it derives from
(the session_id, available on the entity and via its `Session --…--> X` edge)
and ordered by that session's timestamp. Whether the
`(derived_at_kind, derived_at_sha)` provenance columns literally carry the
session_id depends on whether `commit_meta.sha` propagation works for non-git
corpora (step 7); if it does not cheaply, the columns carry the coarser
`current_version` and **that is acceptable for v1** because there is no DAG and
no theme-refinement to make the column's precision matter yet. The *honest*
v1 stance: do not claim per-session provenance precision in the column unless it
is actually there; the session_id lives on the entity regardless. Themes are
out of scope for v1, so "what `Concrete(session_id)` means for a theme" is
deferred with the theme pass.

## Acceptance criteria

- `cargo build --workspace` and `cargo test --workspace` pass.
- A new `callimachus-adapter-sessions` crate exists, is a workspace member, and
  `build_adapter` in `index.rs` returns a `SessionsAdapter` for corpus kind
  `"sessions"`. A test analogous to `code_corpus_selects_code_adapter`
  (`index.rs:260`) covers this with a dry-run.
- The adapter has no `git2` dependency (verify: `git2` does not appear in
  `crates/adapters/callimachus-adapter-sessions/Cargo.toml`).
- Given the committed fixture `tests/fixtures/sample-session.jsonl`:
  - `discover` returns exactly one `DiscoveredSource`.
  - `parse` extracts the correct `session_id`, `cwd`, at least one
    `files_touched` entry, and the `pr-link`, and tolerates the `attachment` /
    `agent-setting` / `queue-operation` / `last-prompt` records without error.
  - `extract_structure` produces a `Session` entity plus at least one
    `FileTouched` entity and the corresponding `touched_file` edge.
  - A full dry-run pipeline (`ProviderConfig::DryRun`) over the fixture
    completes without error and writes entities.
- Re-indexing the same fixture corpus a second time (no source change) processes
  **zero** sessions via the change manifest (incremental skip works), and a
  re-run with `--full` re-upserts the same entities without creating duplicates
  (mirror `full_flag_forces_reupsert`, `index.rs:276`).
- Layer-2 session extraction goes through the `layer2_cache`: a test or a logged
  assertion shows a cache hit on the second extraction of identical session
  content with the same model.
- Migration `015_session_kinds.sql` is registered in `db.rs` and a fresh DB
  open applies it; `list_abstract_kinds` does not error after it.
- `session_search`, `decision_history`, and `file_history` are present in
  `TOOL_LIST` (`tools.rs`) and have working `dispatch` arms; a test asserts all
  three names appear in `tools_list_json()` (mirror the scholia registration
  tests at `tools.rs:454-501`). Each returns a well-formed (possibly empty)
  result against an indexed fixture corpus.
- `file_history` against the fixture returns the session that touched the
  fixture's known file path.
- `apply_scholion` accepts a session entity id and `list_scholia` returns it
  (test or documented manual verification).
- The adapter module doc (`src/lib.rs` or `src/adapter.rs`) documents: the v1
  session-unit decision (A), the provenance/`session_id` decision (B), and the
  all-history-vs-from-now config (`indexed_after`).
- PR body references the vision doc (`docs/visions/sessions-corpus.md`) and this
  plan, and states v1 scope + explicit deferrals.

## Out of scope

- Do **not** build SessionAncestry / continuation-DAG edges across files. Record
  `parent_session_ids` as metadata only.
- Do **not** merge `Problem`/`Decision` entities across sessions. No
  cross-session alias resolution. Accept duplicates; the corpus measures the
  duplication rate for a future decision.
- Do **not** implement cross-session theme emergence or theme timelines. Leave
  `extract_themes` as the default no-op.
- Do **not** implement proactive session-start surfacing or any session-start
  harness integration.
- Do **not** add a verbatim-transcript-browser MCP tool; the existing `sessions`
  skill owns raw browsing.
- Do **not** touch `history_walk.rs` or add a git handle to the sessions adapter.
- Do **not** refactor the provenance-propagation path (`structure_pass`/
  `semantic_pass`/`history_layer`) beyond, at most, a one-line change to let
  `commit_meta.sha` flow through for non-git corpora. If it's more than trivial,
  take the coarse-provenance fallback (step 7 / Open Question B) and move on.
- Do **not** renumber or edit existing migrations; add `015` only.
- Do **not** add cross-machine sync, filtering, or privacy controls.

```yaml
suggested_config:
  cody:
    model: opus
    effort: high
    rationale: "New cross-crate adapter + provenance-seam judgement calls + 3 MCP tools wired through query service, dispatch, and tool list; correctness-sensitive."
  redd:
    model: sonnet
    effort: high
    rationale: "First sessions fixture + incremental-skip, Layer-2 cache-hit, and adapter-registration tests; coverage is the trust signal for a new corpus kind."
  marty:
    model: sonnet
    effort: medium
    rationale: "Standard pass to consolidate adapter scaffolding against the book/code adapter patterns and dedupe parser/structure helpers."
  perri:
    model: sonnet
    effort: high
    rationale: "Reviewer must catch git-coupling leaks, phantom-history (cache/determinism) gaps, and provenance-honesty regressions across the indexing seam."
```
