//! Ground cover: grass tufts, stones, and forest.
//!
//! Props are not stored anywhere. They live on a **global lattice**: every
//! candidate position is derived from its own cell index and the world seed, so
//! the same tuft grows in the same place whichever direction you walk in from,
//! and nothing has to be saved, streamed, or reconciled.
//!
//! Each class carries a window radius, and the layer rebuilds the window when
//! the player leaves its centre, when terrain streams in, or when render space
//! rebases. Rebuilding is cheap because a candidate is rejected on the cheapest
//! test that can reject it: the lattice roll first, then the atlas fields, then
//! the resident ground, and only then the full surface column.
//!
//! Heights for the walked ring come from the same contact grid the player
//! stands on, never from a fresh surface probe, so a tuft cannot sink into
//! ground that was drawn slightly differently. A second band of cheap pine
//! stand-ins sits on the medium heightfield out to the horizon. Full-res trees
//! replace those proxies per 200 m cell as their bins land, so a wood does not
//! vanish and then pop in as one sheet.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use engine::collision::{ColliderId, ColliderLayer, ColliderShape, StaticCollider};
use engine::color::Color;
use engine::contact::ContactSnapshot;
use engine::error::EngineResult;
use engine::mesh::Mesh;
use engine::model::Model;
use engine::place::Place;
use engine::space::{GlobalPosition, GlobalXZ, RenderOrigin};
use engine::world::{EntityId, World};
use thiserror::Error;

use glam::{Vec2, Vec3};

use super::coords::CHUNK_SPAN_M;
use super::footprint::BuildingIndex;
use super::look::{SNOW_FULL_M, SNOW_LINE_M, SNOW_SLOPE_END, SNOW_SLOPE_START, SUN_DIR};
use super::paths::road_ribbon_width;
use super::ponds::PondField;
use super::rng::{value_noise, CellRng};
use super::settlement::{HamletStand, ROAD_CLEAR_M};
use super::surface::{lerp, smoothstep, ContinentalSurface, SurfaceColumn};
use super::world_stream::{WorldStream, MEDIUM, NEAR};

/// Extra metres past the ribbon half-width so a scaled tuft cannot hang over dirt.
const ROAD_VEG_PAD_M: f32 = 0.35;

/// Lattice spacing and window radius per class, in metres.
const GRASS_SPACING_M: f64 = 1.7;
const GRASS_RADIUS_M: f64 = 58.0;
const ROCK_SPACING_M: f64 = 16.0;
/// Stones fill the walked ring: they are sparse enough to keep every cell.
const ROCK_RADIUS_M: f64 = NEAR.covers_m();
const TREE_SPACING_M: f64 = 10.0;
/// Full pines fill the walked ground; beyond this the far band takes over.
const TREE_RADIUS_M: f64 = NEAR.covers_m();
/// Inner disk that keeps the authored 10 m lattice.
const TREE_DENSE_RADIUS_M: f64 = 400.0;
/// Effective spacing at the edge of the walked ring.
const TREE_OUTER_SPACING_M: f64 = 25.0;
/// Bank dressing: a narrow subject, so a short window and close spacing.
const REED_SPACING_M: f64 = 1.1;
/// Closer in than the other classes: a reed bed is dense, and a two-metre stem
/// a hundred metres off is a pixel that costs a lattice cell.
const REED_RADIUS_M: f64 = 70.0;
const BUSH_SPACING_M: f64 = 4.2;
const BUSH_RADIUS_M: f64 = NEAR.covers_m();
const BUSH_DENSE_RADIUS_M: f64 = 160.0;
const BUSH_OUTER_SPACING_M: f64 = 10.0;
const SNAG_SPACING_M: f64 = 14.0;
const SNAG_RADIUS_M: f64 = NEAR.covers_m();
const MUSHROOM_SPACING_M: f64 = 2.8;
const MUSHROOM_RADIUS_M: f64 = 58.0;
const BERRY_SPACING_M: f64 = 5.0;
const BERRY_RADIUS_M: f64 = NEAR.covers_m();

/// Cheap pine stand-ins on the medium heightfield, outside the full-mesh ring.
const FAR_TREE_SPACING_M: f64 = 36.0;
const FAR_TREE_RADIUS_M: f64 = MEDIUM.reach_m();
const FAR_TREE_SALT: u64 = 0x6661_7254_7265;

/// Rebuild the near window once the player is this far from where it was centred.
const RESEED_M: f64 = 20.0;
/// The far band is a seven-kilometre disk; it can drift further before a resow.
const FAR_RESEED_M: f64 = 100.0;
/// Pending full-res trees. Farther ones stay as LOD until they walk into this set.
const MAX_TREE_QUEUE: usize = 1_000;
/// Pending grass / rock / bush pieces. Farther bins stay empty until you walk in.
const MAX_OTHER_QUEUE: usize = 10_000;
/// Pending horizon pine *draw* cells. Each is a kilometre; the rest wait.
const MAX_FAR_QUEUE: usize = 24;
/// Full-res trees to promote per walking frame. Whole 200 m bins were hundreds.
const MAX_TREES_PER_FRAME: usize = 40;
/// Grass / rock / bush bins to upload per walking frame.
const MAX_OTHER_BINS_PER_FRAME: usize = 4;
/// Horizon pine *draw* cells to upload per frame. Each is a kilometre, not 200 m.
const MAX_FAR_BINS_PER_FRAME: usize = 8;
/// When near cover also wrote this frame, far uploads share the GPU less.
const MAX_FAR_BINS_WHEN_NEAR_DIRTY: usize = 2;
/// Main-thread upload slice. GPU sync is after this, so the cap is conservative.
const UPLOAD_BUDGET_MS: f32 = 4.0;
/// How many walked chunks fold into one far draw call. 5 × 200 m = 1 km.
const FAR_DRAW_FOLD: i32 = 5;
/// A 1 km square on the 36 m far lattice contains at most 29 × 29 candidates.
/// Reserve the next power of two so changing forest density never reallocates
/// the shared GPU batch.
const FAR_DRAW_INSTANCE_RESERVE: usize = 1_024;
/// Maximum 1 km draw cells touched by a 7 km-radius disk at any sub-cell offset.
const FAR_DRAW_POOL: usize = 256;
/// Engine collider layer for full-res tree trunks. Far proxies do not collide.
const COLLIDER_LAYER: ColliderLayer = 1;
/// Trunk radius at scale 1. Grows with the placed tree.
const TREE_TRUNK_RADIUS_M: f32 = 0.32;

/// Highest ground trees still grow on, and the band they thin out over.
const TREELINE_M: f32 = 780.0;
const TREELINE_FADE_M: f32 = 220.0;

/// Dry margin a prop keeps from standing water, in metres of the signed field.
const GRASS_DRY_MARGIN: f32 = 0.4;
const TREE_DRY_MARGIN: f32 = 2.0;

/// How far from a waterline the ground still counts as a bank.
const BANK_REACH_M: f32 = 22.0;

/// Global tree occupancy scale. Each lattice cell rolls against `cover.tree`.
const TREE_DENSITY: f32 = 0.7;

/// Stand size, and its own stream out of the world seed.
const CANOPY_NOISE_SCALE_M: f64 = 260.0;
const CANOPY_NOISE_SALT: u64 = 0x0F0_1E5;
/// Mottling inside a meadow: patches of dry sward vs lush, tens of metres.
const SOIL_PATCH_SCALE_M: f64 = 48.0;
const SOIL_PATCH_SALT: u64 = 0x5011_7A7C;
/// Which way a whole hillside leans, so a dry flank is a place, not speckles.
const SOIL_DRIFT_SCALE_M: f64 = 170.0;
const SOIL_DRIFT_SALT: u64 = 0xD81F_700D;

/// Coarse cells that may host a forest glade. A wild roll both decides whether
/// a clearing exists and how wide it is.
const CLEARING_CELL_M: f64 = 160.0;
const CLEARING_SALT: u64 = 0xC1EA_1216;
const CLEARING_MIN_ROLL: f32 = 0.875;
const CLEARING_RADIUS_MIN_M: f64 = 22.0;
const CLEARING_RADIUS_MAX_M: f64 = 110.0;

/// Which stand type belongs on a patch: conifer on dry high ground, broadleaf
/// in humid lowlands, with hundreds of metres of drift between them.
const SPECIES_PATCH_SCALE_M: f64 = 420.0;
const SPECIES_PATCH_SALT: u64 = 0x5E3C_1355;
const MUSHROOM_CLUSTER_SCALE_M: f64 = 40.0;
const MUSHROOM_CLUSTER_SALT: u64 = 0x4D55_5348;
const BERRY_CLUSTER_SCALE_M: f64 = 55.0;
const BERRY_CLUSTER_SALT: u64 = 0xBE42_0001;

/// Where a stand is thicker or thinner than its atlas cell says.
///
/// A biome cell is a kilometre across, so without this a wood fades to open
/// ground as one smooth gradient with no clearings and no edge. Public because
/// the ground tint reads the same field: the dark floor under a stand has to be
/// under the trees that are actually placed, not near them.
pub(super) fn canopy_noise(seed: u64, p: GlobalXZ) -> f32 {
    value_noise(
        seed ^ CANOPY_NOISE_SALT,
        p.x / CANOPY_NOISE_SCALE_M,
        p.z / CANOPY_NOISE_SCALE_M,
    )
}

/// `[0, 1]` mottling inside a meadow. Shared with the terrain splat so a dry
/// patch of grass tufts sits on dry ground, not on a lush tile.
pub(super) fn soil_patch(seed: u64, p: GlobalXZ) -> f32 {
    value_noise(
        seed ^ SOIL_PATCH_SALT,
        p.x / SOIL_PATCH_SCALE_M,
        p.z / SOIL_PATCH_SCALE_M,
    ) * 0.5
        + 0.5
}

/// `[0, 1]` hillside lean: which soil a slope prefers, tens to hundreds of metres.
pub(super) fn soil_drift(seed: u64, p: GlobalXZ) -> f32 {
    value_noise(
        seed ^ SOIL_DRIFT_SALT,
        p.x / SOIL_DRIFT_SCALE_M,
        p.z / SOIL_DRIFT_SCALE_M,
    ) * 0.5
        + 0.5
}

/// How open a forest glade is at `p`, in `[0, 1]`. Each coarse cell rolls once;
/// a wild roll both creates the clearing and sets its radius.
pub(super) fn clearing_field(seed: u64, p: GlobalXZ) -> f32 {
    let cell = CLEARING_CELL_M;
    let cx0 = (p.x / cell).floor() as i64 - 1;
    let cx1 = cx0 + 2;
    let cz0 = (p.z / cell).floor() as i64 - 1;
    let cz1 = cz0 + 2;
    let mut best = 0.0f32;
    for cz in cz0..=cz1 {
        for cx in cx0..=cx1 {
            let mut rng = CellRng::new(seed ^ CLEARING_SALT, cx, cz);
            let wild = rng.unit();
            if wild < CLEARING_MIN_ROLL {
                continue;
            }
            let t = (wild - CLEARING_MIN_ROLL) / (1.0 - CLEARING_MIN_ROLL);
            let radius = CLEARING_RADIUS_MIN_M
                + (t as f64) * (CLEARING_RADIUS_MAX_M - CLEARING_RADIUS_MIN_M);
            let center_x = (cx as f64 + 0.5) * cell + (rng.unit() as f64 - 0.5) * cell * 0.35;
            let center_z = (cz as f64 + 0.5) * cell + (rng.unit() as f64 - 0.5) * cell * 0.35;
            let dx = p.x - center_x;
            let dz = p.z - center_z;
            let dist = (dx * dx + dz * dz).sqrt();
            if dist >= radius {
                continue;
            }
            let depth = lerp(0.55, 1.0, t);
            let edge = 1.0 - smoothstep((radius * 0.55) as f32, radius as f32, dist as f32);
            best = best.max(depth * edge);
        }
    }
    best
}

/// Regional tree-lineage bias in `[0, 1]`: 0 = broadleaf, 1 = conifer.
pub(super) fn species_conifer_bias(
    seed: u64,
    p: GlobalXZ,
    ground_m: f32,
    humidity: f32,
    alpine: f32,
    sea: f32,
) -> f32 {
    let patch = value_noise(
        seed ^ SPECIES_PATCH_SALT,
        p.x / SPECIES_PATCH_SCALE_M,
        p.z / SPECIES_PATCH_SCALE_M,
    ) * 0.5
        + 0.5;
    let elevation = smoothstep(sea + 40.0, sea + 520.0, ground_m);
    (alpine * 0.42 + elevation * 0.28 + (1.0 - humidity) * 0.22 + (1.0 - patch) * 0.18)
        .clamp(0.0, 1.0)
}

#[derive(Debug, Error)]
pub enum ScatterError {
    #[error("no prop assets found under {0}; run the asset generator sync")]
    NoAssets(PathBuf),

    #[error("prop {path} failed to load: {source}")]
    BadProp {
        path: PathBuf,
        #[source]
        source: engine::error::EngineError,
    },

    #[error("scatter GPU setup failed: {0}")]
    Engine(#[from] engine::error::EngineError),

    #[error(
        "rock {0} has no baked albedo; generate it in the Asset Lab and re-run tools/sync_props.py"
    )]
    UntexturedRock(PathBuf),
}

/// What a scattered prop is, which decides how it is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropClass {
    Grass,
    Rock,
    Tree,
    /// Stands in the shallows themselves.
    Reed,
    /// Scrub, thickest along a bank and on open ground.
    Bush,
    /// Fallen or broken trunks on glades and thin floor.
    Snag,
    /// Clustered forest-floor fungi.
    Mushroom,
    /// Berry patches on sunny glade edges.
    Berry,
}

