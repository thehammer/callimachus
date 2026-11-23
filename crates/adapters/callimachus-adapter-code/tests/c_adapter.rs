/// C language adapter behavioural tests.
///
/// Operate on `extract_structure` directly (no LLM, no storage) using
/// synthetic, hand-written C with neutral decompiler-style names.
use callimachus_adapter_code::extractor::{ExtractedCodeStructure, extract_structure};
use callimachus_core::types::{Chunk, Location};

const CORP: &str = "corp";

fn make_chunk(corpus_id: &str, path: &str, kind: &str, content: &str) -> Chunk {
    Chunk::new(
        corpus_id.to_string(),
        None,
        kind.to_string(),
        Location::new(corpus_id, path),
        content.to_string(),
    )
}

fn lang(ext: &str) -> &'static callimachus_adapter_code::languages::LangConfig {
    callimachus_adapter_code::languages::for_extension(ext).unwrap()
}

fn extract_at(path: &str, kind: &str, ext: &str, src: &str) -> ExtractedCodeStructure {
    let chunk = make_chunk(CORP, path, kind, src);
    extract_structure(&chunk, lang(ext)).unwrap()
}

fn extract(src: &str) -> ExtractedCodeStructure {
    extract_at("src/FUN_00001c04.c", "file", "c", src)
}

/// Entity id convention: `{corpus}:{lowercased name, non-alnum/_ -> _}`.
fn id(name: &str) -> String {
    // Field entities (`unaff_A2+0x10`, `p-0x4`) keep their sign in the id.
    if let Some(pos) = name.rfind(['+', '-'])
        && name[pos + 1..].starts_with("0x")
    {
        return format!("{CORP}:{}", name.to_lowercase());
    }
    let n: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{CORP}:{n}")
}

fn indirect_id() -> String {
    format!("{CORP}:_indirect_")
}

fn entities_of_kind<'a>(r: &'a ExtractedCodeStructure, kind: &str) -> Vec<&'a str> {
    r.entities
        .iter()
        .filter(|e| e.kind == kind)
        .map(|e| e.canonical_name.as_str())
        .collect()
}

fn count_edges(r: &ExtractedCodeStructure, kind: &str, from: &str, to: &str) -> usize {
    r.edges
        .iter()
        .filter(|e| e.kind == kind && e.from_entity_id == id(from) && e.to_entity_id == to)
        .count()
}

fn has_edge(r: &ExtractedCodeStructure, kind: &str, from: &str, to_name: &str) -> bool {
    count_edges(r, kind, from, &id(to_name)) > 0
}

fn writes(r: &ExtractedCodeStructure, from: &str, var: &str) -> bool {
    has_edge(r, "writes", from, var)
}
fn reads(r: &ExtractedCodeStructure, from: &str, var: &str) -> bool {
    has_edge(r, "reads", from, var)
}

fn assert_no_ref(r: &ExtractedCodeStructure, from: &str, var: &str) {
    assert!(
        !reads(r, from, var) && !writes(r, from, var),
        "{var} must not be a reads/writes ref of {from}; edges: {:?}",
        r.edges
            .iter()
            .map(|e| format!("{} {} -> {}", e.kind, e.from_entity_id, e.to_entity_id))
            .collect::<Vec<_>>()
    );
}

// ── Entities ─────────────────────────────────────────────────────────────────

#[test]
fn function_definitions_become_function_entities() {
    let r = extract("int FUN_00001c04(int a) { return a; }\nvoid FUN_00001c40(void) { }\n");
    let fns = entities_of_kind(&r, "function");
    assert!(fns.contains(&"FUN_00001c04"), "{fns:?}");
    assert!(fns.contains(&"FUN_00001c40"), "{fns:?}");
    assert!(r.entities.iter().any(|e| e.id == id("FUN_00001c04")));
}

#[test]
fn file_scope_declarations_become_variable_entities_but_prototypes_do_not() {
    let r =
        extract("int DAT_00ff99f4;\nstatic char _DAT_ffff99f4 = 3;\nint FUN_00001c04(int a);\n");
    let vars = entities_of_kind(&r, "variable");
    assert!(vars.contains(&"DAT_00ff99f4"), "{vars:?}");
    assert!(vars.contains(&"_DAT_ffff99f4"), "{vars:?}");
    assert!(
        !vars.contains(&"FUN_00001c04"),
        "a prototype is not a variable: {vars:?}"
    );
    assert!(
        !entities_of_kind(&r, "function").contains(&"FUN_00001c04"),
        "a bare prototype is not a function definition"
    );
}

#[test]
fn named_typedefs_structs_enums_and_unions_become_type_entities() {
    let r = extract(
        "typedef unsigned int uint_t;\n\
         struct S_0001 { int a; };\n\
         enum E_0001 { E_A, E_B };\n\
         union U_0001 { int a; char b; };\n",
    );
    let types = entities_of_kind(&r, "type");
    for n in ["uint_t", "S_0001", "E_0001", "U_0001"] {
        assert!(types.contains(&n), "missing type {n}: {types:?}");
    }
}

