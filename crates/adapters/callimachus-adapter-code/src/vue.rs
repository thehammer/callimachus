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
    let mut parts: Vec<&str> = Vec::new();
    let mut is_tsx = false;

    let mut remaining = content;

    loop {
        // Find the next <script opening tag (case-insensitive would be ideal but
        // Vue templates are always lowercase in practice).
        let tag_start = match remaining.find("<script") {
            Some(i) => i,
            None => break,
        };

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

        // Find closing </script>.
        let close = match rest.find("</script>") {
            Some(i) => i,
            None => break,
        };

        let body = &rest[..close];
        parts.push(body);

        // Advance past the closing tag.
        let consumed = body_start + close + "</script>".len();
        remaining = &remaining[consumed..];
    }

    if parts.is_empty() {
        return None;
    }

    Some((parts.join("\n"), is_tsx))
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
