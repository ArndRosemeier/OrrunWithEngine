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
//! Heights come from the same contact grid the player stands on, never from a
//! fresh surface probe, so a tuft cannot sink into ground that was drawn
//! slightly differently.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use engine::color::Color;
use engine::contact::ContactSnapshot;
use engine::error::EngineResult;
use engine::model::Model;
use engine::place::Place;
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use thiserror::Error;

use glam::{Vec2, Vec3};

use super::brooks::{BrookDetail, BrookField};
use super::look::SUN_DIR;
use super::rng::{hash3, unit01, value_noise, CellRng};
use super::surface::{lerp, smoothstep, ContinentalSurface, SurfaceColumn};
use super::world_stream::WorldStream;

/// Lattice spacing and window radius per class, in metres.
const GRASS_SPACING_M: f64 = 1.7;
const GRASS_RADIUS_M: f64 = 58.0;
const ROCK_SPACING_M: f64 = 16.0;
const ROCK_RADIUS_M: f64 = 230.0;
const TREE_SPACING_M: f64 = 10.0;
const TREE_RADIUS_M: f64 = 340.0;
/// Bank dressing: a narrow subject, so a short window and close spacing.
const REED_SPACING_M: f64 = 1.1;
/// Closer in than the other classes: a reed bed is dense, and a two-metre stem
/// a hundred metres off is a pixel that costs a lattice cell.
const REED_RADIUS_M: f64 = 70.0;
const BUSH_SPACING_M: f64 = 5.5;
const BUSH_RADIUS_M: f64 = 120.0;

/// Rebuild the window once the player is this far from where it was centred.
const RESEED_M: f64 = 10.0;

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
            // The pines are authored 9–17 m tall; anything above about 1.2
            // turns a wood into a redwood grove.
            Self::Tree => (0.7, 1.2),
            // The clumps are authored just under two metres, which is a reed.
            Self::Reed => (0.7, 1.15),
            Self::Bush => (0.7, 1.5),
        }
    }

    /// Whether being near water changes how much of this class belongs here,
    /// so hydrology has to be read before density rather than after it.
    ///
    /// It is the expensive query, and for grass and stones the cheap climate
    /// layers throw out most candidates first. Reeds and scrub have nothing to
    /// throw out — away from water they do not grow at all — and a wood follows
    /// a watercourse across ground that could not hold one otherwise.
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

        // Torn canopy edges: a kilometre-wide biome cell would otherwise fade
        // into open ground as one smooth gradient, with no clearings.
        let tree = smoothstep(0.20, 0.58, canopy * shade + canopy_noise * 0.30)
            * flat
            * (1.0 - alpine)
            * (1.0 - beach);

        let grass =
            (0.30 + 0.62 * humidity) * shade * flat * (1.0 - 0.75 * beach) * (1.0 - 0.35 * alpine);

        // Stones come loose where the ground is steep, high, or bare.
        let rock =
            (0.05 + 0.55 * smoothstep(0.20, 0.75, slope) + 0.35 * alpine) * (1.0 - 0.6 * canopy);

        // Scrub is the cover of open ground: it fills the gaps a wood leaves
        // and the ground a wood never took.
        let bush = (0.06 + 0.30 * humidity) * flat * (1.0 - alpine) * (1.0 - beach);

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
        }
    }

    /// Fold in the one hydrological fact the cheap layers cannot know: how far
    /// this spot is from water.
    ///
    /// `wetness_m` is the surface's signed field, so a brook bank and an ocean
    /// shore are the same measurement, and the whole sub-atlas layer dresses
    /// itself without knowing that it exists.
    pub fn with_water(mut self, wetness_m: f32) -> Self {
        self.bank = (1.0 + wetness_m / BANK_REACH_M).clamp(0.0, 1.0);
        self.moisture = (self.moisture + 0.45 * self.bank).clamp(0.0, 1.0);
        // Gallery woodland: a line of trees follows water across ground far too
        // dry to carry a wood of its own.
        self.tree = (self.tree + 0.40 * self.bank * (1.0 - self.alpine)).clamp(0.0, 1.0);
        self.openness = (1.0 - self.tree).clamp(0.0, 1.0);
        // Scrub grows anywhere flat and damp enough, and crowds the edge of the
        // water on top of that — added rather than scaled, or a bank running
        // through dry country would have nothing on it.
        let fringe = self.bank * self.footing * (1.0 - self.alpine);
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
                return Self::load(root);
            }
        }
        Err(ScatterError::NoAssets(
            tried.into_iter().next().unwrap_or_default(),
        ))
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