#[test]
fn every_file_yields_a_file_entity_and_defines_edges_to_its_symbols() {
    let r = extract("int DAT_00ff99f4;\nvoid FUN_00001c04(void) { }\n");
    assert!(!entities_of_kind(&r, "file").is_empty(), "no file entity");
    let file = r.entities.iter().find(|e| e.kind == "file").unwrap();
    for sym in ["FUN_00001c04", "DAT_00ff99f4"] {
        assert!(
            r.edges.iter().any(|e| e.kind == "defines"
                && e.from_entity_id == file.id
                && e.to_entity_id == id(sym)),
            "missing defines edge file -> {sym}"
        );
    }
}

#[test]
fn header_files_are_extracted_like_c_sources() {
    let r = extract_at(
        "include/FUN_00001c04.h",
        "file",
        "h",
        "extern int DAT_00ff99f4;\nstruct S_0001 { int a; };\nstatic inline int FUN_00001c04(void) { return DAT_00ff99f4; }\n",
    );
    assert!(entities_of_kind(&r, "function").contains(&"FUN_00001c04"));
    assert!(entities_of_kind(&r, "type").contains(&"S_0001"));
    assert!(reads(&r, "FUN_00001c04", "DAT_00ff99f4"));
}

// ── Calls ────────────────────────────────────────────────────────────────────

#[test]
fn direct_call_produces_calls_edge_and_callee_is_not_a_variable() {
    let r = extract("void FUN_00001c04(void) { FUN_00001c40(1); }\n");
    assert!(has_edge(&r, "calls", "FUN_00001c04", "FUN_00001c40"));
    assert!(!entities_of_kind(&r, "variable").contains(&"FUN_00001c40"));
    assert_no_ref(&r, "FUN_00001c04", "FUN_00001c40");
}

#[test]
fn repeated_direct_calls_collapse_with_occurrence_count() {
    let r =
        extract("void FUN_00001c04(void) { FUN_00001c40(); FUN_00001c40(); FUN_00001c40(); }\n");
    let e: Vec<_> = r
        .edges
        .iter()
        .filter(|e| e.kind == "calls" && e.to_entity_id == id("FUN_00001c40"))
        .collect();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].occurrence_count, 3);
}

#[test]
fn call_through_cast_pointer_expression_is_an_indirect_call() {
    let r = extract("void FUN_00001c04(void) { (**(code **)(DAT_tbl + 4))(1); }\n");
    assert!(count_edges(&r, "calls", "FUN_00001c04", &indirect_id()) >= 1);
    let syn = r.entities.iter().find(|e| e.id == indirect_id());
    let syn = syn.expect("synthetic <indirect> entity must be emitted");
    assert_eq!(syn.kind, "synthetic");
    assert_eq!(syn.canonical_name, "<indirect>");
}

#[test]
fn call_through_subscript_is_an_indirect_call() {
    let r = extract("void FUN_00001c04(int i) { DAT_tbl[i](5); }\n");
    assert!(count_edges(&r, "calls", "FUN_00001c04", &indirect_id()) >= 1);
}

#[test]
fn call_through_field_access_is_an_indirect_call() {
    let r = extract("void FUN_00001c04(struct S_0001 *p) { p->fn(); }\n");
    assert!(count_edges(&r, "calls", "FUN_00001c04", &indirect_id()) >= 1);
}

#[test]
fn calling_a_function_pointer_parameter_is_an_indirect_call() {
    let r = extract("void FUN_00001c04(void (*cb)(int)) { cb(1); }\n");
    assert!(count_edges(&r, "calls", "FUN_00001c04", &indirect_id()) >= 1);
    assert!(!has_edge(&r, "calls", "FUN_00001c04", "cb"));
}

#[test]
fn calling_a_local_function_pointer_is_an_indirect_call() {
    let r = extract("void FUN_00001c04(void) { void (*fp)(void) = FUN_00001c40; fp(); }\n");
    assert!(count_edges(&r, "calls", "FUN_00001c04", &indirect_id()) >= 1);
    assert!(!has_edge(&r, "calls", "FUN_00001c04", "fp"));
}

// ── Writes ───────────────────────────────────────────────────────────────────

#[test]
fn plain_assignment_is_a_write_not_a_read() {
    let r = extract("void FUN_00001c04(void) { DAT_00ff99f4 = 1; }\n");
    assert!(writes(&r, "FUN_00001c04", "DAT_00ff99f4"));
    assert!(!reads(&r, "FUN_00001c04", "DAT_00ff99f4"));
}

