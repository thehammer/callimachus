# Plan: Apple Developer Documentation Corpus — macOS 26 (v2)

**Status:** Implemented — PR open on `feature/apple-docs-corpus-v1`
**Branch:** `feature/apple-docs-corpus-v2`
**Base:** `feature/apple-docs-corpus-v1` (PR #35)

## Summary

v2 adds a dedicated `callimachus-adapter-docs` crate that reads DocC JSON directly,
producing a richer pinakes (`apple-docs-macos-26-v2.pinakes`) with:

- Rich entity taxonomy: `class`, `struct`, `enum`, `protocol`, `method`, `property`,
  `initializer`, `enum_case`, `notification`, `typealias`, `constant`
- Structured edges: `inherits_from`, `conforms_to`, `references_type`, `member_of`
- Per-method pages via `--depth 2` fetch (child symbol pages from `topicSections`)
- Availability metadata in entity description prefix for FTS coverage

The v1 pipeline and pinakes are unchanged. Both can coexist.

## New files

- `crates/adapters/callimachus-adapter-docs/` — new adapter crate
  - `Cargo.toml` — crate manifest
  - `src/lib.rs` — re-exports `DocsAdapter` and `create()`
  - `src/adapter.rs` — `SourceAdapter` impl
  - `src/docc.rs` — typed DocC JSON structs
  - `src/chunker.rs` — page + section chunk production
  - `src/extractor.rs` — structured edge extraction
  - `src/render.rs` — Rust port of v1 Python markdown renderer
  - `src/summarizer.rs` — section/page/corpus summarization
  - `tests/adapter_smoke.rs` — integration smoke test
  - `tests/fixtures/AppKit/NSView.json` — class with edges + topics
  - `tests/fixtures/AppKit/NSView-tag.json` — child property with availability
  - `tests/fixtures/AppKit/NSStackView.json` — class inheriting NSView, 2 protocols
- `docs/plans/apple-docs-corpus-v2.md` — this plan

## Modified files

- `Cargo.toml` — added `callimachus-adapter-docs` to workspace members
- `crates/callimachus-cli/Cargo.toml` — added adapter dependency
- `crates/callimachus-cli/src/commands/corpus.rs` — extended help text (book, code, wiki, docs)
- `crates/callimachus-cli/src/commands/index.rs` — added `"docs"` match arm
- `scripts/fetch-apple-docs.py` — added `--format` and `--depth` flags (backward-compat)
- `scripts/build-apple-docs-macos-26.sh` — added v2 block after v1 block
- `docs/apple-docs-corpus.md` — appended v2 section
- `.gitignore` — added explicit v2 patterns

## Out of scope

- No storage-schema changes
- No MCP tool-surface changes
- No changes to the wiki adapter
- No honest-provenance work
- No additional frameworks beyond AppKit, Combine, Foundation
- No cross-corpus query primitives
