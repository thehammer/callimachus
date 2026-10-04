#![allow(clippy::double_must_use)] // async_trait expansion + newer clippy; see callimachus-llm/src/lib.rs
pub mod adapter;
pub mod chunker;
pub mod contracts;
pub mod extractor;
pub mod git;
pub mod languages;
pub mod summarizer;
pub mod vue;

pub use adapter::CodeAdapter;

pub fn create() -> CodeAdapter {
    CodeAdapter::new()
}