/// Every class, in the order they are scattered.
pub const PROP_CLASSES: [PropClass; 8] = [
    PropClass::Grass,
    PropClass::Rock,
    PropClass::Tree,
    PropClass::Reed,
    PropClass::Bush,
    PropClass::Snag,
    PropClass::Mushroom,
    PropClass::Berry,
];

impl PropClass {
    fn spacing_m(self) -> f64 {
        match self {
            Self::Grass => GRASS_SPACING_M,
            Self::Rock => ROCK_SPACING_M,
            Self::Tree => TREE_SPACING_M,
            Self::Reed => REED_SPACING_M,
            Self::Bush => BUSH_SPACING_M,
            Self::Snag => SNAG_SPACING_M,
            Self::Mushroom => MUSHROOM_SPACING_M,
            Self::Berry => BERRY_SPACING_M,
        }
    }

    fn radius_m(self) -> f64 {
        match self {
            Self::Grass => GRASS_RADIUS_M,
            Self::Rock => ROCK_RADIUS_M,
            Self::Tree => TREE_RADIUS_M,
            Self::Reed => REED_RADIUS_M,
            Self::Bush => BUSH_RADIUS_M,
            Self::Snag => SNAG_RADIUS_M,
            Self::Mushroom => MUSHROOM_RADIUS_M,
            Self::Berry => BERRY_RADIUS_M,
        }
    }

    /// Scale range, so a stand of one mesh does not read as copies.
    fn scale_range(self) -> (f32, f32) {
        match self {
            // The generated tufts are ankle height; a meadow needs knee height.
            Self::Grass => (1.0, 2.1),
            Self::Rock => (0.5, 1.6),
            // Measured: sapling 4.8 m, alpine 9.4 m, ponderosa 14.9 m, spruce
            // 16.8 m. Floor at 1.575 keeps even saplings above knee height at
            // spawn; 4.0 lets spruce reach ~67 m — cathedral timber, not shrubbery.
            Self::Tree => (1.575, 4.0),
            // The clumps are authored just under two metres, which is a reed.
            Self::Reed => (0.7, 1.15),
            // The tall shrub is authored ~2 m; the low ones half that. A wide
            // range is what makes a hillside of them, not a cloned hedge.
            Self::Bush => (0.75, 1.75),
            Self::Snag => (0.85, 1.35),
            Self::Mushroom => (0.75, 1.25),
            Self::Berry => (0.80, 1.20),
        }
    }

    /// Towns keep wilderness grass, trees, and scrub off streets and roofs.
    fn skips_urban(self) -> bool {
        matches!(
            self,
            Self::Grass | Self::Tree | Self::Bush | Self::Snag | Self::Mushroom | Self::Berry
        )
    }

    /// Dirt ribbons stay plant-free; stones may still sit on the bed.
    fn skips_road(self) -> bool {
        self.skips_urban() || matches!(self, Self::Reed)
    }

    /// Whether being near water changes how much of this class belongs here,
    /// so hydrology has to be read before density rather than after it.
    ///
    /// It is the expensive query, and for grass and stones the cheap climate
    /// layers throw out most candidates first. Reeds have nothing to throw out
    /// — away from water they do not grow at all — and a wood and its scrub
    /// follow a watercourse across ground that could not hold a stand otherwise.
    fn follows_water(self) -> bool {
        matches!(self, Self::Reed | Self::Bush | Self::Tree)
    }

    /// How far the prop is pushed into the ground, in metres of its own size.
    fn bed_in(self) -> f32 {
        match self {
            Self::Grass => 0.04,
            // Stones sit in a hollow rather than balancing on the surface.
            Self::Rock => 0.28,
            Self::Tree => 0.12,
            Self::Reed => 0.06,
            Self::Bush => 0.14,
            Self::Snag => 0.10,
            Self::Mushroom => 0.02,
            Self::Berry => 0.04,
        }
    }

    /// The band of the signed water field this class stands in, in metres:
    /// negative is dry ground, positive is standing water.
    fn wet_band(self) -> (f32, f32) {
        match self {
            Self::Grass => (f32::NEG_INFINITY, -GRASS_DRY_MARGIN),
            Self::Rock => (f32::NEG_INFINITY, 0.0),
            Self::Tree => (f32::NEG_INFINITY, -TREE_DRY_MARGIN),
            // In the shallows and on the mud beside them, and nowhere a boat
            // could float: a reed is two metres tall and rooted in the bottom.
            Self::Reed => (-1.2, 0.8),
            Self::Bush => (f32::NEG_INFINITY, -0.5),
            Self::Snag | Self::Mushroom | Self::Berry => (f32::NEG_INFINITY, -0.8),
        }
    }

    /// Whether this class can stand where the water field reads `wetness_m`.
    pub fn stands_in(self, wetness_m: f32) -> bool {
        let (lo, hi) = self.wet_band();
        (lo..=hi).contains(&wetness_m)
    }

    /// How much of the fine lattice to keep at `dist_m` from the player.
    ///
    /// `None` means every cell: grass and reeds stay dense and close, and
    /// stones are already a wide lattice. Trees and bushes thin toward the
    /// walked ring so the window can grow without a squared instance count.
    fn keep_fraction(self, dist_m: f64) -> Option<f32> {
        match self {
            Self::Tree => Some(thin_keep(
                dist_m,
                TREE_DENSE_RADIUS_M,
                TREE_RADIUS_M,
                TREE_SPACING_M,
                TREE_OUTER_SPACING_M,
            )),
            Self::Bush => Some(thin_keep(
                dist_m,
                BUSH_DENSE_RADIUS_M,
                BUSH_RADIUS_M,
                BUSH_SPACING_M,
                BUSH_OUTER_SPACING_M,
            )),
            Self::Grass | Self::Rock | Self::Reed | Self::Snag | Self::Mushroom | Self::Berry => {
                None
            }
        }
    }
}

/// Keep-probability that interpolates from a dense inner disk to a coarser rim.
fn thin_keep(
    dist_m: f64,
    dense_m: f64,
    window_m: f64,
    inner_spacing: f64,
    outer_spacing: f64,
) -> f32 {
    if dist_m <= dense_m {
        return 1.0;
    }
    let span = (window_m - dense_m).max(1.0);
    let t = ((dist_m - dense_m) / span).clamp(0.0, 1.0);
    let outer = inner_spacing / outer_spacing;
    (1.0 + t * (outer - 1.0)) as f32
}

/// How much of each cover class belongs at one spot, as a probability that a
/// lattice cell there is occupied, plus the conditions that decide which
/// variant of a class belongs there.
#[derive(Clone, Copy, Debug, Default)]
pub struct GroundCover {
    pub grass: f32,
    pub rock: f32,
    pub tree: f32,
    pub reed: f32,
    pub bush: f32,
    pub snag: f32,
    pub mushroom: f32,
    pub berry: f32,
    /// 0 = parched, 1 = soaking.
    pub moisture: f32,
    /// 0 = lowland, 1 = at the treeline.
    pub alpine: f32,
    /// 0 = deep in the stand, 1 = open ground.
    pub openness: f32,
    /// 0 = broadleaf stand, 1 = conifer stand.
    pub conifer: f32,
    /// 0 = closed canopy, 1 = deep glade.
    pub clearing: f32,
    /// 0 = a face nothing can root on, 1 = ground that holds soil.
    pub footing: f32,
    /// 0 = out of reach of any water, 1 = at the waterline. Zero until
    /// [`GroundCover::with_water`] has been asked.
    pub bank: f32,
    /// How square the ground faces the sun: 0 in the shade of its own slope,
    /// 0.5 on the flat, 1 straight into it.
    pub aspect: f32,
    /// How much of this ground the terrain shader will draw as snow.
    pub snow: f32,
}

/// Which way the drawn ground falls under one point.
///
/// The direction is the half of a gradient that usually gets thrown away, and
/// it is what makes a hillside asymmetric: the sunny flank is thin and dry
/// while the shaded one holds its moisture.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fall {
    /// `1 - n.y`, the measure the terrain shader blends rock with.
    pub steep: f32,
    /// Unit downhill heading; zero on the flat.
    pub downhill: Vec2,
}

impl Fall {
    /// The fall a surface normal describes.
    pub fn of(normal: Vec3) -> Self {
        Self {
            steep: (1.0 - normal.y).clamp(0.0, 1.0),
            downhill: Vec2::new(normal.x, normal.z).normalize_or_zero(),
        }
    }

    /// Sun on this ground, 0 to 1, flat ground reading half.
    fn sunniness(self) -> f32 {
        let sun = Vec2::new(SUN_DIR.0, SUN_DIR.2).normalize_or_zero();
        0.5 + 0.5 * self.downhill.dot(sun) * smoothstep(0.0, 0.35, self.steep)
    }

    /// `n · light`, matching the terrain shader's snow shade.
    fn ndl(self) -> f32 {
        let ny = (1.0 - self.steep).clamp(0.0, 1.0);
        let horiz = (1.0 - ny * ny).max(0.0).sqrt();
        let normal = Vec3::new(self.downhill.x * horiz, ny, self.downhill.y * horiz);
        let sun = Vec3::new(SUN_DIR.0, SUN_DIR.1, SUN_DIR.2).normalize();
        normal.dot(sun).max(0.0)
    }
}

/// How much of this ground the terrain shader will draw as snow, in `[0, 1]`.
///
/// Same line, full-cover height, and slope-cling as the terrain material,
/// so a bush is not standing on a white cap the GPU has already covered.
fn snow_cover(height_above_sea: f32, fall: Fall) -> f32 {
    let shade = 1.0 - fall.ndl();
    let line = lerp(SNOW_LINE_M + 160.0, SNOW_LINE_M - 240.0, shade);
    let mut snow = smoothstep(line, SNOW_FULL_M, height_above_sea);
    let cling = 1.0 - smoothstep(SNOW_SLOPE_START, SNOW_SLOPE_END, fall.steep);
    snow = snow * cling + snow * snow * 0.22 * (1.0 - cling);
    snow.clamp(0.0, 1.0)
}

/// How much rooted cover still belongs once the ground reads as snow.
fn thaw(snow: f32) -> f32 {
    1.0 - smoothstep(0.04, 0.28, snow)
}

impl GroundCover {
    fn of(self, class: PropClass) -> f32 {
        match class {
            PropClass::Grass => self.grass,
            PropClass::Rock => self.rock,
            PropClass::Tree => self.tree,
            PropClass::Reed => self.reed,
            PropClass::Bush => self.bush,
            PropClass::Snag => self.snag,
            PropClass::Mushroom => self.mushroom,
            PropClass::Berry => self.berry,
        }
    }

    /// Cover from the cheap layers only: atlas climate plus the drawn slope.
    ///
    /// Deliberately does not touch hydrology — that costs an order of magnitude
    /// more, and most candidates are rejected before it would matter.
    pub fn sample(
        seed: u64,
        surface: &ContinentalSurface,
        p: GlobalXZ,
        ground_m: f32,
        fall: Fall,
        canopy_noise: f32,
    ) -> Self {
        let fields = surface.fields();
        let x = p.x as f32;
        let z = p.z as f32;
        let humidity = fields
            .sample_smooth(&fields.humidity01, x, z)
            .clamp(0.0, 1.0);
        let canopy = fields.sample_smooth(&fields.canopy01, x, z).clamp(0.0, 1.0);
        let sea = surface.sea_surface_z();

        let slope = fall.steep;
        let aspect = fall.sunniness();
        // A slope in full sun dries out; the shaded flank of the same hill
        // keeps what falls on it.
        let shade = lerp(1.15, 0.85, aspect);
        let flat = (1.0 - smoothstep(0.18, 0.62, slope)).clamp(0.0, 1.0);
        let beach = 1.0 - smoothstep(0.0, 6.0, ground_m - sea);
        let alpine = smoothstep(TREELINE_M - TREELINE_FADE_M, TREELINE_M, ground_m);
        let snow = snow_cover(ground_m - sea, fall);
        // Green tufts and scrub on a white cap read as a mistake; stones stay.
        let rooted = thaw(snow);

        let conifer = species_conifer_bias(seed, p, ground_m, humidity, alpine, sea);
        let clearing = if canopy > 0.35 {
            clearing_field(seed, p)
        } else {
            0.0
        };

        // Torn canopy edges: a kilometre-wide biome cell would otherwise fade
        // into open ground as one smooth gradient, with no clearings.
        let mut tree = smoothstep(0.20, 0.58, canopy * shade + canopy_noise * 0.30)
            * flat
            * (1.0 - alpine)
            * (1.0 - beach)
            * rooted;
        tree *= 1.0 - clearing * 0.92;
        tree *= TREE_DENSITY;

        let patch = soil_patch(seed, p);
        let floor_sparse = tree * smoothstep(0.10, 0.32, patch) * (1.0 - clearing * 0.65);

        let mut grass = (0.30 + 0.62 * humidity)
            * shade
            * flat
            * (1.0 - 0.75 * beach)
            * (1.0 - 0.35 * alpine)
            * rooted;
        grass *= 1.0 + clearing * 0.85;
        grass *= 1.0 - floor_sparse * 0.55;

        // Stones come loose where the ground is steep, high, or bare.
        let rock = (0.05 + 0.55 * smoothstep(0.20, 0.75, slope) + 0.35 * alpine)
            * (1.0 - 0.6 * canopy)
            * (1.0 + floor_sparse * 0.35);

        // Scrub fills glades and the shaded mid-canopy band under partial cover.
        let understory = (tree * (1.0 - tree) * 4.0).min(1.0);
        let bush_base = (0.22 + 0.32 * humidity)
            * flat
            * (1.0 - 0.85 * beach)
            * lerp(1.0, 0.48, alpine)
            * rooted;
        let open_scrub = lerp(0.40, 1.0, 1.0 - tree);
        let canopy_scrub = understory * 0.72 + 0.12;
        let bush = bush_base
            * open_scrub.max(canopy_scrub * smoothstep(0.12, 0.55, tree))
            * (1.0 + clearing * 0.25);

        let mushroom_cluster = value_noise(
            seed ^ MUSHROOM_CLUSTER_SALT,
            p.x / MUSHROOM_CLUSTER_SCALE_M,
            p.z / MUSHROOM_CLUSTER_SCALE_M,
        ) * 0.5
            + 0.5;
        let mushroom = smoothstep(0.80, 0.94, mushroom_cluster)
            * tree
            * (0.35 + 0.45 * humidity)
            * (1.0 - clearing * 0.55)
            * flat
            * rooted;

        let berry_cluster = value_noise(
            seed ^ BERRY_CLUSTER_SALT,
            p.x / BERRY_CLUSTER_SCALE_M,
            p.z / BERRY_CLUSTER_SCALE_M,
        ) * 0.5
            + 0.5;
        let berry = smoothstep(0.74, 0.90, berry_cluster)
            * (clearing * 0.55 + (1.0 - tree) * 0.35)
            * (0.35 + 0.55 * humidity)
            * flat
            * (1.0 - alpine * 0.85)
            * rooted;

        let snag =
            (clearing * 0.50 + floor_sparse * 0.38 + tree * 0.07) * flat * (1.0 - beach) * rooted;

        Self {
            grass: grass.clamp(0.0, 1.0),
            rock: rock.clamp(0.0, 1.0),
            tree: tree.clamp(0.0, 1.0),
            // No water has been asked about yet, and neither of these grows
            // anywhere else.
            reed: 0.0,
            bush: bush.clamp(0.0, 1.0),
            snag: snag.clamp(0.0, 1.0),
            mushroom: mushroom.clamp(0.0, 1.0),
            berry: berry.clamp(0.0, 1.0),
            // Atlas humidity is a rainfall index, not soil moisture, and
            // standing timber is itself evidence of water. Taken raw it puts
            // straw tufts all over a forest floor that is plainly green.
            moisture: ((0.16 + 0.46 * humidity + 0.44 * canopy) * shade * (1.0 - 0.55 * beach))
                .clamp(0.0, 1.0),
            alpine,
            openness: (1.0 - tree).clamp(0.0, 1.0),
            conifer,
            clearing,
            footing: flat,
            bank: 0.0,
            aspect,
            snow,
        }
    }

