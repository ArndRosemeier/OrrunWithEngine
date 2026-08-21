//! 2D marketplace hamlet lab (port of Godot HamletLabPlanner).
//!
//! Packing is still 2D. When a [`Plot`] is supplied, candidates on wet, steep,
//! or high-relief ground lose, and the door is later seated at grade rather than
//! on the lowest corner.

mod castle;
pub(crate) mod castle_kit;
mod catalog;
mod config;
mod dwelling;
pub(crate) mod house_gen;
pub(crate) mod interior;
pub(crate) mod kit;
mod occupancy;
mod planner;
mod seat;

#[cfg(test)]
mod tests;

pub use castle::{id_for_tier as castle_id_for_tier, layout_for as castle_layout, CastleLayout};
pub use catalog::{ids_with_role, spec_for, BuildingRole, BuildingSpec};
pub use config::{tier_market_radius, tier_market_sides, HamletLabConfig, CIVIC_BY_TIER};
pub use dwelling::{
    footprints_fallback_order, max_footprint_depth_m, roll_footprint, roll_storeys, DwellingBrief,
    HouseTheme, FOOTPRINTS, FOUNDATION_M, PITCH_XZ,
};
pub use house_gen::generate as generate_dwelling;
pub use planner::{plan, plan_on};
pub use seat::{
    accept, door_point, ground_score, sample_castle_footprint, sample_footprint, seat_building,
    FootprintSample, Plot, Seat, BANK_EXTRA_M, DOOR_SINK_M, MAX_SKIRT_M, MIN_UPNESS, SEAT_INSET,
    SKIRT_BITE_M, WATERLINE_MARGIN,
};

use glam::Vec2;
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    House,
    Market,
    Castle,
}

#[derive(Clone, Debug)]
pub struct Shape {
    pub kind: ShapeKind,
    pub center: Vec2,
    pub half_size: Vec2,
    pub yaw: f32,
    pub radius: f32,
    /// Civic / castle catalog id. Empty for generated dwellings.
    pub catalog_id: String,
    /// Set for village dwellings; `None` for civics and castles.
    pub dwelling: Option<DwellingBrief>,
    pub polygon: Vec<Vec2>,
}

#[derive(Clone, Debug, Default)]
pub struct Plan2D {
    pub shapes: Vec<Shape>,
    pub plaza: Vec2,
    pub market_radius: f32,
    pub market_sides: usize,
    pub market_polygon: Vec<Vec2>,
    pub markets: Vec<Vec<Vec2>>,
    pub market_centers: Vec<Vec2>,
    pub built_envelope: f32,
    pub house_count: u32,
    pub civic_count: u32,
    pub castle_count: u32,
    pub want_count: u32,
    /// Non-empty when `house_count < want_count`.
    pub underfill_message: String,
    pub occupancy_dots: Vec<Vec2>,
}

#[derive(Clone, Debug)]
pub struct PlacedBuilding {
    pub center: Vec2,
    pub half_x: f32,
    pub half_z: f32,
    pub yaw: f32,
    pub catalog_id: String,
    pub dwelling: Option<DwellingBrief>,
}

#[derive(Debug, Error)]
pub enum HamletError {
    #[error("unknown village catalog id '{id}'")]
    UnknownCatalogId { id: String },
    #[error("civic '{id}' min_tier {min_tier} > settlement tier {settlement_tier}")]
    CivicTier {
        id: String,
        min_tier: u8,
        settlement_tier: u8,
    },
    #[error("failed to place building '{catalog_id}'")]
    PlaceFailed { catalog_id: String },
    #[error("failed to place secondary marketplace {index}")]
    SecondaryMarket { index: usize },
    #[error("market needs at least 3 sides, got {sides}")]
    MarketSides { sides: usize },
    #[error("plaza left the market polygon — reduce jitter")]
    PlazaOutsideMarket,
    #[error("degenerate ellipse polar denominator")]
    DegenerateEllipse,
}