#[test]
fn every_compound_assignment_operator_is_a_write_only() {
    for op in ["+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>="] {
        let src = format!("void FUN_00001c04(void) {{ DAT_00ff99f4 {op} 2; }}\n");
        let r = extract(&src);
        assert!(
            writes(&r, "FUN_00001c04", "DAT_00ff99f4"),
            "{op} not a write"
        );
        assert!(
            !reads(&r, "FUN_00001c04", "DAT_00ff99f4"),
            "{op} also a read"
        );
    }
}

#[test]
fn increment_and_decrement_prefix_and_postfix_are_writes() {
    for stmt in ["DAT_a++;", "++DAT_a;", "DAT_a--;", "--DAT_a;"] {
        let r = extract(&format!("void FUN_00001c04(void) {{ {stmt} }}\n"));
        assert!(writes(&r, "FUN_00001c04", "DAT_a"), "{stmt} not a write");
    }
}

#[test]
fn write_through_cast_and_deref_targets_the_base_identifier() {
    let r = extract("void FUN_00001c04(void) { *(short *)&DAT_00ff99f4 = 1; }\n");
    assert!(writes(&r, "FUN_00001c04", "DAT_00ff99f4"));
}

#[test]
fn write_through_subscript_targets_the_base_identifier() {
    let r = extract("void FUN_00001c04(int i) { DAT_00ff99f4[i] = 2; }\n");
    assert!(writes(&r, "FUN_00001c04", "DAT_00ff99f4"));
    assert!(!writes(&r, "FUN_00001c04", "i"));
}

#[test]
fn write_through_arrow_and_dot_field_access_targets_the_base_identifier() {
    let r = extract("void FUN_00001c04(void) { DAT_p->f = 3; DAT_s.f = 4; }\n");
    assert!(writes(&r, "FUN_00001c04", "DAT_p"));
    assert!(writes(&r, "FUN_00001c04", "DAT_s"));
}

// ── Reads ────────────────────────────────────────────────────────────────────

#[test]
fn identifiers_in_conditions_rhs_and_call_arguments_are_reads() {
    let r = extract(
        "int FUN_00001c04(void) {\n\
           int x = DAT_rhs;\n\
           if (DAT_cond) { return 0; }\n\
           FUN_00001c40(DAT_arg);\n\
           return x;\n\
         }\n",
    );
    for v in ["DAT_rhs", "DAT_cond", "DAT_arg"] {
        assert!(reads(&r, "FUN_00001c04", v), "{v} should be read");
        assert!(!writes(&r, "FUN_00001c04", v), "{v} should not be written");
    }
}

#[test]
fn address_of_passed_as_argument_is_a_read() {
    let r = extract("void FUN_00001c04(void) { FUN_00001c40(&DAT_00ff99f4); }\n");
    assert!(reads(&r, "FUN_00001c04", "DAT_00ff99f4"));
    assert!(!writes(&r, "FUN_00001c04", "DAT_00ff99f4"));
}

#[test]
fn index_expression_variable_is_read_while_the_array_is_written() {
    let r = extract("void FUN_00001c04(void) { DAT_arr[DAT_i] = 1; }\n");
    assert!(writes(&r, "FUN_00001c04", "DAT_arr"));
    assert!(reads(&r, "FUN_00001c04", "DAT_i"));
    assert!(!writes(&r, "FUN_00001c04", "DAT_i"));
}

#[test]
fn many_reads_collapse_into_one_edge_with_count_one() {
    let r = extract("int FUN_00001c04(void) { return DAT_a + DAT_a + DAT_a; }\n");
    let e: Vec<_> = r
        .edges
        .iter()
        .filter(|e| e.kind == "reads" && e.to_entity_id == id("DAT_a"))
        .collect();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].occurrence_count, 1);
}

#[test]
fn a_function_may_both_read_and_write_the_same_variable() {
    let r = extract("void FUN_00001c04(void) { DAT_a = DAT_a + 1; }\n");
    assert!(reads(&r, "FUN_00001c04", "DAT_a"));
    assert!(writes(&r, "FUN_00001c04", "DAT_a"));
}

#[test]
fn each_free_identifier_yields_a_variable_entity_without_a_file_scope_declaration() {
    let r = extract("void FUN_00001c04(void) { DAT_a = DAT_b; }\n");
    let vars = entities_of_kind(&r, "variable");
    assert!(vars.contains(&"DAT_a"), "{vars:?}");
    assert!(vars.contains(&"DAT_b"), "{vars:?}");
}

#[test]
fn declared_global_and_its_use_share_one_entity() {
    let r = extract("int g;\nint FUN_00001c04(void) { return g; }\n");
    let n = r.entities.iter().filter(|e| e.id == id("g")).count();
    assert_eq!(n, 1, "duplicate entity ids for g");
    let mut ids: Vec<_> = r.entities.iter().map(|e| e.id.clone()).collect();
    let total = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "result.entities contains duplicate ids");
    assert!(reads(&r, "FUN_00001c04", "g"));
}

