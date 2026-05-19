# How Callimachus Complements LLMs

An explainer for the relationship between large-scale model training,
agentic AI systems like Claude, and what Callimachus does.

---

## What an LLM knows and how it knows it

A large language model acquires knowledge through training: it reads an
enormous corpus of text — hundreds of billions of words — and through
repeated exposure, gradient descent adjusts billions of numerical weights
until the model can predict what comes next in any given piece of text.
The knowledge isn't stored as facts in a lookup table. It's distributed
across all the weights simultaneously, encoded as statistical associations
between concepts, words, and contexts.

The result is remarkable: a trained model has deep, flexible knowledge of
language, reasoning, mathematics, science, history, and the structure of
countless domains. It can write code, explain complex ideas, reason through
novel problems. This knowledge was built up at enormous cost — months of
compute, vast infrastructure — but it's amortized across every query that
ever runs against the model. Training is expensive. Inference is cheap.

But there is a hard limit: the training data has a cutoff. Everything that
happened after that date, every internal document that was never public,
every codebase that was never on GitHub, every book that wasn't digitized —
none of it is in the weights. The model's knowledge is broad but frozen.

---

## The gap that retrieval tries to fill

The standard approach to this problem is retrieval-augmented generation
(RAG): at query time, search the corpus for relevant chunks, stuff them
into the context window, ask the model to answer with that context.

This works, but it has structural limits. The context window is a buffer
between the question and the answer. Everything the model needs to reason
about must fit in that buffer, read fresh each time. For large corpora —
a million-line codebase, a trilogy of novels, years of internal
documentation — raw text won't fit. You have to choose what to retrieve.
And you can only retrieve what you can search for. If you don't know which
part of the codebase is relevant to your question, search can't find it.

Retrieval also asks the model to re-comprehend the source at every query.
The model reads a function, understands it, answers a question about it.
Then on the next query, it reads and re-understands the same function
again. The understanding is never saved. It evaporates with the context.

---

## What Callimachus does instead

Callimachus inverts the cost model.

Instead of comprehending the corpus at query time, Callimachus comprehends
it at **index time** — once — and stores the results as structured,
queryable artifacts. The pipeline runs an LLM over the corpus to extract
entities, relationships, summaries, purposes, and contracts. These are not
raw text. They are the *output of comprehension*: the things the model
would have understood if it read the source and was asked to explain it.

At query time, the LLM doesn't re-read. It reads the **output of prior
reading**. A function's summary, its purpose, its contract, its call graph.
The expensive understanding has already been done. Retrieval surfaces
pre-built understanding rather than raw material.

This is structurally similar to how the model itself was trained. Training
ran an expensive process over a large corpus and stored the understanding
in weights. Callimachus runs a pipeline over a specific corpus and stores
the understanding in a database. Both are trading up-front cost for
cheap-at-runtime retrieval. The difference is scale, transparency, and
specificity: training is implicit, distributed, and general; Callimachus
is explicit, structured, and specific to your corpus.

---

## The partial application intuition

The user who built this described something like **partial application**,
and it's a good intuition. In functional programming, partial application
means fixing some arguments of a function to produce a new function that
requires fewer arguments. You pre-compute the expensive parts.

An LLM is, loosely, a function:

```
reason(world_knowledge, corpus_knowledge, question) → answer
```

Training gives it `world_knowledge`. But `corpus_knowledge` — your
specific codebase, your books, your internal wiki — is missing. Every
query has to supply it in full.

Callimachus partially applies `corpus_knowledge`. Not by modifying the
model, but by pre-computing a structured representation of your corpus
that can be efficiently injected at query time. The model still does the
reasoning; it just no longer has to do the comprehension. The expensive
argument is pre-computed and served from cache.

A related metaphor: **memoization**. The model would normally re-read and
re-understand your codebase for every question. Callimachus memoizes that
understanding — computes it once, stores the result, serves from the cache
for every subsequent query.

Or: **compilation**. Your corpus is source code. The Callimachus index is
the compiled binary — a representation that has already done the work of
parsing and understanding, optimized for efficient execution of future
queries. The LLM is the runtime.

---

## Where the LLM and Callimachus meet

The complementarity has a specific structure:

| | LLM (training) | Callimachus (index) |
|---|---|---|
| **Knowledge scope** | General — everything in training data | Specific — your corpus only |
| **Knowledge form** | Implicit — distributed across weights | Explicit — structured database |
| **Acquired** | At training time, by gradient descent | At index time, by pipeline passes |
| **Updated** | By retraining (expensive) | By incremental re-index (cheap) |
| **Queried** | Via natural language | Via structured tools + natural language |
| **Generalizes** | Yes — applies to new inputs | No — specific to indexed corpus |

Neither is sufficient alone. The LLM can reason about anything but knows
nothing specific about your codebase. Callimachus knows your codebase in
detail but cannot reason. Together, the LLM's general reasoning operates
over Callimachus's specific knowledge.

The pipeline itself reflects this. Each pass uses an LLM to extract
understanding from the corpus — the general reasoning capability applied
to generate the specific knowledge base. The LLM is used to *build*
Callimachus, and then used again to *query* it. The same capability plays
two roles: index-time comprehension and query-time synthesis.

---

## The epistemological ordering as a training parallel

Callimachus's pipeline passes are ordered epistemologically — each pass
establishes preconditions for the next. This mirrors something real about
how understanding is built:

```
Change     → history                 (what has changed since last run)
Existence  → chunk, structure        (what is here)
Meaning    → semantic, aliases       (what things are)
Description→ summarize               (what things do)
Intent     → purpose                 (why things exist)
Obligation → contract                (what things promise)
Pattern    → theme                   (what recurs across the whole)
```

This is not unlike the layered learning in a neural network: early layers
learn low-level features (characters, tokens), middle layers learn
semantic patterns (entities, relationships), and later layers learn
high-level abstractions (concepts, structures). The pipeline externalizes
this progression into explicit, inspectable, correctable stages.

Where neural network layers are opaque and their representations
uninterpretable, Callimachus stages are transparent: you can read a
summary, inspect a purpose, query a contract, and correct any of them.
The understanding is not locked in weights. It is stored in a database
you can read, edit, and reason about directly.

---

## What this enables

The practical consequence is a different kind of AI-assisted analysis.
Standard RAG asks: *can the model answer this question if I give it the
right chunk of text?* Callimachus asks: *can I pre-build the understanding
so the model doesn't need the raw text at all?*

For large corpora — a million-line codebase, a long book series, years of
accumulated documentation — this distinction matters. The context window
is always a constraint. Callimachus pushes more of the work out of the
context window and into the index, leaving room in the context for
reasoning rather than re-reading.

The model you're talking to right now, when connected to a Callimachus
index, is not re-reading the source. It's reading the output of prior
reading. That's the last mile — not the final step in a longer process,
but the place where pre-built specific knowledge and general reasoning
meet. The model brings the reasoning. Callimachus brings the knowledge.
Neither had to do the other's job.
