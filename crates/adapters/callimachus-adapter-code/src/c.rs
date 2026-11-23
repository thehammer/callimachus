//! C-specific structure extraction.
//!
//! C needs more than the generic query-driven path: names live inside nested
//! declarators (`int *(*f)(void)`), file-scope declarations become `variable`
//! and `type` entities, and function bodies are analysed for references to
//! global memory (free identifiers) and indirect calls.
//!
//! See `docs/c-free-identifiers.md` for the semantics.

use std::collections::{BTreeMap, HashSet};

use callimachus_adapter_contract::{Chunk, Edge, Entity};
use tree_sitter::Node;

use crate::extractor::{ExtractedCodeStructure, edge_id, entity_id, scope_for_pos};

/// Canonical name of the synthetic callee for calls through pointers.
pub const INDIRECT_NAME: &str = "<indirect>";

/// Decompiler naming conventions recognised as carrying an address: a name is
/// one of these prefixes (case-insensitive) followed by 4-16 hex digits.
pub const ADDRESS_PREFIXES: &[&str] = &[
    "_DAT_", "DAT_", "PTR_DAT_", "PTR_", "RAM_", "FUN_", "LAB_", "SUB_",
];

/// Address encoded in a decompiler-style name (`_DAT_00c00004`, `FUN_000071d0`).
/// The value is returned as written; no masking or width normalisation.
pub fn address_of_name(name: &str) -> Option<u64> {
    let lower = name.to_ascii_lowercase();
    ADDRESS_PREFIXES.iter().find_map(|p| {
        let digits = lower.strip_prefix(&p.to_ascii_lowercase())?;
        if (4..=16).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            u64::from_str_radix(digits, 16).ok()
        } else {
            None
        }
    })
}

/// Canonical alias form of an address: lowercase hex, no leading zeros.
pub fn canonical_address(v: u64) -> String {
    format!("0x{v:x}")
}

/// Tree-sitter query for top-level items; used by the chunker.
pub const C_TOP_LEVEL_QUERY: &str = r#"
(translation_unit
  [(function_definition) (declaration) (type_definition)
   (struct_specifier) (union_specifier) (enum_specifier)] @item)
"#;

/// Callee query kept for registry symmetry; C calls are handled by this module.
pub const C_CALL_QUERY: &str = r#"
(call_expression function: (identifier) @callee)
"#;

/// Name of the symbol declared by a chunker item (`function_definition`,
/// `declaration`, `type_definition`, …) given the item's source text.
pub fn item_name(text: &str) -> Option<String> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_c::LANGUAGE.into()).ok()?;
    let tree = parser.parse(text, None)?;
    let source = text.as_bytes();
    let item = top_level_items(tree.root_node()).into_iter().next()?;
    declared_symbols(item, source)
        .into_iter()
        .next()
        .map(|(name, _)| name)
}

/// Emit entities (`function`, `variable`, `type`) and all edges for a C chunk.
pub fn extract(
    chunk: &Chunk,
    root: Node<'_>,
    source: &[u8],
    test_ranges: &[std::ops::Range<usize>],
    result: &mut ExtractedCodeStructure,
) {
    let file_path = chunk
        .location
        .path
        .split('#')
        .next()
        .unwrap_or(&chunk.location.path)
        .to_string();
    let file_id = entity_id(&chunk.corpus_id, &file_path);

    let items = top_level_items(root);
    let enum_constants = collect_enum_constants(root, source);

    for item in &items {
        for (name, kind) in declared_symbols(*item, source) {
            let id = entity_id(&chunk.corpus_id, &name);
            push_entity(chunk, result, &id, &name, kind, Some(*item), 0.9);
            let scope = scope_for_pos(item.start_byte(), test_ranges);
            push_edge(chunk, result, &file_id, "defines", &id, scope);
        }
    }

    for item in &items {
        if item.kind() == "function_definition" {
            analyse_function(chunk, *item, source, &enum_constants, test_ranges, result);
        }
    }

    // Keep the first entity emitted per id (declarations before free-identifier
    // stubs); a HashSet keeps this linear on large files.
    let mut seen = HashSet::new();
    result.entities.retain(|e| seen.insert(e.id.clone()));
}

// ── Declarations ─────────────────────────────────────────────────────────────

/// Top-level items, looking through preprocessor conditionals and `extern "C"`.
fn top_level_items(root: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    // Explicit stack: nesting depth is attacker-controlled in source files.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        let mut containers = Vec::new();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "function_definition"
                | "declaration"
                | "type_definition"
                | "struct_specifier"
                | "union_specifier"
                | "enum_specifier" => out.push(child),
                "preproc_if"
                | "preproc_ifdef"
                | "preproc_else"
                | "preproc_elif"
                | "linkage_specification"
                | "declaration_list" => containers.push(child),
                _ => {}
            }
        }
        stack.extend(containers.into_iter().rev());
    }
    out.sort_by_key(|n| n.start_byte());
    out
}

