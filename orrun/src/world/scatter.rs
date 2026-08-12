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

use engine::color::Color;
use engine::error::EngineResult;
use engine::model::Model;
use engine::place::Place;
use engine::space::{GlobalXZ, RenderOrigin};
use engine::world::{EntityId, World};
use thiserror::Error;

use super::surface::{ContinentalSurface, SurfaceColumn};
use super::world_stream::WorldStream;

/// Lattice spacing and window radius per class, in metres.
const GRASS_SPACING_M: f64 = 1.7;
const GRASS_RADIUS_M: f64 = 58.0;
const ROCK_SPACING_M: f64 = 16.0;
const ROCK_RADIUS_M: f64 = 230.0;
const TREE_SPACING_M: f64 = 10.0;
const TREE_RADIUS_M: f64 = 340.0;

/// Rebuild the window once the player is this far from where it was centred.
const RESEED_M: f64 = 10.0;

/// Highest ground trees still grow on, and the band they thin out over.
const TREELINE_M: f32 = 780.0;
const TREELINE_FADE_M: f32 = 220.0;

/// Dry margin a prop keeps from standing water, in metres of the signed field.
const GRASS_DRY_MARGIN: f32 = 0.4;
const TREE_DRY_MARGIN: f32 = 2.0;

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
}

impl PropClass {
    fn spacing_m(self) -> f64 {
        match self {
            Self::Grass => GRASS_SPACING_M,
            Self::Rock => ROCK_SPACING_M,
            Self::Tree => TREE_SPACING_M,
        }
    }

    fn radius_m(self) -> f64 {
        match self {
            Self::Grass => GRASS_RADIUS_M,
            Self::Rock => ROCK_RADIUS_M,
            Self::Tree => TREE_RADIUS_M,
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
        }
    }

    /// How far the prop is pushed into the ground, in metres of its own size.
    fn bed_in(self) -> f32 {
        match self {
            Self::Grass => 0.04,
            // Stones sit in a hollow rather than balancing on the surface.
            Self::Rock => 0.28,
            Self::Tree => 0.12,
        }
    }

    fn dry_margin(self) -> f32 {
        match self {
            Self::Grass => GRASS_DRY_MARGIN,
            Self::Rock => 0.0,
            Self::Tree => TREE_DRY_MARGIN,
        }
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
    /// 0 = parched, 1 = soaking.
    pub moisture: f32,
    /// 0 = lowland, 1 = at the treeline.
    pub alpine: f32,
    /// 0 = deep in the stand, 1 = open ground.
    pub openness: f32,
}

impl GroundCover {
    fn of(self, class: PropClass) -> f32 {
        match class {
            PropClass::Grass => self.grass,
            PropClass::Rock => self.rock,
            PropClass::Tree => self.tree,
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
        slope: f32,
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

        let flat = (1.0 - smoothstep(0.18, 0.62, slope)).clamp(0.0, 1.0);
        let beach = 1.0 - smoothstep(0.0, 6.0, ground_m - sea);
        let alpine = smoothstep(TREELINE_M - TREELINE_FADE_M, TREELINE_M, ground_m);

        // Torn canopy edges: a kilometre-wide biome cell would otherwise fade
        // into open ground as one smooth gradient, with no clearings.
        let tree = smoothstep(0.20, 0.58, canopy + canopy_noise * 0.30)
            * flat
            * (1.0 - alpine)
            * (1.0 - beach);

        let grass = (0.30 + 0.62 * humidity) * flat * (1.0 - 0.75 * beach) * (1.0 - 0.35 * alpine);

        // Stones come loose where the ground is steep, high, or bare.
        let rock =
            (0.05 + 0.55 * smoothstep(0.20, 0.75, slope) + 0.35 * alpine) * (1.0 - 0.6 * canopy);

        Self {
            grass: grass.clamp(0.0, 1.0),
            rock: rock.clamp(0.0, 1.0),
            tree: tree.clamp(0.0, 1.0),
            // Atlas humidity is a rainfall index, not soil moisture, and
            // standing timber is itself evidence of water. Taken raw it puts
            // straw tufts all over a forest floor that is plainly green.
            moisture: ((0.16 + 0.46 * humidity + 0.44 * canopy) * (1.0 - 0.55 * beach))
                .clamp(0.0, 1.0),
            alpine,
            openness: (1.0 - tree).clamp(0.0, 1.0),
        }
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

/// The live cover around the player.
#[derive(Debug)]
pub struct ScatterLayer {
    props: Vec<LiveProp>,
    seed: u64,
    centre: Option<GlobalXZ>,
    resident_chunks: usize,
    placed: usize,
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
            placed: 0,
        })
    }

