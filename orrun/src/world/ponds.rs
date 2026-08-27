//! Sub-atlas ponds: closed hollows the atlas cannot hold.
//!
//! The atlas cannot hold any of this. Its cells are a kilometre and its smallest
//! lake is six square kilometres, so everything below that has to be generated —
//! and generated identically every time, from nothing but the world seed and a
//! position.
//!
//! Water is seated the same way atlas lakes are: a column is wet or it is not,
//! the bed is that column's ground, and the sheet is drawn by the same marching
//! squares. There is no second mesh. A pond has to cover a few four-metre
//! samples or those squares cannot see it.
//!
//! # A window, not a continent
//!
//! Ponds are a **moving window around the player**, not part of
//! [`ContinentalSurface`]. A continent of them would scale memory with area the
//! way the far-tier snapshot did, and an eighty-metre basin is meaningless at
//! the far tier's hundred-and-twenty-five-metre sampling.
//!
//! That has a price worth stating plainly: [`ContinentalSurface::column`] is no
//! longer the whole truth about the walked ground. It remains the whole truth
//! about *landform*, which is what keeps the visibility tiers agreeing.
//!
//! # Why a window can be trusted
//!
//! Seeds are placed in atlas cells out to [`SEED_RADIUS_M`], in absolute cell
//! order, and each walks downhill to a sink. Only ground within [`COVERS_M`] is
//! promised: a pond that touches that ground has its sink at most
//! [`POND_MAX_RADIUS_M`] further out, and the seed that found it started at most
//! a walk further than that. Two windows that both hold a seed walk it the same
//! way and produce the same pond. That is what lets two chunks baked either
//! side of a window rebuild agree on their shared seam.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;

use glam::Vec2;

use super::coords::{AtlasBounds, CHUNK_SAMPLE_M};
use super::rng::CellRng;
use super::surface::{
    lerp, smoothstep, ContinentalSurface, SurfaceColumn, WaterBody, WaterCarve, MIN_WATER_DEPTH,
};
use super::world_stream::MEDIUM;
use crate::atlas::CELL_METRES;
use engine::space::GlobalXZ;
use engine::EngineResult;

/// Rebuild the window once the player is this far from where it was centred.
pub const REBUILD_M: f64 = 800.0;

/// Ground the window promises to have every pond for.
///
/// The medium tier is the furthest that reads the field at all, and the player
/// may be past the centre when a chunk out at that reach is baked. Two rebuilds'
/// worth of travel, not one: a replacement is asked for at the first, and it has
/// to still be allowed to arrive late.
pub const COVERS_M: f64 = MEDIUM.reach_m() + 2.0 * REBUILD_M;

/// How far out seeds are placed.
///
/// A pond that touches the promised ground has its centre at most
/// [`POND_MAX_RADIUS_M`] past that edge, and the seed that discovers it may have
/// started a walk further uphill.
pub const SEED_RADIUS_M: f64 = COVERS_M + SINK_WALK_M as f64 + 2.0 * POND_MAX_RADIUS_M as f64;

/// Arm the landform is read with: wide enough that a tussock is not a dam.
const LANDFORM_ARM_M: f32 = 90.0;
/// How far a seed walks downhill looking for a closed hollow.
const SINK_WALK_M: f32 = 280.0;
const SINK_STEP_M: f32 = 24.0;
const SEEDS_PER_CELL: f32 = 2.8;
const MAX_SEEDS_PER_CELL: usize = 4;
const POND_SALT: u64 = 0x506F_6E64_0000;

/// Pond shape, all in metres.
const POND_MIN_DEPTH_M: f32 = 0.9;
const POND_BED_DEPTH_M: f32 = 2.4;
/// Floor cells may sit below the dam; past this the whole basin is a pane.
///
/// Ponds only cut, they never raise a bed, so this drop *is* the visual hang.
/// A closed hollow may be this deep; a hillside roofed at ridge height is
/// deeper, and is not a pond.
const POND_MAX_CELL_DROP_M: f32 = 12.0;
/// A bearing that falls this far before it has climbed out of the hollow is
/// open downhill, not a rim. Above grit, below a real slope over one probe.
const POND_OPEN_M: f32 = 2.5;
pub(super) const POND_MAX_RADIUS_M: f32 = 80.0;
const POND_PROBE_STEP_M: f32 = 8.0;
const POND_RAYS: usize = 24;
/// Water stands this far under the pass it would spill over.
const POND_FREEBOARD_M: f32 = 0.30;
/// Fewest 4 m cells a hollow must cover before it is a pond, not a puddle.
const POND_MIN_CELLS: usize = 8;
/// Dry ground pulled down toward a pond, so the land slopes in rather than
/// standing as a four-metre cliff. The contour interpolates against this.
const POND_BANK_M: f32 = 8.0;
const BANK_RISE_M: f32 = 1.6;

