pub mod contract;
pub mod registry;

pub use contract::{
    DiscoveredSource, EntityMerge, ExtractedBlock, ExtractedContract, ExtractedPurpose,
    ExtractedSemantic, ExtractedStructure, ExtractedTheme, ExtractedThemes, LocationRef,
    SourceAdapter,
};
pub use registry::AdapterRegistry;