// ── Non-references ───────────────────────────────────────────────────────────

#[test]
fn parameter_shadowing_a_global_is_not_a_ref() {
    let r = extract("int DAT_x;\nint FUN_00001c04(int DAT_x) { DAT_x = 1; return DAT_x; }\n");
    assert_no_ref(&r, "FUN_00001c04", "DAT_x");
}

#[test]
fn local_shadowing_a_global_is_not_a_ref() {
    let r =
        extract("int DAT_x;\nint FUN_00001c04(void) { int DAT_x = 2; DAT_x++; return DAT_x; }\n");
    assert_no_ref(&r, "FUN_00001c04", "DAT_x");
}

#[test]
fn goto_labels_are_not_variable_refs() {
    let r = extract("void FUN_00001c04(void) { goto LAB_00001c10; LAB_00001c10: return; }\n");
    assert_no_ref(&r, "FUN_00001c04", "LAB_00001c10");
    assert!(!entities_of_kind(&r, "variable").contains(&"LAB_00001c10"));
}

#[test]
fn struct_field_names_are_not_variable_refs() {
    let r = extract(
        "int FUN_00001c04(struct S_0001 s, struct S_0001 *p) { return s.DAT_x + p->DAT_y; }\n",
    );
    assert_no_ref(&r, "FUN_00001c04", "DAT_x");
    assert_no_ref(&r, "FUN_00001c04", "DAT_y");
}

#[test]
fn enum_constants_defined_in_the_same_file_are_not_variable_refs() {
    let r = extract(
        "enum E_0001 { E_FIRST, E_SECOND };\nint FUN_00001c04(void) { return E_SECOND; }\n",
    );
    assert_no_ref(&r, "FUN_00001c04", "E_SECOND");
}

// ── Chunk shapes ─────────────────────────────────────────────────────────────

#[test]
fn edges_in_a_file_chunk_originate_from_the_function_not_the_file() {
    let r = extract("void FUN_00001c04(void) { DAT_a = FUN_00001c40(); }\n");
    let file = r.entities.iter().find(|e| e.kind == "file").unwrap();
    for kind in ["calls", "writes"] {
        let e = r.edges.iter().find(|e| e.kind == kind).unwrap();
        assert_eq!(e.from_entity_id, id("FUN_00001c04"), "{kind}");
        assert_ne!(e.from_entity_id, file.id);
    }
}

#[test]
fn function_fragment_chunk_extracts_refs_and_calls() {
    let r = extract_at(
        "src/f.c#FUN_x",
        "function",
        "c",
        "void FUN_x(void) { DAT_a = FUN_y(); }\n",
    );
    assert!(entities_of_kind(&r, "function").contains(&"FUN_x"));
    assert!(has_edge(&r, "calls", "FUN_x", "FUN_y"));
    assert!(writes(&r, "FUN_x", "DAT_a"));
}

// ── Scale ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn two_thousand_file_flat_directory_is_chunked_and_extracted_in_time() {
    use callimachus_adapter_code::chunker::{ChunkOptions, chunk_directory};

    let dir = tempfile::tempdir().unwrap();
    for i in 0..2000u32 {
        let src = format!("void FUN_{i:08x}(void) {{ DAT_{i:08x} = FUN_other(); }}\n");
        std::fs::write(dir.path().join(format!("FUN_{i:08x}.c")), src).unwrap();
    }

    let start = std::time::Instant::now();
    let chunks = chunk_directory(dir.path(), "corp", &ChunkOptions::default())
        .await
        .unwrap();

    let c = lang("c");
    let mut functions = 0usize;
    let mut writes_edges = 0usize;
    for chunk in &chunks {
        let r = extract_structure(chunk, c).unwrap();
        functions += r.entities.iter().filter(|e| e.kind == "function").count();
        writes_edges += r.edges.iter().filter(|e| e.kind == "writes").count();
    }
    let elapsed = start.elapsed();
    println!(
        "2000-file corpus: {} chunks, {functions} function entities, {writes_edges} writes edges, {elapsed:?}",
        chunks.len()
    );

    assert!(
        functions >= 2000,
        "expected >= 2000 functions, got {functions}"
    );
    assert!(
        writes_edges >= 2000,
        "expected >= 2000 writes edges, got {writes_edges}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "took {elapsed:?}"
    );
}

// ── Casts to undeclared typedefs ─────────────────────────────────────────────

fn assert_no_indirect(r: &ExtractedCodeStructure) {
    assert!(
        !r.edges.iter().any(|e| e.to_entity_id == indirect_id()),
        "unexpected <indirect> edge"
    );
}

#[test]
fn cast_to_unknown_typedef_is_not_a_call_and_operand_is_read() {
    let r = extract("int FUN_00001c04(void) { return (uint)(DAT_x); }\n");
    assert_no_indirect(&r);
    assert_no_ref(&r, "FUN_00001c04", "uint");
    assert!(reads(&r, "FUN_00001c04", "DAT_x"));
}