/// A spring of water needs this much ground above the sea to stand in.
const POND_MIN_HEIGHT_M: f32 = 12.0;
/// Drier than this, a closed dimple is not a pond.
const POND_MIN_HUMIDITY: f32 = 0.20;

/// A filled hollow: every 4 m cell whose uncarved ground sits under the sheet,
/// connected to the dam, on the same lattice the near terrain walks.
#[derive(Debug)]
pub struct Pond {
    centre: Vec2,
    sheet_z: f32,
    reach_m: f32,
    cells: HashSet<(i32, i32)>,
}

impl Pond {
    pub fn centre(&self) -> Vec2 {
        self.centre
    }

    pub fn sheet_z(&self) -> f32 {
        self.sheet_z
    }

    pub fn reach_m(&self) -> f32 {
        self.reach_m
    }

    pub fn contains(&self, p: Vec2) -> bool {
        self.cells.contains(&lattice(p))
    }

    /// Metres into the water; negative on the bank, non-finite if this pond
    /// does not speak for `p`.
    fn signed_distance(&self, p: Vec2) -> f32 {
        let (cx, cz) = lattice(p);
        let step = CHUNK_SAMPLE_M as f32;
        if self.cells.contains(&(cx, cz)) {
            let mut to_dry = step;
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                if self.cells.contains(&(cx + dx, cz + dz)) {
                    continue;
                }
                let edge = match (dx, dz) {
                    (1, 0) => (cx + 1) as f32 * step - p.x,
                    (-1, 0) => p.x - cx as f32 * step,
                    (0, 1) => (cz + 1) as f32 * step - p.y,
                    (0, -1) => p.y - cz as f32 * step,
                    _ => unreachable!("axis-aligned neighbour"),
                };
                to_dry = to_dry.min(edge.max(0.0));
            }
            return to_dry.max(0.05);
        }
        let r = (POND_BANK_M / step).ceil() as i32 + 1;
        let mut to_wet = f32::INFINITY;
        for dz in -r..=r {
            for dx in -r..=r {
                if self.cells.contains(&(cx + dx, cz + dz)) {
                    to_wet = to_wet.min(dist_to_cell(p, cx + dx, cz + dz, step));
                }
            }
        }
        if to_wet.is_finite() {
            -to_wet
        } else {
            f32::NEG_INFINITY
        }
    }
}

/// Every pond around one centre.
#[derive(Debug)]
pub struct PondField {
    centre: GlobalXZ,
    covers_m: f64,
    ponds: Vec<Pond>,
    basins: PondGrid,
}

impl PondField {
    /// A field with nothing in it, for before a world has been entered.
    pub fn empty(centre: GlobalXZ) -> Self {
        Self {
            centre,
            covers_m: 0.0,
            ponds: Vec::new(),
            basins: PondGrid::default(),
        }
    }

    /// Find every pond that can reach within [`COVERS_M`] of `centre`.
    pub fn build(surface: &ContinentalSurface, centre: GlobalXZ) -> Self {
        Finder::new(surface, centre, COVERS_M).run()
    }

    /// Build the exact procedural pond authority needed around one off-window query.
    ///
    /// Unlike the streaming window this scans only seeds that can influence
    /// `reach_m`, while retaining absolute seed order and identical basin carving.
    pub fn build_covering(surface: &ContinentalSurface, centre: GlobalXZ, reach_m: f64) -> Self {
        assert!(
            reach_m.is_finite() && reach_m >= 0.0,
            "pond authority reach must be finite and non-negative"
        );
        Finder::new(surface, centre, reach_m).run()
    }

