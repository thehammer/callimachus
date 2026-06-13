pub mod adapter;
pub mod chunker;
pub mod extractor;
pub mod normalize;
pub mod summarizer;

pub use adapter::CaptureAdapter;

pub fn create() -> CaptureAdapter {
    CaptureAdapter::new()
}