#[test]
fn cast_of_shifted_global_reads_the_global() {
    let r = extract("int FUN_00001c04(void) { return (uint)(DAT_a >> 8); }\n");
    assert_no_indirect(&r);
    assert!(reads(&r, "FUN_00001c04", "DAT_a"));
    assert_no_ref(&r, "FUN_00001c04", "uint");
}

#[test]
fn cast_of_bare_global_to_unknown_typedef_reads_it() {
    let r = extract("int FUN_00001c04(void) { return (byte)DAT_x; }\n");
    assert_no_indirect(&r);
    assert!(reads(&r, "FUN_00001c04", "DAT_x"));
    assert_no_ref(&r, "FUN_00001c04", "byte");
}

#[test]
fn cast_to_undefined4_of_parenthesized_global_reads_it() {
    let r = extract("int FUN_00001c04(void) { return (undefined4)(DAT_f); }\n");
    assert_no_indirect(&r);
    assert!(reads(&r, "FUN_00001c04", "DAT_f"));
    assert_no_ref(&r, "FUN_00001c04", "undefined4");
}

#[test]
fn sizeof_unknown_typedef_is_not_a_ref() {
    let r = extract("int FUN_00001c04(void) { return sizeof(ushort); }\n");
    assert_no_ref(&r, "FUN_00001c04", "ushort");
    assert_no_indirect(&r);
}

#[test]
fn bound_parenthesized_callee_stays_indirect() {
    let r = extract("void FUN_00001c04(void (*cb)(int)) { (cb)(1); }\n");
    assert!(
        r.edges
            .iter()
            .any(|e| e.kind == "calls" && e.to_entity_id == indirect_id())
    );
}

// ── Writes through address arithmetic ────────────────────────────────────────

#[test]
fn write_through_cast_pointer_plus_offset_targets_base() {
    let r = extract("void FUN_00001c04(int v) { *(int *)(DAT_base + 0x10) = v; }\n");
    assert!(writes(&r, "FUN_00001c04", "DAT_base"));
}

#[test]
fn write_through_address_of_plus_index_targets_base_not_index() {
    let r = extract("void FUN_00001c04(int v, int i) { *(short *)((int)&DAT_base + i) = v; }\n");
    assert!(writes(&r, "FUN_00001c04", "DAT_base"));
}

#[test]
fn offset_identifier_on_the_right_stays_a_read() {
    let r = extract("void FUN_00001c04(int v) { *(int *)(DAT_base + DAT_off) = v; }\n");
    assert!(writes(&r, "FUN_00001c04", "DAT_base"));
    assert!(reads(&r, "FUN_00001c04", "DAT_off"));
    assert!(!writes(&r, "FUN_00001c04", "DAT_off"));
}

// ── Prototypes / extern inside a body ────────────────────────────────────────

#[test]
fn prototype_inside_body_keeps_later_call_direct() {
    let r = extract("int FUN_00001c04(void) { int FUN_b(int); return FUN_b(1); }\n");
    assert!(has_edge(&r, "calls", "FUN_00001c04", "FUN_b"));
    assert_no_indirect(&r);
}

#[test]
fn extern_declaration_inside_body_keeps_later_call_direct() {
    let r = extract("int FUN_00001c04(void) { extern int FUN_b(int); return FUN_b(1); }\n");
    assert!(has_edge(&r, "calls", "FUN_00001c04", "FUN_b"));
    assert_no_indirect(&r);
}

// ── Addresses and table use ──────────────────────────────────────────────────

use callimachus_adapter_code::c::{address_of_name, canonical_address};

#[test]
fn address_of_name_recognises_decompiler_conventions() {
    assert_eq!(address_of_name("_DAT_00c00004"), Some(0xc00004));
    assert_eq!(address_of_name("RAM_ff981a"), Some(0xff981a));
    assert_eq!(address_of_name("PTR_DAT_00014000"), Some(0x14000));
    assert_eq!(address_of_name("FUN_000071d0"), Some(0x71d0));
    assert_eq!(address_of_name("fun_000071D0"), Some(0x71d0));
    assert_eq!(address_of_name("DATA_x"), None);
    assert_eq!(address_of_name("DAT_"), None);
    assert_eq!(address_of_name("DAT_zz"), None);
    assert_eq!(address_of_name("DAT_123"), None);
    assert_eq!(address_of_name("DAT_00012340x"), None);
    assert_eq!(canonical_address(0xc00004), "0xc00004");
    assert_eq!(canonical_address(0), "0x0");
}

fn aliases_of<'a>(r: &'a ExtractedCodeStructure, name: &str) -> &'a [String] {
    &r.entities
        .iter()
        .find(|e| e.canonical_name == name)
        .unwrap_or_else(|| panic!("entity {name} missing"))
        .aliases
}

