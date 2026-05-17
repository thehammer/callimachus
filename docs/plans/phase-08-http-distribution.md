# Phase 8 — HTTP server + distribution

## Context

Phase 7 delivered the code adapter. The MCP stdio server is the primary interface for LLM clients, but it requires a subprocess per connection and is not suitable for non-MCP integrations (web UIs, scripts, remote clients).

This phase implements two things:

1. **`calli serve`** — an HTTP REST API server that mirrors the MCP tool surface. Thin layer over the same `QueryService`. Enables browser-based clients, `curl` scripting, and programmatic integration without a Claude Desktop configuration.
2. **Distribution pipeline** — GitHub Actions release workflow that cross-compiles binaries for all targets and publishes them as GitHub Release assets. A Homebrew formula for macOS users.

Reference: `docs/plans/callimachus-standalone.md §3, §9`.

## Target

- **Repo:** `callimachus`
- **Branch:** `main` (trunk-based)
- **Base:** Phase 7 commit

## Files to change

---

### `crates/callimachus-http/Cargo.toml`

Replace the stub.

```toml
[package]
name = "callimachus-http"
version = "0.1.0"
edition = "2024"

[dependencies]
callimachus-core = { path = "../callimachus-core" }
axum = { version = "0.7", features = ["json", "tokio"] }
tower = "0.4"
tower-http = { version = "0.5", features = ["cors", "trace"] }
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
anyhow = "1"
```

---

### `crates/callimachus-http/src/lib.rs`

```rust
pub mod router;
pub mod handlers;
pub mod error;
pub use router::build_router;
```

---

### `src/error.rs`

Unified HTTP error type that maps `ToolResult` errors to HTTP status codes.

```rust
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({"error": self.message}))).into_response()
    }
}

impl From<callimachus_core::error::CalError> for ApiError { ... }

/// Map ToolResult errors:
/// not_found → 404, ambiguous → 422, invalid_input → 400, error → 500
pub fn tool_result_to_response<T: serde::Serialize>(
    result: callimachus_core::types::result::ToolResult<T>,
) -> axum::response::Result<Json<serde_json::Value>, ApiError>
```

---

### `src/router.rs`

Build the Axum router with all routes and CORS middleware.

```rust
use std::sync::Arc;
use callimachus_core::query::QueryService;

pub fn build_router(qs: Arc<QueryService>) -> axum::Router {
    use axum::routing::{get, post};

    axum::Router::new()
        // Corpus tools
        .route("/corpora",                   get(handlers::corpus_list))
        .route("/corpora/:id",               get(handlers::corpus_overview))
        // Search
        .route("/corpora/:id/search",        post(handlers::search))
        // Entity tools
        .route("/corpora/:id/entity/:name",  get(handlers::entity))
        .route("/corpora/:id/entity/:id/edges", get(handlers::entity_edges))
        .route("/corpora/:id/meet",          post(handlers::entity_meet))
        // Read tools
        .route("/corpora/:id/read",          get(handlers::read))
        .route("/corpora/:id/summary",       get(handlers::summarize))
        .route("/corpora/:id/related",       get(handlers::related))
        // Composite tools
        .route("/corpora/:id/chapter/:ch",   get(handlers::chapter_summary))
        .route("/corpora/:id/character/:name", get(handlers::character_profile))
        .route("/corpora/:id/scene",         post(handlers::find_scene))
        // Health
        .route("/health",                    get(handlers::health))
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods(tower_http::cors::Any)
                .allow_headers(tower_http::cors::Any),
        )
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(qs)
}
```

CORS is open (`Any`) because `calli serve` is local-only by default (bound to 127.0.0.1). Document this clearly.

---

### `src/handlers.rs`

One handler function per route. All handlers extract `State<Arc<QueryService>>` and call the appropriate `QueryService` method.

Pattern for GET handlers:

