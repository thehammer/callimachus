# Vocabulary Primer

A progression from first principles to fluency in Callimachus concepts and terminology.
For a compact alphabetical reference, see the [Glossary](glossary.md).

---

## Stage 1 — The problem

You have a large body of text. A novel. A codebase. A technical wiki. You can read it, but you can't *query* it — you can't ask "what are all the relationships between these concepts?" or "what does this function assume about its inputs?" or "how does the documentation's description of this service compare to what the code actually does?"

Search helps at the surface. But search finds *words*. What you want is to find *meaning*.

Callimachus is a pipeline that reads a body of text and builds a structured representation of its meaning: the things that exist in it, the relationships between those things, descriptions of their purpose and behavior, and the obligations they encode.

---

## Stage 2 — Corpus and corpora

The body of text you're indexing is called a [**corpus**](glossary.md#corpus). One book is a corpus. One codebase is a corpus. One wiki is a corpus.

The plural is **corpora** (Latin; the anglicized "corpuses" is acceptable but less precise). When you're working with multiple bodies of text together — say, a trilogy of novels — you have multiple corpora. Each corpus is indexed independently and can be queried individually or together via a [**collection**](glossary.md#collection).

The word "corpus" is borrowed from linguistics, where it means a body of text used for analysis. Callimachus uses it the same way, extended to non-textual source materials (code) whose content can still be read as text.

