//! The 3D continent: one surface authority, chunked meshes, one entry path.
//!
//! * [`ContinentalSurface`] answers every question about ground and water.
//! * [`TerrainChunkBuilder`] turns it into seam-safe land/water/contact chunks.
//! * [`WorldStream`] keeps those chunks around the player and rebases render
//!   space so `f32` precision stays local.
//! * [`WorldSession`] runs atlas → loading → walking in one process.

mod atlas_fields;
mod chunk_mesh;
mod coords;
mod entry;
mod hydro_geom;
mod look;
mod ring_field;
mod scatter;
mod session;
mod surface;
mod world_stream;

#[cfg(test)]
mod tests;

pub use atlas_fields::AtlasFields;
pub use chunk_mesh::TerrainChunkBuilder;
pub use coords::{
    chunk_of, chunk_span, AtlasBounds, AtlasCell, CoordError, Heading, MapPoint, CHUNK_SAMPLE_M,
    CHUNK_SPAN_M,
};
pub use entry::{resolve_spawn, EntryError, SpawnPose, WorldEntryRequest};
pub use look::{install_daylight, install_materials};
pub use scatter::{GroundCover, PropClass, ScatterCatalog, ScatterError, ScatterLayer};
pub use session::{Locomotion, SessionError, SessionState, WalkInput, WorldSession};
pub use surface::{
    ContinentalSurface, SurfaceColumn, SurfaceError, SurfaceMaterial, WaterBody, MIN_WATER_DEPTH,
};
pub use world_stream::{
    TerrainTier, WorldStream, DISTANT, ENTRY_RING, FAR, FAR_VIEW_M, MEDIUM, NEAR,
    REBASE_DISTANCE_M, VISUAL_RING,
};
