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
use super::footprint::{self, HousePlot};
use super::look::{SNOW_FULL_M, SNOW_LINE_M, SNOW_SLOPE_END, SNOW_SLOPE_START, SUN_DIR};
use super::ponds::PondField;
use super::rng::{value_noise, CellRng};
use super::surface::{lerp, smoothstep, ContinentalSurface, SurfaceColumn};
use super::world_stream::{WorldStream, MEDIUM, NEAR};

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
/// Full-res trees to promote per walking frame. Whole 200 m bins were hundreds.
const MAX_TREES_PER_FRAME: usize = 40;
/// Grass / rock / bush bins to upload per walking frame.
const MAX_OTHER_BINS_PER_FRAME: usize = 4;
/// Horizon pine *draw* cells to upload per frame. Each is a kilometre, not 200 m.
const MAX_FAR_BINS_PER_FRAME: usize = 8;
/// How many walked chunks fold into one far draw call. 5 × 200 m = 1 km.
const FAR_DRAW_FOLD: i32 = 5;

/// Highest ground trees still grow on, and the band they thin out over.
const TREELINE_M: f32 = 780.0;
const TREELINE_FADE_M: f32 = 220.0;

/// Dry margin a prop keeps from standing water, in metres of the signed field.
const GRASS_DRY_MARGIN: f32 = 0.4;
const TREE_DRY_MARGIN: f32 = 2.0;

/// How far from a waterline the ground still counts as a bank.
const BANK_REACH_M: f32 = 22.0;

/// Stand size, and its own stream out of the world seed.
const CANOPY_NOISE_SCALE_M: f64 = 260.0;
const CANOPY_NOISE_SALT: u64 = 0x0F0_1E5;
/// Mottling inside a meadow: patches of dry sward vs lush, tens of metres.
const SOIL_PATCH_SCALE_M: f64 = 48.0;
const SOIL_PATCH_SALT: u64 = 0x5011_7A7C;
/// Which way a whole hillside leans, so a dry flank is a place, not speckles.
const SOIL_DRIFT_SCALE_M: f64 = 170.0;
const SOIL_DRIFT_SALT: u64 = 0xD81F_700D;

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
}

/// Every class, in the order they are scattered.
pub const PROP_CLASSES: [PropClass; 5] = [
    PropClass::Grass,
    PropClass::Rock,
    PropClass::Tree,
    PropClass::Reed,
    PropClass::Bush,
];

impl PropClass {
    fn spacing_m(self) -> f64 {
        match self {
            Self::Grass => GRASS_SPACING_M,
            Self::Rock => ROCK_SPACING_M,
            Self::Tree => TREE_SPACING_M,
            Self::Reed => REED_SPACING_M,
            Self::Bush => BUSH_SPACING_M,
        }
    }

    fn radius_m(self) -> f64 {
        match self {
            Self::Grass => GRASS_RADIUS_M,
            Self::Rock => ROCK_RADIUS_M,
            Self::Tree => TREE_RADIUS_M,
            Self::Reed => REED_RADIUS_M,
            Self::Bush => BUSH_RADIUS_M,
        }
    }