    /// Props standing in the world right now.
    pub fn placed_count(&self) -> usize {
        self.placed
    }

    /// Take everything down (leaving the world).
    pub fn clear(&mut self, world: &mut World) -> EngineResult<()> {
        for prop in &mut self.props {
            prop.places.clear();
            world.set_instances(prop.entity, &[])?;
        }
        self.centre = None;
        self.placed = 0;
        Ok(())
    }

    /// Keep the cover window centred on the player.
    ///
    /// Returns whether it was rebuilt this frame.
    pub fn follow(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        surface: &ContinentalSurface,
        focus: GlobalXZ,
        rebased: bool,
    ) -> EngineResult<bool> {
        let resident = stream.resident_count();
        let moved = self
            .centre
            .map(|c| ((c.x - focus.x).powi(2) + (c.z - focus.z).powi(2)).sqrt())
            .unwrap_or(f64::INFINITY);
        // Newly streamed ground has no props on it until the window is rebuilt,
        // so residency counts as movement.
        if !rebased && moved < RESEED_M && resident == self.resident_chunks {
            return Ok(false);
        }

        self.centre = Some(focus);
        self.resident_chunks = resident;
        let origin = world.render_origin();
        for prop in &mut self.props {
            prop.places.clear();
        }
        self.placed = 0;

        for class in [PropClass::Grass, PropClass::Rock, PropClass::Tree] {
            let variants: Vec<usize> = self
                .props
                .iter()
                .enumerate()
                .filter(|(_, p)| p.class == class)
                .map(|(i, _)| i)
                .collect();
            if variants.is_empty() {
                continue;
            }
            self.scatter_class(class, &variants, stream, surface, focus, origin);
        }

        for prop in &self.props {
            world.set_instances(prop.entity, &prop.places)?;
        }
        Ok(true)
    }

    fn scatter_class(
        &mut self,
        class: PropClass,
        variants: &[usize],
        stream: &WorldStream,
        surface: &ContinentalSurface,
        focus: GlobalXZ,
        origin: RenderOrigin,
    ) {
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

                // The drawn ground is the only ground: a prop on a chunk that
                // has not arrived yet would stand at a height nothing else agrees
                // with.
                let Some(ground) = stream.contact_height(p) else {
                    continue;
                };
                let slope = slope_at(stream, p);
                let noise = value_noise(self.seed ^ 0x0F0_1E5, jx / 260.0, jz / 260.0);
                let cover = GroundCover::sample(surface, p, ground, slope, noise);
                let density = cover.of(class);
                if density <= 0.0 || rng.unit() >= density {
                    continue;
                }

                let column: SurfaceColumn = surface.column(p);
                if column.wetness() > -class.dry_margin() {
                    continue;
                }

                let scale = scale_lo + (scale_hi - scale_lo) * rng.unit();
                let yaw = 360.0 * rng.unit();
                let variant = self.pick_variant(variants, &cover, rng.unit());
                let y = (ground - class.bed_in() * scale) as f64;
                let Ok(global) = p.with_height(y) else {
                    continue;
                };
                let Ok(render) = global.to_render(origin) else {
                    continue;
                };
                let at = render.vec3();
                self.props[variant].places.push(
                    Place::new(at.x, at.y, at.z)
                        .with_yaw_deg(yaw)
                        .with_scale(scale),
                );
                self.placed += 1;
            }
        }
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

impl ScatterLayer {
    /// Choose a variant, weighted by how well each suits the spot.
    ///
    /// Weighted rather than best-fit: a stand of one mesh reads as wallpaper,
    /// and a real wood has the odd tree that does not belong.
    fn pick_variant(&self, variants: &[usize], cover: &GroundCover, roll: f32) -> usize {
        let mut total = 0.0;
        for &i in variants {
            total += self.props[i].taste.fit(cover);
        }
        let mut cursor = roll * total;
        for &i in variants {
            cursor -= self.props[i].taste.fit(cover);
            if cursor <= 0.0 {
                return i;
            }
        }
        variants[variants.len() - 1]
    }
}

/// Slope of the drawn ground, from the same grid the player walks on.
fn slope_at(stream: &WorldStream, p: GlobalXZ) -> f32 {
    const STEP: f64 = 2.0;
    let sample = |x: f64, z: f64| stream.contact_height(GlobalXZ::at(x, z));
    let (Some(west), Some(east), Some(south), Some(north)) = (
        sample(p.x - STEP, p.z),
        sample(p.x + STEP, p.z),
        sample(p.x, p.z - STEP),
        sample(p.x, p.z + STEP),
    ) else {
        return 0.0;
    };
    let gx = (east - west) / (2.0 * STEP as f32);
    let gz = (north - south) / (2.0 * STEP as f32);
    // Same measure the terrain shader blends rock with: 1 - n.y.
    1.0 - 1.0 / (1.0 + gx * gx + gz * gz).sqrt()
}

fn class_salt(class: PropClass) -> u64 {
    match class {
        PropClass::Grass => 0x6772_6173_7300,
        PropClass::Rock => 0x726F_636B_7300,
        PropClass::Tree => 0x7472_6565_7300,
    }
}

/// A stream of `[0, 1)` draws for one lattice cell.
///
/// Slicing a single hash into fixed bit windows ran out of bits: the variant
/// roll was taken from the top twenty, could never exceed 1/16, and so every
/// class placed nothing but its first mesh across the whole world.
struct CellRng(u64);

impl CellRng {
    fn new(seed: u64, x: i64, z: i64) -> Self {
        Self(hash3(seed, x, z))
    }

    fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut v = self.0;
        v ^= v >> 30;
        v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        v ^= v >> 27;
        v = v.wrapping_mul(0x94D0_49BB_1331_11EB);
        v ^= v >> 31;
        unit01(v)
    }
}

/// SplitMix64 over a lattice cell — cheap, and independent per class.
fn hash3(seed: u64, x: i64, z: i64) -> u64 {
    let mut v = seed
        ^ (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (z as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    v ^= v >> 30;
    v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    v ^= v >> 27;
    v = v.wrapping_mul(0x94D0_49BB_1331_11EB);
    v ^ (v >> 31)
}

/// Bits of a hash as a `[0, 1)` fraction.
#[inline]
fn unit01(h: u64) -> f32 {
    (h & 0xFF_FFFF) as f32 / 16_777_216.0
}

/// Smooth value noise on a unit lattice, for tearing biome edges.
fn value_noise(seed: u64, x: f64, z: f64) -> f32 {
    let x0 = x.floor();
    let z0 = z.floor();
    let tx = smooth((x - x0) as f32);
    let tz = smooth((z - z0) as f32);
    let ix = x0 as i64;
    let iz = z0 as i64;
    let corner = |dx: i64, dz: i64| unit01(hash3(seed, ix + dx, iz + dz)) * 2.0 - 1.0;
    let a = lerp(corner(0, 0), corner(1, 0), tx);
    let b = lerp(corner(0, 1), corner(1, 1), tx);
    lerp(a, b, tz)
}

#[inline]
fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_draw_in_a_cell_covers_the_whole_range() {
        // The placement reads six numbers per cell, and the last of them chose
        // the mesh. Drawn from a spent hash it never rose above 1/16, so every
        // tuft on the continent was the same tuft.
        const DRAWS: usize = 6;
        let mut lo = [1.0f32; DRAWS];
        let mut hi = [0.0f32; DRAWS];
        let mut sum = [0.0f64; DRAWS];
        let cells = 64 * 64;
        for z in 0..64i64 {
            for x in 0..64i64 {
                let mut rng = CellRng::new(0x51EED, x, z);
                for d in 0..DRAWS {
                    let v = rng.unit();
                    lo[d] = lo[d].min(v);
                    hi[d] = hi[d].max(v);
                    sum[d] += v as f64;
                }
            }
        }
        for d in 0..DRAWS {
            let mean = sum[d] / cells as f64;
            assert!(lo[d] < 0.02, "draw {d} never went low: {}", lo[d]);
            assert!(hi[d] > 0.98, "draw {d} never went high: {}", hi[d]);
            assert!(
                (mean - 0.5).abs() < 0.02,
                "draw {d} is biased: mean {mean:.3}"
            );
        }
    }
}
