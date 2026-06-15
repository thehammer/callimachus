//! `Location` and `LocationUri` now live in `callimachus-adapter-contract`.
//!
//! They are part of the adapter type closure (the `SourceAdapter` trait names
//! `Location` through `Chunk`/`Entity`/`Edge`), so they were extracted to the
//! contract crate. Core re-exports them here at their historical path so all
//! existing `crate::types::location::…` references keep resolving.
pub use callimachus_adapter_contract::types::location::{Location, LocationUri};
