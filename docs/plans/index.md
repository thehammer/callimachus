# Plans Index

Quick reference for all plans and their status.

---

## Active / Next Up

### Quality Improvement (B → A)
**`quality-improvement.md`** — Phased plan to make the codebase exceptional.
Generated 2026-05-31 by self-indexing callimachus with its own tool.

| Phase | Slug | Status |
|-------|------|--------|
| 0 | Workspace lint gate + baseline | Not started |
| 1 | Type API boundaries (EdgeDirection enum, SemanticWeight) | Not started |
| 2 | Mutex-poison safety in storage | Not started |
| 3 | Drive unwraps out of non-test code (per-crate) | Not started |
| 4 | Decompose oversized logic files | Not started |
| 5 | Reduce nesting / name anonymous blocks | Not started |
| 6 | Flip lints to deny, re-grade | Not started |
| 7 | Fix analyzer's bail! false positive (optional) | Not started |

Queue Phase 0 + 1 to Mother first. Phase 3 first crate needs human review to lock the error-type policy.

---

## Embedding / Voyage AI (Shipped PR #47)
- **`embed-pass-voyage-ai.md`** — PRD (Ada). What to build and why.
- **`embed-pass-voyage-ai-impl.md`** — Implementation plan (Archie). Shipped 2026-05-31.

Config: `[embedding]` in `~/Library/Application Support/callimachus/config.toml`.
Key: `VOYAGE_API_KEY` in `~/.zshenv`.
Model: `voyage-code-3`.

---

## Apple Docs Corpus
- **`apple-docs-corpus-v1.md`** — V1 plan (shipped PR #35).
- **`apple-docs-corpus-v2.md`** — V2 plan with dedicated docs adapter (PR #37, open).

---

## Feature Backlog (from `.claude/features/backlog/`)

| Feature | Notes |
|---------|-------|
| `embed-nl-artifacts.md` | Embed summaries/purposes/contracts. Depends on embed pass (now shipped). |
| `calli-index-provider-flag.md` | Per-invocation provider override |
| `data-schema-pinakes-adapter.md` | |
| `jira-pinakes-adapter.md` | |
| `pr-description-to-changeset.md` | |
| `theme-pass-default.md` | **Shipped PR #45** |
| `documentation-corpus-claude-api-docs.md` | Filed 2026-05-31. Use llms-full.txt path. |
| `documentation-corpus-apple-appkit.md` | Has PRD. |
| `uspto-use-case.md` | |

---

## Historical Phase Plans (completed)
Implementation plans for the original build-out of Callimachus:
`phase-02` through `phase-11`, `stage-0-change-detection.md`, `tiered-model-selection.md`,
`multi-model-artifact-storage.md`, `callimachus-standalone.md`, `session-corpus.md`.