    /// Fold in the one hydrological fact the cheap layers cannot know: how far
    /// this spot is from water.
    ///
    /// `wetness_m` is the surface's signed field, so a pond bank and an ocean
    /// shore are the same measurement, and the whole sub-atlas layer dresses
    /// itself without knowing that it exists.
    pub fn with_water(mut self, wetness_m: f32) -> Self {
        self.bank = (1.0 + wetness_m / BANK_REACH_M).clamp(0.0, 1.0);
        self.moisture = (self.moisture + 0.45 * self.bank).clamp(0.0, 1.0);
        // Gallery woodland: a line of trees follows water across ground far too
        // dry to carry a wood of its own.
        self.tree = (self.tree
            + 0.40 * self.bank * (1.0 - self.alpine) * thaw(self.snow) * TREE_DENSITY)
            .clamp(0.0, 1.0);
        self.openness = (1.0 - self.tree).clamp(0.0, 1.0);
        // Scrub grows anywhere flat and damp enough, and crowds the edge of the
        // water on top of that — added rather than scaled, or a bank running
        // through dry country would have nothing on it. Snow still wins: a
        // frozen shore is not a reed-bed's cousin in green shrubs.
        let fringe = self.bank * self.footing * (1.0 - self.alpine) * thaw(self.snow);
        self.bush = (self.bush + 0.34 * fringe).clamp(0.0, 1.0);
        self.mushroom = (self.mushroom + 0.18 * self.moisture * self.tree * fringe).clamp(0.0, 1.0);
        self.berry = (self.berry + 0.12 * fringe * (1.0 - self.alpine)).clamp(0.0, 1.0);
        self.reed = (0.45 + 0.5 * self.moisture).clamp(0.0, 1.0) * self.bank;
        self
    }
}

/// The conditions a prop variant belongs in, read from the name the generator
/// gave it.
///
/// Naming is the only description the meshes carry, and it is a deliberate
/// contract: a straw tuft dropped into a rainforest reads as dead grass, and a
/// sapling standing alone in deep timber reads as a mistake.
#[derive(Clone, Copy, Debug)]
struct PropTaste {
    /// Wants dry ground.
    dry: f32,
    /// Wants high, thin ground.
    alpine: f32,
    /// Wants open ground rather than a closed stand.
    open: f32,
    /// 0 = broadleaf, 1 = conifer.
    conifer: f32,
}

impl PropTaste {
    const NEUTRAL: Self = Self {
        dry: 0.5,
        alpine: 0.5,
        open: 0.5,
        conifer: 0.5,
    };

    fn of(stem: &str) -> Self {
        let mut taste = Self::NEUTRAL;
        if stem.contains("dry") {
            taste.dry = 1.0;
        }
        if stem.contains("lush") {
            taste.dry = 0.0;
        }
        if stem.contains("sparse") {
            taste.dry = 0.75;
            taste.open = 0.8;
        }
        if stem.contains("meadow") {
            taste.dry = 0.35;
            taste.open = 1.0;
        }
        // Valley scrub; `bush_alpine_low` overrides below. Neutral 0.5 would
        // still pick berry and lush clumps on a snow line.
        if stem.contains("bush") {
            taste.alpine = 0.1;
        }
        if stem.contains("alpine") || stem.contains("talus") || stem.contains("basalt") {
            taste.alpine = 1.0;
        }
        if stem.contains("riverstone") || stem.contains("cobble") {
            taste.alpine = 0.0;
        }
        if stem.contains("sapling") {
            taste.open = 1.0;
            taste.alpine = 0.35;
            taste.conifer = 0.85;
        }
        if stem.contains("spruce") || stem.contains("ponderosa") || stem.contains("pine") {
            taste.open = 0.15;
            taste.conifer = 1.0;
        }
        if stem.contains("cypress") {
            taste.open = 0.25;
            taste.conifer = 1.0;
            taste.dry = 0.25;
        }
        if stem.contains("oak")
            || stem.contains("birch")
            || stem.contains("maple")
            || stem.contains("poplar")
            || stem.contains("willow")
        {
            taste.conifer = 0.0;
            taste.open = 0.45;
        }
        if stem.contains("snag") {
            taste.open = 0.82;
            taste.conifer = 0.65;
        }
        if stem.contains("mushroom") {
            taste.dry = 0.15;
            taste.open = 0.22;
            taste.alpine = 0.08;
        }
        if stem.contains("berry") {
            taste.open = 0.78;
            taste.dry = 0.35;
        }
        // Berries want light and an edge to stand on; a reed only ever stands
        // in the open water it is rooted in.
        if stem.contains("berries") {
            taste.open = 0.85;
        }
        if stem.contains("open") {
            taste.open = 0.9;
        }
        if stem.contains("broad") {
            taste.open = 0.7;
            taste.dry = 0.15;
            taste.conifer = 0.0;
        }
        if stem.contains("reed") {
            taste.dry = 0.0;
            taste.alpine = 0.2;
            taste.open = 0.9;
        }
        taste
    }

    /// How well this variant suits a spot, in `[0, 1]`.
    fn fit(self, cover: &GroundCover) -> f32 {
        let want_dry = 1.0 - cover.moisture;
        let miss = (self.dry - want_dry).abs() * 1.15
            + (self.alpine - cover.alpine).abs() * 0.45
            + (self.open - cover.openness).abs() * 0.40
            + (self.conifer - cover.conifer).abs() * 0.55;
        (1.0 - miss).clamp(0.04, 1.0)
    }
}

/// A prop mesh on disk, before it reaches the world.
#[derive(Clone, Debug)]
pub struct PropAsset {
    pub class: PropClass,
    pub path: PathBuf,
}

/// The prop meshes the scatter may use, resolved on disk.
#[derive(Clone, Debug)]
pub struct ScatterCatalog {
    assets: Vec<PropAsset>,
}

impl ScatterCatalog {
    /// Find the vendored props, wherever this build is running from.
    ///
    /// `ORRUN_ASSETS` wins, then the folder next to the executable (a shipped
    /// build), then the crate's own assets (a `cargo run`).
    pub fn discover() -> Result<Self, ScatterError> {
        Self::load(props_dir()?)
    }

    /// Collect every prop under `root`, sorted by name so the variant a lattice
    /// cell picks does not depend on directory order.
    pub fn load(root: impl AsRef<Path>) -> Result<Self, ScatterError> {
        let root = root.as_ref();
        let mut assets = Vec::new();
        for (dir, class) in [
            ("grass", PropClass::Grass),
            ("rocks", PropClass::Rock),
            ("trees", PropClass::Tree),
            ("reeds", PropClass::Reed),
            ("bushes", PropClass::Bush),
            ("snags", PropClass::Snag),
            ("mushrooms", PropClass::Mushroom),
            ("berries", PropClass::Berry),
        ] {
            let dir = root.join(dir);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("glb")))
                .collect();
            paths.sort();
            assets.extend(paths.into_iter().map(|path| PropAsset { class, path }));
        }
        if assets.is_empty() {
            return Err(ScatterError::NoAssets(root.to_path_buf()));
        }
        Ok(Self { assets })
    }

    pub fn len(&self) -> usize {
        self.assets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    pub fn count_of(&self, class: PropClass) -> usize {
        self.assets.iter().filter(|a| a.class == class).count()
    }
}

/// Folder that holds the vendored prop glbs (`grass/`, `trees/`, …).
pub(super) fn props_dir() -> Result<PathBuf, ScatterError> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir).join("props"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets").join("props"));
        }
    }
    tried.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("props"),
    );
    for root in &tried {
        if root.is_dir() {
            return Ok(root.clone());
        }
    }
    Err(ScatterError::NoAssets(
        tried.into_iter().next().unwrap_or_default(),
    ))
}

/// One prop variant that has been uploaded and can be placed.
///
/// Instances are split into 200 m bins, each its own instanced entity, so the
/// renderer can frustum-cull a hillside behind the camera instead of unioning
/// the whole window into one bounds sphere. Bins share the prototype's mesh.
struct LiveProp {
    class: PropClass,
    taste: PropTaste,
    prototype: EntityId,
    bins: HashMap<(i32, i32), EntityId>,
    /// Empty entities retained so a sliding window does not change the GPU
    /// batch layout every time a cell leaves on one edge and enters on another.
    spare_bins: Vec<EntityId>,
}

/// One prop standing on the ground, in the world's own coordinates.
///
/// Global rather than render space, because a sow outlives the origin it was
/// made under: when render space rebases, the same sprigs are handed to the
/// renderer against the new origin instead of the window being sown again.
#[derive(Clone, Copy, Debug)]
struct Sprig {
    variant: usize,
    at: GlobalPosition,
    yaw_deg: f32,
    scale: f32,
    /// Linear multiply on the shared mesh albedo (white = authored look).
    tint: Color,
}

/// Everything a sow reads, all of it owned or shared — nothing borrowed from the
/// frame it was started on.
///
/// This is what makes the sow a job rather than a stall. A window of cover is
/// tens of thousands of lattice cells across five classes, and it is re-sown
/// whenever the player walks off its centre or ground streams in; on the main
/// thread that is a hitch, and at a spawn — when every chunk in the ring arrives
/// at once — it was a hitch every frame.
struct Sowing {
    seed: u64,
    /// Indexed like [`ScatterLayer::props`], so a sprig can name its variant.
    variants: Vec<(PropClass, PropTaste)>,
    surface: Arc<ContinentalSurface>,
    ponds: Arc<PondField>,
    ground: ContactSnapshot,
    plots: Arc<BuildingIndex>,
    /// Atlas roads and hamlet dirt cuts in the sow window.
    roads: RoadClear,
}

/// Far-band pine stand-ins: same cover field as the tint, medium-tier height.
struct FarSowing {
    seed: u64,
    surface: Arc<ContinentalSurface>,
    ponds: Arc<PondField>,
    inner_m: f64,
    outer_m: f64,
    spacing_m: f64,
    roads: RoadClear,
}

/// Road and village-cut segments that must stay free of plant cover.
struct RoadClear {
    segs: Vec<(Vec2, Vec2, f32)>,
}

impl RoadClear {
    fn gather(
        surface: &ContinentalSurface,
        hamlets: &[HamletStand],
        focus: GlobalXZ,
        reach_m: f64,
    ) -> Self {
        let origin = Vec2::new(focus.x as f32, focus.z as f32);
        let reach = reach_m as f32;
        let mut segs = Vec::new();
        for road in surface.roads() {
            let half = road_ribbon_width(road.class) * 0.5 + ROAD_VEG_PAD_M;
            for w in road.points.windows(2) {
                if dist_point_seg(origin, w[0], w[1]) <= reach + half {
                    segs.push((w[0], w[1], half));
                }
            }
        }
        let cut_half = ROAD_CLEAR_M * 0.5 + ROAD_VEG_PAD_M;
        for hamlet in hamlets {
            for w in hamlet.cut.windows(2) {
                if dist_point_seg(origin, w[0], w[1]) <= reach + cut_half {
                    segs.push((w[0], w[1], cut_half));
                }
            }
        }
        Self { segs }
    }

    fn blocks(&self, p: GlobalXZ) -> bool {
        let q = Vec2::new(p.x as f32, p.z as f32);
        self.segs
            .iter()
            .any(|&(a, b, half)| dist_point_seg(q, a, b) < half)
    }
}

