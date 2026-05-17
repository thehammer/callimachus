# Phase 6 — Reindex + watcher

## Context

Phase 5 delivered the corrections overlay and operator CLI tools. The index is now queryable and correctable, but it is static: if the source file changes (a new chapter is added, a codebase is updated), the operator must blow away the corpus and re-index from scratch, losing all corrections.

This phase implements two incremental update mechanisms:

1. **`calli reindex`** — one-shot incremental reindex. Detects what changed since the last index run and re-runs only the affected chunks through all passes.
2. **`calli watch`** — long-running daemon. Subscribes to filesystem events via `notify` and triggers incremental reindex automatically on every source change.

Both mechanisms preserve corrections (they are applied as an overlay and are never touched by re-indexing). Both are idempotent (content-addressed chunks mean unchanged content is silently skipped).

Reference: `docs/plans/callimachus-standalone.md §6.2–§6.3`.

## Target

- **Repo:** `callimachus`
- **Branch:** `main` (trunk-based)
- **Base:** Phase 5 commit

## Files to change

---

### `crates/callimachus-core` — change detection

New module: `src/indexing/change_detector.rs`. Expose from `src/indexing/mod.rs`.

#### `src/indexing/change_detector.rs`

```rust
pub struct ChangeSet {
    /// Paths (relative to corpus source root) of files that changed.
    pub changed_paths: Vec<String>,
    /// Chunk IDs that should be removed (source file deleted).
    pub deleted_chunk_ids: Vec<String>,
    /// Detected change strategy.
    pub strategy: ChangeStrategy,
}

#[derive(Debug, Clone)]
pub enum ChangeStrategy {
    /// Compare source mtime to corpus last_indexed_at.
    Mtime { since: chrono::DateTime<chrono::Utc> },
    /// Use git to list files changed since a commit/ref.
    Git { since_ref: String },
    /// Full reindex (no prior indexed_at or strategy unavailable).
    Full,
}

pub fn detect(
    corpus: &Corpus,
    db: &Database,
    since: Option<&str>,   // "--since" flag value: commit ref, ISO date, or None
) -> anyhow::Result<ChangeSet>
```

**Strategy selection** (in order):
1. If `since` is a git-looking ref (e.g. a SHA or `HEAD~3`) AND the corpus source path has a `.git` directory → `Git`.
2. If `since` is an ISO 8601 date string → `Mtime` with the parsed timestamp.
3. If `since` is None and corpus has `last_indexed_at` → `Mtime` with `last_indexed_at`.
4. Otherwise → `Full`.

**`Mtime` strategy**: Walk the source path with `walkdir`. Collect files where `mtime > since`. For `book` corpora (single EPUB/file), always return that single file if it changed.

**`Git` strategy**: Run `git diff --name-only <since_ref> HEAD` in the corpus source directory as a subprocess. Parse stdout (one path per line). Filter to files that exist (deletions are handled separately via `git show --stat`).

**Deleted chunks**: For each changed path, look up chunks in the DB whose `location_uri` starts with the corpus-relative path prefix. Any chunk that is no longer produced by the adapter after re-chunking will be orphaned — `reindex_pass` handles removal (see below).

---

### `crates/callimachus-core` — reindex pass

New file: `src/indexing/reindex_pass.rs`. Expose from `src/indexing/mod.rs`.

```rust
pub async fn run(
    db: &Mutex<Database>,
    corpus: &Corpus,
    adapter: &dyn SourceAdapter,
    llm: &dyn LlmProvider,
    change_set: &ChangeSet,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats>
```

Steps:
1. For each changed path in `change_set.changed_paths`:
   a. Re-run `adapter.discover` scoped to that path.
   b. Re-run `chunk_pass::run` for those sources only (upsert new content-addressed chunks).
   c. Re-run `structure_pass::run` for new/modified chunks only.
   d. Re-run `semantic_pass::run` for new/modified chunks only (set `semantic_processed = 0` on affected chunks first so semantic pass re-processes them).
   e. Re-run `summarize_pass::run` for affected chapter/corpus levels.

2. For each chunk ID in `change_set.deleted_chunk_ids`:
   - Delete from `chunks`, `edges` (cascade), any `summaries` whose `target_id` is the chunk ID.
   - Do NOT delete entities — they may appear in other chunks. `appearance_count` will naturally drift; a full re-index is needed to recalibrate.

