pub mod address;
pub mod engine;
pub mod fact_import;
pub mod facts;
pub mod layouts;
pub mod link_addresses;
pub mod types;

pub use engine::CorrectionsEngine;
pub use facts::FactRecord;
pub use types::{Correction, CorrectionKind, EntityLinkKind, SplitGranularity};
