pub mod aliases_pass;
pub mod change_detector;
pub mod chunk_pass;
pub mod contract_pass;
pub mod embed_pass;
pub mod pipeline;
pub mod purpose_pass;
pub mod reindex_pass;
pub mod semantic_pass;
pub mod structure_pass;
pub mod summarize_pass;
pub mod theme_pass;
pub mod watcher;

pub use change_detector::{ChangeSet, ChangeStrategy};
pub use pipeline::{IndexOptions, IndexPipeline, IndexResult};
pub use reindex_pass::ReindexStats;
pub use watcher::{CorpusWatcher, WatcherConfig};
