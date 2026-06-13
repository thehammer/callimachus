/// Extract the script content from a Vue SFC (`.vue` file).
///
/// Returns `(script_body, is_tsx)` where:
/// - `script_body` is the concatenated content of all `<script ...>` blocks
///   (excluding the opening and closing tags).
/// - `is_tsx` is `true` when any script block has `lang="ts"`, `lang="tsx"`,
///   or `<script setup>` (treated as TypeScript for chunking purposes).
///
/// If no `<script>` block is present, returns `None`.
/// Multiple script blocks (e.g. `<script>` + `<script setup>`) are concatenated
/// with a newline separator.
pub fn extract_script_block(content: &str) -> Option<(String, bool)> {
    extract_script_block_with_line_offset(content).map(|(body, is_tsx, _)| (body, is_tsx))
}

/// Like `extract_script_block`, but also returns the 0-based line number in the
/// full `.vue` file at which the *first* script block's body begins.
///
/// This offset is used by the chunker to convert tree-sitter row numbers
/// (which are relative to the extracted `script_body`) into file-relative line
/// numbers suitable for GitHub `#L<n>-L<m>` deep-links.
///
/// For files with multiple `<script>` blocks the offset of the *first* block is
/// returned; items from subsequent blocks will have slightly wrong line numbers
/// in the rare multi-block case, which is acceptable until explicit multi-block
/// tracking is warranted.
pub fn extract_script_block_with_line_offset(content: &str) -> Option<(String, bool, usize)> {
    let mut parts: Vec<&str> = Vec::new();
    let mut is_tsx = false;
    // Byte offset of the first script body's first character within `content`.
    let mut first_body_offset: Option<usize> = None;
    // Running byte offset of `remaining` relative to the start of `content`.
    let mut consumed_total: usize = 0;

    let mut remaining = content;

    while let Some(tag_start) = remaining.find("<script") {
        // Found the next <script opening tag (case-insensitive would be ideal but
        // Vue templates are always lowercase in practice).

        let after_open = &remaining[tag_start + 7..]; // skip "<script"

        // Find the end of the opening tag (may span several chars for attrs).
        let tag_end = match after_open.find('>') {
            Some(i) => i,
            None => break,
        };

        let attrs = &after_open[..tag_end];

        // Detect whether this is a TypeScript block.
        if attrs.contains("setup")
            || attrs.contains("lang=\"ts\"")
            || attrs.contains("lang=\"tsx\"")
            || attrs.contains("lang='ts'")
            || attrs.contains("lang='tsx'")
        {
            is_tsx = true;
        }

        // Skip `<script lang="js">` — still valid JS, not TSX.
        // is_tsx stays false for that variant unless a later block sets it.

        // Body starts right after '>'.
        let body_start = tag_start + 7 + tag_end + 1; // after '>'
        let rest = &remaining[body_start..];

        // Record where the first script body starts (absolute byte offset in `content`).
        if first_body_offset.is_none() {
            first_body_offset = Some(consumed_total + body_start);
        }

        // Find closing </script>.
        let close = match rest.find("</script>") {
            Some(i) => i,
            None => break,
        };

        let body = &rest[..close];
        parts.push(body);

        // Advance past the closing tag.
        let consumed = body_start + close + "</script>".len();
        consumed_total += consumed;
        remaining = &remaining[consumed..];
    }

    if parts.is_empty() {
        return None;
    }

    // Convert the first body's byte offset to a 0-based line number.
    let first_line = first_body_offset
        .map(|byte_off| content[..byte_off].chars().filter(|&c| c == '\n').count())
        .unwrap_or(0);

    Some((parts.join("\n"), is_tsx, first_line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_script_block_extracted() {
        let vue = r#"<template><div/></template>
<script>
export default { name: "Foo" }
</script>
"#;
        let (body, is_tsx) = extract_script_block(vue).unwrap();
        assert!(body.contains("export default"));
        assert!(!is_tsx);
    }

    #[test]
    fn script_setup_is_tsx() {
        let vue = r#"<template><div/></template>
<script setup>
const x = 1;
</script>"#;
        let (body, is_tsx) = extract_script_block(vue).unwrap();
        assert!(body.contains("const x = 1"));
        assert!(is_tsx);
    }

    #[test]
    fn script_lang_ts_is_tsx() {
        let vue = r#"<template><div/></template>
<script lang="ts">
function greet() {}
</script>"#;
        let (body, is_tsx) = extract_script_block(vue).unwrap();
        assert!(body.contains("function greet"));
        assert!(is_tsx);
    }

    #[test]
    fn two_script_blocks_concatenated() {
        let vue = r#"<script>
const a = 1;
</script>
<script setup lang="ts">
const b = 2;
</script>"#;
        let (body, is_tsx) = extract_script_block(vue).unwrap();
        assert!(body.contains("const a = 1"));
        assert!(body.contains("const b = 2"));
        assert!(is_tsx);
    }

    #[test]
    fn missing_script_returns_none() {
        let vue = r#"<template><div>hello</div></template>
<style scoped>.foo { color: red; }</style>"#;
        assert!(extract_script_block(vue).is_none());
    }

    #[test]
    fn script_lang_js_not_tsx() {
        let vue = r#"<script lang="js">
function foo() {}
</script>"#;
        let (body, is_tsx) = extract_script_block(vue).unwrap();
        assert!(body.contains("function foo"));
        assert!(!is_tsx);
    }
}
