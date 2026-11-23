# Running LLM passes on a subset, with caps

The LLM passes (`summarize`, `purpose`, `contract`, `theme`) can run as
budget-aware, resumable runs over a chosen subset of a corpus. All of the flags
below work on `calli index` and `calli ingest`.

## Choosing entities: `--entities`

`--entities <spec>` restricts `summarize`, `purpose` and `contract` to matching
entities. It is repeatable; an entity is selected if it matches **any** spec.

| Spec | Selects |
|------|---------|
| `name:<glob>` | canonical name, case-insensitive, `*` and `?` wildcards |
| `kind:<kind>` | entities of that kind |
| `id:<id or name>` | one entity, by id or exact canonical name |
| `callers-of:<name>` | entities with a `calls` edge to `<name>` |
| `callees-of:<name>` | entities `<name>` calls |
| `touches:<name>` | entities with a `reads`, `writes` or `uses_table` edge to `<name>` |
| `@<path>` | a file with one spec (or bare name) per line; `#` starts a comment |

A bare word is shorthand for `id:<word>`.

```
calli index mylib --pass purpose --entities 'name:fun_00*' --entities 'callers-of:fun_0042'
calli index mylib --pass purpose --entities @evaluation-set.txt
```

For `summarize`, a chunk is selected when an entity located at it matches (a
file is selected when any of its functions is). The corpus-level summary and the
`theme` pass describe the whole corpus, so they only run when no filter is given.
Graph specs (`callers-of`, ...) are resolved once the entities exist, so a single
run can chunk, extract structure and then filter by the resulting graph.

## Caps

| Flag | Stops the run after |
|------|---------------------|
| `--max-calls <n>` | `n` LLM calls |
| `--max-input-tokens <n>` | about `n` input tokens (real counts when the provider reports them) |
| `--stop-after <duration>` | wall time: `90m`, `4h`, `1h30m` |

Caps count across all passes of the run. When one is reached, calls already
started finish, the pass stops cleanly, the run is recorded as `stopped` with a
reason such as `run cap reached: max_calls`, and later passes do not start.

## Resume by re-running

A pass skips every entity that already has an artifact for the model, so
**re-running the same command resumes**: it processes only what is left. A
stopped run does not advance the version anchor, so nothing is lost.

## Estimate without calling the LLM: `--estimate`

```
calli index mylib --pass decompile --entities 'name:fun_0000*' --estimate
```

For each selected LLM pass it computes the candidates exactly as the real run
would (same filter, same skip of existing artifacts, same cache check) and
renders each real prompt without sending it. Nothing is written: no summaries,
purposes, contracts, cache entries or run-log rows. Cache hits and single-child
reuse cost nothing.

The table shows entities, calls, estimated input and output tokens and wall
time. Input tokens are `chars / 3.5` plus a per-call overhead (700 for the lean
`claude-code` provider, 0 for the API); output tokens are half of `max_tokens`.
Wall time is `calls / calls_per_minute` for `claude-code`, and concurrency-based
for the API, which also gets an estimated USD figure.

The `semantic` and `aliases` passes are not estimated. Purpose and contract
prompts include the summary when one exists; before `summarize` has run the
purpose and contract numbers are a lower bound, and the output says so.
`--estimate` cannot be combined with `--with-history`.

## The `decompile` profile

`--pass decompile` expands to `chunk,structure,summarize,purpose`: for decompiler
output (C, one function per file) the structure pass already gives entities and
edges, so the `semantic` and `aliases` passes only cost calls. Comma lists work
too (`--pass decompile,contract`). To make it the default for a corpus, set
`"passes": "decompile"` (or a list) in the corpus config; `--pass` overrides it.

In a one-function-per-file corpus the file summary would only repeat the
function summary, so any summary level whose chunk has exactly one child summary
reuses it without a call.

## Dumping artifacts for evaluation

```
calli inspect artifacts mylib --entities @evaluation-set.txt
calli inspect artifacts mylib --entities 'name:fun_0000*' --json
```

Per entity: id, name, kind, location, the best summary (the entity's own, else
its chunk's), purpose and model, contract, and the latest artifact timestamp.
`--json` prints an array of objects.
