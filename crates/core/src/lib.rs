//! woints core: geometry, domain types, ports, labels, placement, theme and
//! the socket protocol. No Wayland, D-Bus or runtime dependencies.

pub mod act;
pub mod geom;
pub mod labels;
pub mod model;
pub mod place;
pub mod ports;
pub mod profile;
pub mod proto;
pub mod theme;

pub use geom::{Device, Global, OutputGeom, OutputId, OutputLocal, Point, Rect, Size, WindowLocal};
pub use model::{
    Action, AtspiCap, Element, ElementId, ElementSource, Scope, Surface, Target, Timings,
};