/// Descend `declarator` fields to the declared identifier.
fn declarator_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut n = node;
    loop {
        match n.kind() {
            "identifier" | "type_identifier" | "field_identifier" => {
                return n.utf8_text(source).ok().map(str::to_string);
            }
            _ => {
                n = n
                    .child_by_field_name("declarator")
                    .or_else(|| n.named_child(0))?;
            }
        }
    }
}

fn is_function_declarator(node: Node<'_>) -> bool {
    let mut n = node;
    loop {
        match n.kind() {
            // `int (*fp)(void)` has a parenthesized declarator: a pointer
            // variable, not a prototype.
            "function_declarator" => {
                return n
                    .child_by_field_name("declarator")
                    .is_some_and(|d| d.kind() == "identifier");
            }
            "pointer_declarator" | "array_declarator" | "init_declarator" => {
                match n.child_by_field_name("declarator") {
                    Some(d) => n = d,
                    None => return false,
                }
            }
            _ => return false,
        }
    }
}

/// The `(name, entity kind)` pairs an item declares.
fn declared_symbols(item: Node<'_>, source: &[u8]) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    let text = |n: Node<'_>| n.utf8_text(source).ok().map(str::to_string);
    match item.kind() {
        "function_definition" => {
            if let Some(name) = item
                .child_by_field_name("declarator")
                .and_then(|d| declarator_name(d, source))
            {
                out.push((name, "function"));
            }
        }
        "struct_specifier" | "union_specifier" | "enum_specifier" => {
            if let Some(name) = item.child_by_field_name("name").and_then(text) {
                out.push((name, "type"));
            }
        }
        "declaration" | "type_definition" => {
            let is_typedef = item.kind() == "type_definition";
            // A tagged struct/enum/union definition in the type position.
            if let Some(ty) = item.child_by_field_name("type")
                && matches!(
                    ty.kind(),
                    "struct_specifier" | "union_specifier" | "enum_specifier"
                )
                && ty.child_by_field_name("body").is_some()
                && let Some(name) = ty.child_by_field_name("name").and_then(text)
            {
                out.push((name, "type"));
            }
            let mut cursor = item.walk();
            for decl in item.children_by_field_name("declarator", &mut cursor) {
                if !is_typedef && is_function_declarator(decl) {
                    continue; // prototype
                }
                if let Some(name) = declarator_name(decl, source) {
                    out.push((name, if is_typedef { "type" } else { "variable" }));
                }
            }
        }
        _ => {}
    }
    out
}