3. After all changed paths: call `adapter.resolve_aliases` on the full entity set and apply new merges (but don't un-apply existing correction-based merges).

4. Update `corpus.last_indexed_at` via `corpus_store::set_last_indexed`.

5. Return stats: modified/added/deleted chunk counts.

---

### `crates/callimachus-core` — watcher

New file: `src/indexing/watcher.rs`. Expose from `src/indexing/mod.rs`.

Add to `callimachus-core/Cargo.toml`:
```toml
notify = { version = "6", features = ["macos_kqueue"] }
tokio = { workspace = true }
```

```rust
pub struct WatcherConfig {
    pub debounce_ms: u64,       // default 500
    pub concurrency: Option<usize>,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self { debounce_ms: 500, concurrency: None }
    }
}

pub struct CorpusWatcher {
    corpus: Corpus,
    db: Arc<Mutex<Database>>,
    adapter: Arc<dyn SourceAdapter>,
    llm: Arc<dyn LlmProvider>,
    config: WatcherConfig,
}

impl CorpusWatcher {
    pub fn new(
        corpus: Corpus,
        db: Arc<Mutex<Database>>,
        adapter: Arc<dyn SourceAdapter>,
        llm: Arc<dyn LlmProvider>,
        config: WatcherConfig,
    ) -> Self { ... }

    /// Start watching. Returns when SIGINT/SIGTERM is received.
    pub async fn run(&self) -> anyhow::Result<()> { ... }
}
```

**`run` implementation:**

```rust
// Pseudo-code
let (tx, mut rx) = tokio::sync::mpsc::channel(64);
let mut watcher = notify::recommended_watcher(move |event| {
    tx.blocking_send(event).ok();
})?;
watcher.watch(&source_path, notify::RecursiveMode::Recursive)?;

let shutdown = tokio::signal::ctrl_c();
tokio::pin!(shutdown);

let mut pending: HashSet<PathBuf> = HashSet::new();
let mut debounce_timer: Option<tokio::time::Instant> = None;

loop {
    tokio::select! {
        Some(event) = rx.recv() => {
            // Accumulate changed paths
            for path in event?.paths {
                pending.insert(path);
            }
            debounce_timer = Some(tokio::time::Instant::now()
                + Duration::from_millis(self.config.debounce_ms));
        }
        _ = async {
            if let Some(deadline) = debounce_timer {
                tokio::time::sleep_until(deadline).await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            // Debounce expired — process pending changes
            if !pending.is_empty() {
                let paths: Vec<_> = pending.drain().collect();
                let change_set = build_change_set(&paths, &self.corpus, &self.db)?;
                reindex_pass::run(&self.db, &self.corpus, &*self.adapter,
                                  &*self.llm, &change_set, &IndexOptions::default()).await?;
                tracing::info!("[watch] reindexed {} paths", paths.len());
            }
            debounce_timer = None;
        }
        _ = &mut shutdown => {
            tracing::info!("[watch] shutting down gracefully");
            break;
        }
    }
}
```

---

### `crates/callimachus-cli` — `calli reindex`

#### `src/commands/reindex.rs` (new)

```rust
pub async fn run(
    corpus_id: &str,
    since: Option<String>,
    dry_run: bool,
    provider_override: Option<String>,
    db: &Database,
    config: &GlobalConfig,
) -> anyhow::Result<()>
```

- Load corpus.
- Build provider and adapter.
- Call `change_detector::detect(corpus, db, since.as_deref())`.
- If `change_set.strategy == Full`, print a warning: "No change baseline found; running full reindex. Use `calli index` for initial indexing."
- Print detected change summary: `"Detected N changed paths, M deleted chunks."`
- If `--dry-run`: print the change set and exit without running passes.
- Call `reindex_pass::run`.
- Print final stats: added/modified/deleted chunks, elapsed time.

Wire through `src/main.rs` → `Command::Reindex`.

---

### `crates/callimachus-cli` — `calli watch`

#### `src/commands/watch.rs` (new)

```rust
pub async fn run(
    corpus_id: &str,
    debounce_ms: Option<u64>,
    provider_override: Option<String>,
    db_path: &Path,
    config: &GlobalConfig,
) -> anyhow::Result<()>
```

- Load corpus.
- Build provider and adapter.
- Print: `"Watching <source_path> for changes. Press Ctrl+C to stop."`.
- Construct `CorpusWatcher` with `WatcherConfig { debounce_ms: debounce_ms.unwrap_or(500), .. }`.
- Call `watcher.run().await`.

Note: `calli watch` opens its own DB connection (not shared with other CLI commands) to avoid WAL contention during long-running sessions.

Wire through `src/main.rs` → `Command::Watch`. Must be `#[tokio::main]` (already required for Phase 3 index command).

---

### CLI flag additions

Update `src/main.rs` to add the `--since` flag to `Command::Reindex` and `--debounce` to `Command::Watch`:

```rust
Reindex {
    corpus_id: String,
    #[arg(long)]
    since: Option<String>,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    provider: Option<String>,
},
Watch {
    corpus_id: String,
    #[arg(long, default_value = "500")]
    debounce: u64,
    #[arg(long)]
    provider: Option<String>,
},
```

Remove `not_yet_plain("reindex")` and `not_yet_plain("watch")`.

---

### `crates/callimachus-core/Cargo.toml`

Add:
```toml
notify = { version = "6", features = ["macos_kqueue"] }
walkdir = "2"
```

---

## Tests

### `callimachus-core/change_detector`

`src/indexing/change_detector.rs` `#[cfg(test)]`:

- **Mtime strategy**: Create a temp directory with two files. Set `corpus.last_indexed_at` to 1 hour ago. Modify one file. Call `detect` → `changed_paths` contains the modified file only.
- **Full strategy**: Corpus with no `last_indexed_at`, no `--since` → `strategy = Full`, all source files returned.
- **Git strategy** (integration, skipped if git not in PATH): init a git repo, commit a file, modify it, call `detect(since="HEAD~1")` → changed_paths contains the modified file.
- **Deleted chunks**: Seed a chunk whose `location_uri` starts with a path that doesn't exist on disk → appears in `deleted_chunk_ids`.

### `callimachus-core/reindex_pass`

`src/indexing/reindex_pass.rs` `#[cfg(test)]`:

- **Incremental reindex**: Index a small fixture corpus fully (2 chunks). Modify the content of chunk 1's source. Run reindex_pass with a ChangeSet covering only chunk 1's path. Assert chunk 1 has a new ID (content changed) and chunk 2 is unchanged.
- **Delete handling**: Seed 2 chunks. Run reindex_pass with `deleted_chunk_ids = [chunk2_id]`. Assert chunk 2 is no longer in the DB.
- **Idempotency**: Run reindex_pass twice on unchanged source → second run has 0 modified chunks (all skipped by content hash).
- **Corrections preserved**: Seed a merge correction. Run reindex_pass. Assert the correction still exists in `correction_store`.

### `callimachus-core/watcher`

`src/indexing/watcher.rs` `#[cfg(test)]`:

- Construct a `CorpusWatcher` with a temp directory source and `DryRunProvider`.
- Spawn `watcher.run()` as a background task.
- Write a file to the watched directory.
- Wait up to 2 seconds for the reindex to trigger (poll `chunk_store::count`).
- Send SIGINT equivalent (`tx.send(ctrl_c_signal)`). Assert the watcher task completes without panic.

### Integration test: end-to-end reindex

`crates/callimachus-core/tests/reindex_integration.rs`:

- Index a fixture plain-text corpus (2 "chapters" separated by blank lines).
- Record a correction (rename an entity).
- Modify the fixture file (change one sentence in chapter 2).
- Call `change_detector::detect` → one path returned.
- Call `reindex_pass::run`.
- Assert chapter 1 chunks are unchanged (same IDs), chapter 2 chunk has a new ID.
- Assert the correction from step 2 is still present in the DB.
- Assert `corpus.last_indexed_at` was updated.

## Acceptance criteria

- `calli reindex xenos` on an unchanged corpus prints "0 paths changed, 0 chunks modified" and exits cleanly
- `calli reindex xenos --since=<yesterday-date>` correctly detects modifications made today
- `calli watch xenos` starts, prints the watch message, and responds to Ctrl+C gracefully
- After modifying a source file while `calli watch xenos` is running, the chunk is re-indexed within 1 second
- Corrections recorded in Phase 5 survive a full `calli reindex`
- `cargo test --all` passes
- `cargo clippy --all -- -D warnings` passes
