#![allow(clippy::double_must_use)] // async_trait expansion + newer clippy; see callimachus-llm/src/lib.rs
pub mod adapter;
pub mod chunker;
pub mod extractor;
pub mod links;
pub mod summarizer;

pub use adapter::WikiAdapter;

pub fn create() -> WikiAdapter {
    WikiAdapter::new()
}