    pub fn centre(&self) -> GlobalXZ {
        self.centre
    }

    pub fn ponds(&self) -> &[Pond] {
        &self.ponds
    }

    /// Whether this field still speaks for everything within `reach_m` of
    /// `focus`.
    pub fn covers(&self, focus: GlobalXZ, reach_m: f64) -> bool {
        let dx = focus.x - self.centre.x;
        let dz = focus.z - self.centre.z;
        (dx * dx + dz * dz).sqrt() + reach_m <= self.covers_m
    }

    /// Sink whatever pond stands on this column.
    ///
    /// Atlas hydrology wins outright: where an ocean, lake or river already
    /// stands, a pond has no business arguing about the sheet height.
    pub fn carve(&self, p: GlobalXZ, column: &mut SurfaceColumn) {
        if column.is_wet() {
            return;
        }
        let xz = Vec2::new(p.x as f32, p.z as f32);
        if let Some(carve) = self.basin_at(xz) {
            column.carve(carve);
        }
    }

    /// Governing pond near `p` with its signed distance, positive inside.
    pub fn nearest_pond(&self, p: Vec2) -> Option<(&Pond, f32)> {
        nearest_pond(&self.ponds, self.basins.at(p), p)
    }

    /// How near this point is to a pond, as the carve measures it: positive
    /// inside a basin, negative on the bank.
    ///
    /// The counterpart of [`ContinentalSurface::water_reach`], and used the same
    /// way — to settle most of a scatter lattice without carving a column.
    pub fn water_reach(&self, p: Vec2) -> f32 {
        self.nearest_pond(p)
            .map(|(_, sd)| sd)
            .unwrap_or(f32::NEG_INFINITY)
    }

    /// Ponds whose centre falls inside the box.
    ///
    /// Used to pick a chunk that actually has a pond on it, not to draw: the
    /// water is contoured from the columns like everything else.
    pub fn ponds_in(&self, min: Vec2, max: Vec2) -> usize {
        self.ponds
            .iter()
            .filter(|pond| {
                pond.centre.x >= min.x
                    && pond.centre.x < max.x
                    && pond.centre.y >= min.y
                    && pond.centre.y < max.y
            })
            .count()
    }

    fn basin_at(&self, p: Vec2) -> Option<WaterCarve> {
        let (pond, sd) = self.nearest_pond(p)?;
        if sd < -POND_BANK_M {
            return None;
        }
        let depth_m = if sd >= 0.0 {
            lerp(
                POND_MIN_DEPTH_M,
                POND_BED_DEPTH_M,
                smoothstep(0.0, CHUNK_SAMPLE_M as f32 * 3.0, sd),
            )
        } else {
            lerp(
                MIN_WATER_DEPTH,
                -BANK_RISE_M,
                smoothstep(0.0, 1.0, -sd / POND_BANK_M),
            )
        };
        Some(WaterCarve {
            sheet_z: pond.sheet_z,
            depth_m,
            margin_m: sd,
            body: WaterBody::Pond,
        })
    }
}

fn nearest_pond<'p>(ponds: &'p [Pond], which: &[usize], p: Vec2) -> Option<(&'p Pond, f32)> {
    let mut best: Option<(&Pond, f32)> = None;
    for &i in which {
        let pond = &ponds[i];
        let range = pond.reach_m + POND_BANK_M;
        if p.distance(pond.centre) > range {
            continue;
        }
        let sd = pond.signed_distance(p);
        if !sd.is_finite() {
            continue;
        }
        if best.map(|(_, b)| sd > b).unwrap_or(true) {
            best = Some((pond, sd));
        }
    }
    best
}

/// Which ponds are worth asking about, by bucket.
///
/// A window holds hundreds of ponds and a near chunk asks about two and a half
/// thousand columns; walking the whole list per column is the one place in this
/// layer where the cost is quadratic in how good the terrain is.
#[derive(Debug, Default)]
struct PondGrid {
    cells: std::collections::HashMap<(i32, i32), Vec<usize>>,
}

/// Comfortably wider than the widest pond plus its bank, so one bucket lookup
/// answers the query.
const POND_BUCKET_M: f32 = 256.0;

