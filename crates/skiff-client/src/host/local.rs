//! Local Host compatibility facade. Endpoint selection lives in
//! [`selection`], and legacy source and procedural adapters live in [`legacy`].

mod selection;
mod legacy;

pub use selection::*;
pub use legacy::*;
