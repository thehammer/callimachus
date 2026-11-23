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

Known limitations:

- A function name used as a value (`cb = FUN_0001;`) is recorded as a read of a
  same-named entity (the entity upsert keeps a definition's kind whichever
  file is indexed first). Macros used as constants look like globals.
- Enum constants are known only within the chunk that defines them.
- There is one `bound` set per function: a local declared in an inner block
  hides a same-named global everywhere in that function.
- `*p = v` and `p->f = v` count as writes of `p`; pointer and pointee are not
  distinguished. Address arithmetic writes (`*(int *)(g + 4) = v`) count as
  writes of the left-most base `g`.
- A call through an unbound global name is a `calls` edge to that name.
- A parenthesized unbound name before an operand, `(uint)(x)`, is treated as a
  cast to an undeclared typedef, not a call; `sizeof(name)` with an unbound
  name is treated as a type.
- `reads`/`writes` edges are not part of the SCIP export.
- C entities carry no doc comments yet.

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

## Addresses

Decompiler exports encode addresses in names. The adapter recognises these
naming conventions (case-insensitive prefix, then 4-16 hex digits, then end of
name): `_DAT_`, `DAT_`, `PTR_DAT_`, `PTR_`, `RAM_`, `FUN_`, `LAB_`, `SUB_`.

Function entities and free-identifier `variable` entities whose name matches
get an alias `0x{value:x}` (lowercase, no leading zeros): `_DAT_00c00004` has
alias `0xc00004`, `FUN_000071d0` has `0x71d0`. The `entity` lookup ranks an
exact case-insensitive name or alias match first and returns only those, so
`0xc0000` never shadows `0xc00004`.

There is no masking: the value is exactly what the name says. Masking for a bus
width happens where facts anchor, not in the adapter, which knows nothing about
memory maps or what a range means.

## Table use

A `uses_table` edge goes from the function to the data entity (in addition to
the usual `reads`/`writes`) when:

1. the identifier is the operand of unary `&` (`&DAT_x`);
2. the identifier is the base of a subscript, directly or as `(&X)[i]`;
3. the identifier name starts with `PTR_`;
4. a number literal >= `0x100` is the operand of a pointer cast
   (`(T *)0x18000`), or an operand of `+` in a parenthesised expression that is
   the operand of a pointer cast (`(T *)(e + 0x18000)`). The target is the
   `variable` stub `DAT_{value:08x}`, so it shares its id with a named `DAT_`
   reference to the same address.

Limitations: rule 4 is a heuristic. Literals in comparisons such as
`(T *)0xff3fff < p` are ignored, as are plain integers and struct-field offsets
like `*(short *)(p + 0x10)`. One edge is emitted per (function, target).

## Field accesses

Object state is often reached through a pointer plus a byte offset:

```c
byte *unaff_A2;   // declared as a local by the decompiler
*(short *)(unaff_A2 + 0x10) = *(short *)(unaff_A2 + 0x10) + *(short *)(unaff_A2 + 0x14);
```

The adapter turns each `(base, offset)` into a `field` entity.

**Pattern.** `*(T *)(B + K)`, `*(T *)(B - K)` and `*(T *)B` (offset 0), where `B`
is an identifier and `K` a number literal. The parentheses around `B + K` are
optional.

**Eligible bases** (`is_field_base` in `c.rs`): a parameter of the function, or a
name matching `FIELD_BASE_PATTERN = ^(in|unaff|extraout)_\w+$`: the register
pseudo-variables decompilers declare as locals. Any other name (such as a
local `puVar2`) is not a base.

**Naming.** The entity is named `{B}+0x{k:x}` or `{B}-0x{k:x}` (negative
offsets keep their sign; offset 0 is `param_1+0x0`), kind `field`, confidence
0.5. Its id is `{corpus}:{name lowercased}` (so `+0x4` and `-0x4` stay
distinct). The entity is shared by every function in the corpus that uses that
base and offset.

**Edges.** One edge per kind per (function, field): `writes` when the
dereference is an assignment target (including `+=` and `++`), `reads`
otherwise. The existing handling of the base identifier is unchanged.

**Width.** The cast type gives an alias `width:{n}`: `char`/`byte`/`undefined1`
= 1, `short`/`ushort`/`undefined2` = 2, `int`/`uint`/`long`/`undefined4`/
`pointer` = 4. The first width seen in a function wins; unknown types give no
alias.

Layout facts (`docs/facts.md`, kind `layout`) name these entities (`obj.x`) and
drive source annotation (`unaff_A2->x /* obj+0x10 s16 */`). The adapter itself
knows nothing about layouts.

Limitations: aliasing through locals (`p = unaff_A2; *(short *)(p + 0x10)`) is
not tracked; array-index forms such as `in_A0[0x20]` are not fields; offsets
must be number literals. Binary-export corpora get the same entities, since
the adapter analyses each function's C.
