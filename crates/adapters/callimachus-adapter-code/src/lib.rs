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
