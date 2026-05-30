pub mod adapter;

/// The current pipeline version. Bump this with each new phase that adds
/// indexing passes or schema changes requiring a re-run.
pub const PIPELINE_VERSION: u32 = 3;
pub mod corrections;
pub mod error;
pub mod indexing;
pub mod query;
pub mod storage;
pub mod types;

pub use adapter::{
    AdapterRegistry, DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure,
    LocationRef, SourceAdapter,
};
pub use corrections::CorrectionsEngine;
pub use error::{CalError, Result};
pub use indexing::{
    ChangeKind, ChangeManifest, ChangeSet, ChangeStrategy, ChangedSource, CorpusWatcher,
    IndexOptions, IndexPipeline, IndexResult, ReindexStats, WatcherConfig,
};
pub use query::QueryService;
pub use storage::{
    AncestryReader, Database, Git2Ancestry, MigrateFreshStats, PostgresBackend, SqliteBackend,
    StorageBackend,
};
pub use types::{
    ArchiveSet, ArchiveStats, CachedArtifact, Chunk, Collection, CollectionKind, CollectionMember,
    Corpus, CorpusStatus, CostMetadata, Edge, Entity, Layer2CacheKey, Location, MemberType, Pass,
    Provenance, RefineOutcome, RunStatus, Scope, Summary, SummaryTargetKind, Tombstone, ToolError,
    ToolResult, ToolSuccess, hash_content,
};
