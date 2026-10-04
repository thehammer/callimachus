#![allow(clippy::double_must_use)] // async_trait expansion + newer clippy; see callimachus-llm/src/lib.rs
pub mod adapter;
pub mod chunker;
pub mod extractor;
pub mod normalize;
pub mod summarizer;

pub use adapter::CaptureAdapter;

pub fn create() -> CaptureAdapter {
    CaptureAdapter::new()
}