fn dist_point_seg(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let d = b - a;
    let len2 = d.length_squared();
    if len2 < 1e-8 {
        return p.distance(a);
    }
    let t = ((p - a).dot(d) / len2).clamp(0.0, 1.0);
    p.distance(a + d * t)
}

/// A sow in flight, and the state of the world it speaks for.
struct Pending<T> {
    focus: GlobalXZ,
    resident_chunks: usize,
    job: JoinHandle<(T, f32)>,
}

/// Near-window sprigs, already grouped on the sow thread.
struct NearSown {
    buckets: HashMap<(usize, i32, i32), Vec<Sprig>>,
}

/// Far-band pines, already grouped on the sow thread.
struct FarSown {
    cells: HashMap<(i32, i32), Vec<Sprig>>,
    draws: HashMap<(i32, i32), Vec<(i32, i32)>>,
}

/// One tree-variant cell waiting to go full-res. `shown` trees already stand.
#[derive(Clone, Copy, Debug)]
struct TreeJob {
    vi: usize,
    bx: i32,
    bz: i32,
    shown: usize,
}

/// The live cover around the player.
pub struct ScatterLayer {
    props: Vec<LiveProp>,
    far_entity: EntityId,
    seed: u64,
    /// Where the standing cover was centred, and the residency it was sown
    /// against. Both are `None`/zero until the first sow lands.
    centre: Option<GlobalXZ>,
    resident_chunks: usize,
    far_centre: Option<GlobalXZ>,
    near_placed: usize,
    far_placed: usize,
    pending: Option<Pending<NearSown>>,
    far_pending: Option<Pending<FarSown>>,
    /// What the last near sow took on its own thread. Worth watching: it is the
    /// budget that decides how far behind the cover can fall while moving.
    sow_ms: f32,
    /// Last footprints a sow was started against.
    house_plots: Arc<BuildingIndex>,
    /// Sprigs of the standing near window, grouped for budgeted upload.
    near_buckets: HashMap<(usize, i32, i32), Vec<Sprig>>,
    /// Tree cells waiting to go full-res, nearest to the player.
    ///
    /// Rebuilt each follow from the standing window, so walking toward a wood
    /// promotes that wood instead of finishing a FIFO left over from the sow.
    tree_queue: VecDeque<TreeJob>,
    /// Grass / rock / bush bins not yet spawned, nearest to the player.
    other_queue: VecDeque<(usize, i32, i32)>,
    /// How many full-res trees each tree bin already shows.
    tree_shown: HashMap<(usize, i32, i32), usize>,
    /// Collider handles per visible tree cell, kept in lockstep with
    /// `tree_shown` instead of rebuilding the complete collision layer.
    tree_colliders: HashMap<(usize, i32, i32), Vec<ColliderId>>,
    /// Walked-tier cells that already have full-res trees, so far proxies hide.
    near_tree_bins: HashSet<(i32, i32)>,
    /// Far proxies, still in global space, one list per walked chunk.
    far_cells: HashMap<(i32, i32), Vec<Sprig>>,
    /// Draw cell → walked cells. Built on the sow thread.
    far_draws: HashMap<(i32, i32), Vec<(i32, i32)>>,
    /// GPU bin for each far cell. The prototype [`far_entity`] stays empty.
    far_bins: HashMap<(i32, i32), EntityId>,
    /// Empty far draw entities retained to keep the GPU batch layout stable.
    far_spares: Vec<EntityId>,
    /// Draw cells this sow still needs to write. The capped queue is a view of these.
    far_unwritten: HashSet<(i32, i32)>,
    /// Nearest unwritten far cells, rebuilt each follow.
    far_queue: VecDeque<(i32, i32)>,
}

impl std::fmt::Debug for ScatterLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScatterLayer")
            .field("variants", &self.props.len())
            .field("placed", &(self.near_placed + self.far_placed))
            .field("sowing", &self.pending.is_some())
            .field("far_sowing", &self.far_pending.is_some())
            .finish()
    }
}

impl ScatterLayer {
    /// Upload every catalogue mesh once; nothing is placed yet.
    ///
    /// Bin entities for the walked ring are spawned on demand as the window
    /// fills. Far pines share one mesh and draw in kilometre bins.
    pub fn install(
        world: &mut World,
        catalog: &ScatterCatalog,
        seed: i32,
    ) -> Result<Self, ScatterError> {
        let mut props = Vec::with_capacity(catalog.assets.len());
        for asset in &catalog.assets {
            let mesh = Model::load(&asset.path).map_err(|source| ScatterError::BadProp {
                path: asset.path.clone(),
                source,
            })?;
            if asset.class == PropClass::Rock && mesh.albedo().is_none() {
                return Err(ScatterError::UntexturedRock(asset.path.clone()));
            }
            let stem = asset
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let prototype = world.spawn_instanced(mesh);
            if asset.class != PropClass::Tree {
                world
                    .set_casts_shadow(prototype, false)
                    .expect("just spawned prop prototype");
            }
            props.push(LiveProp {
                class: asset.class,
                taste: PropTaste::of(&stem),
                prototype,
                bins: HashMap::new(),
                spare_bins: Vec::new(),
            });
        }
        let far_mesh = pine_proxy().expect("far-band pine proxy");
        let far_entity = world.spawn_instanced(far_mesh);
        world
            .set_casts_shadow(far_entity, false)
            .expect("just spawned far pine prototype");
        let mut far_spares = Vec::with_capacity(FAR_DRAW_POOL);
        for _ in 0..FAR_DRAW_POOL {
            let id = world.spawn_instanced_like(far_entity)?;
            world.reserve_instances(id, FAR_DRAW_INSTANCE_RESERVE)?;
            far_spares.push(id);
        }
        Ok(Self {
            props,
            far_entity,
            seed: seed as u32 as u64,
            centre: None,
            resident_chunks: 0,
            far_centre: None,
            near_placed: 0,
            far_placed: 0,
            pending: None,
            far_pending: None,
            sow_ms: 0.0,
            house_plots: Arc::new(BuildingIndex::new(Vec::new())),
            near_buckets: HashMap::new(),
            tree_queue: VecDeque::new(),
            other_queue: VecDeque::new(),
            tree_shown: HashMap::new(),
            tree_colliders: HashMap::new(),
            near_tree_bins: HashSet::new(),
            far_cells: HashMap::new(),
            far_draws: HashMap::new(),
            far_bins: HashMap::new(),
            far_spares,
            far_unwritten: HashSet::new(),
            far_queue: VecDeque::new(),
        })
    }

    /// Props standing in the world right now, full mesh and far stand-ins.
    pub fn placed_count(&self) -> usize {
        self.near_placed + self.far_placed
    }

    /// A near-window sow is on a worker. The loading screen waits; walking does not.
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// What the last sow took, in milliseconds, on its own thread.
    pub fn sow_ms(&self) -> f32 {
        self.sow_ms
    }

    /// Bins still waiting to upload this window.
    pub fn upload_backlog(&self) -> usize {
        self.tree_queue.len() + self.other_queue.len()
    }

    /// Horizon pine cells not yet on the GPU.
    pub fn far_backlog(&self) -> usize {
        self.far_unwritten.len()
    }

    /// Take everything down (leaving the world).
    ///
    /// A sow already in flight is abandoned rather than waited for: whatever it
    /// was standing on belongs to a world that is being left.
    pub fn clear(&mut self, world: &mut World) -> EngineResult<()> {
        for prop in &mut self.props {
            for id in prop.bins.values().copied() {
                world.despawn(id);
            }
            for id in prop.spare_bins.drain(..) {
                world.despawn(id);
            }
            prop.bins.clear();
        }
        world.set_instances(self.far_entity, &[])?;
        for id in self.far_bins.values().copied() {
            world.despawn(id);
        }
        for id in self.far_spares.drain(..) {
            world.despawn(id);
        }
        self.far_bins.clear();
        self.pending = None;
        self.far_pending = None;
        self.centre = None;
        self.far_centre = None;
        self.resident_chunks = 0;
        self.near_placed = 0;
        self.far_placed = 0;
        self.house_plots = Arc::new(BuildingIndex::new(Vec::new()));
        self.near_buckets.clear();
        self.tree_queue.clear();
        self.other_queue.clear();
        self.tree_shown.clear();
        self.tree_colliders.clear();
        self.near_tree_bins.clear();
        self.far_cells.clear();
        self.far_draws.clear();
        self.far_unwritten.clear();
        self.far_queue.clear();
        world.collision_mut().clear_layer(COLLIDER_LAYER);
        Ok(())
    }

    /// Keep the cover window centred on the player.
    ///
    /// Returns whether the cover standing in the world changed this frame. Never
    /// blocks: the first near sow of an entry runs on a worker like the rest,
    /// and the loading screen waits on [`Self::busy`]. The far band always sows
    /// on its own thread.
    pub fn follow(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        surface: &Arc<ContinentalSurface>,
        ponds: &Arc<PondField>,
        focus: GlobalXZ,
        house_plots: &Arc<BuildingIndex>,
        hamlets: &[HamletStand],
        rebased: bool,
    ) -> EngineResult<bool> {
        let resident = stream.resident_count();
        let mut changed = false;

        let mut landed_near = false;

        if let Some(pending) = self.pending.take() {
            if pending.job.is_finished() {
                let (sown, took_ms) = pending.job.join().expect("scatter thread");
                self.sow_ms = took_ms;
                self.centre = Some(pending.focus);
                self.resident_chunks = pending.resident_chunks;
                self.accept_near(world, sown.buckets)?;
                landed_near = true;
                changed = true;
            } else {
                self.pending = Some(pending);
            }
        }
        // A near landing already dirties many instance batches. Applying a far
        // sow on the same frame stacked the 200 ms hitches in the log.
        if !landed_near {
            if let Some(pending) = self.far_pending.take() {
                if pending.job.is_finished() {
                    let (sown, _) = pending.job.join().expect("scatter far thread");
                    self.far_centre = Some(pending.focus);
                    self.accept_far(world, sown)?;
                    changed = true;
                } else {
                    self.far_pending = Some(pending);
                }
            }
        }
        if rebased {
            if !self.near_buckets.is_empty() {
                self.restand_applied(world)?;
                changed = true;
            }
            if !self.far_bins.is_empty() {
                self.restand_far(world)?;
                changed = true;
            }
        }

        if self.follow_near(
            stream,
            surface,
            ponds,
            focus,
            house_plots,
            hamlets,
            resident,
        )? {
            changed = true;
        }
        self.reprioritize_uploads(focus);
        let upload = self.drain_uploads(world, landed_near)?;
        self.follow_far(surface, ponds, focus, hamlets)?;
        Ok(changed || upload.trees || upload.other || upload.far)
    }

    fn follow_near(
        &mut self,
        stream: &WorldStream,
        surface: &Arc<ContinentalSurface>,
        ponds: &Arc<PondField>,
        focus: GlobalXZ,
        house_plots: &Arc<BuildingIndex>,
        hamlets: &[HamletStand],
        resident: usize,
    ) -> EngineResult<bool> {
        let moved = self
            .centre
            .map(|c| ((c.x - focus.x).powi(2) + (c.z - focus.z).powi(2)).sqrt())
            .unwrap_or(f64::INFINITY);
        // Newly streamed ground has no cover on it until the window is sown
        // again, so residency counts as movement — but only once the streamer has
        // stopped, or a spawn would chase every chunk of the ring in turn and sow
        // the whole window a hundred and sixty-nine times.
        let plots_changed =
            !Arc::ptr_eq(&self.house_plots, house_plots) && *self.house_plots != **house_plots;
        let wanted = moved >= RESEED_M
            || (resident != self.resident_chunks && stream.walked_pending_count() == 0)
            || plots_changed;
        if !wanted || self.pending.is_some() {
            return Ok(false);
        }

        self.house_plots = Arc::clone(house_plots);
        let near_reach = PROP_CLASSES
            .iter()
            .map(|c| c.radius_m())
            .fold(0.0_f64, f64::max);
        let roads = RoadClear::gather(surface, hamlets, focus, near_reach);
        let sowing = Sowing {
            seed: self.seed,
            variants: self
                .props
                .iter()
                .map(|prop| (prop.class, prop.taste))
                .collect(),
            surface: Arc::clone(surface),
            ponds: Arc::clone(ponds),
            ground: stream.contact_snapshot(),
            plots: Arc::clone(&self.house_plots),
            roads,
        };
        self.pending = Some(Pending {
            focus,
            resident_chunks: resident,
            job: std::thread::Builder::new()
                .name("scatter".into())
                .spawn(move || timed(|| sowing.sow(focus)))
                .expect("spawn the scatter thread"),
        });
        Ok(false)
    }

    fn follow_far(
        &mut self,
        surface: &Arc<ContinentalSurface>,
        ponds: &Arc<PondField>,
        focus: GlobalXZ,
        hamlets: &[HamletStand],
    ) -> EngineResult<()> {
        if self.far_pending.is_some() {
            return Ok(());
        }
        let moved = self
            .far_centre
            .map(|c| ((c.x - focus.x).powi(2) + (c.z - focus.z).powi(2)).sqrt())
            .unwrap_or(f64::INFINITY);
        if moved < FAR_RESEED_M {
            return Ok(());
        }
        let roads = RoadClear::gather(surface, hamlets, focus, FAR_TREE_RADIUS_M);
        let sowing = FarSowing {
            seed: self.seed,
            surface: Arc::clone(surface),
            ponds: Arc::clone(ponds),
            inner_m: 0.0,
            outer_m: FAR_TREE_RADIUS_M,
            spacing_m: FAR_TREE_SPACING_M,
            roads,
        };
        self.far_pending = Some(Pending {
            focus,
            resident_chunks: 0,
            job: std::thread::Builder::new()
                .name("scatter-far".into())
                .spawn(move || timed(|| sowing.sow(focus)))
                .expect("spawn the scatter far thread"),
        });
        Ok(())
    }