```rust
async fn corpus_list(
    State(qs): State<Arc<QueryService>>,
) -> axum::response::Result<Json<serde_json::Value>, ApiError> {
    let input = callimachus_core::query::types::CorpusListInput {};
    let result = qs.corpus_list(input).map_err(ApiError::from)?;
    Ok(Json(serde_json::to_value(result)?))
}
```

Pattern for POST handlers (body deserialization):

```rust
async fn search(
    State(qs): State<Arc<QueryService>>,
    Path(id): Path<String>,
    Json(mut input): Json<callimachus_core::query::types::SearchInput>,
) -> axum::response::Result<Json<serde_json::Value>, ApiError> {
    input.corpus_id = id;   // path param overrides body
    let result = qs.search(input).map_err(ApiError::from)?;
    Ok(Json(serde_json::to_value(result)?))
}
```

**Query param extraction** for GET handlers with inputs (e.g. `read`):

```
GET /corpora/:id/read?location=calli://xenos/ch/3&depth=full
```

Use `Query<HashMap<String, String>>` and manually map to the input type. Validate required params; return 400 if missing.

**`health` handler**: Returns `{"status": "ok", "corpus_count": N}`. Calls `corpus_list` internally.

---

### `crates/callimachus-cli` — `calli serve`

#### `src/commands/serve.rs` (new)

```rust
pub async fn run(
    host: &str,
    port: u16,
    db_path: &Path,
    config: &GlobalConfig,
) -> anyhow::Result<()>
```

- Open DB.
- Load corrections engine.
- Construct `QueryService::with_corrections(db, corrections)`.
- Wrap in `Arc`.
- Call `callimachus_http::build_router(qs)`.
- Bind with `tokio::net::TcpListener::bind(format!("{host}:{port}"))`.
- Print: `"Callimachus HTTP API listening on http://{host}:{port}"`.
- `axum::serve(listener, router).await`.

Wire through `src/main.rs` → `Command::Serve`. Remove `not_yet_plain("serve")`.

Add to `crates/callimachus-cli/Cargo.toml`:
```toml
callimachus-http = { path = "../callimachus-http" }
```

Update `Command::Serve`:
```rust
Serve {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value = "7700")]
    port: u16,
},
```

---

### `.github/workflows/release.yml` (new)

Cross-compilation workflow triggered on version tags (`v*`).

```yaml
name: Release

on:
  push:
    tags: ["v*"]

jobs:
  build:
    name: Build ${{ matrix.target }}
    runs-on: ${{ matrix.runner }}
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            runner: ubuntu-latest
          - target: aarch64-unknown-linux-gnu
            runner: ubuntu-latest
          - target: x86_64-apple-darwin
            runner: macos-latest
          - target: aarch64-apple-darwin
            runner: macos-latest
          - target: x86_64-pc-windows-msvc
            runner: windows-latest

    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}

      - name: Install cross (Linux aarch64)
        if: matrix.target == 'aarch64-unknown-linux-gnu'
        run: cargo install cross --git https://github.com/cross-rs/cross

      - name: Build (native)
        if: matrix.target != 'aarch64-unknown-linux-gnu'
        run: cargo build --release --target ${{ matrix.target }} -p callimachus-cli

      - name: Build (cross)
        if: matrix.target == 'aarch64-unknown-linux-gnu'
        run: cross build --release --target ${{ matrix.target }} -p callimachus-cli

      - name: Rename binary
        shell: bash
        run: |
          EXT=""
          if [[ "${{ matrix.target }}" == *windows* ]]; then EXT=".exe"; fi
          mkdir -p dist
          cp target/${{ matrix.target }}/release/callimachus-cli${EXT} \
             dist/calli-${{ matrix.target }}${EXT}

      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: calli-${{ matrix.target }}
          path: dist/

  release:
    name: Create GitHub Release
    needs: build
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/download-artifact@v4
        with:
          path: dist/
          merge-multiple: true

      - name: Create release
        uses: softprops/action-gh-release@v2
        with:
          files: dist/*
          generate_release_notes: true
          draft: false
          prerelease: ${{ contains(github.ref_name, '-') }}
```

---

