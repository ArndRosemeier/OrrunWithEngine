//! Turning a point on the atlas into a place to stand.
//!
//! The request carries the exact fractional position the player clicked, not a
//! cell index, so entering at a river bend means that bend. If no valid ground
//! exists within the search radius the entry fails and the atlas says so —
//! there is no quiet relocation to a different part of the continent.

use engine::space::{GlobalPosition, GlobalXZ};
use glam::Vec2;
use thiserror::Error;

use super::coords::{AtlasBounds, CoordError, Heading, MapPoint};
use super::ponds::PondField;
use super::surface::{ContinentalSurface, SettlementPin, SurfaceColumn};

/// Spacing of candidate spawn positions.
pub const SEARCH_STEP_M: f64 = 8.0;
/// Largest distance the resolver will walk away from the requested point.
pub const SEARCH_RADIUS_M: f64 = 480.0;
/// Candidate directions per ring.
const RING_SAMPLES: usize = 24;
/// Standing ground must clear the water by this much.
pub const MIN_FREEBOARD_M: f32 = 0.75;
/// Steepest ground the player may be placed on (rise over run).
pub const MAX_SPAWN_SLOPE: f32 = 0.85;
/// Baseline used for slope and water-facing probes.
const PROBE_M: f64 = 4.0;

#[derive(Debug, Error)]
pub enum EntryError {
    #[error(transparent)]
    Coord(#[from] CoordError),

    #[error(
        "no dry, walkable ground within {radius} m of ({x:.0} m, {z:.0} m); \
         the selected point is open water or too steep"
    )]
    NoSpawn { x: f64, z: f64, radius: f64 },
}

/// A request to enter the 3D world at an exact map position.
#[derive(Clone, Copy, Debug)]
pub struct WorldEntryRequest {
    point: MapPoint,
    heading: Option<Heading>,
}

impl WorldEntryRequest {
    pub fn at(point: MapPoint) -> Self {
        Self {
            point,
            heading: None,
        }
    }

    pub fn facing(mut self, heading: Heading) -> Self {
        self.heading = Some(heading);
        self
    }

    /// Entry at an absolute world position.
    pub fn at_global(bounds: AtlasBounds, p: GlobalXZ) -> Result<Self, EntryError> {
        Ok(Self::at(MapPoint::from_global(bounds, p)?))
    }

    pub fn point(self) -> MapPoint {
        self.point
    }

    pub fn requested(self) -> GlobalXZ {
        self.point.to_global()
    }

    pub fn heading(self) -> Option<Heading> {
        self.heading
    }
}

/// Highest-tier settlement that has dry, walkable ground nearby, then population,
/// then stable id. Skips pins that sit in open water or on cliffs the player
/// cannot stand on.
pub fn best_settlement_entry(
    surface: &ContinentalSurface,
    ponds: &PondField,
) -> Result<(SettlementPin, WorldEntryRequest), EntryError> {
    let bounds = surface.bounds();
    let mut pins: Vec<SettlementPin> = surface.settlements().to_vec();
    if pins.is_empty() {
        return Err(EntryError::NoSpawn {
            x: 0.0,
            z: 0.0,
            radius: SEARCH_RADIUS_M,
        });
    }
    pins.sort_by_key(|b| std::cmp::Reverse((b.tier, b.population, b.id)));
    let mut last = None;
    for pin in pins {
        let point = match MapPoint::from_global(bounds, pin.at) {
            Ok(point) => point,
            Err(err) => {
                last = Some(EntryError::Coord(err));
                continue;
            }
        };
        let request = WorldEntryRequest::at(point);
        match resolve_spawn(surface, ponds, request) {
            Ok(_) => return Ok((pin, request)),
            Err(err) => last = Some(err),
        }
    }
    Err(last.expect("settlement list was non-empty"))
}

/// Where and how the player enters the world.
#[derive(Clone, Copy, Debug)]
pub struct SpawnPose {
    position: GlobalPosition,
    heading: Heading,
    /// Distance the resolver had to move from the requested point.
    offset_m: f64,
}

impl SpawnPose {
    pub fn position(self) -> GlobalPosition {
        self.position
    }

    pub fn ground(self) -> GlobalXZ {
        self.position.horizontal()
    }