    /// Take a window the sow thread already grouped. Main thread only drops
    /// cells that walked off and stores the buckets.
    fn accept_near(
        &mut self,
        world: &mut World,
        buckets: HashMap<(usize, i32, i32), Vec<Sprig>>,
    ) -> EngineResult<()> {
        let mut dropped_trees = false;
        let mut dropped_tree_cells = Vec::new();
        for (vi, prop) in self.props.iter_mut().enumerate() {
            let stale: Vec<(i32, i32)> = prop
                .bins
                .keys()
                .copied()
                .filter(|&(bx, bz)| !buckets.contains_key(&(vi, bx, bz)))
                .collect();
            let is_tree = prop.class == PropClass::Tree;
            for key in stale {
                if let Some(id) = prop.bins.remove(&key) {
                    world.set_instances(id, &[])?;
                    prop.spare_bins.push(id);
                    if is_tree {
                        dropped_trees = true;
                        self.tree_shown.remove(&(vi, key.0, key.1));
                        dropped_tree_cells.push((vi, key.0, key.1));
                    }
                }
            }
        }
        for key in dropped_tree_cells {
            self.remove_tree_cell_colliders(world, key);
        }
        if dropped_trees {
            self.rebuild_tree_bins();
        }

        self.near_buckets = buckets;
        self.recount_near();
        Ok(())
    }

    /// Nearest pending uploads first, from where the player is now.
    ///
    /// A sow sorts once against the focus it started with. Walking for the
    /// next half-second would otherwise drain that stale FIFO while the wood
    /// you walked into stays at the back — or falls off the cap.
    fn reprioritize_uploads(&mut self, focus: GlobalXZ) {
        self.tree_queue = cap_nearest(
            self.tree_jobs_nearest(focus),
            |job| self.tree_remaining(job),
            MAX_TREE_QUEUE,
        );
        let mut other: Vec<(usize, i32, i32)> = self
            .near_buckets
            .keys()
            .copied()
            .filter(|&(vi, bx, bz)| {
                self.props[vi].class != PropClass::Tree
                    && !self.props[vi].bins.contains_key(&(bx, bz))
            })
            .collect();
        other.sort_by_key(|&(_, bx, bz)| bin_dist_key(bx, bz, focus));
        self.other_queue = cap_nearest(
            other,
            |&(vi, bx, bz)| {
                self.near_buckets
                    .get(&(vi, bx, bz))
                    .map(Vec::len)
                    .unwrap_or(0)
            },
            MAX_OTHER_QUEUE,
        );
        let mut far: Vec<(i32, i32)> = self.far_unwritten.iter().copied().collect();
        far.sort_by_key(|&draw| far_draw_dist_key(draw, focus));
        self.far_queue = cap_nearest(far, |_| 1, MAX_FAR_QUEUE);
    }

    fn drain_uploads(&mut self, world: &mut World, landed_near: bool) -> EngineResult<UploadDrain> {
        let started = Instant::now();
        let trees = self.drain_trees(world, MAX_TREES_PER_FRAME, started)?;
        let other = if upload_budget_left(started) {
            self.drain_other(world, MAX_OTHER_BINS_PER_FRAME, started)?
        } else {
            false
        };
        let far = if landed_near || !upload_budget_left(started) {
            false
        } else {
            let far_n = if trees || other {
                MAX_FAR_BINS_WHEN_NEAR_DIRTY
            } else {
                MAX_FAR_BINS_PER_FRAME
            };
            self.drain_far(world, far_n, started)?
        };
        Ok(UploadDrain { trees, other, far })
    }

    fn tree_jobs_nearest(&self, focus: GlobalXZ) -> Vec<TreeJob> {
        let mut jobs: Vec<TreeJob> = self
            .near_buckets
            .iter()
            .filter_map(|(&(vi, bx, bz), sprigs)| {
                if self.props[vi].class != PropClass::Tree {
                    return None;
                }
                let shown = self
                    .tree_shown
                    .get(&(vi, bx, bz))
                    .copied()
                    .unwrap_or(0)
                    .min(sprigs.len());
                if shown >= sprigs.len() {
                    return None;
                }
                Some(TreeJob { vi, bx, bz, shown })
            })
            .collect();
        jobs.sort_by_key(|job| bin_dist_key(job.bx, job.bz, focus));
        jobs
    }

    fn tree_remaining(&self, job: &TreeJob) -> usize {
        self.near_buckets
            .get(&(job.vi, job.bx, job.bz))
            .map(|s| s.len().saturating_sub(job.shown))
            .unwrap_or(0)
    }

    fn drain_trees(
        &mut self,
        world: &mut World,
        budget: usize,
        started: Instant,
    ) -> EngineResult<bool> {
        let origin = world.render_origin();
        let mut left = budget;
        let mut completed_cell = false;
        while left > 0 && upload_budget_left(started) {
            let Some(mut job) = self.tree_queue.pop_front() else {
                break;
            };
            let (places, capacity) = {
                let Some(sprigs) = self.near_buckets.get(&(job.vi, job.bx, job.bz)) else {
                    continue;
                };
                let total = sprigs.len();
                if job.shown >= total {
                    continue;
                }
                let take = (total - job.shown).min(left);
                job.shown += take;
                left -= take;
                (places_of(&sprigs[..job.shown], origin), total)
            };
            let prop = &mut self.props[job.vi];
            let id = match prop.bins.get(&(job.bx, job.bz)) {
                Some(id) => *id,
                None => {
                    let id = match prop.spare_bins.pop() {
                        Some(id) => id,
                        None => world.spawn_instanced_like(prop.prototype)?,
                    };
                    prop.bins.insert((job.bx, job.bz), id);
                    id
                }
            };
            world.reserve_instances(id, capacity)?;
            world.set_instances(id, &places)?;
            self.tree_shown.insert((job.vi, job.bx, job.bz), job.shown);
            self.sync_tree_cell_colliders(world, (job.vi, job.bx, job.bz), job.shown)?;
            let done = self
                .near_buckets
                .get(&(job.vi, job.bx, job.bz))
                .is_some_and(|s| job.shown >= s.len());
            if done {
                if self.near_tree_bins.insert((job.bx, job.bz)) {
                    completed_cell = true;
                    self.hide_far_bin(world, (job.bx, job.bz))?;
                }
            } else {
                self.tree_queue.push_front(job);
                break;
            }
        }
        if left != budget {
            self.recount_near();
        }
        Ok(completed_cell)
    }

    fn drain_other(
        &mut self,
        world: &mut World,
        budget: usize,
        started: Instant,
    ) -> EngineResult<bool> {
        let origin = world.render_origin();
        let mut wrote = false;
        for _ in 0..budget {
            if !upload_budget_left(started) {
                break;
            }
            let Some((vi, bx, bz)) = self.other_queue.pop_front() else {
                break;
            };
            let places = {
                let Some(sprigs) = self.near_buckets.get(&(vi, bx, bz)) else {
                    continue;
                };
                places_of(sprigs, origin)
            };
            let prop = &mut self.props[vi];
            let id = match prop.bins.get(&(bx, bz)) {
                Some(id) => *id,
                None => {
                    let id = match prop.spare_bins.pop() {
                        Some(id) => id,
                        None => world.spawn_instanced_like(prop.prototype)?,
                    };
                    prop.bins.insert((bx, bz), id);
                    id
                }
            };
            world.reserve_instances(id, places.len())?;
            world.set_instances(id, &places)?;
            wrote = true;
        }
        if wrote {
            self.recount_near();
        }
        Ok(wrote)
    }

    fn restand_applied(&mut self, world: &mut World) -> EngineResult<()> {
        let origin = world.render_origin();
        for (vi, prop) in self.props.iter().enumerate() {
            for (&(bx, bz), &id) in &prop.bins {
                let Some(sprigs) = self.near_buckets.get(&(vi, bx, bz)) else {
                    continue;
                };
                let shown = if prop.class == PropClass::Tree {
                    self.tree_shown
                        .get(&(vi, bx, bz))
                        .copied()
                        .unwrap_or(0)
                        .min(sprigs.len())
                } else {
                    sprigs.len()
                };
                world.set_instances(id, &places_of(&sprigs[..shown], origin))?;
            }
        }
        self.recount_near();
        Ok(())
    }

    fn recount_near(&mut self) {
        let mut n = 0;
        for (&(vi, bx, bz), sprigs) in &self.near_buckets {
            if self.props[vi].class == PropClass::Tree {
                n += self
                    .tree_shown
                    .get(&(vi, bx, bz))
                    .copied()
                    .unwrap_or(0)
                    .min(sprigs.len());
            } else if self.props[vi].bins.contains_key(&(bx, bz)) {
                n += sprigs.len();
            }
        }
        self.near_placed = n;
    }

    fn rebuild_tree_bins(&mut self) {
        self.near_tree_bins.clear();
        for prop in &self.props {
            if prop.class != PropClass::Tree {
                continue;
            }
            self.near_tree_bins.extend(prop.bins.keys().copied());
        }
    }

    fn remove_tree_cell_colliders(&mut self, world: &mut World, key: (usize, i32, i32)) {
        let Some(ids) = self.tree_colliders.remove(&key) else {
            return;
        };
        for id in ids {
            world.collision_mut().remove(id);
        }
    }

    fn sync_tree_cell_colliders(
        &mut self,
        world: &mut World,
        key: (usize, i32, i32),
        shown: usize,
    ) -> EngineResult<()> {
        let sprigs = self
            .near_buckets
            .get(&key)
            .unwrap_or_else(|| panic!("tree collider cell {key:?} has no sprigs"));
        if shown > sprigs.len() {
            panic!(
                "tree collider cell {key:?} shows {shown} of {} sprigs",
                sprigs.len()
            );
        }

        let existing = self.tree_colliders.get(&key).map(Vec::len).unwrap_or(0);
        if existing > shown {
            let removed = self
                .tree_colliders
                .get_mut(&key)
                .expect("tree collider cell disappeared")
                .split_off(shown);
            for id in removed {
                world.collision_mut().remove(id);
            }
        }
        if existing < shown {
            let added: Vec<StaticCollider> = sprigs[existing..shown]
                .iter()
                .map(|sprig| {
                    StaticCollider::new(
                        sprig.at.horizontal(),
                        0.0,
                        ColliderShape::Cylinder {
                            radius: TREE_TRUNK_RADIUS_M * sprig.scale,
                        },
                    )
                })
                .collect();
            let ids = self.tree_colliders.entry(key).or_default();
            for collider in added {
                ids.push(world.collision_mut().insert(COLLIDER_LAYER, collider)?);
            }
        }
        Ok(())
    }

    fn accept_far(&mut self, world: &mut World, sown: FarSown) -> EngineResult<()> {
        self.far_cells = sown.cells;
        self.far_draws = sown.draws;
        self.retire_stale_far_bins(world)?;
        self.queue_all_far();
        Ok(())
    }

    fn restand_far(&mut self, world: &mut World) -> EngineResult<()> {
        let draws: Vec<(i32, i32)> = self.far_bins.keys().copied().collect();
        for draw in draws {
            self.refresh_far_draw(world, draw)?;
        }
        Ok(())
    }

    fn retire_stale_far_bins(&mut self, world: &mut World) -> EngineResult<()> {
        let stale: Vec<(i32, i32)> = self
            .far_bins
            .keys()
            .copied()
            .filter(|draw| !self.draw_bin_has_live(*draw))
            .collect();
        for draw in stale {
            self.far_unwritten.remove(&draw);
            if let Some(id) = self.far_bins.remove(&draw) {
                world.set_instances(id, &[])?;
                self.far_spares.push(id);
            }
        }
        self.recount_far();
        Ok(())
    }

    fn queue_all_far(&mut self) {
        self.far_unwritten = self.far_draws.keys().copied().collect();
    }

    fn hide_far_bin(&mut self, world: &mut World, cell: (i32, i32)) -> EngineResult<()> {
        self.far_cells.remove(&cell);
        let draw = far_draw_bin(cell);
        if let Some(cells) = self.far_draws.get_mut(&draw) {
            cells.retain(|c| *c != cell);
            if cells.is_empty() {
                self.far_draws.remove(&draw);
            }
        }
        self.refresh_far_draw(world, draw)?;
        self.recount_far();
        Ok(())
    }

    fn refresh_far_draw(&mut self, world: &mut World, draw: (i32, i32)) -> EngineResult<()> {
        let places = self.places_for_draw_bin(draw, world.render_origin());
        if places.is_empty() {
            self.far_queue.retain(|b| *b != draw);
            self.far_unwritten.remove(&draw);
            if let Some(id) = self.far_bins.remove(&draw) {
                world.set_instances(id, &[])?;
                self.far_spares.push(id);
            }
            return Ok(());
        }
        if let Some(&id) = self.far_bins.get(&draw) {
            world.set_instances(id, &places)?;
            self.far_unwritten.remove(&draw);
        }
        Ok(())
    }

    fn drain_far(
        &mut self,
        world: &mut World,
        budget: usize,
        started: Instant,
    ) -> EngineResult<bool> {
        let mut wrote = false;
        for _ in 0..budget {
            if !upload_budget_left(started) {
                break;
            }
            let Some(draw) = self.far_queue.pop_front() else {
                break;
            };
            let places = self.places_for_draw_bin(draw, world.render_origin());
            if places.is_empty() {
                if let Some(id) = self.far_bins.remove(&draw) {
                    world.set_instances(id, &[])?;
                    self.far_spares.push(id);
                }
                continue;
            }
            let id = match self.far_bins.get(&draw) {
                Some(id) => *id,
                None => {
                    let id = match self.far_spares.pop() {
                        Some(id) => id,
                        None => world.spawn_instanced_like(self.far_entity)?,
                    };
                    self.far_bins.insert(draw, id);
                    id
                }
            };
            world.reserve_instances(id, FAR_DRAW_INSTANCE_RESERVE)?;
            world.set_instances(id, &places)?;
            self.far_unwritten.remove(&draw);
            wrote = true;
        }
        if wrote {
            self.recount_far();
        }
        Ok(wrote)
    }

