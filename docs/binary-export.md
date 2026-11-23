# Binary-export adapter

Kind: `binary-export`. It indexes a **bundle** written by an external exporter: for every
function the decompiled C **and** the CPU disassembly with addresses, plus the ROM tables,
hardware ports and RAM locations the functions touch, and a manifest naming the platform.
Each function chunk carries both texts, so the LLM sees the assembly next to the C.

```bash
calli corpus add binary-export "My Program" /path/to/bundle
calli index my-program
# or in one step:
calli ingest binary-export "My Program" /path/to/bundle
```

`--pass decompile` and the other pass flags work as for any corpus. The structure pass needs
the bundle loaded by the chunk pass of the same invocation.

## Bundle format 1

```
<bundle>/
  manifest.json   {"format":1,"kind":"binary-export",
                   "platform":{"id":"<id>","profile_sha256":"…","cpu":"<cpu>","address_bits":24},
                   "cartridge":{"sha256":"…"}, "tools":{"decompiler":"…","exporter":"…"}, "rounds":N,
                   "counts":{"functions":N,"decompile_failed":N,"tables":N,"ports":N,"ram":N}}
  functions.jsonl {"address":"0x0071d0","name":"FUN_000071d0","size":212,"c":"c/000071d0.c","asm":"asm/000071d0.s",
                   "decompile_ok":true,"discovered_by":["analysis"|"vector"|"table:0x01a2c0"|"declared:0x…"]}
  c/<addr8>.c     decompiled C; globals already normalized: RAM_<6hex>, IO_<6hex>, DAT_<8hex>, PTR_DAT_<8hex>
  asm/<addr8>.s   "<addr6>\t<hex bytes>\t<instruction>" per line
  tables.jsonl    {"address","name","kind":"code-pointers"|"code-offsets"|"data","entry":"u32"|"u16"|null,"count":N|null,
                   "readers":["0x…"],"source":"dispatch"|"dossier"|"profile"|"refs"}
  ports.jsonl     {"address","port":"<port id>","name":"IO_<6hex>","readers":[…],"writers":[…]}
  ram.jsonl       {"address","name":"RAM_<6hex>","readers":[…],"writers":[…]}
```

Unknown manifest keys are ignored. A `format` other than 1 is refused with a clear error, as is
a `kind` other than `binary-export`. A function with `decompile_ok: false` has no usable C;
only its disassembly is indexed.

## Chunks

One chunk per function, kind `function`, language `c`. The location path is `fn/<addr8>`
(8 lowercase hex digits), so URIs are stable: `calli://<corpus>/fn/000071d0`. Tables, ports
and RAM are entities only, never chunks.

Content layout:

```
// <name> @ 0x<addr6>  (size <n> bytes; discovered_by <a, b>)
<the C file text, minus its first comment line>
/* ---- disassembly ---- */
/* <addr6>  <instruction> */          (one per asm line; hex bytes dropped)
```

The disassembly sits inside C block comments, so the whole content is valid C. A literal `*/`
inside an instruction is written `*\/`.

## Entities and edges

| Source | Entity | Notes |
|---|---|---|
| function | `function`, id `{corpus}:{lowercased name}` | aliases include the canonical address (`0x71d0`); created even when `decompile_ok` is false |
| `tables.jsonl` | `table` | same id as the C variable of that name; aliases: address, `entry:<u16\|u32>`, `count:<n>`; description `"<kind> table"` |
| `ports.jsonl` | `port` | same id as the C variable; aliases: address, port id |
| `ram.jsonl` | `variable` | aliases: address |

Only tables, ports and RAM whose `readers`/`writers` list the function are attached to its
chunk. Edges: `uses_table` (tables), `reads`/`writes` (ports, RAM), plus everything the C
analysis finds (`calls`, indirect calls, reads/writes, address aliases, `uses_table`). Edge ids
are deterministic, so an edge found by both the bundle and the C analysis is stored once.

**Kind precedence.** A free identifier in C becomes a low-confidence `variable` stub. The entity
upsert lets a higher-confidence non-`variable` kind replace such a stub, so `table` and `port`
win over the stub regardless of which chunk is indexed first.

## LLM prompts

Summaries, purposes and contracts reuse the code adapter's prompts, with trimmed content: the
C part (header included) keeps up to 4,000 chars, followed by as many whole disassembly lines as
fit in a further 3,000 chars, ending with `/* … N more instructions */` when lines were dropped.
The C part is never trimmed to make room for assembly. When disassembly is present the prompt
gains the line: "The disassembly of the same routine follows the C; use it for register
parameters, exact offsets and access widths." Plain `.c` corpora are unaffected.

## Platform config

When the corpus is registered (`corpus add` or `ingest`), the manifest is merged into the
corpus config:

```json
{ "platform": {"id": "…", "profile_sha256": "…", "cpu": "…", "address_bits": 24},
  "cartridge_sha256": "…", "address_mask": "0xffffff" }
```

`address_mask` is derived from `address_bits` unless already set; `cpu_spaces` and other keys are
left alone. `corpus_overview` returns the `platform` record.
