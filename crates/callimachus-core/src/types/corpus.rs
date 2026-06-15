//! `Corpus` and `CorpusStatus` now live in `callimachus-adapter-contract`.
//!
//! Part of the adapter type closure; re-exported here at the historical path so
//! all existing `crate::types::corpus::…` references keep resolving.
pub use callimachus_adapter_contract::types::corpus::{Corpus, CorpusStatus};