#[test]
fn entities_get_address_aliases() {
    let r = extract("void FUN_00001000(void) { _DAT_00c00004 = 1; }");
    assert_eq!(aliases_of(&r, "FUN_00001000"), ["0x1000"]);
    assert_eq!(aliases_of(&r, "_DAT_00c00004"), ["0xc00004"]);
}

fn uses_table(r: &ExtractedCodeStructure, from: &str, to: &str) -> bool {
    has_edge(r, "uses_table", from, to)
}

#[test]
fn uses_table_for_address_of_subscript_and_ptr_names() {
    let r = extract(
        "void f(int i, int n) {\n\
         int *p; int x; int y; long q;\n\
         p = &DAT_00012340;\n\
         x = (&DAT_00012341)[i];\n\
         y = DAT_00012342[i];\n\
         q = (int)&PTR_DAT_00014000 + 4;\n\
         }",
    );
    for t in [
        "DAT_00012340",
        "DAT_00012341",
        "DAT_00012342",
        "PTR_DAT_00014000",
    ] {
        assert!(uses_table(&r, "f", t), "missing uses_table to {t}");
    }
}

#[test]
fn uses_table_for_ptr_prefixed_name_without_address_taken() {
    let r = extract("void f(void) { g(PTR_DAT_00014000); }");
    assert!(uses_table(&r, "f", "PTR_DAT_00014000"));
}

#[test]
fn pointer_literal_table_shares_id_with_named_reference() {
    let a = extract_at(
        "src/a.c",
        "file",
        "c",
        "void f(int n) { short *r; r = (short *)((ushort)(n * 0x800) + 0x18000); }",
    );
    assert!(uses_table(&a, "f", "DAT_00018000"));
    assert_eq!(aliases_of(&a, "DAT_00018000"), ["0x18000"]);
    let b = extract_at(
        "src/b.c",
        "file",
        "c",
        "void g(void) { int v; v = DAT_00018000; }",
    );
    let stub_a = a
        .entities
        .iter()
        .find(|e| e.canonical_name == "DAT_00018000");
    let stub_b = b
        .entities
        .iter()
        .find(|e| e.canonical_name == "DAT_00018000");
    assert_eq!(stub_a.unwrap().id, stub_b.unwrap().id);
    assert!(reads(&b, "g", "DAT_00018000"));
}

#[test]
fn direct_pointer_cast_literal_is_a_table() {
    let r = extract("void f(void) { int *p; p = (int *)0x18000; }");
    assert!(uses_table(&r, "f", "DAT_00018000"));
}

#[test]
fn uses_table_negative_cases() {
    let r = extract(
        "void f(unsigned char *p) {\n\
         int x; int v;\n\
         if ((unsigned char *)0xff3fff < p) { x = 1; }\n\
         x = 0x1234;\n\
         v = DAT_00012340;\n\
         }",
    );
    assert!(!uses_table(&r, "f", "DAT_00ff3fff"));
    assert!(
        !r.edges.iter().any(|e| e.kind == "uses_table"
            && (e.to_entity_id.contains("ff3fff") || e.to_entity_id.contains("1234"))),
        "unexpected uses_table edge"
    );
    assert!(reads(&r, "f", "DAT_00012340"));
    assert!(!uses_table(&r, "f", "DAT_00012340"));
}

// ── S10d: decompiler-name filter is scoped ───────────────────────────────────

const DECOMPILER_NAMED_SRC: &str = "void f(void) { code = 1; addr = word; g(in_buffer); }";

#[test]
fn plain_c_keeps_globals_named_like_ghidra_types() {
    let r = extract(DECOMPILER_NAMED_SRC);
    let vars = entities_of_kind(&r, "variable");
    for name in ["code", "addr", "word", "in_buffer"] {
        assert!(vars.contains(&name), "{name} missing from {vars:?}");
    }
    assert!(has_edge(&r, "writes", "f", "code"));
    assert!(has_edge(&r, "reads", "f", "word"));
    assert!(has_edge(&r, "reads", "f", "in_buffer"));
}

#[test]
fn decompiler_names_option_filters_ghidra_names() {
    let chunk = make_chunk(CORP, "src/f.c", "file", DECOMPILER_NAMED_SRC);
    let r = callimachus_adapter_code::analyse_c(&chunk, &chunk.content, true).unwrap();
    assert!(entities_of_kind(&r, "variable").is_empty(), "{r:?}");
}

#[test]
fn ghidra_stub_prefixes_carry_addresses() {
    assert_eq!(address_of_name("PTR_LAB_00001234"), Some(0x1234));
    assert_eq!(address_of_name("PTR_FUN_00001234"), Some(0x1234));
    assert_eq!(address_of_name("PTR_caseD_00001234"), Some(0x1234));
}

