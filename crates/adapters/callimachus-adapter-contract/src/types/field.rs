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
