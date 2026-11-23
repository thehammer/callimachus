//! Object layouts: `layout` facts that name struct fields by byte offset.
//!
//! A layout binds field names to `(base, offset)` pairs. The C adapter emits
//! one `field` entity per pair (`unaff_A2+0x10`); the layout names it
//! (`obj.x`) and, at query time, supplies the names used to annotate source.
//! See `docs/facts.md`.

use std::collections::BTreeMap;

use callimachus_adapter_contract::{FieldArray, FieldName};

use crate::corrections::address::{apply_mask, parse_address};
use crate::corrections::engine::CorrectionsEngine;
use crate::corrections::facts::{CONFIDENCE_VALUES, FactRecord};

/// Kind string of layout facts.
pub const LAYOUT_KIND: &str = "layout";

/// A parsed `layout` fact payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub name: String,
    pub bases: Vec<String>,
    /// Functions (ids, names or addresses) the layout is scoped to; empty means
    /// every function. Written as `data.functions: [..]` or
    /// `data.functions: {include: [..], exclude: [..]}`. A scoped layout anchors
    /// only the field entities accessed in these functions.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub fields: Vec<LayoutField>,
    /// Optional base → CPU register overrides (lowercase).
    pub registers: BTreeMap<String, String>,
    /// Set when the layout describes a fixed-address array of records.
    pub array: Option<LayoutArray>,
}

/// `data.array`: N records of `stride` bytes at a constant address.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutArray {
    /// Start address as written (unmasked).
    pub address: u64,
    pub stride: u64,
    pub count: Option<u64>,
}

