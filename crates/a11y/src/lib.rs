//! AT-SPI element provider and actuator (design D13–D21).
//!
//! One persistent connection to the a11y bus; elements come from
//! `Collection.GetMatches` or a bounded tree walk, with `CoordType::WINDOW`
//! extents converted to global coordinates (design D3).

pub mod actuator;
pub mod conn;
pub mod dbus;
pub mod filter;
pub mod pair;
pub mod provider;
pub mod rule;
pub mod walk;

pub use actuator::AtspiActuator;
pub use conn::A11yConnection;
pub use provider::AtspiProvider;
