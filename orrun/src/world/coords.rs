//! Atlas-aware coordinates.
//!
//! Three levels, converted explicitly:
//!
//! * [`AtlasCell`] — the 1 km strategic lookup unit. Never a mesh unit.
//! * [`MapPoint`] — an exact fractional position inside a cell. This is what a
//!   click on the map means; cell centres are a special case, not the contract.
//! * [`engine::GlobalXZ`] — absolute metres, what generation and streaming use.
//!
//! Every constructor is bounds-checked against the atlas, so an out-of-map
//! selection is a typed error instead of a clamp that quietly relocates the
//! player.

use engine::space::{ChunkCoord, ChunkSpan, GlobalXZ};
use glam::Vec2;
use thiserror::Error;

use crate::atlas::{ContinentAtlas, CELL_METRES};

/// Edge length of one streamed terrain chunk.
///
/// Five chunks per atlas cell: small enough to bake within a frame budget,
/// large enough that the ring stays cheap. Atlas cells stay strategic units.
pub const CHUNK_SPAN_M: f64 = 200.0;

/// Distance between terrain samples inside a chunk.
pub const CHUNK_SAMPLE_M: f64 = 4.0;

#[derive(Debug, Error, PartialEq)]
pub enum CoordError {
    #[error("atlas cell ({ax}, {az}) is outside the {size}×{size} atlas")]
    CellOutOfBounds { ax: i32, az: i32, size: usize },

    #[error("cell fraction ({fx}, {fz}) must be finite and inside [0, 1]")]
    BadCellFraction { fx: f32, fz: f32 },

    #[error("world position ({x} m, {z} m) is outside the atlas")]
    PointOutOfBounds { x: f64, z: f64 },

    #[error("heading {0} must be finite")]
    BadHeading(f32),
}

/// Extent of a generated atlas, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasBounds {
    size: usize,
}

impl AtlasBounds {
    pub fn of(atlas: &ContinentAtlas) -> Self {
        Self { size: atlas.size }
    }

    pub fn new(size: usize) -> Self {
        Self { size }
    }

    pub fn size(self) -> usize {
        self.size
    }

    /// Full atlas width in metres.
    pub fn metres(self) -> f64 {
        self.size as f64 * CELL_METRES as f64
    }

    pub fn contains_cell(self, ax: i32, az: i32) -> bool {
        ax >= 0 && az >= 0 && (ax as usize) < self.size && (az as usize) < self.size
    }

    pub fn contains_point(self, p: GlobalXZ) -> bool {
        let m = self.metres();
        p.x >= 0.0 && p.z >= 0.0 && p.x < m && p.z < m
    }
}

/// A 1 km atlas cell that is known to exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AtlasCell {
    ax: i32,
    az: i32,
}

impl AtlasCell {
    pub fn new(bounds: AtlasBounds, ax: i32, az: i32) -> Result<Self, CoordError> {
        if !bounds.contains_cell(ax, az) {
            return Err(CoordError::CellOutOfBounds {
                ax,
                az,
                size: bounds.size(),
            });
        }
        Ok(Self { ax, az })
    }

    pub fn ax(self) -> i32 {
        self.ax
    }

    pub fn az(self) -> i32 {
        self.az
    }

    /// Minimum corner of the cell in metres.
    pub fn origin(self) -> GlobalXZ {
        GlobalXZ::at(
            self.ax as f64 * CELL_METRES as f64,
            self.az as f64 * CELL_METRES as f64,
        )
    }

    pub fn centre(self) -> MapPoint {
        MapPoint {
            cell: self,
            fx: 0.5,
            fz: 0.5,
        }
    }
}

/// An exact position on the map: a cell plus the fraction inside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapPoint {
    cell: AtlasCell,
    fx: f32,
    fz: f32,
}

impl MapPoint {
    pub fn new(cell: AtlasCell, fx: f32, fz: f32) -> Result<Self, CoordError> {
        if !fx.is_finite()
            || !fz.is_finite()
            || !(0.0..=1.0).contains(&fx)
            || !(0.0..=1.0).contains(&fz)
        {
            return Err(CoordError::BadCellFraction { fx, fz });
        }
        Ok(Self { cell, fx, fz })
    }

    /// From viewer map coordinates, where 1.0 is one atlas cell.
    pub fn from_cell_units(
        bounds: AtlasBounds,
        cell_x: f64,
        cell_z: f64,
    ) -> Result<Self, CoordError> {
        if !cell_x.is_finite() || !cell_z.is_finite() {
            return Err(CoordError::PointOutOfBounds {
                x: cell_x,
                z: cell_z,
            });
        }
        let ax = cell_x.floor();
        let az = cell_z.floor();
        let cell = AtlasCell::new(bounds, ax as i32, az as i32)?;
        Self::new(cell, (cell_x - ax) as f32, (cell_z - az) as f32)
    }