impl PondGrid {
    fn key(p: Vec2) -> (i32, i32) {
        (
            (p.x / POND_BUCKET_M).floor() as i32,
            (p.y / POND_BUCKET_M).floor() as i32,
        )
    }

    fn add(&mut self, i: usize, centre: Vec2, reach_m: f32) {
        let range = Vec2::splat(reach_m + POND_BANK_M);
        let (x0, z0) = Self::key(centre - range);
        let (x1, z1) = Self::key(centre + range);
        for z in z0..=z1 {
            for x in x0..=x1 {
                self.cells.entry((x, z)).or_default().push(i);
            }
        }
    }

    fn at(&self, p: Vec2) -> &[usize] {
        self.cells
            .get(&Self::key(p))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Every pond in the bucket around `p` and its neighbours — for asking what
    /// is nearby rather than what is underfoot.
    fn around(&self, p: Vec2) -> Vec<usize> {
        let (kx, kz) = Self::key(p);
        let mut out = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                if let Some(bucket) = self.cells.get(&(kx + dx, kz + dz)) {
                    out.extend_from_slice(bucket);
                }
            }
        }
        out
    }
}

/// The field in use, shared with whoever bakes ground out of it.
///
/// Two layers of sharing, and both earn their keep: the outer lock is what lets
/// a new window be swapped in while chunks are being baked, and the inner
/// handle is what lets a bake hold the field it started with for as long as it
/// needs, so a swap halfway through a chunk cannot leave half of it with ponds
/// and half without.
pub type SharedPonds = Arc<RwLock<Arc<PondField>>>;

/// The live window: one field in use, and possibly another being scanned.
///
/// Scanning a window costs tens of milliseconds on a busy continent, which is
/// nothing next to the ground the streamer bakes in the same time but too much
/// to spend between two frames. So it happens on its own thread, and the field
/// in hand stays in use until the new one lands.
pub struct PondWindow {
    surface: Arc<ContinentalSurface>,
    field: SharedPonds,
    pending: Option<(GlobalXZ, JoinHandle<PondField>)>,
}

impl PondWindow {
    pub fn new(surface: Arc<ContinentalSurface>) -> Self {
        Self {
            field: Arc::new(RwLock::new(Arc::new(PondField::empty(GlobalXZ::at(
                f64::MAX,
                f64::MAX,
            ))))),
            surface,
            pending: None,
        }
    }

    /// The handle a chunk builder holds.
    pub fn shared(&self) -> SharedPonds {
        Arc::clone(&self.field)
    }

    pub fn field(&self) -> Arc<PondField> {
        Arc::clone(&self.field.read().expect("pond window"))
    }

    /// Scan the window around `focus` and wait for it.
    ///
    /// Called behind the loading screen, and again if the player ever outruns
    /// the field: baking ground from a window that does not reach it would put
    /// a pond in one chunk and not in its neighbour.
    pub fn settle(&mut self, focus: GlobalXZ) -> EngineResult<()> {
        if let Some((_, handle)) = self.pending.take() {
            self.install(crate::worker::join_worker("pond window", handle)?);
        }
        if !self.field().covers(focus, MEDIUM.reach_m()) {
            let field = PondField::build(&self.surface, focus);
            self.install(field);
        }
        Ok(())
    }

    /// Whether the window in hand speaks for `focus` yet, scanning one that does
    /// if it does not.
    ///
    /// The entry path: the ground around a spawn cannot be baked, and the spawn
    /// itself cannot be chosen, until the water under it is known — but tens of
    /// milliseconds spent finding out is tens of milliseconds the window is not
    /// being drawn. So the caller asks, shows its loading screen, and asks
    /// again next frame.
    pub fn traced(&mut self, focus: GlobalXZ) -> EngineResult<bool> {
        if let Some((_, handle)) = self.pending.take() {
            if handle.is_finished() {
                self.install(crate::worker::join_worker("pond window", handle)?);
            } else {
                self.pending = Some((self.wanted(), handle));
                return Ok(false);
            }
        }
        if self.field().covers(focus, MEDIUM.reach_m()) {
            return Ok(true);
        }
        self.scan(focus);
        Ok(false)
    }