fn collect_enum_constants(root: Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut set = HashSet::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "enumerator"
            && let Some(name) = n
                .child_by_field_name("name")
                .and_then(|x| x.utf8_text(source).ok())
        {
            set.insert(name.to_string());
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    set
}

// ── Function bodies ──────────────────────────────────────────────────────────

#[derive(Default)]
struct Access {
    reads: bool,
    writes: bool,
    uses_table: bool,
}

fn analyse_function(
    chunk: &Chunk,
    func: Node<'_>,
    source: &[u8],
    enum_constants: &HashSet<String>,
    test_ranges: &[std::ops::Range<usize>],
    result: &mut ExtractedCodeStructure,
) {
    let Some(func_name) = func
        .child_by_field_name("declarator")
        .and_then(|d| declarator_name(d, source))
    else {
        return;
    };
    let Some(body) = func.child_by_field_name("body") else {
        return;
    };
    let from_id = entity_id(&chunk.corpus_id, &func_name);

    // Names that shadow globals: parameters plus every local declared anywhere
    // in the body (function-wide, not block-scoped — see docs).
    let mut bound: HashSet<String> = HashSet::new();
    if let Some(decl) = func.child_by_field_name("declarator") {
        collect_parameters(decl, source, &mut bound);
    }
    collect_locals(body, source, &mut bound);

    let mut accesses: BTreeMap<String, Access> = BTreeMap::new();
    let mut direct_calls: Vec<(String, usize)> = Vec::new();
    let mut indirect_calls: Vec<usize> = Vec::new();
    // Address -> table stub target for pointer literals.
    let mut literal_tables: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();

    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                let callee = n.child_by_field_name("function");
                match callee {
                    Some(c) if c.kind() == "identifier" => {
                        let name = text_of(c, source);
                        if bound.contains(&name) {
                            indirect_calls.push(n.start_byte());
                        } else {
                            direct_calls.push((name, n.start_byte()));
                        }
                    }
                    Some(c) => match paren_identifier(c) {
                        // `(uint)(x)`: an unbound name in parentheses is a
                        // cast to an undeclared typedef, not a call.
                        Some(id) if !bound.contains(&text_of(id, source)) => {}
                        // `(cb)(x)` with a bound name is a real pointer call.
                        _ => {
                            indirect_calls.push(n.start_byte());
                            stack.push(c);
                        }
                    },
                    None => {}
                }
                if let Some(args) = n.child_by_field_name("arguments") {
                    stack.push(args);
                }
                continue;
            }
            // `sizeof(ushort)`: an unbound bare name is a type name.
            "sizeof_expression" => {
                if let Some(v) = n.child_by_field_name("value")
                    && let Some(id) =
                        paren_identifier(v).or(Some(v).filter(|v| v.kind() == "identifier"))
                    && !bound.contains(&text_of(id, source))
                {
                    continue;
                }
            }
            "number_literal" => {
                if let Some(v) = parse_int_literal(&text_of(n, source))
                    && v >= 0x100
                    && is_pointer_literal(n, source)
                {
                    literal_tables.insert(v);
                }
                continue;
            }
            "identifier" => {
                let name = text_of(n, source);
                if !bound.contains(&name) && !enum_constants.contains(&name) {
                    let table = is_table_use(n, &name);
                    let access = accesses.entry(name).or_default();
                    access.uses_table |= table;
                    if is_write_target(n) {
                        access.writes = true;
                    } else {
                        access.reads = true;
                    }
                }
                continue;
            }
            _ => {}
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    for (callee, pos) in direct_calls {
        let to_id = entity_id(&chunk.corpus_id, &callee);
        let scope = scope_for_pos(pos, test_ranges);
        push_edge(chunk, result, &from_id, "calls", &to_id, scope);
    }
    if !indirect_calls.is_empty() {
        let to_id = entity_id(&chunk.corpus_id, INDIRECT_NAME);
        push_entity(chunk, result, &to_id, INDIRECT_NAME, "synthetic", None, 1.0);
        for pos in indirect_calls {
            let scope = scope_for_pos(pos, test_ranges);
            push_edge(chunk, result, &from_id, "calls", &to_id, scope);
        }
    }
    let scope = scope_for_pos(func.start_byte(), test_ranges);
    for (name, access) in accesses {
        let to_id = entity_id(&chunk.corpus_id, &name);
        push_entity(chunk, result, &to_id, &name, "variable", None, 0.5);
        if access.reads {
            push_edge(chunk, result, &from_id, "reads", &to_id, scope.clone());
        }
        if access.writes {
            push_edge(chunk, result, &from_id, "writes", &to_id, scope.clone());
        }
        if access.uses_table {
            push_edge(chunk, result, &from_id, "uses_table", &to_id, scope.clone());
        }
    }
    for v in literal_tables {
        let name = format!("DAT_{v:08x}");
        let to_id = entity_id(&chunk.corpus_id, &name);
        push_entity(chunk, result, &to_id, &name, "variable", None, 0.5);
        push_edge(chunk, result, &from_id, "uses_table", &to_id, scope.clone());
    }
}

/// Table-use heuristic for a named identifier: operand of unary `&`, base of a
/// subscript (directly or via `(&X)[i]`), or a `PTR_`-prefixed name.
fn is_table_use(ident: Node<'_>, name: &str) -> bool {
    if name.len() >= 4 && name[..4].eq_ignore_ascii_case("PTR_") {
        return true;
    }
    let Some(parent) = ident.parent() else {
        return false;
    };
    match parent.kind() {
        "pointer_expression" => parent.child(0).is_some_and(|op| op.kind() == "&"),
        "subscript_expression" => parent
            .child_by_field_name("argument")
            .is_some_and(|a| a.id() == ident.id()),
        _ => false,
    }
}

/// Heuristic: a literal is a table address when it is the operand of a pointer
/// cast, `(T *)0x18000`, or an operand of `+` inside a parenthesised expression
/// that is itself the operand of a pointer cast, `(T *)(e + 0x18000)`.
fn is_pointer_literal(lit: Node<'_>, source: &[u8]) -> bool {
    let Some(parent) = lit.parent() else {
        return false;
    };
    match parent.kind() {
        "cast_expression" => is_pointer_cast(parent, source) && !in_comparison(parent),
        "binary_expression" => {
            let is_plus = parent
                .child_by_field_name("operator")
                .is_some_and(|o| o.kind() == "+");
            let paren = parent
                .parent()
                .filter(|p| p.kind() == "parenthesized_expression");
            is_plus
                && paren
                    .and_then(|p| p.parent())
                    .is_some_and(|c| c.kind() == "cast_expression" && is_pointer_cast(c, source))
        }
        _ => false,
    }
}

