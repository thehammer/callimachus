# Facts: confirmed knowledge as scholia

Some knowledge cannot be derived from source code: "RAM `0xFF981A` is the
current stage number, `u16`, confirmed by a watch", or "reading the version
port tells you the region". Callimachus imports such **facts** from JSON files,
stores them as scholia (no LLM involved), anchors them to entities by address,
and lets a game corpus link to a shared **reference corpus** for its platform.

Facts live in the existing scholia table; no migration is involved.

## Fact format (format 1)

A facts file is `{"format": 1, "facts": [ ... ]}`. A fact:

```json
{"id": "var.stage", "kind": "variable", "topic": "main-loop",
 "summary": "Current stage number, 1-5",
 "data": {"space": "wram", "address": "0xFF981A", "size": 2, "type": "u16"},
 "confidence": "confirmed", "evidence": ["ev.watch-stage"], "notes": "optional"}
```

| Field | Rule |
|---|---|
| `id` | `[a-z0-9][a-z0-9._-]{0,95}` |
| `kind` | any string (see below) |
| `summary` | 1-300 characters |
| `confidence` | `confirmed`, `inferred` or `unknown` |
| `evidence` | non-empty list of strings (ids are stored, not validated) |
| `topic`, `notes` | optional strings |
| `data` | kind-specific object |

Kinds with defined `data`:

| Kind | `data` |
|---|---|
| `variable` | `space`, `address`, `size` |
| `table` | `space`, `address`, and `record` or `stride` |
| `routine` | `space`, `address`, `role` |
| `region` | `space`, `start`, `end` (`end` must not be less than `start`) |
| `format`, `rule`, `constant`, `behaviour`, `asset` | optional, free JSON object (e.g. `{"bytes": 32}`); a non-object `data` is rejected |
| `variant` | `variant` (id), `affects` (list of fact ids or addresses), optional `detects` |
| `platform` | free object naming the variant values a game uses, e.g. `{"region": "pal-overseas", "pad": "6-button"}` |

Other kinds (`codec`, or any string)
are accepted and stored without anchoring.

An **index file** is any JSON object with a `"facts": ["a.json", "b.json"]`
list of plain file names in the same folder. All listed files are imported and
other keys are ignored, so an existing folder of fact files that has such an
index can be imported by pointing `calli facts import` at the index. Listed
names containing `/`, or equal to `.` or `..`, are refused.

An invalid record is rejected with a line naming the file, the fact id and the
reason; the other records still import.

### Confidence

A fact's `confidence` label maps to the numeric confidence stored on its
materialised entity and on `affects` edges (`confidence_score` in
`corrections/facts.rs`): `confirmed` → 1.0, `inferred` → 0.6, `unknown` → 0.3.
An `affects` edge takes the lower of the variant's and the target fact's
score. `entity` output carries the label on each `reference` and `variant`
entry, and text output marks non-confirmed ones, e.g. `(inferred)`.

### Import warnings

A variant whose `data.affects` names a fact id not in the import, or an
address that matches no entity, does not fail the import. The import prints
`warning: variant <id>: ...` and counts it in the summary (`warnings N`).

## Anchoring

On every import, facts of kind `variable`, `table` and `routine` with a
`data.address` are anchored to entities of the corpus with the same address.
An entity's address is the first alias in canonical form (`0x` + lowercase hex)
such as `0xff981a`; the C adapter adds these for decompiler names
(`RAM_ff981a`, `FUN_000071d0`, `_DAT_00c00004`). For `routine` facts,
entities of kind `function` are preferred.

Two corpus config keys control this (set them with `calli corpus set`):

- `address_mask`: hex string. Both the fact address and entity addresses are
  masked before comparison, so `0xFFFF0010` matches `RAM_ff0010` with mask
  `0xffffff`.
- `cpu_spaces`: list of address spaces that anchor, e.g. `["rom","wram","io"]`.
  A fact whose `data.space` is not listed (say, a video-memory space) never
  anchors. When absent, every space anchors.

