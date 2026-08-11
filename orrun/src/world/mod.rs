//! 3D continental world layers (surface sample, later sectors).

mod atlas_fields;
mod hydro_geom;
mod surface;

#[cfg(test)]
mod tests;

pub use atlas_fields::AtlasFields;
pub use surface::{find_land_spawn, find_water_view_spawn, ContinentalSurface};
