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
    ChangeSet, ChangeStrategy, CorpusWatcher, IndexOptions, IndexPipeline, IndexResult,
    ReindexStats, WatcherConfig,
};
pub use query::QueryService;
pub use storage::{Database, PostgresBackend, SqliteBackend, StorageBackend};
pub use types::{
    Chunk, Collection, CollectionKind, CollectionMember, Corpus, CorpusStatus, CostMetadata, Edge,
    Entity, Location, MemberType, Pass, RunStatus, Scope, Summary, SummaryTargetKind, ToolError,
    ToolResult, ToolSuccess, hash_content,
};