#[test]
fn address_of_name_handles_multi_segment_ghidra_stubs() {
    assert_eq!(address_of_name("PTR_caseD_21_00010080"), Some(0x10080));
    assert_eq!(address_of_name("PTR_caseD_43_00010110"), Some(0x10110));
    assert_eq!(
        address_of_name("PTR_LAB_000043c6_2_00060878"),
        Some(0x60878)
    );
    assert_eq!(address_of_name("PTR_caseD_21_zz"), None);
    assert_eq!(address_of_name("PTR_caseD_21_"), None);
    assert_eq!(address_of_name("PTR_caseD_21_00010080x"), None);
    assert_eq!(address_of_name("DAT_00012340x"), None);
}

#[test]
fn multi_segment_ghidra_stubs_carry_address_aliases() {
    let r = extract(
        "void f(void) {\n\
         g(PTR_caseD_21_00010080);\n\
         g(PTR_caseD_43_00010110);\n\
         g(PTR_LAB_000043c6_2_00060878);\n\
         }",
    );
    assert_eq!(aliases_of(&r, "PTR_caseD_21_00010080"), ["0x10080"]);
    assert_eq!(aliases_of(&r, "PTR_caseD_43_00010110"), ["0x10110"]);
    assert_eq!(aliases_of(&r, "PTR_LAB_000043c6_2_00060878"), ["0x60878"]);
}

// ── Struct-field access entities ─────────────────────────────────────────────

fn field_entity<'a>(
    r: &'a ExtractedCodeStructure,
    name: &str,
) -> Option<&'a callimachus_core::types::Entity> {
    r.entities
        .iter()
        .find(|e| e.kind == "field" && e.canonical_name == name)
}

fn field_names(r: &ExtractedCodeStructure) -> Vec<&str> {
    entities_of_kind(r, "field")
}

#[test]
fn write_through_cast_deref_of_free_register_base_is_a_field_write() {
    let r = extract("void FUN_a(void)\n{\n  *(short *)(unaff_A2 + 0x10) = 1;\n}\n");
    let f = field_entity(&r, "unaff_A2+0x10")
        .unwrap_or_else(|| panic!("no field entity: {:?}", field_names(&r)));
    assert_eq!(f.id, id("unaff_A2+0x10"));
    assert_eq!(f.id, format!("{CORP}:unaff_a2+0x10"));
    assert!((f.confidence - 0.5).abs() < 1e-6, "{}", f.confidence);
    assert!(writes(&r, "FUN_a", "unaff_A2+0x10"));
    assert!(!reads(&r, "FUN_a", "unaff_A2+0x10"));
}

#[test]
fn read_through_cast_deref_is_a_field_read_with_width_alias() {
    let r = extract("void FUN_a(void)\n{\n  short x;\n  x = *(short *)(unaff_A2 + 0x12);\n}\n");
    let f = field_entity(&r, "unaff_A2+0x12").expect("field entity");
    assert!(
        f.aliases.contains(&"width:2".to_string()),
        "{:?}",
        f.aliases
    );
    assert!(reads(&r, "FUN_a", "unaff_A2+0x12"));
    assert!(!writes(&r, "FUN_a", "unaff_A2+0x12"));
}

#[test]
fn field_width_aliases_follow_the_pointee_type() {
    let r = extract(
        "void FUN_a(void)\n{\n  int a;\n  a = *(char *)(unaff_A2 + 0x1);\n  a = *(byte *)(unaff_A2 + 0x2);\n  a = *(undefined1 *)(unaff_A2 + 0x3);\n  a = *(ushort *)(unaff_A2 + 0x4);\n  a = *(undefined2 *)(unaff_A2 + 0x6);\n  a = *(int *)(unaff_A2 + 0x8);\n  a = *(uint *)(unaff_A2 + 0xc);\n  a = *(undefined4 *)(unaff_A2 + 0x10);\n}\n",
    );
    for (name, width) in [
        ("unaff_A2+0x1", 1),
        ("unaff_A2+0x2", 1),
        ("unaff_A2+0x3", 1),
        ("unaff_A2+0x4", 2),
        ("unaff_A2+0x6", 2),
        ("unaff_A2+0x8", 4),
        ("unaff_A2+0xc", 4),
        ("unaff_A2+0x10", 4),
    ] {
        let f = field_entity(&r, name).unwrap_or_else(|| panic!("missing {name}"));
        assert!(
            f.aliases.contains(&format!("width:{width}")),
            "{name}: {:?}",
            f.aliases
        );
    }
}

#[test]
fn first_seen_width_wins_when_a_field_is_accessed_at_two_widths() {
    let r = extract(
        "void FUN_a(void)\n{\n  int a;\n  a = *(short *)(unaff_A2 + 0x10);\n  a = *(int *)(unaff_A2 + 0x10);\n}\n",
    );
    let f = field_entity(&r, "unaff_A2+0x10").expect("field");
    assert!(
        f.aliases.contains(&"width:2".to_string()),
        "{:?}",
        f.aliases
    );
    assert!(
        !f.aliases.contains(&"width:4".to_string()),
        "{:?}",
        f.aliases
    );
}

