//! The `SourceAdapter` trait and its inline type closure now live in
//! `callimachus-adapter-contract`.
//!
//! The trait was extracted so adapters can compile against a thin, storage-free
//! contract (no `callimachus-core`, no `rusqlite`). Core re-exports the full set
//! here at the historical `crate::adapter::contract::…` path so every internal
//! caller and downstream crate is unaffected.
pub use callimachus_adapter_contract::contract::{
    DiscoveredSource, EntityMerge, ExtractedBlock, ExtractedContract, ExtractedPurpose,
    ExtractedSemantic, ExtractedStructure, ExtractedTheme, ExtractedThemes, LocationRef,
    SourceAdapter, default_changed_sources, default_current_version,
};