    fn draw_bin_has_live(&self, draw: (i32, i32)) -> bool {
        self.far_draws.get(&draw).is_some_and(|cells| {
            cells.iter().any(|cell| {
                !self.near_tree_bins.contains(cell) && self.far_cells.contains_key(cell)
            })
        })
    }

    fn places_for_draw_bin(&self, draw: (i32, i32), origin: RenderOrigin) -> Vec<Place> {
        let Some(cells) = self.far_draws.get(&draw) else {
            return Vec::new();
        };
        let mut places = Vec::new();
        for cell in cells {
            if self.near_tree_bins.contains(cell) {
                continue;
            }
            let Some(sprigs) = self.far_cells.get(cell) else {
                continue;
            };
            places.extend(places_of(sprigs, origin));
        }
        places
    }

    fn recount_far(&mut self) {
        self.far_placed = self
            .far_draws
            .iter()
            .filter(|(draw, _)| self.far_bins.contains_key(*draw))
            .flat_map(|(_, cells)| cells.iter())
            .filter(|cell| !self.near_tree_bins.contains(cell))
            .filter_map(|cell| self.far_cells.get(cell).map(Vec::len))
            .sum();
    }
}

impl Sowing {
    /// Sow every class over the window around `focus`, already binned.
    fn sow(&self, focus: GlobalXZ) -> NearSown {
        let mut out = Vec::new();
        for class in PROP_CLASSES {
            let variants: Vec<usize> = self
                .variants
                .iter()
                .enumerate()
                .filter(|(_, (c, _))| *c == class)
                .map(|(i, _)| i)
                .collect();
            if variants.is_empty() {
                continue;
            }
            self.sow_class(class, &variants, focus, &mut out);
        }
        NearSown {
            buckets: bucket_near_sprigs(out),
        }
    }

    fn sow_class(
        &self,
        class: PropClass,
        variants: &[usize],
        focus: GlobalXZ,
        out: &mut Vec<Sprig>,
    ) {
        let (surface, ponds) = (self.surface.as_ref(), self.ponds.as_ref());
        // The water a prop stands beside includes the sub-atlas water, or a
        // pond would grow trees.
        let column_at = |p: GlobalXZ| -> SurfaceColumn {
            let mut column = surface.column(p);
            ponds.carve(p, &mut column);
            column
        };
        // Past this, nothing hydrological changes: `with_water` leaves the cover
        // exactly as it found it, and every dry class is comfortably inside its
        // band. So a bound is all it takes to skip the column entirely, which is
        // most of the lattice on most ground.
        let dry_beyond = -BANK_REACH_M;
        let needs_water = class == PropClass::Reed;
        let spacing = class.spacing_m();
        let radius = class.radius_m();
        let radius_sq = radius * radius;
        let (scale_lo, scale_hi) = class.scale_range();
        let salt = class_salt(class);

        let cx0 = ((focus.x - radius) / spacing).floor() as i64;
        let cx1 = ((focus.x + radius) / spacing).ceil() as i64;
        let cz0 = ((focus.z - radius) / spacing).floor() as i64;
        let cz1 = ((focus.z + radius) / spacing).ceil() as i64;

        for cz in cz0..=cz1 {
            for cx in cx0..=cx1 {
                let mut rng = CellRng::new(self.seed ^ salt, cx, cz);
                let jx = (cx as f64 + rng.unit() as f64) * spacing;
                let jz = (cz as f64 + rng.unit() as f64) * spacing;
                let dx = jx - focus.x;
                let dz = jz - focus.z;
                let dist_sq = dx * dx + dz * dz;
                if dist_sq > radius_sq {
                    continue;
                }
                if let Some(keep) = class.keep_fraction(dist_sq.sqrt()) {
                    if rng.unit() >= keep {
                        continue;
                    }
                }
                let p = GlobalXZ::at(jx, jz);
                if self.plots.blocks_prop(p) {
                    continue;
                }
                if class.skips_urban() && self.plots.urban_cover(p) {
                    continue;
                }
                if class.skips_road() && self.roads.blocks(p) {
                    continue;
                }

                let far_from_water = surface
                    .water_reach(p)
                    .max(ponds.water_reach(Vec2::new(jx as f32, jz as f32)))
                    <= dry_beyond;
                if far_from_water && needs_water {
                    continue;
                }

                // The drawn ground is the only ground: a prop on a chunk that
                // has not arrived yet would stand at a height nothing else agrees
                // with.
                let Some(ground) = self.ground.height_at(p) else {
                    continue;
                };
                let fall = self.fall_at(p);
                let mut cover = GroundCover::sample(
                    self.seed,
                    surface,
                    p,
                    ground,
                    fall,
                    canopy_noise(self.seed, p),
                );
                let wants_water = !far_from_water && class.follows_water();
                let early: Option<SurfaceColumn> = wants_water.then(|| column_at(p));
                if let Some(column) = early {
                    cover = cover.with_water(column.wetness());
                }
                let density = cover.of(class);
                if density <= 0.0 || rng.unit() >= density {
                    continue;
                }

                if !far_from_water {
                    let column: SurfaceColumn = early.unwrap_or_else(|| column_at(p));
                    if !class.stands_in(column.wetness()) {
                        continue;
                    }
                }

                let scale = rng.range(scale_lo, scale_hi);
                let yaw = 360.0 * rng.unit();
                let variant = self.pick_variant(variants, &cover, rng.unit());
                // Tint after placement draws so palette rolls never steal yaw/scale.
                let tint = prop_tint(class, &mut rng);
                let y = (ground - class.bed_in() * scale) as f64;
                let Ok(at) = p.with_height(y) else {
                    continue;
                };
                out.push(Sprig {
                    variant,
                    at,
                    yaw_deg: yaw,
                    scale,
                    tint,
                });
            }
        }
    }

    /// Choose a variant, weighted by how well each suits the spot.
    ///
    /// Weighted rather than best-fit: a stand of one mesh reads as wallpaper,
    /// and a real wood has the odd tree that does not belong.
    fn pick_variant(&self, variants: &[usize], cover: &GroundCover, roll: f32) -> usize {
        let fit = |i: usize| self.variants[i].1.fit(cover);
        let total: f32 = variants.iter().copied().map(fit).sum();
        let mut cursor = roll * total;
        for &i in variants {
            cursor -= fit(i);
            if cursor <= 0.0 {
                return i;
            }
        }
        variants[variants.len() - 1]
    }

    /// Which way the drawn ground falls, from the same grid the player walks on.
    fn fall_at(&self, p: GlobalXZ) -> Fall {
        const STEP: f64 = 2.0;
        let sample = |x: f64, z: f64| self.ground.height_at(GlobalXZ::at(x, z));
        let (Some(west), Some(east), Some(south), Some(north)) = (
            sample(p.x - STEP, p.z),
            sample(p.x + STEP, p.z),
            sample(p.x, p.z - STEP),
            sample(p.x, p.z + STEP),
        ) else {
            return Fall::default();
        };
        let gx = (east - west) / (2.0 * STEP as f32);
        let gz = (north - south) / (2.0 * STEP as f32);
        Fall::of(Vec3::new(-gx, 1.0, -gz).normalize_or_zero())
    }
}

impl FarSowing {
    fn sow(&self, focus: GlobalXZ) -> FarSown {
        bucket_far_sprigs(sow_far_forest(
            self.seed,
            self.surface.as_ref(),
            self.ponds.as_ref(),
            &self.roads,
            focus,
            self.inner_m,
            self.outer_m,
            self.spacing_m,
        ))
    }
}

/// Far-band pines around `focus`, on the same cover field the terrain tint uses.
///
/// `inner_m` is exclusive: a cell whose jittered point falls inside it is skipped.
/// The live far band uses `inner_m = 0` and hides proxies per cell once full-res
/// trees are standing, so a wood does not vanish and then pop in as one sheet.
fn sow_far_forest(
    seed: u64,
    surface: &ContinentalSurface,
    ponds: &PondField,
    roads: &RoadClear,
    focus: GlobalXZ,
    inner_m: f64,
    outer_m: f64,
    spacing_m: f64,
) -> Vec<Sprig> {
    let inner_sq = inner_m * inner_m;
    let outer_sq = outer_m * outer_m;
    let dry_beyond = -BANK_REACH_M;
    let (scale_lo, scale_hi) = PropClass::Tree.scale_range();
    let mut out = Vec::new();

    let cx0 = ((focus.x - outer_m) / spacing_m).floor() as i64;
    let cx1 = ((focus.x + outer_m) / spacing_m).ceil() as i64;
    let cz0 = ((focus.z - outer_m) / spacing_m).floor() as i64;
    let cz1 = ((focus.z + outer_m) / spacing_m).ceil() as i64;

    for cz in cz0..=cz1 {
        for cx in cx0..=cx1 {
            let mut rng = CellRng::new(seed ^ FAR_TREE_SALT, cx, cz);
            let jx = (cx as f64 + rng.unit() as f64) * spacing_m;
            let jz = (cz as f64 + rng.unit() as f64) * spacing_m;
            let dx = jx - focus.x;
            let dz = jz - focus.z;
            let dist_sq = dx * dx + dz * dz;
            if dist_sq <= inner_sq || dist_sq > outer_sq {
                continue;
            }
            let p = GlobalXZ::at(jx, jz);
            if roads.blocks(p) {
                continue;
            }

            let far_from_water = surface
                .water_reach(p)
                .max(ponds.water_reach(Vec2::new(jx as f32, jz as f32)))
                <= dry_beyond;

            let mut column = surface.column(p);
            ponds.carve(p, &mut column);
            let ground = column.ground();
            let fall = far_fall(surface, p);
            let mut cover =
                GroundCover::sample(seed, surface, p, ground, fall, canopy_noise(seed, p));
            if !far_from_water {
                cover = cover.with_water(column.wetness());
            }
            let density = cover.tree;
            if density <= 0.0 || rng.unit() >= density {
                continue;
            }
            if !PropClass::Tree.stands_in(column.wetness()) {
                continue;
            }

            let scale = rng.range(scale_lo, scale_hi);
            let yaw = 360.0 * rng.unit();
            let tint = prop_tint(PropClass::Tree, &mut rng);
            let y = (ground - MEDIUM.sink_m - PropClass::Tree.bed_in() * scale) as f64;
            let Ok(at) = p.with_height(y) else {
                continue;
            };
            out.push(Sprig {
                variant: 0,
                at,
                yaw_deg: yaw,
                scale,
                tint,
            });
        }
    }
    out
}

/// Slope on the medium sample step, from the same surface the chunks bake.
fn far_fall(surface: &ContinentalSurface, p: GlobalXZ) -> Fall {
    let step = MEDIUM.sample_m;
    let h = |x: f64, z: f64| surface.column(GlobalXZ::at(x, z)).ground();
    let gx = (h(p.x + step, p.z) - h(p.x - step, p.z)) / (2.0 * step as f32);
    let gz = (h(p.x, p.z + step) - h(p.x, p.z - step)) / (2.0 * step as f32);
    Fall::of(Vec3::new(-gx, 1.0, -gz).normalize_or_zero())
}

/// A pine the GPU can instance by the tens of thousands: trunk plus two crowns.
fn pine_proxy() -> EngineResult<Mesh> {
    let mut mesh = Mesh::new();
    let bark = Color::rgb(78, 54, 38);
    let needle = Color::rgb(46, 74, 42);
    mesh.add_box(Vec3::new(0.0, 3.2, 0.0), Vec3::new(0.7, 6.4, 0.7), bark)?;
    mesh.add_box(Vec3::new(0.0, 8.4, 0.0), Vec3::new(5.0, 4.2, 5.0), needle)?;
    mesh.add_box(Vec3::new(0.0, 11.4, 0.0), Vec3::new(3.2, 3.2, 3.2), needle)?;
    Ok(mesh)
}

/// Run `job`, reporting how long it took in milliseconds.
fn far_draw_bin(cell: (i32, i32)) -> (i32, i32) {
    (
        cell.0.div_euclid(FAR_DRAW_FOLD),
        cell.1.div_euclid(FAR_DRAW_FOLD),
    )
}

fn xz_bin(p: GlobalXZ) -> (i32, i32) {
    (
        (p.x / CHUNK_SPAN_M).floor() as i32,
        (p.z / CHUNK_SPAN_M).floor() as i32,
    )
}

fn bin_dist_key(bx: i32, bz: i32, focus: GlobalXZ) -> i64 {
    let cx = (f64::from(bx) + 0.5) * CHUNK_SPAN_M;
    let cz = (f64::from(bz) + 0.5) * CHUNK_SPAN_M;
    let dx = cx - focus.x;
    let dz = cz - focus.z;
    (dx * dx + dz * dz) as i64
}

fn far_draw_dist_key(draw: (i32, i32), focus: GlobalXZ) -> i64 {
    let span = CHUNK_SPAN_M * f64::from(FAR_DRAW_FOLD);
    let cx = (f64::from(draw.0) + 0.5) * span;
    let cz = (f64::from(draw.1) + 0.5) * span;
    let dx = cx - focus.x;
    let dz = cz - focus.z;
    (dx * dx + dz * dz) as i64
}