/// One prop variant that has been uploaded and can be placed.
#[derive(Debug)]
struct LiveProp {
    class: PropClass,
    taste: PropTaste,
    entity: EntityId,
    places: Vec<Place>,
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
/// twenty thousand lattice cells across five classes, and it is re-sown every
/// ten metres of travel and whenever ground streams in; on the main thread that
/// is a hitch every second or two, and at a spawn — when every chunk in the ring
/// arrives at once — it was a hitch every frame.
struct Sowing {
    seed: u64,
    /// Indexed like [`ScatterLayer::props`], so a sprig can name its variant.
    variants: Vec<(PropClass, PropTaste)>,
    surface: Arc<ContinentalSurface>,
    brooks: Arc<BrookField>,
    ground: ContactSnapshot,
}

/// A sow in flight, and the state of the world it speaks for.
struct Pending {
    focus: GlobalXZ,
    resident_chunks: usize,
    job: JoinHandle<(Vec<Sprig>, f32)>,
}

/// The live cover around the player.
pub struct ScatterLayer {
    props: Vec<LiveProp>,
    seed: u64,
    /// Where the standing cover was centred, and the residency it was sown
    /// against. Both are `None`/zero until the first sow lands.
    centre: Option<GlobalXZ>,
    resident_chunks: usize,
    /// The standing cover, in global metres.
    sown: Vec<Sprig>,
    placed: usize,
    pending: Option<Pending>,
    /// What the last sow took on its own thread. Worth watching: it is the
    /// budget that decides how far behind the cover can fall while moving.
    sow_ms: f32,
}

impl std::fmt::Debug for ScatterLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScatterLayer")
            .field("variants", &self.props.len())
            .field("placed", &self.placed)
            .field("sowing", &self.pending.is_some())
            .finish()
    }
}

impl ScatterLayer {
    /// Upload every catalogue mesh once; nothing is placed yet.
    pub fn install(
        world: &mut World,
        catalog: &ScatterCatalog,
        seed: i32,
    ) -> Result<Self, ScatterError> {
        let mut props = Vec::with_capacity(catalog.assets.len());
        for asset in &catalog.assets {
            let mut mesh = Model::load(&asset.path).map_err(|source| ScatterError::BadProp {
                path: asset.path.clone(),
                source,
            })?;
            if let Some(stone) = stone_colour(asset) {
                mesh.paint_all(stone);
            }
            let stem = asset
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            props.push(LiveProp {
                class: asset.class,
                taste: PropTaste::of(&stem),
                entity: world.spawn_instanced(mesh),
                places: Vec::new(),
            });
        }
        Ok(Self {
            props,
            seed: seed as u32 as u64,
            centre: None,
            resident_chunks: 0,
            sown: Vec::new(),
            placed: 0,
            pending: None,
            sow_ms: 0.0,
        })
    }

    /// Props standing in the world right now.
    pub fn placed_count(&self) -> usize {
        self.placed
    }

    /// What the last sow took, in milliseconds, on its own thread.
    pub fn sow_ms(&self) -> f32 {
        self.sow_ms
    }

    /// Take everything down (leaving the world).
    ///
    /// A sow already in flight is abandoned rather than waited for: whatever it
    /// was standing on belongs to a world that is being left.
    pub fn clear(&mut self, world: &mut World) -> EngineResult<()> {
        for prop in &mut self.props {
            prop.places.clear();
            world.set_instances(prop.entity, &[])?;
        }
        self.pending = None;
        self.sown.clear();
        self.centre = None;
        self.resident_chunks = 0;
        self.placed = 0;
        Ok(())
    }