impl LayoutArray {
    fn parse(raw: &serde_json::Value) -> Result<LayoutArray, String> {
        let obj = raw
            .as_object()
            .ok_or("`data.array` must be an object with `address` and `stride`")?;
        let number = |key: &str| -> Result<u64, String> {
            match obj.get(key) {
                Some(serde_json::Value::String(s)) => {
                    parse_address(s).ok_or_else(|| format!("`data.array.{key}` is not hex: `{s}`"))
                }
                Some(_) => Err(format!("`data.array.{key}` must be a hex string")),
                None => Err(format!("`data.array.{key}` is required")),
            }
        };
        let address = number("address")?;
        let stride = number("stride")?;
        if stride == 0 {
            return Err("`data.array.stride` must be greater than zero".into());
        }
        let count = match obj.get("count") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => match v.as_u64() {
                Some(c) if c > 0 => Some(c),
                _ => return Err("`data.array.count` must be a positive integer".into()),
            },
        };
        Ok(LayoutArray {
            address,
            stride,
            count,
        })
    }

    /// Start address with the corpus mask applied.
    pub fn masked_address(&self, mask: Option<u64>) -> u64 {
        apply_mask(self.address, mask)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayoutField {
    pub offset: i64,
    pub name: String,
    pub ty: Option<String>,
    pub summary: Option<String>,
    pub confidence: Option<String>,
}

impl Layout {
    /// Parse and validate `data` of a `layout` fact. The error is the rejection reason.
    pub fn parse(data: &serde_json::Value) -> Result<Layout, String> {
        let obj = data
            .as_object()
            .ok_or("`data` must be an object for kind `layout`")?;
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or("`data.name` is required for kind `layout`")?;
        if !is_valid_name(name) {
            return Err(format!(
                "`data.name` must match [a-z][a-z0-9_]{{0,31}} (got `{name}`)"
            ));
        }
        let array = match obj.get("array") {
            None | Some(serde_json::Value::Null) => None,
            Some(a) => Some(LayoutArray::parse(a)?),
        };
        let bases = string_list(obj.get("bases"), "data.bases")?;
        if bases.is_empty() && array.is_none() {
            return Err("`data.bases` must name at least one base (or give `data.array`)".into());
        }
        if let Some(b) = bases.iter().find(|b| !is_identifier(b)) {
            return Err(format!("`data.bases` entry `{b}` is not an identifier"));
        }
        let (include, exclude) = match obj.get("functions") {
            None | Some(serde_json::Value::Null) => (vec![], vec![]),
            Some(serde_json::Value::Array(_)) => {
                (string_list(obj.get("functions"), "data.functions")?, vec![])
            }
            Some(f) => {
                let f = f.as_object().ok_or(
                    "`data.functions` must be a list or an object with `include`/`exclude`",
                )?;
                (
                    string_list(f.get("include"), "data.functions.include")?,
                    string_list(f.get("exclude"), "data.functions.exclude")?,
                )
            }
        };
        let mut registers = BTreeMap::new();
        match obj.get("registers") {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::Object(m)) => {
                for (base, reg) in m {
                    let reg = reg
                        .as_str()
                        .ok_or("`data.registers` must map base names to register names")?;
                    registers.insert(base.clone(), reg.to_ascii_lowercase());
                }
            }
            Some(_) => return Err("`data.registers` must map base names to register names".into()),
        }
        let raw_fields = match obj.get("fields") {
            Some(serde_json::Value::Array(a)) if !a.is_empty() => a,
            _ => return Err("`data.fields` must be a non-empty list".into()),
        };
        let mut fields: Vec<LayoutField> = Vec::new();
        for raw in raw_fields {
            let field = LayoutField::parse(raw)?;
            if fields.iter().any(|f| f.name == field.name) {
                return Err(format!("duplicate field name `{}`", field.name));
            }
            fields.push(field);
        }
        Ok(Layout {
            name: name.to_string(),
            bases,
            include,
            exclude,
            fields,
            registers,
            array,
        })
    }

    /// Parse the payload of `fact` when it is a `layout` fact.
    pub fn of_fact(fact: &FactRecord) -> Option<Layout> {
        (fact.kind == LAYOUT_KIND)
            .then(|| Layout::parse(&fact.data).ok())
            .flatten()
    }

    /// True when the layout applies to `function_name` (annotation scope). An
    /// entry may be the function name or its id (`corpus:name`).
    pub fn admits(&self, function_name: &str) -> bool {
        let is = |entry: &String| {
            entry.eq_ignore_ascii_case(function_name)
                || entry
                    .split_once(':')
                    .is_some_and(|(_, n)| n.eq_ignore_ascii_case(function_name))
        };
        (self.include.is_empty() || self.include.iter().any(is)) && !self.exclude.iter().any(is)
    }

    /// True when `data.functions` names the functions the layout is scoped to.
    pub fn is_scoped(&self) -> bool {
        !self.include.is_empty()
    }

    /// Bases that are per-function register names (`in_A0`, `unaff_A2`, ...).
    pub fn register_bases(&self) -> impl Iterator<Item = &String> {
        self.bases.iter().filter(|b| is_register_base(b))
    }

    /// Candidate anchor entity ids in `corpus_id`: a register-style base gives
    /// one id per function name in `functions` (ids are function-qualified for
    /// those, `{corpus}:{base+0xoff}@{function}`), any other base gives its
    /// global id. The caller keeps those that exist.
    pub fn anchor_ids(&self, corpus_id: &str, functions: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        for b in &self.bases {
            for f in &self.fields {
                let id = entity_id(corpus_id, b, f.offset);
                if is_register_base(b) {
                    out.extend(
                        functions
                            .iter()
                            .map(|func| format!("{id}@{}", func.to_lowercase())),
                    );
                } else {
                    out.push(id);
                }
            }
        }
        out
    }

    /// The field of an array layout at slot-field `(address, offset)` (an
    /// entity `slot_<address>+0x<offset>`): the address is masked and must lie
    /// in the first record (`start <= address < start + stride`), so the access
    /// stands for field `address - start + offset`.
    pub fn array_field_at(
        &self,
        mask: Option<u64>,
        address: u64,
        offset: i64,
    ) -> Option<&LayoutField> {
        let array = self.array.as_ref()?;
        let delta = apply_mask(address, mask).checked_sub(array.masked_address(mask))?;
        if delta >= array.stride {
            return None;
        }
        let effective = i64::try_from(delta).ok()?.checked_add(offset)?;
        self.fields.iter().find(|f| f.offset == effective)
    }

    /// The field that `anchor_id` (an entity id in `corpus_id`) names. `mask`
    /// is the corpus `address_mask`, used by array layouts.
    pub fn field_for_anchor(
        &self,
        corpus_id: &str,
        anchor_id: &str,
        mask: Option<u64>,
    ) -> Option<&LayoutField> {
        if let Some((address, offset)) = slot_of_entity_id(anchor_id) {
            return self.array_field_at(mask, address, offset);
        }
        let anchor_id = anchor_id.rsplit_once('@').map_or(anchor_id, |(a, _)| a);
        self.bases.iter().find_map(|b| {
            self.fields
                .iter()
                .find(|f| entity_id(corpus_id, b, f.offset) == anchor_id)
        })
    }

    /// `obj.x`
    pub fn qualified(&self, field: &LayoutField) -> String {
        format!("{}.{}", self.name, field.name)
    }
}

impl LayoutField {
    fn parse(raw: &serde_json::Value) -> Result<LayoutField, String> {
        let obj = raw
            .as_object()
            .ok_or("`data.fields` entries must be objects")?;
        let text = |key: &str| -> Result<Option<String>, String> {
            match obj.get(key) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(format!("field `{key}` must be a string")),
            }
        };
        let name = text("name")?.ok_or("field is missing `name`")?;
        if !is_valid_name(&name) {
            return Err(format!(
                "field name must match [a-z][a-z0-9_]{{0,31}} (got `{name}`)"
            ));
        }
        let offset_text =
            text("offset")?.ok_or_else(|| format!("field `{name}` is missing `offset`"))?;
        let offset = parse_offset(&offset_text)
            .ok_or_else(|| format!("field `{name}`: offset is not hex: `{offset_text}`"))?;
        let ty = text("type")?;
        if let Some(t) = &ty
            && (!(1..=32).contains(&t.len())
                || !t
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ' ' | '*')))
        {
            return Err(format!("field `{name}`: invalid type `{t}`"));
        }
        let confidence = text("confidence")?;
        if let Some(c) = &confidence
            && !CONFIDENCE_VALUES.contains(&c.as_str())
        {
            return Err(format!(
                "field `{name}`: confidence must be confirmed|inferred|unknown (got `{c}`)"
            ));
        }
        Ok(LayoutField {
            offset,
            name,
            ty,
            summary: text("summary")?,
            confidence,
        })
    }
}