    pub fn heading(self) -> Heading {
        self.heading
    }

    pub fn offset_m(self) -> f64 {
        self.offset_m
    }
}

/// Nearest valid standing position to the requested point.
///
/// Candidates are visited in rings of increasing radius and, within a ring, in
/// a fixed angular order, so the same request always yields the same spawn.
///
/// Takes the pond window as well as the surface because this is the one place
/// where the difference matters to a person rather than to a mesh: the surface
/// alone would happily put the player waist deep in a pond it has never heard
/// of.
pub fn resolve_spawn(
    surface: &ContinentalSurface,
    ponds: &PondField,
    request: WorldEntryRequest,
) -> Result<SpawnPose, EntryError> {
    let wanted = request.requested();
    let bounds = surface.bounds();
    let rings = (SEARCH_RADIUS_M / SEARCH_STEP_M).round() as i32;

    for ring in 0..=rings {
        let radius = ring as f64 * SEARCH_STEP_M;
        let samples = if ring == 0 { 1 } else { RING_SAMPLES };
        for k in 0..samples {
            let angle = std::f64::consts::TAU * k as f64 / samples as f64;
            let p = GlobalXZ::at(
                wanted.x + radius * angle.cos(),
                wanted.z + radius * angle.sin(),
            );
            if !bounds.contains_point(p) {
                continue;
            }
            let Some(ground) = standable_height(surface, ponds, p) else {
                continue;
            };
            let heading = request
                .heading
                .unwrap_or_else(|| face_nearest_water(surface, ponds, p));
            return Ok(SpawnPose {
                position: GlobalPosition::at(p.x, ground as f64, p.z),
                heading,
                offset_m: radius,
            });
        }
    }

    Err(EntryError::NoSpawn {
        x: wanted.x,
        z: wanted.z,
        radius: SEARCH_RADIUS_M,
    })
}

/// The column as the player will meet it: landform plus whatever sub-atlas
/// water runs through it.
fn walked_column(surface: &ContinentalSurface, ponds: &PondField, p: GlobalXZ) -> SurfaceColumn {
    let mut column = surface.column(p);
    ponds.carve(p, &mut column);
    column
}

/// Ground height at `p` when a player can stand there, else `None`.
fn standable_height(surface: &ContinentalSurface, ponds: &PondField, p: GlobalXZ) -> Option<f32> {
    let column = walked_column(surface, ponds, p);
    if column.is_wet() || column.wetness() > -MIN_FREEBOARD_M {
        return None;
    }
    let ground = column.ground();
    if !ground.is_finite() {
        return None;
    }
    let at = |x: f64, z: f64| walked_column(surface, ponds, GlobalXZ::at(x, z)).ground();
    let east = at(p.x + PROBE_M, p.z);
    let west = at(p.x - PROBE_M, p.z);
    let north = at(p.x, p.z + PROBE_M);
    let south = at(p.x, p.z - PROBE_M);
    let slope_x = (east - west).abs() / (2.0 * PROBE_M as f32);
    let slope_z = (north - south).abs() / (2.0 * PROBE_M as f32);
    if slope_x.max(slope_z) > MAX_SPAWN_SLOPE {
        return None;
    }
    Some(ground)
}

/// Look at the nearest water, so arriving at a river, a coast or a pond shows
/// the water.
fn face_nearest_water(surface: &ContinentalSurface, ponds: &PondField, p: GlobalXZ) -> Heading {
    const LOOK_M: f64 = 60.0;
    let mut best = f32::NEG_INFINITY;
    let mut dir = Vec2::ZERO;
    for k in 0..RING_SAMPLES {
        let angle = std::f64::consts::TAU * k as f64 / RING_SAMPLES as f64;
        let d = Vec2::new(angle.cos() as f32, angle.sin() as f32);
        let q = GlobalXZ::at(p.x + LOOK_M * angle.cos(), p.z + LOOK_M * angle.sin());
        let wetness = walked_column(surface, ponds, q).wetness();
        if wetness > best {
            best = wetness;
            dir = d;
        }
    }
    if best > -40.0 {
        Heading::towards(dir).unwrap_or(Heading::NORTH)
    } else {
        Heading::NORTH
    }
}
