pub mod adapter;
pub mod chunker;
pub mod docc;
pub mod extractor;
pub mod render;
pub mod summarizer;

pub use adapter::DocsAdapter;

pub fn create() -> DocsAdapter {
    DocsAdapter::new()
}