    /// Keep the cover window centred on the player.
    ///
    /// Returns whether the cover standing in the world changed this frame. Never
    /// blocks, except on the very first sow of an entry, which happens while the
    /// loading screen is still up and would otherwise leave the player standing
    /// on bare ground.
    pub fn follow(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        surface: &Arc<ContinentalSurface>,
        brooks: &Arc<BrookField>,
        focus: GlobalXZ,
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
                self.stand(world)?;
                changed = true;
            } else {
                self.pending = Some(pending);
            }
        }
        // Render space moved under the cover: same sprigs, new origin.
        if rebased && !self.sown.is_empty() {
            self.stand(world)?;
            changed = true;
        }

        let moved = self
            .centre
            .map(|c| ((c.x - focus.x).powi(2) + (c.z - focus.z).powi(2)).sqrt())
            .unwrap_or(f64::INFINITY);
        // Newly streamed ground has no cover on it until the window is sown
        // again, so residency counts as movement — but only once the streamer has
        // stopped, or a spawn would chase every chunk of the ring in turn and sow
        // the whole window a hundred and sixty-nine times.
        let wanted = moved >= RESEED_M
            || (resident != self.resident_chunks && stream.walked_pending_count() == 0);
        if !wanted || self.pending.is_some() {
            return Ok(changed);
        }

        let sowing = Sowing {
            seed: self.seed,
            variants: self
                .props
                .iter()
                .map(|prop| (prop.class, prop.taste))
                .collect(),
            surface: Arc::clone(surface),
            brooks: Arc::clone(brooks),
            ground: stream.contact_snapshot(),
        };
        if self.centre.is_none() {
            // First cover of an entry: there is nothing standing to look at
            // while a thread works, and the loading screen is still up.
            let (sown, took_ms) = timed(|| sowing.sow(focus));
            self.sown = sown;
            self.sow_ms = took_ms;
            self.centre = Some(focus);
            self.resident_chunks = resident;
            self.stand(world)?;
            return Ok(true);
        }
        self.pending = Some(Pending {
            focus,
            resident_chunks: resident,
            job: std::thread::Builder::new()
                .name("scatter".into())
                .spawn(move || timed(|| sowing.sow(focus)))
                .expect("spawn the scatter thread"),
        });
        Ok(changed)
    }

    /// Hand the standing cover to the renderer in render space.
    fn stand(&mut self, world: &mut World) -> EngineResult<()> {
        let origin = world.render_origin();
        for prop in &mut self.props {
            prop.places.clear();
        }
        self.placed = 0;
        for sprig in &self.sown {
            let Ok(render) = sprig.at.to_render(origin) else {
                continue;
            };
            let at = render.vec3();
            self.props[sprig.variant].places.push(
                Place::new(at.x, at.y, at.z)
                    .with_yaw_deg(sprig.yaw_deg)
                    .with_scale(sprig.scale),
            );
            self.placed += 1;
        }
        for prop in &self.props {
            world.set_instances(prop.entity, &prop.places)?;
        }
        Ok(())
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
        let (surface, brooks) = (self.surface.as_ref(), self.brooks.as_ref());
        // The water a prop stands beside includes the sub-atlas water, or a
        // pond would grow trees.
        let column_at = |p: GlobalXZ| -> SurfaceColumn {
            let mut column = surface.column(p);
            brooks.carve(p, &mut column, BrookDetail::Channels);
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
                if dx * dx + dz * dz > radius_sq {
                    continue;
                }
                let p = GlobalXZ::at(jx, jz);

                let far_from_water = surface
                    .water_reach(p)
                    .max(brooks.water_reach(Vec2::new(jx as f32, jz as f32)))
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

/// The colour a stone is painted, or `None` for props that brought their own.
///
/// The rock generator bakes its look into albedo and normal maps, and this
/// pipeline has no textured prop material to sample them with, so a stone would
/// arrive plain white. The game names the stone instead, varying the shade per
/// variant so a scree slope is not one flat grey.
fn stone_colour(asset: &PropAsset) -> Option<Color> {
    if asset.class != PropClass::Rock {
        return None;
    }
    let name = asset.path.file_stem()?.to_string_lossy();
    let h = name
        .bytes()
        .fold(0x2545_F491_4F6C_DD1Du64, |acc, b| hash3(acc, b as i64, 7));
    let t = unit01(h);
    let warm = unit01(h >> 20);
    Some(Color::rgb(
        (86.0 + 54.0 * t + 16.0 * warm) as u8,
        (84.0 + 50.0 * t + 6.0 * warm) as u8,
        (80.0 + 44.0 * t) as u8,
    ))
}

/// Run `job`, reporting how long it took in milliseconds.
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