/// Bounds checks like `(T *)0xff3fff < p` compare addresses; they are not uses.
fn in_comparison(node: Node<'_>) -> bool {
    node.parent().is_some_and(|p| {
        p.kind() == "binary_expression"
            && p.child_by_field_name("operator")
                .is_some_and(|o| matches!(o.kind(), "<" | ">" | "<=" | ">=" | "==" | "!="))
    })
}

fn is_pointer_cast(cast: Node<'_>, source: &[u8]) -> bool {
    cast.child_by_field_name("type")
        .is_some_and(|t| text_of(t, source).contains('*'))
}

fn parse_int_literal(text: &str) -> Option<u64> {
    let t = text.trim_end_matches(['u', 'U', 'l', 'L']);
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else {
        t.parse().ok()
    }
}

/// True when `ident` is (the base of) the target of an assignment or `++`/`--`.
///
/// Climbs through parentheses, casts, `*`/`&`, subscripts (as the base, not the
/// index) and field access.
fn is_write_target(ident: Node<'_>) -> bool {
    let mut node = ident;
    while let Some(parent) = node.parent() {
        let is_field = |name: &str| {
            parent
                .child_by_field_name(name)
                .is_some_and(|c| c.id() == node.id())
        };
        match parent.kind() {
            "parenthesized_expression" | "pointer_expression" => {}
            "cast_expression" if is_field("value") => {}
            // Address arithmetic: `*(int *)(DAT_base + 0x10) = v`. Only the
            // left operand is treated as the base, so an offset on the right
            // stays a read.
            "binary_expression"
                if is_field("left")
                    && parent
                        .child_by_field_name("operator")
                        .is_some_and(|o| matches!(o.kind(), "+" | "-")) => {}
            "subscript_expression" | "field_expression" if is_field("argument") => {}
            "assignment_expression" => return is_field("left"),
            "update_expression" => return is_field("argument"),
            _ => return false,
        }
        node = parent;
    }
    false
}

fn collect_parameters(declarator: Node<'_>, source: &[u8], out: &mut HashSet<String>) {
    let mut stack = vec![declarator];
    while let Some(n) = stack.pop() {
        if n.kind() == "parameter_declaration" {
            if let Some(name) = n
                .child_by_field_name("declarator")
                .and_then(|d| declarator_name(d, source))
            {
                out.insert(name);
            }
            continue;
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
}

fn collect_locals(body: Node<'_>, source: &[u8], out: &mut HashSet<String>) {
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "declaration" && !is_extern(n, source) {
            let mut cursor = n.walk();
            for decl in n.children_by_field_name("declarator", &mut cursor) {
                // Prototypes (`int FUN_b(int);`) declare no local binding.
                if is_function_declarator(decl) {
                    continue;
                }
                if let Some(name) = declarator_name(decl, source) {
                    out.insert(name);
                }
            }
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
}

/// `extern` storage class on a declaration.
fn is_extern(decl: Node<'_>, source: &[u8]) -> bool {
    let mut cursor = decl.walk();
    decl.children(&mut cursor)
        .any(|c| c.kind() == "storage_class_specifier" && text_of(c, source) == "extern")
}

/// The identifier in `(name)`, if `node` is a parenthesized bare identifier.
fn paren_identifier(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() != "parenthesized_expression" {
        return None;
    }
    node.named_child(0).filter(|c| c.kind() == "identifier")
}

fn text_of(node: Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or_default().to_string()
}

// ── Emission helpers ─────────────────────────────────────────────────────────

fn push_entity(
    chunk: &Chunk,
    result: &mut ExtractedCodeStructure,
    id: &str,
    name: &str,
    kind: &str,
    node: Option<Node<'_>>,
    confidence: f32,
) {
    let mut entity = Entity::new(
        id.to_string(),
        chunk.corpus_id.clone(),
        name.to_string(),
        kind.to_string(),
    );
    entity.first_location = Some(chunk.location.clone());
    entity.last_location = Some(chunk.location.clone());
    entity.appearance_count = 1;
    entity.confidence = confidence;
    if matches!(kind, "function" | "variable")
        && let Some(addr) = address_of_name(name)
    {
        entity.aliases.push(canonical_address(addr));
    }
    if let Some(n) = node {
        entity.start_line = Some(n.start_position().row as u32);
        entity.end_line = Some(n.end_position().row as u32);
    }
    result.entities.push(entity);
}

fn push_edge(
    chunk: &Chunk,
    result: &mut ExtractedCodeStructure,
    from_id: &str,
    kind: &str,
    to_id: &str,
    scope: String,
) {
    let eid = edge_id(&chunk.corpus_id, from_id, kind, to_id, &scope);
    let mut edge = Edge::new(
        eid,
        chunk.corpus_id.clone(),
        from_id.to_string(),
        to_id.to_string(),
        kind.to_string(),
        chunk.location.clone(),
    );
    edge.origin_scope = scope;
    result.edges.push(edge);
}
