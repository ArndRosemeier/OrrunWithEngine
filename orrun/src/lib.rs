//! Orrun game library — continental atlas and content generation.

pub mod atlas;
pub mod hamlet;
pub mod save;
pub mod world;

pub use atlas::{AtlasError, ContinentAtlas};
pub use hamlet::{plan as plan_hamlet, HamletLabConfig, Plan2D};
pub use save::{SaveError, SavedStand};
pub use world::{AtlasFields, ContinentalSurface};
