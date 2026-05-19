# Query Patterns for Structural Analysis

How to answer the four structural quality questions from the README,
with and without Callimachus. The contrast is the point.

---

## The four questions

From `docs/codebase-analysis.md`:

1. Which functions or files are outliers in size vs. their peers?
2. Is complexity concentrated at the leaves or distributed across the hierarchy?
3. Is the codebase well-factored, and where is the most significant structural debt?
4. If you were a new contributor, where would this codebase most frustrate you?

---

## Q1 — Outliers in size

### With Callimachus

The code adapter runs static analysis at index time — branch count and body
line count per function. Functions above the complexity threshold (`branch_count
>= 3 || body_lines >= 20`) have block-level blurbs included in their contracts.
The presence of block blurbs is itself a queryable complexity signal.

```
entity_search(kind="function", corpus_id="callimachus")
  → scan entity contracts for block blurb presence
  → entity_get(<candidates>) for full contract + purpose text
  → entity_edges(<high-complexity candidates>) to check connectivity
```

The last step matters: a large function that is also heavily connected
(many edges in/out) is more concerning than one that is large but isolated.
Both signals are available without reading any code.

### Without Callimachus

```bash
# File sizes
find . -name "*.rs" | xargs wc -l | sort -n

# Function sizes — no clean equivalent; approximate:
grep -n "^\s*\(pub \)\?\(async \)\?fn " src/**/*.rs
# Then manually count lines between definitions, or reach for
# a separate tool (tokei, cargo-count, etc.)
```

You get size but not semantic weight. A 200-line function that does one
well-defined thing is fine. A 60-line function that owns three unrelated
concerns is not. The code has to be read to tell the difference.

---

## Q2 — Complexity distribution

### With Callimachus

```
explain_component("IndexPipeline")
  → BFS call graph assembled into narrative (zero new LLM calls)
  → reveals how many layers of delegation exist below the orchestrator

entity_edges(<top-level orchestrators>)
  → fan-out per level: high fan-out at top = well-distributed;
    high fan-out pooled at leaves = complexity has accumulated

entity_get(<orchestrators>) → purpose field
  → thin purpose + high fan-out = good delegation
  → rich purpose + high fan-out = god object
```

### Without Callimachus

Follow call chains by grep, building a mental call graph:

```bash
grep -n "semantic_pass\|summarize_pass\|chunk_pass" \
  crates/callimachus-core/src/indexing/pipeline.rs

# Then recurse into each pass
grep -n "fn run" crates/callimachus-core/src/indexing/semantic_pass.rs
```

Accurate but slow. The mental model doesn't survive context switching and
has to be rebuilt for each question. There is no queryable representation.

---

## Q3 — Factoring and structural debt

### With Callimachus

```
entity_search(kind="function", corpus_id="callimachus")
  → entity_get(<each>) → purpose field
  → flag: purposes that describe multiple unrelated concerns
  → flag: purposes that are vague or absent

entity_edges(<suspected god objects>)
  → fan-out > ~15 outgoing edges suggests excessive coupling

fts_search("TODO OR FIXME OR hack OR workaround")
  → surfaces known technical debt in source comments

entity_search_by_abstract_kind("component")
  → cross-kind view of all structural units
  → compare edge density across components
```

The purpose field is the most useful signal here. A well-factored function
has a purpose that can be stated in one sentence. A poorly-factored one
takes three sentences and uses "also" and "additionally."

### Without Callimachus

```bash
# Largest files as a structural debt proxy
wc -l crates/**/*.rs | sort -n | tail -20

# Coupling proxy: longest import lists
grep -c "^use " crates/**/*.rs | sort -t: -k2 -n | tail -10

# Known debt markers
grep -rn "TODO\|FIXME\|HACK\|unwrap()" crates/ --include="*.rs"
```

These are proxies for the actual questions. File size correlates with
but does not equal structural debt. Import count correlates with but
does not equal coupling. You still have to read.

---

## Q4 — New contributor friction

### With Callimachus

```
entity_search(kind="function")
  → entity_get(<each>) → filter: purpose IS NULL or purpose is empty
  → these are the "here be dragons" zones — heavily used, poorly explained

entity_get(<high fan-in entities>) → contract field
  → long assumption lists with no explanation = danger zones
  → many preconditions = caller burden

fts_search("unwrap OR panic OR unsafe OR todo!")
  → surfaces places the code assumes things it doesn't check

entity_edges(<entry points>) → incoming edge count
  → high fan-in + absent purpose = the worst combination
```

Absence of documentation is queryable: "which entities have no purpose
entry?" is a direct query. Without Callimachus, you only find undocumented
code where you happened to look.

### Without Callimachus

```bash
# Absent doc comments
grep -rn "^pub fn\|^pub async fn" crates/ --include="*.rs" \
  | grep -v "///"

# Panic / unwrap density
grep -rn "\.unwrap()\|panic!\|todo!\|unimplemented!" crates/ \
  --include="*.rs" | wc -l

# No structural equivalent for "missing intent"
# You have to read enough code to know what you don't understand
```

The fundamental gap: you can grep for what *is* there. You cannot grep
for what *is not* there. Callimachus makes absence a queryable property.

---

## The structural difference

With Callimachus, all four questions are answered by querying **pre-built
understanding**. The semantic layer was constructed at index time — the
analysis ran once, the results are stored, and query time is retrieval.

Without Callimachus, every question requires **re-comprehending the source**.
The analysis runs at question time, in your head, against raw code. The
answer only holds until the code changes or you lose the context.

The cost model is inverted. Callimachus spends LLM budget once, at index
time, to produce artifacts that answer many future questions cheaply. The
alternative spends human attention repeatedly, at question time, to answer
each question from scratch.