    pub fn from_global(bounds: AtlasBounds, p: GlobalXZ) -> Result<Self, CoordError> {
        if !bounds.contains_point(p) {
            return Err(CoordError::PointOutOfBounds { x: p.x, z: p.z });
        }
        Self::from_cell_units(bounds, p.x / CELL_METRES as f64, p.z / CELL_METRES as f64)
    }

    pub fn cell(self) -> AtlasCell {
        self.cell
    }

    pub fn fraction(self) -> (f32, f32) {
        (self.fx, self.fz)
    }

    pub fn to_global(self) -> GlobalXZ {
        let origin = self.cell.origin();
        GlobalXZ::at(
            origin.x + self.fx as f64 * CELL_METRES as f64,
            origin.z + self.fz as f64 * CELL_METRES as f64,
        )
    }
}

/// Compass heading in degrees, `0` looking down +Z, growing towards +X.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Heading {
    degrees: f32,
}

impl Heading {
    pub const NORTH: Self = Self { degrees: 0.0 };

    pub fn from_degrees(degrees: f32) -> Result<Self, CoordError> {
        if !degrees.is_finite() {
            return Err(CoordError::BadHeading(degrees));
        }
        Ok(Self {
            degrees: degrees.rem_euclid(360.0),
        })
    }

    /// Heading that looks along `direction` on the XZ plane.
    pub fn towards(direction: Vec2) -> Result<Self, CoordError> {
        let d = direction.normalize_or_zero();
        if d.length_squared() < 1e-12 {
            return Err(CoordError::BadHeading(0.0));
        }
        Self::from_degrees(d.x.atan2(d.y).to_degrees())
    }

    pub fn degrees(self) -> f32 {
        self.degrees
    }

    pub fn direction(self) -> Vec2 {
        let r = self.degrees.to_radians();
        Vec2::new(r.sin(), r.cos())
    }
}

/// Chunk grid used by terrain streaming.
pub fn chunk_span() -> ChunkSpan {
    ChunkSpan::new(CHUNK_SPAN_M).expect("terrain chunk span")
}

pub fn chunk_of(p: GlobalXZ) -> ChunkCoord {
    ChunkCoord::containing(p, chunk_span())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> AtlasBounds {
        AtlasBounds::new(64)
    }

    #[test]
    fn map_point_round_trips_through_global_metres() {
        let b = bounds();
        let cell = AtlasCell::new(b, 12, 40).unwrap();
        let p = MapPoint::new(cell, 0.25, 0.75).unwrap();
        let g = p.to_global();
        assert_eq!(g.x, 12_250.0);
        assert_eq!(g.z, 40_750.0);
        let back = MapPoint::from_global(b, g).unwrap();
        assert_eq!(back.cell(), cell);
        let (fx, fz) = back.fraction();
        assert!((fx - 0.25).abs() < 1e-5);
        assert!((fz - 0.75).abs() < 1e-5);
    }

    #[test]
    fn out_of_bounds_selection_is_an_error_not_a_clamp() {
        let b = bounds();
        assert!(matches!(
            AtlasCell::new(b, -1, 0),
            Err(CoordError::CellOutOfBounds { .. })
        ));
        assert!(matches!(
            AtlasCell::new(b, 64, 0),
            Err(CoordError::CellOutOfBounds { .. })
        ));
        assert!(matches!(
            MapPoint::from_global(b, GlobalXZ::at(-1.0, 0.0)),
            Err(CoordError::PointOutOfBounds { .. })
        ));
        assert!(matches!(
            MapPoint::from_global(b, GlobalXZ::at(64_000.0, 0.0)),
            Err(CoordError::PointOutOfBounds { .. })
        ));
    }

    #[test]
    fn chunks_tile_an_atlas_cell_exactly() {
        let b = bounds();
        let cell = AtlasCell::new(b, 3, 3).unwrap();
        let per_cell = CELL_METRES as f64 / CHUNK_SPAN_M;
        assert_eq!(per_cell, 5.0);
        let origin = chunk_of(cell.origin());
        assert_eq!(origin.origin(chunk_span()).x, cell.origin().x);
    }

    #[test]
    fn heading_round_trips_through_a_direction() {
        let h = Heading::from_degrees(215.0).unwrap();
        let back = Heading::towards(h.direction()).unwrap();
        assert!((back.degrees() - 215.0).abs() < 1e-2);
        assert!(Heading::from_degrees(f32::NAN).is_err());
        assert!(Heading::from_degrees(f32::INFINITY).is_err());
        assert!(Heading::from_degrees(f32::NEG_INFINITY).is_err());
    }
}
