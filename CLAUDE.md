# Callimachus

## MCP Server

This project ships with a pre-built index of its own codebase at `data/callimachus.pinakes`.
When you open this project in Claude Code, the `callimachus` MCP server starts automatically
via `.mcp.json` and exposes the full entity graph, summaries, purposes, and contracts
for this codebase as queryable tools.

The `calli` binary must be on PATH. Build it first if needed:

```bash
cargo build --release
export PATH="$PWD/target/release:$PATH"
```

Or install it system-wide:

```bash
cargo install --path crates/callimachus-cli
```

## Working with the index

```bash
# Query the bundled demo index
calli --pinakes data/callimachus.pinakes inspect entities callimachus
calli --pinakes data/callimachus.pinakes inspect runs callimachus

# Index a new corpus
calli corpus add code "My Project" /path/to/project
calli index my-project
```

## Codebase notes

- Core indexing logic: `crates/callimachus-core/src/indexing/`
- Storage layer: `crates/callimachus-core/src/storage/`
- Adapter pattern: `crates/adapters/` — one crate per corpus kind (book, code, wiki)
- MCP tools: `crates/callimachus-mcp/src/tools.rs`
- CLI commands: `crates/callimachus-cli/src/commands/`

See `docs/codebase-analysis.md` for a structural analysis of this codebase
produced by indexing itself.
