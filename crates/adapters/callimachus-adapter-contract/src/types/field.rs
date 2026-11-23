//! A named struct field, used to annotate source (see
//! [`SourceAdapter::annotate_source`](crate::SourceAdapter::annotate_source)).

use serde::{Deserialize, Serialize};

/// One field of a declared object layout, bound to one base name.
///
/// `base` is the pointer name as written in the source (`unaff_A2`);
/// `register` is the CPU register the base stands for, lowercase (`a2`), used
/// to match disassembly operands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldName {
    pub base: String,
    pub register: Option<String>,
    pub offset: i64,
    pub layout: String,
    pub field: String,
    pub ty: Option<String>,
    /// Set for a field of an array layout: `base` is then `slot_<hex>` and the
    /// field matches slot accesses of the array rather than a register base.
    #[serde(default)]
    pub array: Option<FieldArray>,
}

/// A fixed-address array of records (`data.array` of a layout).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldArray {
    /// Start address, already masked with the corpus `address_mask`.
    pub address: u64,
    /// Record size in bytes.
    pub stride: u64,
    /// Corpus `address_mask`, applied to addresses seen in source.
    pub mask: Option<u64>,
}

impl FieldName {
    /// `0x10` or `-0x4`: the offset in the form used in entity names.
    pub fn offset_hex(&self) -> String {
        if self.offset < 0 {
            format!("-0x{:x}", self.offset.unsigned_abs())
        } else {
            format!("0x{:x}", self.offset)
        }
    }
}
