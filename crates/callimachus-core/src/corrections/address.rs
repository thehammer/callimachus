//! Address parsing, masking and canonical formatting for fact anchoring.

use crate::types::entity::Entity;

/// Parse `"0xFF981A"` (case-insensitive hex with `0x` prefix).
pub fn parse_address(s: &str) -> Option<u64> {
    let digits = s
        .trim()
        .strip_prefix("0x")
        .or_else(|| s.trim().strip_prefix("0X"))?;
    if digits.is_empty() {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

/// Apply the corpus address mask, if any.
pub fn apply_mask(addr: u64, mask: Option<u64>) -> u64 {
    match mask {
        Some(m) => addr & m,
        None => addr,
    }
}

/// Canonical form: `0x` + lowercase hex, no padding.
pub fn canonical(addr: u64) -> String {
    format!("0x{addr:x}")
}

/// The corpus `address_mask` config value (hex string), if set and valid.
pub fn mask_from_config(config: &serde_json::Value) -> Option<u64> {
    config
        .get("address_mask")
        .and_then(|v| v.as_str())
        .and_then(parse_address)
}

/// The corpus `cpu_spaces` config value. `None` means every space anchors.
pub fn cpu_spaces_from_config(config: &serde_json::Value) -> Option<Vec<String>> {
    config
        .get("cpu_spaces")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
}

/// True when the corpus config marks it as a reference corpus.
pub fn is_reference(config: &serde_json::Value) -> bool {
    config.get("role").and_then(|v| v.as_str()) == Some("reference")
}

/// The address of an entity: its first alias that is already in canonical
/// hex form (`0x` + lowercase hex).
pub fn entity_address(e: &Entity) -> Option<u64> {
    e.aliases
        .iter()
        .find_map(|a| parse_address(a).filter(|v| canonical(*v) == *a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats() {
        assert_eq!(parse_address("0xFF981A"), Some(0xff981a));
        assert_eq!(parse_address("FF981A"), None);
        assert_eq!(parse_address("0x"), None);
        assert_eq!(canonical(0xff981a), "0xff981a");
        assert_eq!(apply_mask(0xffff0010, Some(0xffffff)), 0xff0010);
    }
}
