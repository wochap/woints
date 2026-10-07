//! AT-SPI element provider (milestone 2).
//!
//! Will implement [`woints_core::ports::ElementProvider`] by walking the
//! accessibility tree with `CoordType::WINDOW` extents (design D3).

pub use woints_core::ports::ElementProvider;