/// `(address, offset)` of a slot field entity id `{corpus}:slot_ff8a00+0x1c`.
pub fn slot_of_entity_id(entity_id: &str) -> Option<(u64, i64)> {
    let name = entity_id.rsplit_once(':').map_or(entity_id, |(_, n)| n);
    slot_of_name(name)
}

/// `(address, offset)` of a slot field name `slot_ff8a00+0x1c`.
pub fn slot_of_name(name: &str) -> Option<(u64, i64)> {
    let rest = name.strip_prefix("slot_")?;
    let (addr, off) = rest.split_once(['+', '-'])?;
    let sign = if rest.as_bytes()[addr.len()] == b'-' {
        -1
    } else {
        1
    };
    let address = u64::from_str_radix(addr, 16).ok()?;
    let magnitude = i64::from_str_radix(off.strip_prefix("0x")?, 16).ok()?;
    Some((address, sign * magnitude))
}

/// Entity id of the field `(base, offset)`: `{corpus}:{base lowercased}+0x{off}`.
pub fn entity_id(corpus_id: &str, base: &str, offset: i64) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    format!(
        "{corpus_id}:{}{sign}0x{:x}",
        base.to_lowercase(),
        offset.unsigned_abs()
    )
}

/// True for decompiler register pseudo-variables (`in_*`, `unaff_*`,
/// `extraout_*`, with or without a `_NN` suffix). They are named after a
/// register at a function's entry, so they mean different objects in
/// different functions.
pub fn is_register_base(base: &str) -> bool {
    ["in_", "unaff_", "extraout_"].iter().any(|p| {
        base.strip_prefix(p).is_some_and(|rest| {
            !rest.is_empty() && rest.chars().all(|c| c.is_alphanumeric() || c == '_')
        })
    })
}

/// Hex offset with optional sign: `0x10`, `-0x4`.
pub fn parse_offset(text: &str) -> Option<i64> {
    let (neg, rest) = match text.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, text),
    };
    let digits = rest
        .strip_prefix("0x")
        .or_else(|| rest.strip_prefix("0X"))?;
    let v = i64::from_str_radix(digits, 16).ok()?;
    Some(if neg { -v } else { v })
}

/// Register a base stands for: `unaff_A2` → `a2` (ends `_<letter><digit>`).
pub fn register_of_base(base: &str) -> Option<String> {
    let b = base.as_bytes();
    let n = b.len();
    (n >= 3 && b[n - 3] == b'_' && b[n - 2].is_ascii_alphabetic() && b[n - 1].is_ascii_digit())
        .then(|| base[n - 2..].to_ascii_lowercase())
}

fn is_valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn string_list(value: Option<&serde_json::Value>, key: &str) -> Result<Vec<String>, String> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(vec![]),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|i| {
                i.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("`{key}` must be a list of strings"))
            })
            .collect(),
        Some(_) => Err(format!("`{key}` must be a list of strings")),
    }
}

/// Every field of every layout (in any corpus) that applies to `function_name`.
pub fn field_names_for(engine: &CorrectionsEngine, function_name: &str) -> Vec<FieldName> {
    names_for(engine, None, function_name)
}

/// Like [`field_names_for`], restricted to the layouts of one corpus.
pub fn field_names_for_corpus(
    engine: &CorrectionsEngine,
    corpus_id: &str,
    function_name: &str,
) -> Vec<FieldName> {
    names_for(engine, Some(corpus_id), function_name)
}

fn names_for(
    engine: &CorrectionsEngine,
    corpus_id: Option<&str>,
    function_name: &str,
) -> Vec<FieldName> {
    let mut out = Vec::new();
    for (fact, _, fact_corpus) in engine.layout_facts_with_corpus(corpus_id) {
        let mask = fact_corpus.and_then(|c| engine.mask_of(c));
        let Some(layout) = Layout::of_fact(fact) else {
            continue;
        };
        if !layout.admits(function_name) {
            continue;
        }
        if let Some(array) = &layout.array {
            let address = array.masked_address(mask);
            for field in &layout.fields {
                out.push(FieldName {
                    base: format!("slot_{address:x}"),
                    register: None,
                    offset: field.offset,
                    layout: layout.name.clone(),
                    field: field.name.clone(),
                    ty: field.ty.clone(),
                    array: Some(FieldArray {
                        address,
                        stride: array.stride,
                        mask,
                    }),
                });
            }
        }
        for field in &layout.fields {
            for base in &layout.bases {
                out.push(FieldName {
                    base: base.clone(),
                    register: layout
                        .registers
                        .get(base)
                        .cloned()
                        .or_else(|| register_of_base(base)),
                    offset: field.offset,
                    layout: layout.name.clone(),
                    field: field.name.clone(),
                    ty: field.ty.clone(),
                    array: None,
                });
            }
        }
    }
    out
}
