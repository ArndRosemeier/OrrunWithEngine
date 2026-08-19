//! The 3D continent: one surface authority, chunked meshes, one entry path.
//!
//! * [`ContinentalSurface`] answers every question about ground and water.
//! * [`TerrainChunkBuilder`] turns it into seam-safe land/water/contact chunks.
//! * [`WorldStream`] keeps those chunks around the player and rebases render
//!   space so `f32` precision stays local.
//! * [`WorldSession`] runs atlas → travel → walking in one process.

mod alpine;
mod ambience;
mod atlas_fields;
mod chunk_mesh;
mod combat_layer;
mod coords;
mod doors;
mod dungeon;
mod entry;
mod fauna;
mod footprint;
mod hydro_geom;
mod look;
mod paths;
mod ponds;
mod ring_field;
mod rng;
mod scatter;
mod session;
mod settlement;
mod surface;
mod travel;
mod world_stream;

#[cfg(test)]
mod tests;

pub use ambience::{Ambience, AmbienceError};
pub use atlas_fields::AtlasFields;
pub use chunk_mesh::TerrainChunkBuilder;
pub use combat_layer::{first_fixture_auto_hit, CombatLayer};
pub use coords::{
    chunk_of, chunk_span, AtlasBounds, AtlasCell, CoordError, Heading, MapPoint, CHUNK_SAMPLE_M,
    CHUNK_SPAN_M,
};
pub use dungeon::{DungeonError, DungeonLayer, LIVE_OPEN_M};
pub use entry::{best_settlement_entry, resolve_spawn, EntryError, SpawnPose, WorldEntryRequest};
pub use fauna::{FaunaCatalog, FaunaError, FaunaLayer, FaunaRole, FaunaSpec};
pub use footprint::{BuildingIndex, BuildingPlot, CastlePlot, DungeonPlot, HousePlot};
pub use look::{install_daylight, install_materials};
pub use paths::PathLayer;
pub use ponds::{Pond, PondField, PondWindow, SharedPonds, COVERS_M, REBUILD_M, SEED_RADIUS_M};
pub use scatter::{
    Fall, GroundCover, PropClass, ScatterCatalog, ScatterError, ScatterLayer, PROP_CLASSES,
};
pub use session::{Locomotion, SessionError, SessionState, WalkInput, WorldSession};
pub use settlement::{HamletStand, SettlementError, SettlementLayer};
pub use surface::{
    classify_settlement, ContinentalSurface, DungeonPin, SettlementPin, SurfaceColumn,
    SurfaceError, SurfaceMaterial, TerrainLayers, WaterBody, MIN_WATER_DEPTH,
};
pub use travel::{
    ContinentProxySpec, TravelPhase, TravelTimings, TravelView, MAX_PROXY_AXIS, PROXY_EXAGGERATION,
};
pub use world_stream::{
    TerrainTier, WorldStream, DISTANT, ENTRY_RING, FAR, FAR_VIEW_M, MEDIUM, NEAR,
    REBASE_DISTANCE_M, VISUAL_RING,
};
