//! Selected transforms sample a virtual cut and remainder; no full-frame cut
//! raster or uncharged resident cache is created.
#[path = "selected/coverage.rs"]
mod coverage;
#[path = "selected/geometry.rs"]
mod geometry;
#[path = "selected/input.rs"]
mod input;
#[path = "selected/render.rs"]
mod render;
#[path = "selected/shift.rs"]
mod shift;
pub(crate) use coverage::write_coverage;
pub(crate) use geometry::Placement;
pub(crate) use input::Prepared as PreparedSelection;
pub(crate) use input::Selection;
pub(crate) use render::{write, Written};
pub(crate) use shift::shift;
