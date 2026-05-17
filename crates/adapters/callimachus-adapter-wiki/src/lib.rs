pub mod adapter;
pub mod chunker;
pub mod extractor;
pub mod links;
pub mod summarizer;

pub use adapter::WikiAdapter;

pub fn create() -> WikiAdapter {
    WikiAdapter::new()
}
