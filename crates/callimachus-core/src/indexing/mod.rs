pub mod aliases_pass;
pub mod cascade;
pub mod change_detector;
pub mod change_manifest;
pub mod chunk_pass;
pub mod contract_pass;
pub mod embed_pass;
pub mod file_shape;
pub mod history_layer;
pub mod history_pass;
pub mod history_walk;
pub mod layer2_cache;
pub mod model_tier;
pub mod pipeline;
pub mod purpose_pass;
pub mod reindex_pass;
pub mod semantic_pass;
pub mod structure_pass;
pub mod summarize_pass;
pub mod theme_pass;
pub mod watcher;

pub use change_detector::{ChangeSet, ChangeStrategy};
pub use change_manifest::{ChangeKind, ChangeManifest, ChangedSource};
pub use history_walk::{WalkOptions, WalkStats, walk_history_backward, walk_history_forward};
pub use pipeline::{
    IndexMode, IndexOptions, IndexPipeline, IndexResult, validate_pass_prerequisites,
};
pub use reindex_pass::ReindexStats;
pub use watcher::{CorpusWatcher, WatcherConfig};
