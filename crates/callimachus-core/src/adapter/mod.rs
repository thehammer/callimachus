pub mod contract;
pub mod registry;

pub use contract::{
    DiscoveredSource, EntityMerge, ExtractedBlock, ExtractedContract, ExtractedPurpose,
    ExtractedSemantic, ExtractedStructure, ExtractedTheme, ExtractedThemes, LocationRef,
    SourceAdapter, default_changed_sources, default_current_version,
};
pub use registry::AdapterRegistry;
