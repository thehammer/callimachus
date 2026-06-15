//! # callimachus-adapter-contract
//!
//! The stable seam between a Callimachus binary and its content adapters.
//!
//! This crate defines the [`SourceAdapter`] trait every adapter implements, the
//! minimal closure of plain-data types that trait names ([`Chunk`], [`Corpus`],
//! [`Entity`], [`Edge`], [`Location`], [`ChangeKind`]/[`ChangedSource`]/
//! [`CommitMeta`], [`RoutingInputs`]), and the [`AdapterRegistry`] that composes
//! adapters into a binary at build time.
//!
//! It depends only on [`callimachus_llm`] (for the `LlmProvider` abstraction);
//! it does **not** depend on `callimachus-core`, `rusqlite`, or any storage
//! layer. An adapter crate can therefore compile against this contract alone —
//! the whole point of the seam — and live in its own repository, pinning this
//! crate by git rev.
//!
//! `callimachus-core` depends on this crate and re-exports every type here at
//! its historical path, so the rest of the workspace is unaffected by the
//! extraction.

pub mod change;
pub mod contract;
pub mod error;
pub mod registry;
pub mod routing;
pub mod types;

// ── Public surface (flat re-exports) ──────────────────────────────────────────

pub use change::{ChangeKind, ChangedSource, CommitMeta};
pub use contract::{
    DiscoveredSource, EntityMerge, ExtractedBlock, ExtractedContract, ExtractedPurpose,
    ExtractedSemantic, ExtractedStructure, ExtractedTheme, ExtractedThemes, LocationRef,
    SourceAdapter, default_changed_sources, default_current_version,
};
pub use error::LocationParseError;
pub use registry::AdapterRegistry;

// Re-export the LLM-provider abstraction the trait's methods name, so an
// adapter can depend on *only* this crate. `callimachus-llm` carries no
// `callimachus-core` dependency, so this keeps the contract closure thin.
pub use callimachus_llm::{self, LlmProvider};
pub use routing::RoutingInputs;
pub use types::{Chunk, Corpus, CorpusStatus, Edge, Entity, Location, LocationUri, hash_content};
