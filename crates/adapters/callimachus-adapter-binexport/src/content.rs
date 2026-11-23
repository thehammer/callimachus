//! Chunk content layout and prompt trimming. See `docs/binary-export.md`.

use crate::bundle::FunctionRecord;

/// Separates the C part from the commented disassembly in a chunk's content.
pub const DISASSEMBLY_MARKER: &str = "/* ---- disassembly ---- */";

/// Prompt budget for the C part (header included), in chars.
pub const C_BUDGET: usize = 4000;
/// Prompt budget for the disassembly part, in chars.
pub const ASM_BUDGET: usize = 3000;

/// Chunk location path for an address: `fn/<addr8>`.
pub fn location_path(address: u64) -> String {
    format!("fn/{address:08x}")
}

/// Inverse of [`location_path`].
pub fn address_of_path(path: &str) -> Option<u64> {
    let digits = path.strip_prefix("fn/")?;
    u64::from_str_radix(digits, 16).ok()
}

/// Build the chunk content for one function.
pub fn build_content(
    f: &FunctionRecord,
    address: u64,
    c_text: Option<&str>,
    asm_text: Option<&str>,
) -> String {
    let size = f.size.map(|s| s.to_string()).unwrap_or_else(|| "?".into());
    let mut out = format!(
        "// {} @ 0x{address:06x}  (size {size} bytes; discovered_by {})\n",
        f.name,
        f.discovered_by.join(", ")
    );
    if let Some(c) = c_text {
        let body = drop_first_comment_line(c);
        out.push_str(body);
        if !body.is_empty() && !body.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push_str(DISASSEMBLY_MARKER);
    out.push('\n');
    for line in asm_text
        .unwrap_or("")
        .lines()
        .filter(|l| !l.trim().is_empty())
    {
        out.push_str(&asm_comment_line(line));
        out.push('\n');
    }
    out
}

/// `"<addr6>\t<hex bytes>\t<instruction>"` → `/* <addr6>  <instruction> */`.
fn asm_comment_line(line: &str) -> String {
    let mut parts = line.splitn(3, '\t');
    let addr = parts.next().unwrap_or("").trim();
    let _bytes = parts.next();
    let insn = parts.next().unwrap_or("").trim();
    format!("/* {addr}  {} */", insn.replace("*/", "*\\/"))
}

fn drop_first_comment_line(c: &str) -> &str {
    let first_end = c.find('\n').map(|i| i + 1).unwrap_or(c.len());
    let first = c[..first_end].trim();
    let is_comment = first.starts_with("//") || (first.starts_with("/*") && first.ends_with("*/"));
    if is_comment { &c[first_end..] } else { c }
}

/// Split content at the disassembly marker: `(c_part, asm_lines)`.
pub fn split_content(content: &str) -> (&str, Option<&str>) {
    match content.find(DISASSEMBLY_MARKER) {
        Some(i) => {
            let rest = &content[i + DISASSEMBLY_MARKER.len()..];
            (&content[..i], Some(rest.strip_prefix('\n').unwrap_or(rest)))
        }
        None => (content, None),
    }
}

/// Trim content for an LLM prompt: the C part keeps up to [`C_BUDGET`] chars,
/// then as many whole disassembly lines as fit in [`ASM_BUDGET`] chars, ending
/// with `/* … N more instructions */` when lines were dropped.
pub fn trim_for_prompt(content: &str) -> String {
    let (c_part, asm) = split_content(content);
    let Some(asm) = asm else {
        return content.to_string();
    };
    let c_trimmed = &c_part[..c_part.floor_char_boundary(C_BUDGET.min(c_part.len()))];
    let mut out = String::from(c_trimmed);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(DISASSEMBLY_MARKER);
    out.push('\n');

    let lines: Vec<&str> = asm.lines().collect();
    let mut used = 0usize;
    let mut kept = 0usize;
    for l in &lines {
        if used + l.len() + 1 > ASM_BUDGET {
            break;
        }
        used += l.len() + 1;
        kept += 1;
        out.push_str(l);
        out.push('\n');
    }
    let dropped = lines.len() - kept;
    if dropped > 0 {
        out.push_str(&format!("/* … {dropped} more instructions */\n"));
    }
    out
}