fn cap_nearest<T>(items: Vec<T>, weight: impl Fn(&T) -> usize, cap: usize) -> VecDeque<T> {
    let mut q = VecDeque::new();
    let mut n = 0;
    for item in items {
        let w = weight(&item);
        if w == 0 {
            continue;
        }
        if n >= cap {
            break;
        }
        n += w;
        q.push_back(item);
    }
    q
}

fn bucket_near_sprigs(
    sprigs: impl IntoIterator<Item = Sprig>,
) -> HashMap<(usize, i32, i32), Vec<Sprig>> {
    let mut buckets: HashMap<(usize, i32, i32), Vec<Sprig>> = HashMap::new();
    for sprig in sprigs {
        let bin = xz_bin(sprig.at.horizontal());
        buckets
            .entry((sprig.variant, bin.0, bin.1))
            .or_default()
            .push(sprig);
    }
    buckets
}

fn bucket_far_sprigs(sprigs: impl IntoIterator<Item = Sprig>) -> FarSown {
    let mut cells: HashMap<(i32, i32), Vec<Sprig>> = HashMap::new();
    for sprig in sprigs {
        cells
            .entry(xz_bin(sprig.at.horizontal()))
            .or_default()
            .push(sprig);
    }
    let mut draws: HashMap<(i32, i32), Vec<(i32, i32)>> = HashMap::new();
    for &cell in cells.keys() {
        draws.entry(far_draw_bin(cell)).or_default().push(cell);
    }
    FarSown { cells, draws }
}

fn upload_budget_left(started: Instant) -> bool {
    started.elapsed().as_secs_f32() * 1_000.0 < UPLOAD_BUDGET_MS
}

struct UploadDrain {
    trees: bool,
    other: bool,
    far: bool,
}

fn places_of(sprigs: &[Sprig], origin: RenderOrigin) -> Vec<Place> {
    let mut places = Vec::with_capacity(sprigs.len());
    for sprig in sprigs {
        let Ok(render) = sprig.at.to_render(origin) else {
            continue;
        };
        let at = render.vec3();
        places.push(
            Place::new(at.x, at.y, at.z)
                .with_yaw_deg(sprig.yaw_deg)
                .with_scale(sprig.scale)
                .with_tint(sprig.tint),
        );
    }
    places
}

/// Chance a sprig picks a loud outlier paint instead of a mild stand tint.
const PROP_OUTLIER_RATE: f32 = 0.012;

/// Per-instance paint for shared prop meshes. Mild greens/greys most of the time;
/// about one in eighty is a clear oddball so woods and scrub are not wallpaper.
fn prop_tint(class: PropClass, rng: &mut CellRng) -> Color {
    let outlier = rng.unit() < PROP_OUTLIER_RATE;
    let roll = rng.unit();
    if outlier {
        prop_outlier_tint(class, roll)
    } else {
        prop_mild_tint(class, roll)
    }
}

fn pick_palette(roll: f32, palettes: &[Color]) -> Color {
    let n = palettes.len();
    debug_assert!(n > 0);
    let i = ((roll.clamp(0.0, 0.999_999) * n as f32) as usize).min(n - 1);
    palettes[i]
}

fn prop_mild_tint(class: PropClass, roll: f32) -> Color {
    match class {
        PropClass::Tree | PropClass::Bush | PropClass::Reed | PropClass::Grass => pick_palette(
            roll,
            &[
                Color::WHITE,
                Color {
                    r: 0.92,
                    g: 1.0,
                    b: 0.82,
                    a: 1.0,
                }, // lime
                Color {
                    r: 0.78,
                    g: 0.92,
                    b: 0.88,
                    a: 1.0,
                }, // blue-green
                Color {
                    r: 0.88,
                    g: 0.90,
                    b: 0.72,
                    a: 1.0,
                }, // olive
                Color {
                    r: 0.72,
                    g: 0.88,
                    b: 0.70,
                    a: 1.0,
                }, // deep leaf
            ],
        ),
        PropClass::Rock | PropClass::Snag => pick_palette(
            roll,
            &[
                Color::WHITE,
                Color {
                    r: 1.0,
                    g: 0.94,
                    b: 0.86,
                    a: 1.0,
                }, // warm stone
                Color {
                    r: 0.86,
                    g: 0.90,
                    b: 0.96,
                    a: 1.0,
                }, // cool grey
                Color {
                    r: 0.90,
                    g: 0.86,
                    b: 0.80,
                    a: 1.0,
                }, // dusty
            ],
        ),
        PropClass::Mushroom | PropClass::Berry => pick_palette(
            roll,
            &[
                Color::WHITE,
                Color {
                    r: 1.0,
                    g: 0.90,
                    b: 0.82,
                    a: 1.0,
                },
                Color {
                    r: 0.90,
                    g: 0.86,
                    b: 1.0,
                    a: 1.0,
                },
                Color {
                    r: 1.0,
                    g: 0.82,
                    b: 0.88,
                    a: 1.0,
                },
            ],
        ),
    }
}

fn prop_outlier_tint(class: PropClass, roll: f32) -> Color {
    match class {
        PropClass::Tree | PropClass::Bush => pick_palette(
            roll,
            &[
                Color {
                    r: 1.0,
                    g: 0.55,
                    b: 0.18,
                    a: 1.0,
                }, // autumn blaze
                Color {
                    r: 0.95,
                    g: 0.82,
                    b: 0.22,
                    a: 1.0,
                }, // gold
                Color {
                    r: 0.72,
                    g: 0.35,
                    b: 0.85,
                    a: 1.0,
                }, // violet oddity
                Color {
                    r: 0.35,
                    g: 0.55,
                    b: 0.95,
                    a: 1.0,
                }, // blue spruce freak
                Color {
                    r: 0.95,
                    g: 0.28,
                    b: 0.32,
                    a: 1.0,
                }, // crimson
            ],
        ),
        PropClass::Grass | PropClass::Reed => pick_palette(
            roll,
            &[
                Color {
                    r: 1.0,
                    g: 0.85,
                    b: 0.25,
                    a: 1.0,
                },
                Color {
                    r: 0.55,
                    g: 0.35,
                    b: 0.90,
                    a: 1.0,
                },
                Color {
                    r: 0.95,
                    g: 0.45,
                    b: 0.20,
                    a: 1.0,
                },
            ],
        ),
        PropClass::Rock | PropClass::Snag => pick_palette(
            roll,
            &[
                Color {
                    r: 1.0,
                    g: 0.45,
                    b: 0.28,
                    a: 1.0,
                }, // rust
                Color {
                    r: 0.95,
                    g: 0.95,
                    b: 1.0,
                    a: 1.0,
                }, // chalk
                Color {
                    r: 0.35,
                    g: 0.32,
                    b: 0.30,
                    a: 1.0,
                }, // charcoal
            ],
        ),
        PropClass::Mushroom | PropClass::Berry => pick_palette(
            roll,
            &[
                Color {
                    r: 1.0,
                    g: 0.15,
                    b: 0.20,
                    a: 1.0,
                },
                Color {
                    r: 0.55,
                    g: 0.20,
                    b: 0.95,
                    a: 1.0,
                },
                Color {
                    r: 0.20,
                    g: 0.85,
                    b: 0.95,
                    a: 1.0,
                },
            ],
        ),
    }
}

fn timed<T>(job: impl FnOnce() -> T) -> (T, f32) {
    let started = std::time::Instant::now();
    let out = job();
    (out, started.elapsed().as_secs_f32() * 1_000.0)
}