    /// Keep the window around the player, without ever blocking on it.
    ///
    /// Scanning a window costs as long as a hitch. The replacement is started
    /// here and installed when it finishes. If the player outruns the field in
    /// hand, the next scan is still a background job — never [`Self::settle`].
    /// Chunks baked in that gap may miss a pond at the medium ring until the
    /// new field lands; a freeze is worse.
    pub fn follow(&mut self, focus: GlobalXZ) -> EngineResult<()> {
        if let Some((_, handle)) = self.pending.take() {
            if handle.is_finished() {
                self.install(crate::worker::join_worker("pond window", handle)?);
            } else {
                self.pending = Some((self.wanted(), handle));
                return Ok(());
            }
        }
        let current = self.field();
        let uncovered = !current.covers(focus, MEDIUM.reach_m());
        if !uncovered {
            let dx = focus.x - current.centre().x;
            let dz = focus.z - current.centre().z;
            if (dx * dx + dz * dz).sqrt() < REBUILD_M {
                return Ok(());
            }
        }
        self.scan(focus);
        Ok(())
    }

    fn scan(&mut self, focus: GlobalXZ) {
        let surface = Arc::clone(&self.surface);
        let handle = std::thread::Builder::new()
            .name("ponds".into())
            .spawn(move || PondField::build(&surface, focus))
            .expect("pond window thread");
        self.pending = Some((focus, handle));
    }
    fn wanted(&self) -> GlobalXZ {
        self.pending
            .as_ref()
            .map(|(at, _)| *at)
            .unwrap_or_else(|| self.field().centre())
    }

    fn install(&mut self, field: PondField) {
        *self.field.write().expect("pond window") = Arc::new(field);
    }
}

impl std::fmt::Debug for PondWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let field = self.field();
        f.debug_struct("PondWindow")
            .field("centre", &field.centre())
            .field("ponds", &field.ponds().len())
            .field("scanning", &self.pending.is_some())
            .finish()
    }
}

struct Finder<'s> {
    surface: &'s ContinentalSurface,
    seed: u64,
    sea: f32,
    bounds: AtlasBounds,
    centre: GlobalXZ,
    covers_m: f64,
    ponds: Vec<Pond>,
    basins: PondGrid,
}

impl<'s> Finder<'s> {
    fn new(surface: &'s ContinentalSurface, centre: GlobalXZ, covers_m: f64) -> Self {
        Self {
            surface,
            seed: surface.world_seed() as u32 as u64,
            sea: surface.sea_surface_z(),
            bounds: surface.bounds(),
            centre,
            covers_m,
            ponds: Vec::new(),
            basins: PondGrid::default(),
        }
    }

    fn run(mut self) -> PondField {
        let cell = CELL_METRES as f64;
        let seed_radius_m = self.covers_m + SINK_WALK_M as f64 + 2.0 * POND_MAX_RADIUS_M as f64;
        let lo_x = ((self.centre.x - seed_radius_m) / cell).floor() as i64;
        let hi_x = ((self.centre.x + seed_radius_m) / cell).ceil() as i64;
        let lo_z = ((self.centre.z - seed_radius_m) / cell).floor() as i64;
        let hi_z = ((self.centre.z + seed_radius_m) / cell).ceil() as i64;

        // Absolute cell order, never window order: two windows that both hold
        // a seed have to walk it in the same sequence, or the basin they fill
        // first comes out differently.
        for az in lo_z..=hi_z {
            for ax in lo_x..=hi_x {
                if !self.bounds.contains_cell(ax as i32, az as i32) {
                    continue;
                }
                for seed in self.seeds_of(ax, az) {
                    if let Some(sink) = self.walk_to_sink(seed) {
                        let pass_z = self.lowest_pass(sink);
                        self.fill(sink, pass_z);
                    }
                }
            }
        }
        PondField {
            centre: self.centre,
            covers_m: self.covers_m,
            basins: self.basins,
            ponds: self.ponds,
        }
    }

