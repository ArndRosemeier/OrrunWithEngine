//! 2D marketplace hamlet lab (port of Godot HamletLabPlanner).
//!
//! Height-agnostic packing only. Plaza seating and 3D buildable pads are later stages.

mod catalog;
mod config;
mod occupancy;
mod planner;

#[cfg(test)]
mod tests;

pub use catalog::{ids_with_role, spec_for, BuildingRole, BuildingSpec};
pub use config::{tier_market_radius, tier_market_sides, HamletLabConfig, CIVIC_BY_TIER};
pub use planner::plan;

use glam::Vec2;
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    House,
    Market,
}

#[derive(Clone, Debug)]
pub struct Shape {
    pub kind: ShapeKind,
    pub center: Vec2,
    pub half_size: Vec2,
    pub yaw: f32,
    pub radius: f32,
    pub catalog_id: String,
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
}

#[derive(Debug, Error)]
pub enum HamletError {
    #[error("no dwelling forms for tier {tier}")]
    NoDwellings { tier: u8 },
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