    /// Scale range, so a stand of one mesh does not read as copies.
    fn scale_range(self) -> (f32, f32) {
        match self {
            // The generated tufts are ankle height; a meadow needs knee height.
            Self::Grass => (1.0, 2.1),
            Self::Rock => (0.5, 1.6),
            // Measured: sapling 4.8 m, alpine 9.4 m, ponderosa 14.9 m, spruce
            // 16.8 m. Floor at 1.05 so timber is not shrunk below authored size;
            // 2.0 lets spruce reach ~34 m, which is tall timber, not redwood.
            Self::Tree => (1.05, 2.0),
            // The clumps are authored just under two metres, which is a reed.
            Self::Reed => (0.7, 1.15),
            // The tall shrub is authored ~2 m; the low ones half that. A wide
            // range is what makes a hillside of them, not a cloned hedge.
            Self::Bush => (0.75, 1.75),
        }
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
            Self::Grass | Self::Rock | Self::Reed => None,
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
    /// 0 = parched, 1 = soaking.
    pub moisture: f32,
    /// 0 = lowland, 1 = at the treeline.
    pub alpine: f32,
    /// 0 = deep in the stand, 1 = open ground.
    pub openness: f32,
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
        }
    }

    /// Cover from the cheap layers only: atlas climate plus the drawn slope.
    ///
    /// Deliberately does not touch hydrology — that costs an order of magnitude
    /// more, and most candidates are rejected before it would matter.
    pub fn sample(
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

        // Torn canopy edges: a kilometre-wide biome cell would otherwise fade
        // into open ground as one smooth gradient, with no clearings.
        let tree = smoothstep(0.20, 0.58, canopy * shade + canopy_noise * 0.30)
            * flat
            * (1.0 - alpine)
            * (1.0 - beach)
            * rooted;

        let grass = (0.30 + 0.62 * humidity)
            * shade
            * flat
            * (1.0 - 0.75 * beach)
            * (1.0 - 0.35 * alpine)
            * rooted;

        // Stones come loose where the ground is steep, high, or bare.
        let rock =
            (0.05 + 0.55 * smoothstep(0.20, 0.75, slope) + 0.35 * alpine) * (1.0 - 0.6 * canopy);

        // Scrub is the cover of open ground: it fills the gaps a wood leaves
        // and the ground a wood never took. A floor so low that dry country
        // reads as empty is what left whole hillsides as grass and nothing.
        // Alpine keeps a thinner stand of low shrubs; snow takes even those.
        let bush = (0.22 + 0.32 * humidity)
            * flat
            * (1.0 - 0.85 * beach)
            * lerp(1.0, 0.48, alpine)
            * lerp(0.40, 1.0, 1.0 - tree)
            * rooted;

        Self {
            grass: grass.clamp(0.0, 1.0),
            rock: rock.clamp(0.0, 1.0),
            tree: tree.clamp(0.0, 1.0),
            // No water has been asked about yet, and neither of these grows
            // anywhere else.
            reed: 0.0,
            bush: bush.clamp(0.0, 1.0),
            // Atlas humidity is a rainfall index, not soil moisture, and
            // standing timber is itself evidence of water. Taken raw it puts
            // straw tufts all over a forest floor that is plainly green.
            moisture: ((0.16 + 0.46 * humidity + 0.44 * canopy) * shade * (1.0 - 0.55 * beach))
                .clamp(0.0, 1.0),
            alpine,
            openness: (1.0 - tree).clamp(0.0, 1.0),
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
        self.tree =
            (self.tree + 0.40 * self.bank * (1.0 - self.alpine) * thaw(self.snow)).clamp(0.0, 1.0);
        self.openness = (1.0 - self.tree).clamp(0.0, 1.0);
        // Scrub grows anywhere flat and damp enough, and crowds the edge of the
        // water on top of that — added rather than scaled, or a bank running
        // through dry country would have nothing on it. Snow still wins: a
        // frozen shore is not a reed-bed's cousin in green shrubs.
        let fringe = self.bank * self.footing * (1.0 - self.alpine) * thaw(self.snow);
        self.bush = (self.bush + 0.34 * fringe).clamp(0.0, 1.0);
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
}

impl PropTaste {
    const NEUTRAL: Self = Self {
        dry: 0.5,
        alpine: 0.5,
        open: 0.5,
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
        }
        if stem.contains("spruce") || stem.contains("ponderosa") {
            taste.open = 0.15;
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
            + (self.open - cover.openness).abs() * 0.40;
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
    plots: Vec<HousePlot>,
}

/// Far-band pine stand-ins: same cover field as the tint, medium-tier height.
struct FarSowing {
    seed: u64,
    surface: Arc<ContinentalSurface>,
    ponds: Arc<PondField>,
    inner_m: f64,
    outer_m: f64,
    spacing_m: f64,
}

/// A sow in flight, and the state of the world it speaks for.
struct Pending {
    focus: GlobalXZ,
    resident_chunks: usize,
    job: JoinHandle<(Vec<Sprig>, f32)>,
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
    /// The standing cover, in global metres.
    sown: Vec<Sprig>,
    far_sown: Vec<Sprig>,
    far_centre: Option<GlobalXZ>,
    near_placed: usize,
    far_placed: usize,
    pending: Option<Pending>,
    far_pending: Option<Pending>,
    /// What the last near sow took on its own thread. Worth watching: it is the
    /// budget that decides how far behind the cover can fall while moving.
    sow_ms: f32,
    /// Last footprints a sow was started against.
    house_plots: Vec<HousePlot>,
    /// Sprigs of the standing near window, grouped for budgeted upload.
    near_buckets: HashMap<(usize, i32, i32), Vec<Sprig>>,
    /// Tree cells waiting to go full-res, nearest first, then FIFO.
    tree_queue: VecDeque<TreeJob>,
    /// Grass / rock / bush bins not yet spawned.
    other_queue: VecDeque<(usize, i32, i32)>,
    /// How many full-res trees each tree bin already shows.
    tree_shown: HashMap<(usize, i32, i32), usize>,
    /// Walked-tier cells that already have full-res trees, so far proxies hide.
    near_tree_bins: HashSet<(i32, i32)>,
    /// Far proxies in render space, one list per walked chunk.
    far_live: HashMap<(i32, i32), Vec<Place>>,
    /// GPU bin for each far cell. The prototype [`far_entity`] stays empty.
    far_bins: HashMap<(i32, i32), EntityId>,
    /// Far cells not yet on the GPU, nearest-last (FIFO after a sow).
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
            });
        }
        let far_mesh = pine_proxy().expect("far-band pine proxy");
        let far_entity = world.spawn_instanced(far_mesh);
        world
            .set_casts_shadow(far_entity, false)
            .expect("just spawned far pine prototype");
        Ok(Self {
            props,
            far_entity,
            seed: seed as u32 as u64,
            centre: None,
            resident_chunks: 0,
            sown: Vec::new(),
            far_sown: Vec::new(),
            far_centre: None,
            near_placed: 0,
            far_placed: 0,
            pending: None,
            far_pending: None,
            sow_ms: 0.0,
            house_plots: Vec::new(),
            near_buckets: HashMap::new(),
            tree_queue: VecDeque::new(),
            other_queue: VecDeque::new(),
            tree_shown: HashMap::new(),
            near_tree_bins: HashSet::new(),
            far_live: HashMap::new(),
            far_bins: HashMap::new(),
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
        self.far_queue.len()
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
            prop.bins.clear();
        }
        world.set_instances(self.far_entity, &[])?;
        for id in self.far_bins.values().copied() {
            world.despawn(id);
        }
        self.far_bins.clear();
        self.pending = None;
        self.far_pending = None;
        self.sown.clear();
        self.far_sown.clear();
        self.centre = None;
        self.far_centre = None;
        self.resident_chunks = 0;
        self.near_placed = 0;
        self.far_placed = 0;
        self.house_plots.clear();
        self.near_buckets.clear();
        self.tree_queue.clear();
        self.other_queue.clear();
        self.tree_shown.clear();
        self.near_tree_bins.clear();
        self.far_live.clear();
        self.far_queue.clear();
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
        house_plots: &[HousePlot],
        rebased: bool,
    ) -> EngineResult<bool> {
        let resident = stream.resident_count();
        let mut changed = false;

        if let Some(pending) = self.pending.take() {
            if pending.job.is_finished() {
                let (sown, took_ms) = pending.job.join().expect("scatter thread");
                self.sown = sown;
                self.sow_ms = took_ms;
                self.centre = Some(pending.focus);
                self.resident_chunks = pending.resident_chunks;
                self.queue_near(world, pending.focus);
                changed = true;
            } else {
                self.pending = Some(pending);
            }
        }
        if let Some(pending) = self.far_pending.take() {
            if pending.job.is_finished() {
                let (sown, _) = pending.job.join().expect("scatter far thread");
                self.far_sown = sown;
                self.far_centre = Some(pending.focus);
                self.apply_far_sown(world)?;
                changed = true;
            } else {
                self.far_pending = Some(pending);
            }
        }
        if rebased {
            if !self.near_buckets.is_empty() {
                self.restand_applied(world)?;
                changed = true;
            }
            if !self.far_sown.is_empty() {
                self.apply_far_sown(world)?;
                changed = true;
            }
        }

        if self.follow_near(stream, surface, ponds, focus, house_plots, resident)? {
            changed = true;
        }
        let trees = self.drain_trees(world, MAX_TREES_PER_FRAME)?;
        let other = self.drain_other(world, MAX_OTHER_BINS_PER_FRAME)?;
        let far = self.drain_far(world, MAX_FAR_BINS_PER_FRAME)?;
        self.follow_far(surface, ponds, focus)?;
        Ok(changed || trees || other || far)
    }

    fn follow_near(
        &mut self,
        stream: &WorldStream,
        surface: &Arc<ContinentalSurface>,
        ponds: &Arc<PondField>,
        focus: GlobalXZ,
        house_plots: &[HousePlot],
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
        let plots_changed = self.house_plots.as_slice() != house_plots;
        let wanted = moved >= RESEED_M
            || (resident != self.resident_chunks && stream.walked_pending_count() == 0)
            || plots_changed;
        if !wanted || self.pending.is_some() {
            return Ok(false);
        }

        self.house_plots = house_plots.to_vec();
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
            plots: self.house_plots.clone(),
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
        let sowing = FarSowing {
            seed: self.seed,
            surface: Arc::clone(surface),
            ponds: Arc::clone(ponds),
            inner_m: 0.0,
            outer_m: FAR_TREE_RADIUS_M,
            spacing_m: FAR_TREE_SPACING_M,
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

    /// Group the standing near window. Tree cells around the player go into a
    /// capped FIFO; farther full-res work is dropped and stays as LOD.
    fn queue_near(&mut self, world: &mut World, focus: GlobalXZ) {
        let mut buckets: HashMap<(usize, i32, i32), Vec<Sprig>> = HashMap::new();
        for sprig in &self.sown {
            let bin = xz_bin(sprig.at.horizontal());
            buckets
                .entry((sprig.variant, bin.0, bin.1))
                .or_default()
                .push(*sprig);
        }

        let mut dropped_trees = false;
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
                    world.despawn(id);
                    if is_tree {
                        dropped_trees = true;
                        self.tree_shown.remove(&(vi, key.0, key.1));
                    }
                }
            }
        }
        if dropped_trees {
            self.rebuild_tree_bins();
        }

        self.near_buckets = buckets;
        self.tree_queue = cap_tree_jobs(
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
        self.other_queue = other.into();
        self.recount_near();
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

    fn drain_trees(&mut self, world: &mut World, budget: usize) -> EngineResult<bool> {
        let origin = world.render_origin();
        let mut left = budget;
        let mut completed_cell = false;
        while left > 0 {
            let Some(mut job) = self.tree_queue.pop_front() else {
                break;
            };
            let places = {
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
                places_of(&sprigs[..job.shown], origin)
            };
            let prop = &mut self.props[job.vi];
            let id = match prop.bins.get(&(job.bx, job.bz)) {
                Some(id) => *id,
                None => {
                    let id = world.spawn_instanced_like(prop.prototype)?;
                    prop.bins.insert((job.bx, job.bz), id);
                    id
                }
            };
            world.set_instances(id, &places)?;
            self.tree_shown.insert((job.vi, job.bx, job.bz), job.shown);
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

    fn drain_other(&mut self, world: &mut World, budget: usize) -> EngineResult<bool> {
        let origin = world.render_origin();
        let mut wrote = false;
        for _ in 0..budget {
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
                    let id = world.spawn_instanced_like(prop.prototype)?;
                    prop.bins.insert((bx, bz), id);
                    id
                }
            };
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

    fn apply_far_sown(&mut self, world: &mut World) -> EngineResult<()> {
        self.rebuild_far_live(world.render_origin());
        self.retire_stale_far_bins(world);
        self.queue_all_far();
        Ok(())
    }

    fn rebuild_far_live(&mut self, origin: RenderOrigin) {
        let mut grouped: HashMap<(i32, i32), Vec<Place>> = HashMap::new();
        for sprig in &self.far_sown {
            let bin = xz_bin(sprig.at.horizontal());
            if self.near_tree_bins.contains(&bin) {
                continue;
            }
            let Ok(render) = sprig.at.to_render(origin) else {
                continue;
            };
            let at = render.vec3();
            grouped.entry(bin).or_default().push(
                Place::new(at.x, at.y, at.z)
                    .with_yaw_deg(sprig.yaw_deg)
                    .with_scale(sprig.scale),
            );
        }
        self.far_live = grouped;
    }

    fn retire_stale_far_bins(&mut self, world: &mut World) {
        let stale: Vec<(i32, i32)> = self
            .far_bins
            .keys()
            .copied()
            .filter(|draw| !self.draw_bin_has_live(*draw))
            .collect();
        for draw in stale {
            if let Some(id) = self.far_bins.remove(&draw) {
                world.despawn(id);
            }
        }
        self.recount_far();
    }

    fn queue_all_far(&mut self) {
        self.far_queue.clear();
        let mut seen = HashSet::new();
        for cell in self.far_live.keys() {
            let draw = far_draw_bin(*cell);
            if seen.insert(draw) {
                self.far_queue.push_back(draw);
            }
        }
    }

    fn hide_far_bin(&mut self, world: &mut World, cell: (i32, i32)) -> EngineResult<()> {
        self.far_live.remove(&cell);
        self.refresh_far_draw(world, far_draw_bin(cell))?;
        self.recount_far();
        Ok(())
    }

    fn refresh_far_draw(&mut self, world: &mut World, draw: (i32, i32)) -> EngineResult<()> {
        let places = self.places_for_draw_bin(draw);
        if places.is_empty() {
            self.far_queue.retain(|b| *b != draw);
            if let Some(id) = self.far_bins.remove(&draw) {
                world.despawn(id);
            }
            return Ok(());
        }
        if let Some(&id) = self.far_bins.get(&draw) {
            world.set_instances(id, &places)?;
        }
        Ok(())
    }

    fn drain_far(&mut self, world: &mut World, budget: usize) -> EngineResult<bool> {
        let mut wrote = false;
        for _ in 0..budget {
            let Some(draw) = self.far_queue.pop_front() else {
                break;
            };
            let places = self.places_for_draw_bin(draw);
            if places.is_empty() {
                if let Some(id) = self.far_bins.remove(&draw) {
                    world.despawn(id);
                }
                continue;
            }
            let id = match self.far_bins.get(&draw) {
                Some(id) => *id,
                None => {
                    let id = world.spawn_instanced_like(self.far_entity)?;
                    self.far_bins.insert(draw, id);
                    id
                }
            };
            world.set_instances(id, &places)?;
            wrote = true;
        }
        if wrote {
            self.recount_far();
        }
        Ok(wrote)
    }

    fn draw_bin_has_live(&self, draw: (i32, i32)) -> bool {
        self.far_live.keys().any(|cell| far_draw_bin(*cell) == draw)
    }

    fn places_for_draw_bin(&self, draw: (i32, i32)) -> Vec<Place> {
        let mut places = Vec::new();
        for (cell, list) in &self.far_live {
            if far_draw_bin(*cell) == draw {
                places.extend_from_slice(list);
            }
        }
        places
    }

    fn recount_far(&mut self) {
        self.far_placed = self
            .far_live
            .iter()
            .filter(|(cell, _)| self.far_bins.contains_key(&far_draw_bin(**cell)))
            .map(|(_, list)| list.len())
            .sum();
    }
}

impl Sowing {
    /// Sow every class over the window around `focus`.
    fn sow(&self, focus: GlobalXZ) -> Vec<Sprig> {
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
        out
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
                if footprint::blocks_prop(&self.plots, p) {
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
                let mut cover =
                    GroundCover::sample(surface, p, ground, fall, canopy_noise(self.seed, p));
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
                let y = (ground - class.bed_in() * scale) as f64;
                let Ok(at) = p.with_height(y) else {
                    continue;
                };
                out.push(Sprig {
                    variant,
                    at,
                    yaw_deg: yaw,
                    scale,
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
    fn sow(&self, focus: GlobalXZ) -> Vec<Sprig> {
        sow_far_forest(
            self.seed,
            self.surface.as_ref(),
            self.ponds.as_ref(),
            focus,
            self.inner_m,
            self.outer_m,
            self.spacing_m,
        )
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

            let far_from_water = surface
                .water_reach(p)
                .max(ponds.water_reach(Vec2::new(jx as f32, jz as f32)))
                <= dry_beyond;

            let mut column = surface.column(p);
            ponds.carve(p, &mut column);
            let ground = column.ground();
            let fall = far_fall(surface, p);
            let mut cover = GroundCover::sample(surface, p, ground, fall, canopy_noise(seed, p));
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
            let y = (ground - MEDIUM.sink_m - PropClass::Tree.bed_in() * scale) as f64;
            let Ok(at) = p.with_height(y) else {
                continue;
            };
            out.push(Sprig {
                variant: 0,
                at,
                yaw_deg: yaw,
                scale,
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

fn cap_tree_jobs(
    jobs: Vec<TreeJob>,
    remaining: impl Fn(&TreeJob) -> usize,
    cap: usize,
) -> VecDeque<TreeJob> {
    let mut q = VecDeque::new();
    let mut n = 0;
    for job in jobs {
        let left = remaining(&job);
        if left == 0 {
            continue;
        }
        if n >= cap {
            break;
        }
        n += left;
        q.push_back(job);
    }
    q
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
                .with_scale(sprig.scale),
        );
    }
    places
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
        let q = cap_tree_jobs(jobs, remaining, 1_000);
        assert_eq!(q.len(), 13);
        assert_eq!(q.front().unwrap().bx, 0);
        assert_eq!(q.back().unwrap().bx, 12);
        let queued: usize = q.iter().map(remaining).sum();
        assert!(queued >= 1_000);
        assert!(queued < 1_000 + 80);
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

    #[test]
    fn near_and_far_tree_bands_do_not_share_a_cell() {
        let (surface, ponds) = world(20260809);
        let focus = forested(&surface);
        let inner = 180.0;
        let outer = 420.0;
        let far = sow_far_forest(7, &surface, &ponds, focus, inner, outer, 24.0);
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
        let far = sow_far_forest(7, &surface, &ponds, focus, 180.0, 420.0, 24.0);
        assert!(!far.is_empty());
        for sprig in &far {
            let p = sprig.at.horizontal();
            let mut column = surface.column(p);
            ponds.carve(p, &mut column);
            let fall = far_fall(&surface, p);
            let mut cover =
                GroundCover::sample(&surface, p, column.ground(), fall, canopy_noise(7, p));
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
        let a = sow_far_forest(7, &surface, &ponds, focus, 180.0, 420.0, 24.0);
        let b = sow_far_forest(7, &surface, &ponds, focus, 180.0, 420.0, 24.0);
        let pos = |s: &[Sprig]| {
            s.iter()
                .map(|s| (s.at.x.to_bits(), s.at.y.to_bits(), s.at.z.to_bits()))
                .collect::<Vec<_>>()
        };
        assert_eq!(pos(&a), pos(&b));
        assert_ne!(
            pos(&a),
            pos(&sow_far_forest(
                8, &surface, &ponds, focus, 180.0, 420.0, 24.0
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
        let low = GroundCover::sample(&surface, p, 80.0, Fall::default(), 0.0);
        let cap = GroundCover::sample(&surface, p, 1_800.0, Fall::default(), 0.0);
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
        let alpine = GroundCover::sample(&surface, p, 820.0, Fall::default(), 0.0);
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
}