All corpora share a single index file — the [**pinakes**](glossary.md#pinakes). Named for the catalogue Callimachus of Cyrene compiled at the Library of Alexandria, it is the SQLite file (`.pinakes` extension) that holds everything the pipeline produces: entities, edges, summaries, purposes, contracts, and corrections. One pinakes per workspace; many corpora per pinakes.

---

## Stage 3 — Chunks

Before anything meaningful can happen, the corpus has to be divided into manageable pieces. This is chunking — the process of splitting a source into discrete units called [**chunks**](glossary.md#chunk).

A chunk has a [**kind**](glossary.md#chunk-kind) that describes what it represents. For a book: a `scene` or a `chapter`. For a codebase: a `function`, a `class`, a `file`. Chunk kinds are specific to the corpus type — there's no universal vocabulary here. This is the [**local ontology**](glossary.md#local-ontology): the taxonomy of things that exist within one type of corpus, owned by the [**adapter**](glossary.md#adapter) for that corpus type.

Chunks are the atomic unit. Everything downstream — entity extraction, summarization, purpose, contract — operates on chunks or on entities derived from chunks.

---

## Stage 4 — Entities and edges

Once text is chunked, the interesting work begins: extracting [**entities**](glossary.md#entity) — the named concepts that appear in the text. A character in a novel. A class in a codebase. A concept in a wiki.

Entities are connected by [**edges**](glossary.md#edge) — directed relationships with labels. `Eisenhorn appears_in Xenos`. `authenticate() calls validate_token()`. `PaymentService depends_on OrderRepository`. Together, entities and edges form the [**entity graph**](glossary.md#entity-graph): a structured representation of what the corpus contains and how its parts relate.

This extraction is the [**semantic pass**](glossary.md#semantic-pass) — "[semantic](glossary.md#semantic)" because it moves from raw text (syntax) to structured meaning. It uses a language model to identify entities and relationships that a simple search couldn't find.

---

## Stage 5 — The pipeline and its order

Callimachus processes a corpus through a sequence of [**passes**](glossary.md#pass), each building on the last:

1. **Chunk** — divide the source into chunks
2. **Structure** — extract structural parent/child relationships between chunks
3. **Semantic** — extract entities and edges from chunk content (LLM)
4. **Aliases** — merge entity name variants (`Eisenhorn` = `Gregor Eisenhorn`)
5. **Summarize** — generate prose descriptions at each level, rolling up from scenes to chapters to corpus via [**rollup**](glossary.md#rollup)
6. **Purpose** — describe *why* each entity exists
7. **Contract** — describe what each entity promises and assumes
8. **Theme** *(opt-in)* — identify cross-cutting patterns across the whole corpus

The order is not arbitrary. It encodes an [**epistemological**](glossary.md#epistemology) argument — a claim about what must be known before something else can be known. You can't extract meaning from text you haven't chunked. You can't describe intent before you've established what something is. You can't merge aliases before you've extracted entities. Each pass is epistemically dependent on the passes before it.

**Epistemology** is the branch of philosophy concerned with how knowledge is acquired and justified. The pipeline's ordering is its epistemology. This is distinct from what the pipeline is *about* — which is a question of ontology, covered next.

Together, [**purpose**](glossary.md#purpose), [**contract**](glossary.md#contract), and [**summary**](glossary.md#summary) form a three-layer description of each entity: what it does (summary), why it exists (purpose), and what it promises (contract).

These pre-built artifacts enable a fourth artifact at query time: the [**diegesis**](glossary.md#diegesis). Greek for "narrative exposition", a diegesis is a multi-paragraph explanation of a component assembled via BFS over `calls` edges — gathering purposes, summaries, and block descriptions without any new LLM calls. `calli inspect diegesis <corpus> <name>` (and the `explain_component` MCP tool) produce it.

---

## Stage 6 — Ontology: local and global

You've now encountered [**ontology**](glossary.md#ontology) in two forms.

[**Local ontology**](glossary.md#local-ontology) is the vocabulary specific to one corpus type: the [chunk kinds](glossary.md#chunk-kind) (`scene`, `chapter` for books; `function`, `class` for code), the [entity kinds](glossary.md#entity-kind) (`character`, `faction`; `class`, `module`). This vocabulary is owned by the [**adapter**](glossary.md#adapter) — the corpus-type-specific component that knows how to process a particular kind of source.

[**Global ontology**](glossary.md#global-ontology) is the shared vocabulary across all corpus types. The [**kind taxonomy**](glossary.md#kind-taxonomy) maps concrete kinds to [**abstract kinds**](glossary.md#abstract-kind): `function` (code) and `workflow` (wiki) both map to the abstract kind `process`. `character` (book) maps to `person`. This enables cross-kind queries — "find all process-like entities across this codebase and its documentation" — without collapsing the precision of local vocabulary.

**Ontology** is the branch of philosophy (and in computing, a formal model) concerned with what kinds of things exist and how they relate. The distinction between local and global ontology is the distinction between "what kinds of things exist in this corpus type" and "what kinds of things exist in the world."

The pipeline is [epistemological](glossary.md#epistemology) (how knowledge is acquired). The entity taxonomy is [ontological](glossary.md#ontology) (what kinds of things exist). These are different questions at different levels of the system, and Callimachus has different answers for each.

---

## Stage 7 — Corrections and evolution

No index is perfect on first pass. The [**corrections model**](glossary.md#corrections-model) provides a mechanism for applying semantic fixes without re-indexing: merging entities the [aliases pass](glossary.md#aliases-pass) missed, linking entities across corpora that refer to the same real-world concept, annotating entities with additional context.

The individual records are called [**scholia**](glossary.md#scholion) (singular: [**scholion**](glossary.md#scholion)) — a term borrowed from the marginal notes ancient scholars added to manuscripts. Like those notes, scholia sit alongside the main text without altering it: they are stored in the `scholia` table and applied at query time by the `CorrectionsEngine`. Apply them via `calli scholion apply`; `calli correct` remains as a deprecated alias.

The [**pipeline version**](glossary.md#pipeline-version) tracks schema generations. When new passes are introduced or schema changes are made, the version increments. `calli upgrade` brings older corpora forward without reprocessing from source. The goal is a single canonical state: all corpora at the current version, no historical divergence.

---

## Stage 8 — Putting it together

You now have the vocabulary to reason fluently about Callimachus:

- A [**corpus**](glossary.md#corpus) *(pl. corpora)* is indexed through a [**pipeline**](glossary.md#pipeline) of [**passes**](glossary.md#pass)
- Passes are ordered [**epistemologically**](glossary.md#epistemology) — by what must be known first
- Passes divide the corpus into [**chunks**](glossary.md#chunk) with adapter-owned [**chunk kinds**](glossary.md#chunk-kind) — the [local ontology](glossary.md#local-ontology)
- The [**semantic pass**](glossary.md#semantic-pass) extracts [**entities**](glossary.md#entity) and [**edges**](glossary.md#edge) into an [**entity graph**](glossary.md#entity-graph)
- The [**summarize pass**](glossary.md#summarize-pass) builds hierarchical prose via [**rollup**](glossary.md#rollup)
- Entities carry [**summary**](glossary.md#summary) (what), [**purpose**](glossary.md#purpose) (why), [**contract**](glossary.md#contract) (obligations), and [**abstract kind**](glossary.md#abstract-kind) (global [ontology](glossary.md#ontology))
- [**Collections**](glossary.md#collection) group corpora for cross-corpus queries
- [**Scholia**](glossary.md#scholion) fix semantic issues at query time without re-indexing
- [**Pipeline versions**](glossary.md#pipeline-version) ensure corpora can be [**upgraded**](glossary.md#upgrade) as the system evolves

Three Greek terms name the three artifacts that matter most:

| Term | Meaning | Artifact |
|---|---|---|
| [**Pinakes**](glossary.md#pinakes) | "tablets / lists" | The index file — the single SQLite store that holds everything |
| [**Scholion**](glossary.md#scholion) | "marginal note" | A post-indexing correction — a small, targeted fix applied at query time |
| [**Diegesis**](glossary.md#diegesis) | "narrative exposition" | A BFS-assembled explanation of a component — zero LLM calls at query time |

The naming is intentional. Callimachus of Cyrene compiled the Pinakes to make knowledge findable. Scholars added scholia to correct and extend what the original text said. Diegesis is the form in which that assembled understanding is narrated back. The three terms describe the file, the fix, and the explanation.