`region` facts never anchor (ranges are for grounding later); `variant` facts
anchor to nothing.

A fact id becomes a virtual alias of its anchor entities, so
`calli inspect entities` / the `entity` tool resolves `var.stage` to
`RAM_ff981a`, and `entity RAM_ff981a` lists the fact (summary, confidence,
evidence, notes).

### Materialised entities

Every fact with no anchor, and every `variant`, `region`, `constant` and
`platform` fact, gets an entity `{corpus}:fact.{fact-id}`: `canonical_name` is
the fact id, kind is `fact_{kind}`, the description is the summary, and the
alias is the canonical address when the fact has one in an anchoring space.
This gives reference corpora (which have no address entities) and never-named
RAM variables something to look up and link.

### Re-import, diff and prune

A scholion is appended only for new or changed records (the record plus its
anchors are compared). Re-importing the same file adds nothing. With
`--prune`, facts present in the corpus but absent from the import get a
`retracted` scholion and disappear from queries.

## Reference corpora, linking and variants

A corpus whose config has `"role": "reference"` is a reference corpus: a
per-platform body of hardware knowledge (for example markdown pages plus a
facts file) shared by every game of that platform. Put the game corpora and the
reference corpus in one collection, then:

```
calli link addresses <collection>
```

For every non-reference corpus G and reference corpus R in the collection, and
every entity of G with an address, if R has a fact entity or an anchored fact
at the same masked address (each corpus uses its own mask), a `references`
entity link is added from G's entity to R's entity. Existing links are not
duplicated; the command prints how many were added.

`variant` facts describe behavioural differences (region bits, pad protocol,
a boot handshake, sound-chip revisions). For each item in `affects` (a fact id
or an address) an `affects` edge is created from the variant entity, with
`origin_scope = "fact"`. These edges are rebuilt on every import. The `entity`
output of a game entity shows the linked `reference` entities and the
`variants` reachable from them through one `affects` edge.

The `platform` fact of a game corpus is reported by `corpus_overview` as
`platform`.

## CLI

```
calli corpus set <corpus> <key> <json-value>      # role, address_mask, cpu_spaces
calli facts import <corpus> <path>... [--prune]   # prints added/changed/unchanged/retracted/rejected
calli facts list <corpus> [--kind k] [--topic t] [--json]
calli facts show <corpus> <fact-id>
calli link addresses <collection>
```

The MCP tool `list_facts {corpus_id, kind?, topic?}` lists facts; fact
scholia also appear in `list_scholia`.

## Worked example (`toy16`)

A fictional `toy16` platform. The game corpus is a decompiled C project in
which `FUN_00001000` writes `RAM_ff0010` and reads `_DAT_00c00004`, and
`FUN_00003000` reads `DAT_00a10001`. The reference corpus holds a facts file:

```json
{"format": 1, "facts": [
  {"id": "io.video-control", "kind": "variable", "summary": "Video control port",
   "data": {"space": "io", "address": "0xC00004", "size": 2},
   "confidence": "confirmed", "evidence": ["manual"]},
  {"id": "io.version", "kind": "variable", "summary": "Version port; low bits hold the region",
   "data": {"space": "io", "address": "0xA10001", "size": 1},
   "confidence": "confirmed", "evidence": ["manual"]},
  {"id": "variant.region-bits", "kind": "variant", "summary": "Region bits in the version port",
   "data": {"variant": "region", "affects": ["io.version"]},
   "confidence": "confirmed", "evidence": ["manual"]}
]}
```

```
calli corpus set game address_mask '"0xffffff"'
calli corpus set game cpu_spaces '["rom","wram","io"]'
calli corpus set toy16-ref role '"reference"'
calli facts import toy16-ref toy16-facts.json
calli facts import game game-facts.json
calli link addresses toy16-collection
```

Afterwards the `entity` tool for `DAT_00a10001` in `game` shows the reference
entity `io.version` and the variant `variant.region-bits`; `entity var.counter`
(a game fact at `0xFF0010`) resolves to `RAM_ff0010`.
