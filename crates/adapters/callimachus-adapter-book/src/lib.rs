pub mod adapter;
pub mod chunker;
pub mod extractor;
pub mod resolver;
pub mod summarizer;

pub use adapter::BookAdapter;

pub fn create() -> BookAdapter {
    BookAdapter::new()
}