    fn seeds_of(&self, ax: i64, az: i64) -> Vec<Vec2> {
        let fields = self.surface.fields();
        let cx = (ax as f32 + 0.5) * CELL_METRES;
        let cz = (az as f32 + 0.5) * CELL_METRES;
        if fields.sample_smooth(&fields.elevation_m, cx, cz) < self.sea + POND_MIN_HEIGHT_M {
            return Vec::new();
        }
        if fields.sample_smooth(&fields.humidity01, cx, cz) < POND_MIN_HUMIDITY {
            return Vec::new();
        }

        let mut rng = CellRng::new(self.seed ^ POND_SALT, ax, az);
        let count = rng.count(SEEDS_PER_CELL, MAX_SEEDS_PER_CELL);
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let p = Vec2::new(
                (ax as f32 + rng.unit()) * CELL_METRES,
                (az as f32 + rng.unit()) * CELL_METRES,
            );
            if self.in_atlas(p) {
                out.push(p);
            }
        }
        out
    }

    /// Steepest descent on the landform, until the ground pens the water in or
    /// it reaches atlas water.
    fn walk_to_sink(&self, start: Vec2) -> Option<Vec2> {
        let mut at = start;
        let steps = (SINK_WALK_M / SINK_STEP_M) as usize;
        for _ in 0..steps {
            if !self.in_atlas(at) {
                return None;
            }
            if self.base(at) < self.sea + POND_MIN_HEIGHT_M {
                return None;
            }
            if self
                .surface
                .water_reach(GlobalXZ::at(at.x as f64, at.y as f64))
                >= 0.0
            {
                return None;
            }
            if self.pond_owns(lattice(at)) {
                return None;
            }
            let fall = self.landform_fall(at);
            if fall.length_squared() < 1e-8 {
                return Some(at);
            }
            let next = at + fall * SINK_STEP_M;
            if self.base(next) >= self.base(at) - 0.02 {
                return Some(at);
            }
            at = next;
        }
        None
    }

    /// Downhill on the averaged landform, so a tussock is not a dam.
    fn landform_fall(&self, p: Vec2) -> Vec2 {
        let e = self.base(Vec2::new(p.x + LANDFORM_ARM_M, p.y));
        let w = self.base(Vec2::new(p.x - LANDFORM_ARM_M, p.y));
        let n = self.base(Vec2::new(p.x, p.y + LANDFORM_ARM_M));
        let s = self.base(Vec2::new(p.x, p.y - LANDFORM_ARM_M));
        Vec2::new(w - e, s - n).normalize_or_zero()
    }

    /// Structural ground, uncarved: filling the carved surface would have a
    /// pond chase its own basin.
    fn base(&self, p: Vec2) -> f32 {
        self.surface
            .base_ground(GlobalXZ::at(p.x as f64, p.y as f64))
    }

    fn in_atlas(&self, p: Vec2) -> bool {
        self.bounds
            .contains_point(GlobalXZ::at(p.x as f64, p.y as f64))
    }

    fn lowest_pass(&self, at: Vec2) -> f32 {
        let mut best = f32::INFINITY;
        for k in 0..POND_RAYS {
            let direction = ray(k);
            let pass_z = match self.ray_hit(at, direction) {
                RayHit::Crest(peak) => peak,
                RayHit::Open => self.base(at + direction * POND_PROBE_STEP_M),
                RayHit::Wall => f32::INFINITY,
            };
            if pass_z < best {
                best = pass_z;
            }
        }
        best
    }

    /// Fill the hollow around `at` if every bearing has climbed out of it.
    ///
    /// A slope that drops away is not a basin, however many spurs the other
    /// rays crest. Taking the min of those crests, ignoring the open downhill,
    /// hung a pane at ridge height over the mountainside.
    fn fill(&mut self, at: Vec2, pass_z: f32) {
        let floor = self.base(at);
        let mut crests = 0usize;
        let mut open = 0usize;
        for k in 0..POND_RAYS {
            match self.ray_hit(at, ray(k)) {
                RayHit::Open => open += 1,
                RayHit::Crest(_) => crests += 1,
                RayHit::Wall => {}
            }
        }
        if open != 0
            || crests == 0
            || pass_z < floor + POND_MIN_DEPTH_M
            || pass_z - floor > POND_MAX_CELL_DROP_M
        {
            return;
        }
        let sheet_z = pass_z - POND_FREEBOARD_M;
        self.flood(at, sheet_z);
    }

    fn flood(&mut self, at: Vec2, sheet_z: f32) {
        let start_cell = lattice(at);
        if self.pond_owns(start_cell) {
            return;
        }
        let start_p = cell_centre(start_cell.0, start_cell.1);
        let start_drop = sheet_z - self.base(start_p);
        if start_drop <= 0.0 || start_drop > POND_MAX_CELL_DROP_M {
            return;
        }

        let max_r2 = POND_MAX_RADIUS_M * POND_MAX_RADIUS_M;
        let mut cells = HashSet::new();
        let mut queue = VecDeque::new();
        cells.insert(start_cell);
        queue.push_back(start_cell);
        while let Some((cx, cz)) = queue.pop_front() {
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let next = (cx + dx, cz + dz);
                if cells.contains(&next) {
                    continue;
                }
                let p = cell_centre(next.0, next.1);
                let drop = sheet_z - self.base(p);
                let under = drop > 0.0;
                if at.distance_squared(p) > max_r2 {
                    if under && drop > 2.0 {
                        return;
                    }
                    continue;
                }
                if !under {
                    continue;
                }
                if self.pond_owns(next) {
                    return;
                }
                cells.insert(next);
                queue.push_back(next);
            }
        }
        if cells.len() < POND_MIN_CELLS {
            return;
        }
        if cells
            .iter()
            .any(|&(cx, cz)| sheet_z - self.base(cell_centre(cx, cz)) > POND_MAX_CELL_DROP_M)
        {
            return;
        }

        let mut reach_m: f32 = 0.0;
        for &(cx, cz) in &cells {
            reach_m = reach_m.max(at.distance(cell_centre(cx, cz)));
        }
        reach_m += CHUNK_SAMPLE_M as f32 * std::f32::consts::SQRT_2 * 0.5;

        self.basins.add(self.ponds.len(), at, reach_m);
        self.ponds.push(Pond {
            centre: at,
            sheet_z,
            reach_m,
            cells,
        });
    }

    /// What this bearing does to the hollow at `at`.
    ///
    /// A few centimetres of grit is not a rim, and a spur on a slope is not a
    /// wall: the land has to climb a pond's worth before it has left the
    /// hollow, and a fall of [`POND_OPEN_M`] before that climb is a way out.
    fn ray_hit(&self, at: Vec2, direction: Vec2) -> RayHit {
        let start = self.base(at);
        let mut peak = start;
        let mut risen = false;
        let mut r = POND_PROBE_STEP_M;
        while r <= POND_MAX_RADIUS_M {
            let h = self.base(at + direction * r);
            if !risen && h < start - POND_OPEN_M {
                return RayHit::Open;
            }
            if h > peak {
                peak = h;
            }
            if peak >= start + POND_MIN_DEPTH_M {
                risen = true;
            }
            if risen && peak - h >= POND_MIN_DEPTH_M * 0.5 {
                return RayHit::Crest(peak);
            }
            r += POND_PROBE_STEP_M;
        }
        if risen {
            RayHit::Wall
        } else {
            RayHit::Open
        }
    }

    fn pond_owns(&self, cell: (i32, i32)) -> bool {
        let p = cell_centre(cell.0, cell.1);
        self.basins
            .around(p)
            .iter()
            .any(|&i| self.ponds[i].cells.contains(&cell))
    }
}

enum RayHit {
    Open,
    Crest(f32),
    Wall,
}

fn lattice(p: Vec2) -> (i32, i32) {
    let step = CHUNK_SAMPLE_M as f32;
    ((p.x / step).floor() as i32, (p.y / step).floor() as i32)
}

fn cell_centre(cx: i32, cz: i32) -> Vec2 {
    let step = CHUNK_SAMPLE_M as f32;
    Vec2::new((cx as f32 + 0.5) * step, (cz as f32 + 0.5) * step)
}

fn dist_to_cell(p: Vec2, cx: i32, cz: i32, step: f32) -> f32 {
    let min = Vec2::new(cx as f32 * step, cz as f32 * step);
    let max = min + Vec2::splat(step);
    p.distance(p.clamp(min, max))
}

fn ray(k: usize) -> Vec2 {
    let a = std::f32::consts::TAU * k as f32 / POND_RAYS as f32;
    Vec2::new(a.cos(), a.sin())
}
