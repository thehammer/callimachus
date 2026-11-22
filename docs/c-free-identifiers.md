# C adapter: global reads/writes and decompiler exports

The code adapter indexes C (`.c`, `.h`) with tree-sitter. It is aimed at
decompiler output: one function per file, generated names such as
`FUN_00001c04` for functions and `DAT_00ff99f4` / `_DAT_ffff99f4` for global
memory locations.

## Entities

| Kind       | Source                                                              |
|------------|---------------------------------------------------------------------|
| `function` | `function_definition` (prototypes are skipped)                      |
| `variable` | file-scope declarations, plus every free identifier (see below)     |
| `type`     | `typedef`, and named `struct` / `union` / `enum` definitions        |
| `synthetic`| `<indirect>`, the target of indirect calls                          |

Entity ids are `{corpus}:{lowercased name}`, so there is **one entity per
name per corpus**: every function that touches `DAT_00ff99f4` points at the
same `variable` entity.

## Edges

- `calls`: `f()` where the callee is an identifier. Calls whose callee is
  anything else (`(**(code **)(x + 4))()`, `tbl[i]()`, `p->fn()`) or a
  parameter/local function pointer are **indirect**: they produce a `calls`
  edge to the synthetic `<indirect>` entity (id `{corpus}:_indirect_`).
  "Which functions call through pointers" is therefore "callers of
  `<indirect>`".
- `reads` / `writes`: from a function to each *free identifier* in its body.
- `defines`: file → function / variable / type.

A free identifier is any identifier in a function body that is not a
parameter, a local declaration, a directly called function, a label, an enum
constant defined in the same file, or a field name. Locals are matched
function-wide, not per block, so a block-local that shadows a global hides it
for the whole function.

`writes` is emitted when the identifier is the left side of `=` or a compound
assignment (`+=` … `>>=`), the operand of `++`/`--`, or the base of such a
target through casts, `*`, `&`, subscripts (`DAT_x[i] = …`, the base, not the
index) and field access (`.` and `->`). Compound assignment is a write only.
Everything else is a `reads`. A function that does both gets both edges; many
reads of one name collapse into a single edge.

Known limitations: a function name used as a value (`cb = FUN_0001;`) is
recorded as a read of a same-named entity, and macros used as constants look
like globals.

These are ordinary edge kinds, so the existing queries and MCP tools list
them (filter by `kind`: `reads`, `writes`, `calls`).

## Indexing a decompiler export (structure only)

Chunking and structure extraction need no LLM. A flat directory of thousands
of files indexes in seconds:

```bash
calli ingest code "my-export" /path/to/export --passes chunk,structure
```

or, for an already registered corpus:

```bash
calli index my-export --pass chunk
calli index my-export --pass structure
```

Omitting `--passes` runs the full pipeline, including the LLM passes.
