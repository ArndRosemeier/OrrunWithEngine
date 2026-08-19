//! Orrun game library — continental atlas and content generation.

pub mod atlas;
pub mod combat;
pub mod hamlet;
pub mod save;
pub mod settings;
pub mod world;

pub use atlas::{AtlasError, ContinentAtlas};
pub use hamlet::{plan as plan_hamlet, plan_on as plan_hamlet_on, HamletLabConfig, Plan2D};
pub use save::{SaveError, SavedShrine, SavedStand};
pub use world::{AtlasFields, ContinentalSurface};