#[test]
fn bare_deref_of_a_parameter_is_field_at_offset_zero() {
    let r = extract("int FUN_a(int *param_1)\n{\n  return *(int *)param_1;\n}\n");
    assert!(
        field_entity(&r, "param_1+0x0").is_some(),
        "{:?}",
        field_names(&r)
    );
    assert!(reads(&r, "FUN_a", "param_1+0x0"));
}

#[test]
fn deref_with_parameter_base_and_offset_is_a_field() {
    let r = extract("int FUN_a(char *param_1)\n{\n  return *(int *)(param_1 + 8);\n}\n");
    assert!(
        field_entity(&r, "param_1+0x8").is_some(),
        "{:?}",
        field_names(&r)
    );
}

#[test]
fn deref_of_an_ordinary_local_is_not_a_field() {
    let r = extract(
        "void FUN_a(void)\n{\n  short *puVar2;\n  puVar2 = (short *)&DAT_00ff0000;\n  *(short *)(puVar2 + 0x10) = 1;\n  *(short *)puVar2 = 2;\n}\n",
    );
    assert!(
        field_names(&r).is_empty(),
        "locals are not field bases: {:?}",
        field_names(&r)
    );
}

#[test]
fn compound_assignment_through_a_field_is_a_write() {
    let r = extract("void FUN_a(void)\n{\n  *(short *)(unaff_A2 + 0x14) += 1;\n}\n");
    assert!(writes(&r, "FUN_a", "unaff_A2+0x14"));
}

#[test]
fn increment_through_a_field_is_a_write() {
    let r = extract("void FUN_a(void)\n{\n  (*(char *)(unaff_A2 + 0x15))++;\n}\n");
    assert!(writes(&r, "FUN_a", "unaff_A2+0x15"));
}

#[test]
fn negative_offsets_are_named_with_a_minus_sign() {
    let r = extract("int FUN_a(void)\n{\n  return *(int *)(unaff_A2 - 0x4);\n}\n");
    assert!(
        field_entity(&r, "unaff_A2-0x4").is_some(),
        "{:?}",
        field_names(&r)
    );
    assert!(reads(&r, "FUN_a", "unaff_A2-0x4"));
}

#[test]
fn in_and_extraout_prefixed_names_are_field_bases() {
    let r = extract(
        "int FUN_a(void)\n{\n  int a;\n  a = *(int *)(in_A3 + 0x2);\n  a = *(int *)(extraout_A4 + 0x2);\n  return a;\n}\n",
    );
    assert!(
        field_entity(&r, "in_A3+0x2").is_some(),
        "{:?}",
        field_names(&r)
    );
    assert!(
        field_entity(&r, "extraout_A4+0x2").is_some(),
        "{:?}",
        field_names(&r)
    );
}

#[test]
fn base_declared_as_a_local_is_still_a_field_base_when_it_matches_the_pattern() {
    let r =
        extract("void FUN_a(void)\n{\n  byte *unaff_A2;\n  *(short *)(unaff_A2 + 0x10) = 1;\n}\n");
    assert!(
        field_entity(&r, "unaff_A2+0x10").is_some(),
        "{:?}",
        field_names(&r)
    );
}

#[test]
fn repeated_accesses_in_one_function_yield_one_edge_per_kind() {
    let r = extract(
        "void FUN_a(void)\n{\n  short x;\n  x = *(short *)(unaff_A2 + 0x10);\n  x = *(short *)(unaff_A2 + 0x10);\n  *(short *)(unaff_A2 + 0x10) = 1;\n  *(short *)(unaff_A2 + 0x10) = 2;\n}\n",
    );
    let f = id("unaff_A2+0x10");
    assert_eq!(count_edges(&r, "reads", "FUN_a", &f), 1);
    assert_eq!(count_edges(&r, "writes", "FUN_a", &f), 1);
    assert_eq!(
        field_names(&r)
            .iter()
            .filter(|n| **n == "unaff_A2+0x10")
            .count(),
        1
    );
}

#[test]
fn two_functions_using_the_same_field_share_one_entity_id() {
    let r = extract(
        "void FUN_a(void)\n{\n  *(short *)(unaff_A2 + 0x10) = 1;\n}\nint FUN_b(void)\n{\n  return *(short *)(unaff_A2 + 0x10);\n}\n",
    );
    let f = id("unaff_A2+0x10");
    assert!(r.entities.iter().any(|e| e.id == f && e.kind == "field"));
    assert_eq!(count_edges(&r, "writes", "FUN_a", &f), 1);
    assert_eq!(count_edges(&r, "reads", "FUN_b", &f), 1);
}