### `Formula/callimachus.rb` (new, Homebrew)

Homebrew formula for macOS. Place in `Formula/` in the repo root (Homebrew tap consumers point at `thehammer/tap`).

```ruby
class Callimachus < Formula
  desc "Queryable index over books, code, and wikis — exposed as LLM tools via MCP"
  homepage "https://github.com/thehammer/callimachus"
  version "0.1.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/thehammer/callimachus/releases/download/v#{version}/calli-aarch64-apple-darwin"
      sha256 "PLACEHOLDER_SHA256_AARCH64"
    else
      url "https://github.com/thehammer/callimachus/releases/download/v#{version}/calli-x86_64-apple-darwin"
      sha256 "PLACEHOLDER_SHA256_X86_64"
    end
  end

  def install
    bin.install Dir["calli-*"].first => "calli"
  end

  test do
    assert_match "callimachus", shell_output("#{bin}/calli --version")
  end
end
```

Note: SHA256 values must be updated after each release. Document this in `CONTRIBUTING.md` under "Releasing".

---

### `docs/architecture.md` (new)

Reference document covering the system architecture. Sections:
1. Overview diagram (ASCII)
2. Crate dependency graph
3. Storage schema (tables, indexes, FTS5)
4. Indexing pipeline (passes, resumability)
5. Query service (12 tools, corrections overlay)
6. MCP transport (JSON-RPC, tool registration)
7. HTTP API (routes, error model, CORS)
8. Provider selection (auto-detect priority)

### `docs/mcp-tools.md` (new)

Complete reference for all 12 MCP tools: name, description, input schema, output schema, example request/response, error cases. One section per tool.

---

## Tests

### `callimachus-http`

`src/handlers.rs` `#[cfg(test)]` using `axum::test`:

- **`GET /health`**: empty DB → `{"status": "ok", "corpus_count": 0}`, status 200.
- **`GET /corpora`**: seed 2 corpora → returns array of 2.
- **`POST /corpora/:id/search`**: seed corpus + 3 chunks + FTS → search returns matching chunks.
- **`GET /corpora/:id/entity/:name`**: seed entity, GET by name → 200 with entity JSON.
- **`GET /corpora/:id/entity/nonexistent`**: 404 with `{"error": "..."}`.
- **`POST /corpora/:id/search`** with missing `query` field: 400.
- **`GET /corpora/invalid-id/read`** with valid `?location` but wrong corpus: 404.
- **CORS headers**: preflight OPTIONS request → response includes `access-control-allow-origin: *`.

### `callimachus-cli/serve`

`src/commands/serve.rs` `#[cfg(test)]`:

- Bind to a random port (`port = 0`), make a `GET /health` request using `reqwest`, assert 200.
- Verify the server shuts down cleanly when the runtime is dropped.

### Release workflow (manual verification)

No automated test for the release workflow. Acceptance criterion covers manual verification with a test tag.

## Acceptance criteria

- `calli serve` starts without error and responds to `GET /health` with 200
- `curl http://localhost:7700/corpora` returns the corpus list
- `curl -X POST http://localhost:7700/corpora/xenos/search -H 'Content-Type: application/json' -d '{"query":"Bequin"}' ` returns results
- Pushing a `v0.1.0` tag triggers the release workflow and produces 5 binary artifacts
- `cargo install --path crates/callimachus-cli` installs `calli` to `~/.cargo/bin/`
- `cargo test --all` passes
- `cargo clippy --all -- -D warnings` passes

## Release checklist (document in `CONTRIBUTING.md`)

1. Update version in all `Cargo.toml` files (`callimachus-core`, `callimachus-cli`, etc.)
2. Update `Cargo.lock` (`cargo check`)
3. Commit: `git commit -m "chore: bump version to vX.Y.Z"`
4. Tag: `git tag vX.Y.Z && git push origin vX.Y.Z`
5. Wait for GitHub Actions release workflow to complete
6. Update SHA256 values in `Formula/callimachus.rb` from the released binary files
7. Commit the formula update
