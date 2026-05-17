# Callimachus at Scale

Notes on large-corpus applications and the infrastructure they'd require.
Written as a thinking document — not a roadmap, but a set of observations
worth revisiting.

---

## The Patent Office Application

The problem the patent office has is almost exactly the problem Callimachus
is designed for: a massive corpus of prior art that examiners need to reason
about at query time, under time pressure, without being able to load the whole
thing into context.

Current examiner workflow is reactive — read a new application, search for
prior art. A pre-indexed corpus inverts this. Claims in a new application
become a query against a graph of already-understood prior art. "Does claim 7
read on anything in G06F 40/30 filed between 2018 and 2023?" becomes
retrieval-and-assembly rather than search-and-read.

Patents have exactly the structure Callimachus exploits:

| Patent element | Callimachus concept |
|---|---|
| Specification | Chunk content |
| Independent claims | Entities |
| Dependent claims | Edges (dependency relationships) |
| Prior art citations | Cross-corpus collection links |
| CPC classification | Corpus kind / collection membership |
| Abstract | Summary |
| Claims language | Contract (obligations, assumptions) |

The irony: you could use Callimachus to determine whether Callimachus itself
is patentable. Self-referential in the same way as `data/callimachus.db`.

---

## Corpus Scale

The USPTO has issued approximately 11 million patents, with ~4 million
published applications. New patents issue at ~400,000/year.

| Metric | Estimate |
|---|---|
| Total patents + applications | ~15 million documents |
| Average length | 15–25 pages |
| Chunks per patent | 5–15 (abstract, claims, spec sections) |
| Total chunks | 75M–225M |
| Entities per patent (claims) | 5–20 |
| Total entities | 75M–300M |
| Edges (citations, dependencies) | 10–50 per patent |
| Total edges | 150M–750M |

For comparison, the Callimachus self-index covers 733 chunks and 1,957
entities. The patent corpus is roughly **100,000× larger**.

---

## What Breaks at This Scale

### SQLite

SQLite is not viable above ~50M rows in any single table. The FTS index
alone would collapse under the chunk volume. The Postgres backend stub
(`crates/callimachus-core/src/storage/postgres.rs`) is the right foundation
— it would need to be completed and the storage abstraction exercised for
real.

Partitioning strategy: partition by `corpus_id` (each patent year-range or
CPC class as a corpus), or by document hash range. The collection model
handles cross-partition queries.

### Indexing throughput

At 100 chunks/min (optimistic API throughput, single worker):

```
100M chunks ÷ 100/min = 1M minutes ≈ 700 days
```

With 1,000 parallel workers: ~17 hours for initial index. Rate limits are
the real constraint, not compute. This requires a distributed work queue
(Redis, SQS, or similar) feeding a pool of indexing workers, each running
the Callimachus pipeline against a shared Postgres backend.

### LLM cost

Rough estimate for a full pipeline pass over the patent corpus (Haiku for
semantic/summarize/purpose, Sonnet for theme):

| Pass | Calls | Est. cost |
|---|---|---|
| Semantic (per chunk) | 150M | ~$60,000 |
| Summarize (per entity) | 150M | ~$60,000 |
| Purpose (per entity) | 150M | ~$60,000 |
| Contract (per entity) | 150M | ~$60,000 |
| Theme (per CPC class corpus) | ~700 | ~$500 |
| **Total** | | **~$240,000** |

This is a one-time cost for initial indexing. Incremental reindex on new
patents (~400K/year) would run ~$6,000/year. These are order-of-magnitude
estimates — actual cost depends heavily on average patent length.

### Storage

Current ratio: 733 chunks → 16MB DB → ~22KB per chunk (including FTS,
entities, edges, summaries, purposes, contracts).

At 150M chunks: **~3.3TB for the index DB**. The FTS data alone would be
~1.5TB. Add vector embeddings (1536 floats × 150M = ~900GB) and you're
looking at 5–6TB total, assuming good compression.

---

## What Wouldn't Change

The pipeline itself. Chunk → Structure → Semantic → Aliases → Summarize →
Purpose → Contract → Theme is correct at any scale. The epistemological
ordering doesn't change because the corpus is larger. What changes is the
infrastructure around it.

The corrections model also scales cleanly — corrections are small records
applied at query time, not rewritten into the base data.

The collection model is exactly right for patent prior art — a collection
per technology domain, with cross-corpus entity links connecting related
claims across patents. `entity_link` corrections that say "claim 7 of
US11003865 reads on claim 3 of US9158847" are the natural query substrate
for an examiner's workflow.

---

## Infrastructure Picture

```
                    ┌─────────────────────┐
                    │   Work Queue        │
                    │   (Redis / SQS)     │
                    └────────┬────────────┘
                             │ patent chunks
              ┌──────────────┼──────────────┐
              ▼              ▼              ▼
        ┌──────────┐   ┌──────────┐   ┌──────────┐
        │ Worker 1 │   │ Worker 2 │   │ Worker N │
        │  calli   │   │  calli   │   │  calli   │
        └────┬─────┘   └────┬─────┘   └────┬─────┘
             │              │              │
             └──────────────┼──────────────┘
                            │
                    ┌───────▼────────┐
                    │   PostgreSQL   │
                    │   (partitioned │
                    │   by corpus)   │
                    └───────┬────────┘
                            │
                    ┌───────▼────────┐
                    │  Query layer   │
                    │  (MCP / HTTP)  │
                    └────────────────┘
```

**Database**: PostgreSQL 16+ with pgvector, partitioned tables, connection
pooling via PgBouncer. 32-core, 256GB RAM, NVMe SSD array. The FTS index
benefits enormously from RAM.

**Workers**: Stateless, horizontally scalable. Each worker is just `calli
index` pointed at the shared DB. Could run on spot/preemptible instances
since the pipeline is resumable. 50–200 workers for initial load, scale
down to ~5 for steady-state incremental.

**Queue**: One message per patent document. Workers claim a document,
run all passes, mark complete. Dead-letter queue for failures (retried with
backoff — already in the LLM provider layer).

**Query serving**: The MCP server and HTTP server are already stateless and
read-only against the DB. Put a read replica (or several) behind a load
balancer. Cold query latency is the only remaining concern — most of it is
DB, not compute.

---

## What Would Need to Be Built

1. **Postgres backend completion** — the stub exists, needs full implementation
   and parity testing against SQLite
2. **Distributed work queue integration** — a `calli index-worker` mode that
   pulls from a queue rather than processing a single corpus linearly
3. **Patent-specific adapter** — `callimachus-adapter-patent` that understands
   claim structure, CPC codes, citation graphs, and the specific XML format
   USPTO distributes bulk data in (Google Patents bulk data or USPTO's PatentsView)
4. **Claim-aware chunking** — patents have a formal claim hierarchy that should
   be preserved as parent/child chunk relationships, not flattened
5. **Prior art collection scaffolding** — tooling to build collections by CPC
   class and populate cross-patent entity links from citation data

Items 3–5 are domain-specific. Items 1–2 are general infrastructure that
would benefit any large-corpus Callimachus deployment.

---

*Notes from a conversation on 2026-05-17. Revisit when considering large
corpus deployments or domain-specific adapter development.*