fn class_salt(class: PropClass) -> u64 {
    match class {
        PropClass::Grass => 0x6772_6173_7300,
        PropClass::Rock => 0x726F_636B_7300,
        PropClass::Tree => 0x7472_6565_7300,
        PropClass::Reed => 0x7265_6564_7300,
        PropClass::Bush => 0x6275_7368_7300,
        PropClass::Snag => 0x534E_4147_7300,
        PropClass::Mushroom => 0x4D55_5348_7300,
        PropClass::Berry => 0x4245_5252_7300,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::ContinentAtlas;
    use crate::world::ponds::PondField;
    use glam::Vec2;

    fn world(seed: i32) -> (Arc<ContinentalSurface>, PondField) {
        let atlas = ContinentAtlas::generate(seed, 64);
        let surface = Arc::new(ContinentalSurface::new(&atlas).expect("surface"));
        let ponds = PondField::build(
            &surface,
            GlobalXZ::at(
                surface.bounds().metres() * 0.5,
                surface.bounds().metres() * 0.5,
            ),
        );
        (surface, ponds)
    }

    fn forested(surface: &ContinentalSurface) -> GlobalXZ {
        let metres = surface.bounds().metres();
        let sea = surface.sea_surface_z();
        let mut best = None;
        let mut best_tree = -1.0_f32;
        let probe = 48usize;
        let step = metres / probe as f64;
        for iz in 0..probe {
            for ix in 0..probe {
                let p = GlobalXZ::at((ix as f64 + 0.5) * step, (iz as f64 + 0.5) * step);
                let column = surface.column(p);
                if column.is_wet() || column.ground() < sea + 20.0 {
                    continue;
                }
                let cover = GroundCover::sample(
                    surface.world_seed() as u32 as u64,
                    surface,
                    p,
                    column.ground(),
                    Fall::default(),
                    canopy_noise(1, p),
                );
                if cover.tree > best_tree {
                    best_tree = cover.tree;
                    best = Some(p);
                }
            }
        }
        assert!(
            best_tree > 0.15,
            "no forested probe on this continent: {best_tree}"
        );
        best.expect("a forested probe")
    }

    #[test]
    fn a_cell_bins_by_the_walked_chunk() {
        let p = GlobalXZ::at(250.0, -50.0);
        assert_eq!(xz_bin(p), (1, -1));
        assert_eq!(xz_bin(GlobalXZ::at(0.0, 0.0)), (0, 0));
    }

    #[test]
    fn far_draw_bins_fold_five_walked_chunks() {
        assert_eq!(far_draw_bin((0, 0)), (0, 0));
        assert_eq!(far_draw_bin((4, 4)), (0, 0));
        assert_eq!(far_draw_bin((5, -1)), (1, -1));
        assert_eq!(far_draw_bin((-1, -6)), (-1, -2));
    }

    #[test]
    fn the_tree_queue_keeps_the_nearest_thousand() {
        let jobs: Vec<TreeJob> = (0..20)
            .map(|i| TreeJob {
                vi: 0,
                bx: i,
                bz: 0,
                shown: 0,
            })
            .collect();
        // 80 pending trees per cell → 13 cells = 1040, so the cap keeps 13 and drops the rest.
        let remaining = |job: &TreeJob| 80 - job.shown;
        let q = cap_nearest(jobs, remaining, 1_000);
        assert_eq!(q.len(), 13);
        assert_eq!(q.front().unwrap().bx, 0);
        assert_eq!(q.back().unwrap().bx, 12);
        let queued: usize = q.iter().map(remaining).sum();
        assert!(queued >= 1_000);
        assert!(queued < 1_000 + 80);
    }

    #[test]
    fn the_tree_queue_follows_the_player_not_the_sow() {
        let jobs = [
            TreeJob {
                vi: 0,
                bx: 0,
                bz: 0,
                shown: 0,
            },
            TreeJob {
                vi: 0,
                bx: 10,
                bz: 0,
                shown: 0,
            },
        ];
        let remaining = |_job: &TreeJob| 80;
        let mut at_sow = jobs.to_vec();
        at_sow.sort_by_key(|job| bin_dist_key(job.bx, job.bz, GlobalXZ::at(0.0, 0.0)));
        let q = cap_nearest(at_sow, remaining, 80);
        assert_eq!(q.front().unwrap().bx, 0);
        assert_eq!(q.len(), 1, "the farther cell must fall off the cap");

        let mut at_walk = jobs.to_vec();
        at_walk.sort_by_key(|job| bin_dist_key(job.bx, job.bz, GlobalXZ::at(2_000.0, 0.0)));
        let q = cap_nearest(at_walk, remaining, 80);
        assert_eq!(
            q.front().unwrap().bx,
            10,
            "walking toward a cell must promote it over the sow's FIFO"
        );
    }

    #[test]
    fn far_draw_cells_sort_nearest_to_the_player() {
        let focus = GlobalXZ::at(2_500.0, 0.0);
        let mut draws = [(0, 0), (2, 0), (5, 0)];
        draws.sort_by_key(|&draw| far_draw_dist_key(draw, focus));
        assert_eq!(draws[0], (2, 0));
    }

    #[test]
    fn the_sow_thread_bins_before_the_main_thread_sees_them() {
        let at = |x, z| GlobalXZ::at(x, z).with_height(0.0).expect("height");
        let sprigs = vec![
            Sprig {
                variant: 0,
                at: at(50.0, 50.0),
                yaw_deg: 0.0,
                scale: 1.0,
                tint: Color::WHITE,
            },
            Sprig {
                variant: 0,
                at: at(250.0, 50.0),
                yaw_deg: 0.0,
                scale: 1.0,
                tint: Color::WHITE,
            },
            Sprig {
                variant: 1,
                at: at(50.0, 50.0),
                yaw_deg: 0.0,
                scale: 1.0,
                tint: Color::WHITE,
            },
        ];
        let buckets = bucket_near_sprigs(sprigs);
        assert_eq!(buckets.len(), 3);
        assert_eq!(buckets[&(0, 0, 0)].len(), 1);
        assert_eq!(buckets[&(0, 1, 0)].len(), 1);
        assert_eq!(buckets[&(1, 0, 0)].len(), 1);
    }

    #[test]
    fn far_sows_group_walked_cells_under_draw_cells() {
        let at = |x, z| GlobalXZ::at(x, z).with_height(0.0).expect("height");
        let sown = bucket_far_sprigs([
            Sprig {
                variant: 0,
                at: at(50.0, 50.0),
                yaw_deg: 0.0,
                scale: 1.0,
                tint: Color::WHITE,
            },
            Sprig {
                variant: 0,
                at: at(250.0, 50.0),
                yaw_deg: 0.0,
                scale: 1.0,
                tint: Color::WHITE,
            },
            Sprig {
                variant: 0,
                at: at(1_050.0, 50.0),
                yaw_deg: 0.0,
                scale: 1.0,
                tint: Color::WHITE,
            },
        ]);
        assert_eq!(sown.cells.len(), 3);
        assert_eq!(sown.draws.len(), 2);
        assert_eq!(sown.draws[&(0, 0)].len(), 2);
        assert_eq!(sown.draws[&(1, 0)].len(), 1);
    }

    #[test]
    fn the_far_queue_keeps_the_nearest_draw_cells() {
        let focus = GlobalXZ::at(0.0, 0.0);
        let mut draws = vec![(0, 0), (3, 0), (1, 0), (8, 0)];
        draws.sort_by_key(|&draw| far_draw_dist_key(draw, focus));
        let q = cap_nearest(draws, |_| 1, 2);
        assert_eq!(q.len(), 2);
        assert_eq!(q.front().unwrap(), &(0, 0));
        assert_eq!(q.back().unwrap(), &(1, 0));
    }

    #[test]
    fn the_far_pine_is_a_handful_of_boxes() {
        let mesh = pine_proxy().expect("proxy").build();
        assert_eq!(mesh.triangle_count(), 36);
    }

    #[test]
    fn full_pines_thin_toward_the_walked_ring() {
        assert_eq!(TREE_RADIUS_M, NEAR.covers_m());
        assert_eq!(FAR_TREE_RADIUS_M, MEDIUM.reach_m());
        assert_eq!(PropClass::Tree.keep_fraction(0.0), Some(1.0));
        assert_eq!(
            PropClass::Tree.keep_fraction(TREE_DENSE_RADIUS_M),
            Some(1.0)
        );
        let rim = PropClass::Tree.keep_fraction(TREE_RADIUS_M).unwrap();
        assert!(rim < 0.5, "rim keep {rim} is still a 10 m lattice");
        assert!(rim > 0.3);
        assert!(PropClass::Grass.keep_fraction(100.0).is_none());
    }

    fn empty_roads() -> RoadClear {
        RoadClear { segs: Vec::new() }
    }

    #[test]
    fn near_and_far_tree_bands_do_not_share_a_cell() {
        let (surface, ponds) = world(20260809);
        let focus = forested(&surface);
        let inner = 180.0;
        let outer = 420.0;
        let roads = empty_roads();
        let far = sow_far_forest(7, &surface, &ponds, &roads, focus, inner, outer, 24.0);
        assert!(!far.is_empty(), "no far pines around a forested probe");
        for sprig in &far {
            let p = sprig.at.horizontal();
            let dist = ((p.x - focus.x).powi(2) + (p.z - focus.z).powi(2)).sqrt();
            assert!(
                dist > inner,
                "far pine at {dist:.1} m sits inside the full-mesh ring"
            );
            assert!(dist <= outer);
        }
    }

    #[test]
    fn far_pines_use_the_same_cover_field_as_the_ground_tint() {
        let (surface, ponds) = world(20260809);
        let focus = forested(&surface);
        let roads = empty_roads();
        let far = sow_far_forest(7, &surface, &ponds, &roads, focus, 180.0, 420.0, 24.0);
        assert!(!far.is_empty());
        for sprig in &far {
            let p = sprig.at.horizontal();
            let mut column = surface.column(p);
            ponds.carve(p, &mut column);
            let fall = far_fall(&surface, p);
            let mut cover =
                GroundCover::sample(7, &surface, p, column.ground(), fall, canopy_noise(7, p));
            let dry_beyond = -BANK_REACH_M;
            let far_from_water = surface
                .water_reach(p)
                .max(ponds.water_reach(Vec2::new(p.x as f32, p.z as f32)))
                <= dry_beyond;
            if !far_from_water {
                cover = cover.with_water(column.wetness());
            }
            assert!(
                cover.tree > 0.0,
                "a far pine stood where GroundCover.tree is 0"
            );
        }
    }

    #[test]
    fn far_forest_sowing_is_deterministic() {
        let (surface, ponds) = world(20260809);
        let focus = forested(&surface);
        let roads = empty_roads();
        let a = sow_far_forest(7, &surface, &ponds, &roads, focus, 180.0, 420.0, 24.0);
        let b = sow_far_forest(7, &surface, &ponds, &roads, focus, 180.0, 420.0, 24.0);
        let pos = |s: &[Sprig]| {
            s.iter()
                .map(|s| (s.at.x.to_bits(), s.at.y.to_bits(), s.at.z.to_bits()))
                .collect::<Vec<_>>()
        };
        assert_eq!(pos(&a), pos(&b));
        assert_ne!(
            pos(&a),
            pos(&sow_far_forest(
                8, &surface, &ponds, &roads, focus, 180.0, 420.0, 24.0
            )),
            "a different seed grew the same stand"
        );
    }

    #[test]
    fn flat_high_ground_holds_snow_and_lowland_does_not() {
        let fall = Fall::default();
        assert!(snow_cover(80.0, fall) < 0.01, "a meadow is already snow");
        let cap = snow_cover(1_800.0, fall);
        assert!(cap > 0.5, "a high plateau is not snow: {cap}");
        assert_eq!(thaw(cap), 0.0, "rooted cover still belongs on a snow cap");
    }

    #[test]
    fn steep_faces_shed_snow_that_a_plateau_keeps() {
        let high = 1_800.0;
        let flat = snow_cover(high, Fall::default());
        let cliff = snow_cover(
            high,
            Fall {
                steep: 0.85,
                downhill: Vec2::new(0.0, 1.0),
            },
        );
        assert!(
            cliff < flat * 0.35,
            "a cliff kept snow ({cliff}) the plateau holds ({flat})"
        );
    }

    #[test]
    fn bushes_and_grass_do_not_stand_on_snow() {
        let (surface, _) = world(20260809);
        let p = forested(&surface);
        let seed = surface.world_seed() as u32 as u64;
        let low = GroundCover::sample(seed, &surface, p, 80.0, Fall::default(), 0.0);
        let cap = GroundCover::sample(seed, &surface, p, 1_800.0, Fall::default(), 0.0);
        assert!(low.bush > 0.08, "open lowland grew no scrub: {}", low.bush);
        assert!(
            low.grass > 0.15,
            "open lowland grew no grass: {}",
            low.grass
        );
        assert!(
            cap.snow > 0.5,
            "1 800 m on the flat is not snow: {}",
            cap.snow
        );
        assert!(
            cap.bush < 0.02,
            "scrub still belongs on a snow cap: {}",
            cap.bush
        );
        assert!(
            cap.grass < 0.02,
            "grass tufts still belong on a snow cap: {}",
            cap.grass
        );
        assert!(cap.rock > low.rock, "high ground lost its stones");
    }

    #[test]
    fn alpine_scrub_still_belongs_below_the_snow() {
        let (surface, _) = world(20260809);
        let p = forested(&surface);
        // Treeline is complete by 780 m; snow on the flat starts around 1 050 m.
        let seed = surface.world_seed() as u32 as u64;
        let alpine = GroundCover::sample(seed, &surface, p, 820.0, Fall::default(), 0.0);
        assert!(
            alpine.snow < 0.05,
            "just above the treeline is already snow: {}",
            alpine.snow
        );
        assert!(
            alpine.bush > 0.04,
            "rocky alpine grew no low scrub: {}",
            alpine.bush
        );
        assert_eq!(alpine.tree, 0.0, "trees climbed past the treeline");
    }

    #[test]
    fn valley_bushes_do_not_taste_alpine() {
        assert!(PropTaste::of("bush_broad_lush").alpine < 0.2);
        assert!(PropTaste::of("bush_berries").alpine < 0.2);
        assert_eq!(PropTaste::of("bush_alpine_low").alpine, 1.0);
    }

    #[test]
    fn clearings_open_the_canopy_without_flattening_the_whole_forest() {
        let (surface, _) = world(20260809);
        let seed = surface.world_seed() as u32 as u64;
        let p = forested(&surface);
        let closed = GroundCover::sample(seed, &surface, p, 80.0, Fall::default(), 0.5);
        let mut open_sum = 0.0f32;
        let mut open_count = 0usize;
        let mut closed_sum = 0.0f32;
        let mut closed_count = 0usize;
        for i in 0..96usize {
            let x = p.x + ((i % 12) as f64 - 6.0) * 18.0;
            let z = p.z + ((i / 12) as f64 - 4.0) * 18.0;
            let at = GlobalXZ::at(x, z);
            let cover = GroundCover::sample(seed, &surface, at, 80.0, Fall::default(), 0.5);
            if cover.clearing > 0.45 {
                open_sum += cover.tree;
                open_count += 1;
            } else if cover.clearing < 0.05 {
                closed_sum += cover.tree;
                closed_count += 1;
            }
        }
        assert!(
            closed.tree > 0.35,
            "forested probe lost its timber: {}",
            closed.tree
        );
        if open_count > 0 && closed_count > 0 {
            let open_mean = open_sum / open_count as f32;
            let closed_mean = closed_sum / closed_count as f32;
            assert!(
                open_mean < closed_mean * 0.45,
                "clearings did not thin the stand ({open_mean} vs {closed_mean})"
            );
        }
        assert!(
            clearing_field(seed, p) == clearing_field(seed, p),
            "clearing field must be deterministic"
        );
    }

    #[test]
    fn species_bias_tracks_humidity_and_height() {
        let (surface, _) = world(20260809);
        let seed = surface.world_seed() as u32 as u64;
        let sea = surface.sea_surface_z();
        let p = forested(&surface);
        let humid_low = species_conifer_bias(seed, p, sea + 90.0, 0.92, 0.05, sea);
        let dry_high = species_conifer_bias(seed, p, sea + 480.0, 0.28, 0.55, sea);
        assert!(
            humid_low < dry_high,
            "humid lowland ({humid_low}) should lean broadleaf vs dry rise ({dry_high})"
        );
    }

    #[test]
    fn broadleaf_and_conifer_meshes_disagree_on_lineage() {
        assert!(PropTaste::of("pine_spruce_narrow").conifer > 0.9);
        assert!(PropTaste::of("oak_round_mature").conifer < 0.1);
    }

    #[test]
    fn plant_cover_stays_off_the_road_bed() {
        let roads = RoadClear {
            segs: vec![
                (Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0), 2.95),
                (Vec2::new(200.0, 0.0), Vec2::new(200.0, 40.0), 2.0),
            ],
        };
        assert!(roads.blocks(GlobalXZ::at(50.0, 0.0)));
        assert!(roads.blocks(GlobalXZ::at(50.0, 2.5)));
        assert!(!roads.blocks(GlobalXZ::at(50.0, 4.0)));
        assert!(roads.blocks(GlobalXZ::at(200.0, 20.0)));
        assert!(!roads.blocks(GlobalXZ::at(205.0, 20.0)));
        assert!(PropClass::Grass.skips_road());
        assert!(PropClass::Tree.skips_road());
        assert!(!PropClass::Rock.skips_road());
    }

    #[test]
    fn prop_mild_tree_tints_are_visibly_apart() {
        let mut colors = Vec::new();
        for i in 0..5 {
            colors.push(prop_mild_tint(PropClass::Tree, (i as f32 + 0.5) / 5.0));
        }
        let mut min_dist = f32::MAX;
        for i in 0..colors.len() {
            for j in (i + 1)..colors.len() {
                let a = colors[i];
                let b = colors[j];
                let d = (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs();
                min_dist = min_dist.min(d);
            }
        }
        assert!(
            min_dist > 0.12,
            "mild tree paints too close (min_dist={min_dist})"
        );
    }

    #[test]
    fn prop_outlier_trees_diverge_hard_from_white() {
        let blaze = prop_outlier_tint(PropClass::Tree, 0.05);
        let dist = (1.0 - blaze.r).abs() + (1.0 - blaze.g).abs() + (1.0 - blaze.b).abs();
        assert!(dist > 0.8, "outlier too mild vs white (dist={dist})");
    }

    #[test]
    fn prop_tint_outliers_are_rare() {
        let mut outliers = 0usize;
        const N: usize = 20_000;
        for i in 0..N {
            let mut rng = CellRng::new(0x7E57_u64, i as i64, (i / 17) as i64);
            let tint = prop_tint(PropClass::Tree, &mut rng);
            // Mild palettes stay near white; outliers sit far from (1,1,1).
            let dist = (1.0 - tint.r).abs() + (1.0 - tint.g).abs() + (1.0 - tint.b).abs();
            if dist > 0.7 {
                outliers += 1;
            }
        }
        let rate = outliers as f32 / N as f32;
        assert!(
            rate > 0.005 && rate < 0.03,
            "outlier rate {rate} not near {PROP_OUTLIER_RATE}"
        );
    }

    #[test]
    fn places_of_keeps_sprig_tint() {
        let origin = RenderOrigin::new(GlobalXZ::at(0.0, 0.0));
        let tint = Color {
            r: 1.0,
            g: 0.55,
            b: 0.18,
            a: 1.0,
        };
        let sprigs = [Sprig {
            variant: 0,
            at: GlobalXZ::at(10.0, 20.0).with_height(5.0).expect("height"),
            yaw_deg: 15.0,
            scale: 1.2,
            tint,
        }];
        let places = places_of(&sprigs, origin);
        assert_eq!(places.len(), 1);
        assert!((places[0].tint.r - tint.r).abs() < 1e-5);
        assert!((places[0].tint.g - tint.g).abs() < 1e-5);
        assert!((places[0].tint.b - tint.b).abs() < 1e-5);
    }
}
