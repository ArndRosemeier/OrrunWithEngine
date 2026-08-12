//! Sub-atlas hydrology: brooks, and the ponds they fall into.
//!
//! The atlas cannot hold any of this. Its cells are a kilometre, its narrowest
//! river is twenty metres across and its smallest lake is six square
//! kilometres, so everything below that has to be generated — and generated
//! identically every time, from nothing but the world seed and a position.
//!
//! Water is seated the same way atlas rivers are: a column is wet or it is not,
//! the bed is that column's ground, and the sheet is drawn by the same marching
//! squares. There is no second mesh. A channel has to span a few four-metre
//! samples or those squares cannot see it, so a brook here is a small stream,
//! not a ditch.
//!
//! # A window, not a continent
//!
//! Brooks are a **moving window around the player**, not part of
//! [`ContinentalSurface`]. A continent of them would scale memory with area the
//! way the far-tier snapshot did, and a twelve-metre channel is meaningless at
//! the far tier's hundred-and-twenty-five-metre sampling, so paying for it on every
//! column of every horizon ring would be waste.
//!
//! That has a price worth stating plainly: [`ContinentalSurface::column`] is no
//! longer the whole truth about the walked ground. It remains the whole truth
//! about *landform*, which is what keeps the visibility tiers agreeing.
//!
//! # Why a window can be trusted
//!
//! Springs are seeded out to [`SEED_RADIUS_M`], but only ground within
//! [`COVERS_M`] is promised: a brook reaching a point started at most
//! [`MAX_BROOK_LEN_M`] away, and one that merges into it started at most that
//! much further out again. Traces run in absolute atlas-cell order, so two
//! windows that both hold a pair of springs trace them in the same sequence and
//! produce the same brooks. That is what lets two chunks baked either side of a
//! window rebuild agree on their shared seam.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;

use glam::Vec2;

use super::coords::{AtlasBounds, CHUNK_SAMPLE_M};
use super::ring_field::SegmentField;
use super::rng::{value_noise, CellRng};
use super::surface::{
    lerp, smoothstep, ContinentalSurface, SurfaceColumn, WaterBody, WaterCarve, MIN_WATER_DEPTH,
};
use super::world_stream::MEDIUM;
use crate::atlas::CELL_METRES;
use engine::space::GlobalXZ;

/// Longest a single trace runs before it soaks into the ground.
pub const MAX_BROOK_LEN_M: f64 = 1_800.0;
/// Rebuild the window once the player is this far from where it was centred.
pub const REBUILD_M: f64 = 800.0;

/// Ground the window promises to have every brook and pond for.
///
/// The medium tier is the furthest that reads the field at all, and the player
/// may be past the centre when a chunk out at that reach is baked. Two rebuilds'
/// worth of travel, not one: a replacement is asked for at the first, and it has
/// to still be allowed to arrive late. With one, the window fell out of reach on
/// the same step that asked for its successor, and there was nothing to do but
/// trace it on the spot — a quarter of a second, on the frame the player crossed
/// an invisible line.
pub const COVERS_M: f64 = MEDIUM.reach_m() + 2.0 * REBUILD_M;

/// How many merges deep two windows have to agree to produce the same brook.
///
/// A tributary snapping onto a trunk changes the tributary, not the trunk, so
/// one link would nearly do; a trunk that itself ended in a merge is the second
/// link. Past the third, the geometry required — three separate traces passing
/// within nine metres of each other, in sequence — does not arise.
const MERGE_CHAIN: f64 = 3.0;

/// How far out springs are seeded.
pub const SEED_RADIUS_M: f64 = COVERS_M + MERGE_CHAIN * MAX_BROOK_LEN_M;

const TRACE_STEP_M: f32 = 15.0;
/// Arm of the stencil the landform is read with: wide enough that a tussock is
/// not a dam and a hummock is not a valley.
const LANDFORM_ARM_M: f32 = 90.0;
/// Arm of the stencil the bearing of the nearest real water is read with.
const OUTLET_ARM_M: f32 = 60.0;
/// Arm the fall of the country is read with: half an atlas cell, so it answers
/// for the valley rather than for the ground the brook is standing on.
const REGIONAL_ARM_M: f32 = 500.0;
/// What the three scales of downhill are worth against each other.
const LOCAL_FALL: f32 = 1.0;
const REGIONAL_FALL: f32 = 0.9;
const OUTLET_PULL: f32 = 0.6;
const MAX_STEPS: usize = (MAX_BROOK_LEN_M / TRACE_STEP_M as f64) as usize;
/// How much of the previous heading survives a step, so a brook curves instead
/// of zigzagging down the fall line.
const INERTIA: f32 = 0.62;
/// Lateral wander, in degrees, and the distance it varies over.
///
/// The wavelength matters as much as the angle. Read over a distance much longer
/// than a step, the wander is a constant offset that the fall line simply
/// balances, and the course settles into a straight diagonal — which is what a
/// brook must not be. Read over four or five steps, it turns the course one way
/// and then the other, and the brook meanders.
const WOBBLE_DEG: f32 = 42.0;
const WOBBLE_SCALE_M: f32 = 65.0;
/// Most a course may bend in one step, whatever the ground says.
///
/// The kept course is smoothed twice before it is carved, which halves every
/// joint; this cap is what is left after that, not a drawing limit.
const MAX_TURN_DEG: f32 = 55.0;
/// Bearings tried either side of the wanted course before a rise counts as a
/// dam. The last of them is the turn cap, so the whole reachable arc is asked.
const FAN_DEG: [f32; 4] = [14.0, 28.0, 42.0, MAX_TURN_DEG];
/// Spilling over a low rim may turn this far, which is a change of course rather
/// than a bend in one. Short of a reversal, which is what ponds are for.
pub(super) const OVER_RIM_TURN_DEG: f32 = 92.0;
/// A step onto ground the same leg has already covered is no step at all.
const REVISIT_M: f32 = TRACE_STEP_M * 0.9;
/// How far a course may wind one way before it has plainly lost the fall line.
///
/// Turns of fifty degrees that never come within a step of old ground can still
/// carry a course clean round a hill: each bend is legal, and the sum of them is
/// a circle. A meander alternates, so its winding stays near zero however long
/// it runs; only an orbit accumulates. Past a horseshoe and a bit, the trace has
/// stopped following the ground and is chasing its own inertia, and the honest
/// thing is to let the water soak away where it lost its way.
pub(super) const MAX_WINDING_DEG: f32 = 130.0;
/// How wide a berth an outlet gives the rim of the basin it just left. Narrower
/// than the half step the spill itself begins beyond the rim.
const POND_SHUN_M: f32 = TRACE_STEP_M * 0.4;

/// A trace ends where it comes within this of a brook already traced.
///
/// Generous, and deliberately so: a tributary snaps onto the exact point it
/// found, so the only thing this width buys is how readily two courses running
/// down the same valley notice each other. Tight, and they run side by side as
/// parallel scratches instead of joining into a network.
const MERGE_SNAP_M: f32 = 22.0;
/// How far off its heading a trace will reach to make that join.
///
/// A tributary comes down to a trunk at a shallow angle; a point behind it, or
/// off to one side, is not a confluence, and snapping the mouth onto it would
/// bend the last chord round a corner no water turns.
const MERGE_CONE_DEG: f32 = 60.0;

/// Water surface below the uncarved ground at the centreline.
///
/// A lip, not a canal: the bed is cut under this, and the sheet is the water
/// the marching squares draw. Both live on the same 4 m columns as the land.
const FREEBOARD_M: f32 = 0.35;
/// Bed below the sheet along the middle of a channel.
const CHANNEL_DEPTH_M: f32 = 1.0;
/// How far above the sheet a dry bank may be pulled down, so the land slopes
/// into the water instead of standing as a four-metre cliff.
const BANK_M: f32 = 8.0;
const BANK_RISE_M: f32 = 1.6;
/// Channel half width at the spring and at the length cap.
///
/// The walked ground is a four-metre grid. A channel has to cover a few samples
/// or the contour that draws every other body of water cannot see it. That is
/// a small stream, twelve to sixteen metres across — below the atlas's
/// narrowest river, and the smallest this mesh can seat flush.
const HALF_WIDTH_SPRING_M: f32 = 6.0;
const HALF_WIDTH_MOUTH_M: f32 = 8.0;
/// Widest a channel's water gets. Two stretches of one brook closer together
/// than this are the same water twice, which is a knot rather than a meander.
pub const CHANNEL_WIDTH_M: f32 = 2.0 * HALF_WIDTH_MOUTH_M;
/// Furthest from a centreline that a channel still touches the ground.
pub const BROOK_REACH_M: f32 = HALF_WIDTH_MOUTH_M + BANK_M;

/// Drop forced on the sheet each step, so it can never rise downstream.
const MIN_DROP_M: f32 = 0.02;
/// How far the sheet may run below the local ground before what is ahead counts
/// as a dam rather than a slope.
const MAX_INCISION_M: f32 = 2.5;

/// Pond shape, all in metres.
const POND_MIN_DEPTH_M: f32 = 0.9;
const POND_BED_DEPTH_M: f32 = 2.4;
/// Floor cells may sit below the dam; past this the whole basin is a pane.
///
/// Ponds only cut, they never raise a bed, so this drop *is* the visual hang.
/// Skipping the deep cells instead of rejecting the basin punched holes in the
/// sheet and the contour draped down them. A closed hollow may be this deep;
/// a hillside roofed at ridge height is deeper, and is not a pond.
const POND_MAX_CELL_DROP_M: f32 = 12.0;
/// A bearing that falls this far before it has climbed out of the hollow is
/// open downhill, not a rim. Above grit, below a real slope over one probe.
const POND_OPEN_M: f32 = 2.5;
const POND_MAX_RADIUS_M: f32 = 80.0;
const POND_PROBE_STEP_M: f32 = 8.0;
const POND_RAYS: usize = 24;
/// Water stands this far under the pass it would spill over.
const POND_FREEBOARD_M: f32 = 0.30;
/// Fewest 4 m cells a hollow must cover before it is a pond, not a puddle.
const POND_MIN_CELLS: usize = 8;
/// Dry ground pulled down toward a pond, so the land slopes in rather than
/// standing as a four-metre cliff. The contour interpolates against this.
const POND_BANK_M: f32 = 8.0;

/// Springs a wet, hilly atlas cell is worth, and the ceiling per cell.
const SPRINGS_PER_CELL: f32 = 1.6;
const MAX_SPRINGS_PER_CELL: usize = 3;
/// A spring needs this much ground above the sea to have anywhere to run.
const SPRING_MIN_HEIGHT_M: f32 = 12.0;
const SPRING_MIN_SLOPE: f32 = 0.012;

/// Independent draw streams. Sharing one would tie a brook's course to how many
/// springs its cell happened to roll.
const SPRING_SALT: u64 = 0x5370_7269_6E67_0000;
const WOBBLE_SALT: u64 = 0x576F_6262_6C65_0000;

/// Why a trace stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminus {
    /// Reached an ocean, a lake, or an atlas river.
    Water,
    /// Joined a brook that had already been traced.
    Merge,
    /// Filled a hollow. An outlet carries on from where it spills.
    Pond,
    /// Left the generated atlas.
    OffMap,
    /// Ran out of length and soaks away; the channel tapers to nothing.
    Soaks,
}

/// One traced watercourse, from a spring to wherever it ended.
#[derive(Clone, Debug)]
pub struct Brook {
    points: Vec<Vec2>,
    /// Water surface at each point. Never rises downstream.
    sheet_z: Vec<f32>,
    half_width_m: Vec<f32>,
    terminus: Terminus,
}

impl Brook {
    pub fn points(&self) -> &[Vec2] {
        &self.points
    }

    pub fn sheets(&self) -> &[f32] {
        &self.sheet_z
    }

    pub fn terminus(&self) -> Terminus {
        self.terminus
    }

    pub fn length_m(&self) -> f32 {
        self.points.windows(2).map(|w| w[0].distance(w[1])).sum()
    }

    fn bounds(&self) -> (Vec2, Vec2) {
        let mut min = self.points[0];
        let mut max = self.points[0];
        for p in &self.points[1..] {
            min = min.min(*p);
            max = max.max(*p);
        }
        let pad = Vec2::splat(BROOK_REACH_M);
        (min - pad, max + pad)
    }
}

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

    /// The pond itself, and a thin ring around it an outlet must not graze.
    fn shuns(&self, at: Vec2) -> bool {
        if at.distance(self.centre) > self.reach_m + POND_SHUN_M + CHUNK_SAMPLE_M as f32 {
            return false;
        }
        let (cx, cz) = lattice(at);
        let r = ((POND_SHUN_M / CHUNK_SAMPLE_M as f32).ceil() as i32).max(1);
        for dz in -r..=r {
            for dx in -r..=r {
                if self.cells.contains(&(cx + dx, cz + dz)) {
                    return true;
                }
            }
        }
        false
    }
}

/// Where a channel runs, close to some point.
#[derive(Clone, Copy, Debug)]
pub struct BrookHit {
    pub dist_m: f32,
    pub half_width_m: f32,
    pub sheet_z: f32,
}

/// How much of the sub-atlas layer a tier can resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrookDetail {
    /// Four-metre ground: channels and basins both, drawn as contours.
    Channels,
    /// Twenty-five-metre ground. A twelve-metre channel is half a sample and
    /// gone; a pond forty metres across still reads, so only the basins are cut.
    Basins,
}

/// Every brook and pond around one centre.
#[derive(Debug)]
pub struct BrookField {
    centre: GlobalXZ,
    brooks: Vec<Brook>,
    /// First index into the shared point list for each brook, plus the total.
    starts: Vec<u32>,
    /// One grid over every centreline.
    index: Option<SegmentField>,
    ponds: Vec<Pond>,
    basins: PondGrid,
}

impl BrookField {
    /// A field with nothing in it, for before a world has been entered.
    pub fn empty(centre: GlobalXZ) -> Self {
        Self {
            centre,
            brooks: Vec::new(),
            starts: vec![0],
            index: None,
            ponds: Vec::new(),
            basins: PondGrid::default(),
        }
    }

    /// Trace every brook that can reach within [`COVERS_M`] of `centre`.
    pub fn build(surface: &ContinentalSurface, centre: GlobalXZ) -> Self {
        Tracer::new(surface, centre).run()
    }

    pub fn centre(&self) -> GlobalXZ {
        self.centre
    }

    pub fn brooks(&self) -> &[Brook] {
        &self.brooks
    }

    pub fn ponds(&self) -> &[Pond] {
        &self.ponds
    }

    /// Whether this field still speaks for everything within `reach_m` of
    /// `focus`.
    pub fn covers(&self, focus: GlobalXZ, reach_m: f64) -> bool {
        let dx = focus.x - self.centre.x;
        let dz = focus.z - self.centre.z;
        (dx * dx + dz * dz).sqrt() + reach_m <= COVERS_M
    }

    /// Sink whatever sub-atlas water runs through this column.
    ///
    /// The basin goes first: where a brook runs into its own pond the pond is
    /// the body, and the channel's sheet has already been raised to match it.
    pub fn carve(&self, p: GlobalXZ, column: &mut SurfaceColumn, detail: BrookDetail) {
        if column.is_wet() {
            return;
        }
        let xz = Vec2::new(p.x as f32, p.z as f32);
        if let Some(carve) = self.basin_at(xz) {
            column.carve(carve);
        }
        if detail == BrookDetail::Channels {
            if let Some(carve) = self.channel_at(xz) {
                column.carve(self.seat_channel(carve, column.ground()));
            }
        }
    }

    /// Nearest channel centreline within [`BROOK_REACH_M`].
    pub fn nearest_brook(&self, p: Vec2) -> Option<BrookHit> {
        let index = self.index.as_ref()?;
        let hit = index.nearest_within(p, BROOK_REACH_M)?;
        let (a, b) = index.segment_ends(hit.segment);
        let which = self.brook_of(a);
        let brook = &self.brooks[which];
        let first = self.starts[which] as usize;
        let (ia, ib) = (a - first, b - first);
        Some(BrookHit {
            dist_m: hit.distance,
            half_width_m: lerp(brook.half_width_m[ia], brook.half_width_m[ib], hit.t),
            sheet_z: lerp(brook.sheet_z[ia], brook.sheet_z[ib], hit.t),
        })
    }

    /// Governing pond near `p` with its signed distance, positive inside.
    pub fn nearest_pond(&self, p: Vec2) -> Option<(&Pond, f32)> {
        nearest_pond(&self.ponds, self.basins.at(p), p)
    }

    /// How near this point is to sub-atlas water, as the carve measures it:
    /// positive inside a channel or a basin, negative on the bank.
    ///
    /// The counterpart of [`ContinentalSurface::water_reach`], and used the same
    /// way — to settle most of a scatter lattice without carving a column.
    pub fn water_reach(&self, p: Vec2) -> f32 {
        let channel = self
            .nearest_brook(p)
            .map(|hit| hit.half_width_m - hit.dist_m)
            .unwrap_or(f32::NEG_INFINITY);
        let basin = self
            .nearest_pond(p)
            .map(|(_, sd)| sd)
            .unwrap_or(f32::NEG_INFINITY);
        channel.max(basin)
    }

    /// Channel steps whose midpoint falls inside the box.
    ///
    /// Used to pick a chunk that actually has a brook on it, not to draw: the
    /// water is contoured from the columns like everything else.
    pub fn channels_in(&self, min: Vec2, max: Vec2) -> usize {
        let mut n = 0;
        for brook in &self.brooks {
            let (lo, hi) = brook.bounds();
            if hi.x < min.x || lo.x > max.x || hi.y < min.y || lo.y > max.y {
                continue;
            }
            for i in 0..brook.points.len() - 1 {
                let mid = (brook.points[i] + brook.points[i + 1]) * 0.5;
                if mid.x >= min.x && mid.x < max.x && mid.y >= min.y && mid.y < max.y {
                    n += 1;
                }
            }
        }
        n
    }

    fn brook_of(&self, point: usize) -> usize {
        self.starts.partition_point(|s| (*s as usize) <= point) - 1
    }

    fn channel_at(&self, p: Vec2) -> Option<WaterCarve> {
        let hit = self.nearest_brook(p)?;
        let hw = hit.half_width_m;
        if hw <= 0.0 {
            return None;
        }
        let depth_m = if hit.dist_m <= hw {
            let t = hit.dist_m / hw;
            // Ends at exactly the depth the bank blend starts from, so the
            // groove and the ground beside it are one continuous profile.
            CHANNEL_DEPTH_M * (1.0 - t * t).max(MIN_WATER_DEPTH / CHANNEL_DEPTH_M)
        } else {
            let s = smoothstep(0.0, 1.0, (hit.dist_m - hw) / BANK_M);
            lerp(MIN_WATER_DEPTH, -BANK_RISE_M, s)
        };
        Some(WaterCarve {
            sheet_z: hit.sheet_z,
            depth_m,
            margin_m: hw - hit.dist_m,
            body: WaterBody::Brook,
            seat: true,
        })
    }

    /// Turn a centreline hit into a trough that meets the land.
    ///
    /// The sheet stays level across the channel so the water is a canal, not a
    /// film on the hillside. Uphill, the bank is cut down to the waterline.
    /// Downhill, a short berm raises a floor so the sheet is not a pane over
    /// the slope. Further than [`MAX_INCISION_M`] above the ground, the sheet
    /// itself drops — that is a hillside, not a canal wall.
    fn seat_channel(&self, mut carve: WaterCarve, original: f32) -> WaterCarve {
        carve.sheet_z = carve.sheet_z.min(original + MAX_INCISION_M);
        if carve.margin_m < 0.0 {
            let u = smoothstep(0.0, 1.0, (-carve.margin_m) / BANK_M);
            let rim = original.min(lerp(carve.sheet_z, original, u));
            carve.depth_m = carve.sheet_z - rim;
        } else {
            let floor = carve.sheet_z - carve.depth_m;
            let seated = floor.min(original + MAX_INCISION_M);
            carve.depth_m = carve.sheet_z - seated;
        }
        carve
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
            seat: false,
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
/// needs, so a swap halfway through a chunk cannot leave half of it with
/// brooks and half without.
pub type SharedBrooks = Arc<RwLock<Arc<BrookField>>>;

/// The live window: one field in use, and possibly another being traced.
///
/// Tracing a window costs a couple of hundred milliseconds on a busy continent,
/// which is nothing next to the ground the streamer bakes in the same time but
/// far too much to spend between two frames. So it happens on its own thread,
/// and the field in hand stays in use until the new one lands.
pub struct BrookWindow {
    surface: Arc<ContinentalSurface>,
    field: SharedBrooks,
    pending: Option<(GlobalXZ, JoinHandle<BrookField>)>,
}

impl BrookWindow {
    pub fn new(surface: Arc<ContinentalSurface>) -> Self {
        Self {
            field: Arc::new(RwLock::new(Arc::new(BrookField::empty(GlobalXZ::at(
                f64::MAX,
                f64::MAX,
            ))))),
            surface,
            pending: None,
        }
    }

    /// The handle a chunk builder holds.
    pub fn shared(&self) -> SharedBrooks {
        Arc::clone(&self.field)
    }

    pub fn field(&self) -> Arc<BrookField> {
        Arc::clone(&self.field.read().expect("brook window"))
    }

    /// Trace the window around `focus` and wait for it.
    ///
    /// Called behind the loading screen, and again if the player ever outruns
    /// the field: baking ground from a window that does not reach it would put
    /// a brook in one chunk and not in its neighbour.
    pub fn settle(&mut self, focus: GlobalXZ) {
        if let Some((_, handle)) = self.pending.take() {
            self.install(handle.join().expect("brook window thread"));
        }
        if self.field().covers(focus, MEDIUM.reach_m()) {
            return;
        }
        let field = BrookField::build(&self.surface, focus);
        self.install(field);
    }

    /// Whether the window in hand speaks for `focus` yet, tracing one that does
    /// if it does not.
    ///
    /// The entry path: the ground around a spawn cannot be baked, and the spawn
    /// itself cannot be chosen, until the water under it is known — but a couple
    /// of hundred milliseconds spent finding out is a couple of hundred
    /// milliseconds the window is not being drawn, which is what made entering
    /// the world freeze. So the caller asks, shows its loading screen, and asks
    /// again next frame.
    pub fn traced(&mut self, focus: GlobalXZ) -> bool {
        if let Some((_, handle)) = self.pending.take() {
            if handle.is_finished() {
                self.install(handle.join().expect("brook window thread"));
            } else {
                self.pending = Some((self.wanted(), handle));
                return false;
            }
        }
        if self.field().covers(focus, MEDIUM.reach_m()) {
            return true;
        }
        self.trace(focus);
        false
    }

    /// Keep the window around the player, without ever blocking on it.
    pub fn follow(&mut self, focus: GlobalXZ) {
        if let Some((_, handle)) = self.pending.take() {
            if handle.is_finished() {
                self.install(handle.join().expect("brook window thread"));
            } else {
                self.pending = Some((self.wanted(), handle));
                return;
            }
        }
        let current = self.field();
        if !current.covers(focus, MEDIUM.reach_m()) {
            // The field no longer speaks for ground the streamer is baking, so
            // there is nothing to do but wait for one that does.
            self.settle(focus);
            return;
        }
        let drift = {
            let dx = focus.x - current.centre().x;
            let dz = focus.z - current.centre().z;
            (dx * dx + dz * dz).sqrt()
        };
        if drift < REBUILD_M {
            return;
        }
        self.trace(focus);
    }

    fn trace(&mut self, focus: GlobalXZ) {
        let surface = Arc::clone(&self.surface);
        let handle = std::thread::Builder::new()
            .name("brooks".into())
            .spawn(move || BrookField::build(&surface, focus))
            .expect("brook window thread");
        self.pending = Some((focus, handle));
    }

    fn wanted(&self) -> GlobalXZ {
        self.pending
            .as_ref()
            .map(|(at, _)| *at)
            .unwrap_or_else(|| self.field().centre())
    }

    fn install(&mut self, field: BrookField) {
        *self.field.write().expect("brook window") = Arc::new(field);
    }
}

impl std::fmt::Debug for BrookWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let field = self.field();
        f.debug_struct("BrookWindow")
            .field("centre", &field.centre())
            .field("brooks", &field.brooks().len())
            .field("ponds", &field.ponds().len())
            .field("tracing", &self.pending.is_some())
            .finish()
    }
}

/// A point of a brook already laid down, for the merge test.
#[derive(Clone, Copy, Debug)]
struct Placed {
    at: Vec2,
    sheet_z: f32,
    /// Order it was laid down in, so ties break the same way every time.
    seq: u32,
}

/// Buckets of placed points, so a merge test looks at nine cells rather than at
/// every brook in the window.
#[derive(Debug, Default)]
struct SnapGrid {
    cells: std::collections::HashMap<(i32, i32), Vec<Placed>>,
    next_seq: u32,
}

impl SnapGrid {
    fn key(p: Vec2) -> (i32, i32) {
        (
            (p.x / MERGE_SNAP_M).floor() as i32,
            (p.y / MERGE_SNAP_M).floor() as i32,
        )
    }

    fn insert(&mut self, at: Vec2, sheet_z: f32) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.cells
            .entry(Self::key(at))
            .or_default()
            .push(Placed { at, sheet_z, seq });
    }

    /// Nearest placed point within [`MERGE_SNAP_M`], lying ahead of a trace on
    /// `heading`. A zero heading accepts any bearing, for testing a spring.
    fn near(&self, p: Vec2, heading: Vec2) -> Option<Placed> {
        let (kx, kz) = Self::key(p);
        let cone = MERGE_CONE_DEG.to_radians();
        let mut best: Option<(f32, Placed)> = None;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let Some(bucket) = self.cells.get(&(kx + dx, kz + dz)) else {
                    continue;
                };
                for placed in bucket {
                    let d = p.distance(placed.at);
                    if d > MERGE_SNAP_M {
                        continue;
                    }
                    let toward = (placed.at - p).normalize_or_zero();
                    if heading != Vec2::ZERO
                        && toward != Vec2::ZERO
                        && heading.angle_to(toward).abs() > cone
                    {
                        continue;
                    }
                    let better = match best {
                        Some((best_d, best_p)) => (d, placed.seq) < (best_d, best_p.seq),
                        None => true,
                    };
                    if better {
                        best = Some((d, *placed));
                    }
                }
            }
        }
        best.map(|(_, placed)| placed)
    }
}

/// One trace in progress.
struct Leg {
    points: Vec<Vec2>,
    sheet_z: Vec<f32>,
    direction: Vec2,
    /// Signed turn summed over every step so far, in radians.
    winding: f32,
}

impl Leg {
    fn new(at: Vec2, sheet_z: f32, direction: Vec2) -> Self {
        Self {
            points: vec![at],
            sheet_z: vec![sheet_z],
            direction,
            winding: 0.0,
        }
    }

    fn head(&self) -> Vec2 {
        *self.points.last().expect("a leg keeps its spring")
    }

    fn sheet(&self) -> f32 {
        *self.sheet_z.last().expect("a leg keeps its sheet")
    }

    /// `want` brought within one step's worth of turn of the current heading.
    ///
    /// Water has momentum: it undercuts the outside of a bend rather than
    /// turning a corner. Without this the fall line may reverse outright the
    /// moment a trace steps into a dip — which is a brook drawn back over
    /// itself — and a rim escape may leave at any bearing at all.
    fn steer(&self, want: Vec2, cap_deg: f32) -> Option<Vec2> {
        let want = want.normalize_or_zero();
        if want == Vec2::ZERO {
            return None;
        }
        if self.direction == Vec2::ZERO {
            return Some(want);
        }
        let turn = self.direction.angle_to(want);
        let cap = cap_deg.to_radians();
        Some(rotate(self.direction, turn.clamp(-cap, cap)))
    }

    /// The bearing from the head to `at`, if a brook could run straight there:
    /// no sharper a turn than a rim spill, and no longer than a step and a half.
    fn chord_to(&self, at: Vec2) -> Option<Vec2> {
        let chord = at - self.head();
        let direction = chord.normalize_or_zero();
        let turn = self.direction.angle_to(direction).abs().to_degrees();
        (direction != Vec2::ZERO
            && chord.length() <= TRACE_STEP_M * 1.6
            && (self.direction == Vec2::ZERO || turn <= MERGE_CONE_DEG))
            .then_some(direction)
    }

    /// Whether `at` is ground this leg has already run over.
    ///
    /// The head is exempt: every candidate is one step from it by construction.
    fn revisits(&self, at: Vec2) -> bool {
        self.points[..self.points.len() - 1]
            .iter()
            .any(|p| p.distance(at) < REVISIT_M)
    }

    fn push(&mut self, step: Step) {
        self.points.push(step.at);
        self.sheet_z.push(step.sheet_z);
        if self.direction != Vec2::ZERO {
            self.winding += self.direction.angle_to(step.direction);
        }
        self.direction = step.direction;
    }

    /// Whether the course has wound so far one way that it is circling.
    fn orbits(&self) -> bool {
        self.winding.abs() > MAX_WINDING_DEG.to_radians()
    }
}

/// A step that passed the incision test.
#[derive(Clone, Copy)]
struct Step {
    at: Vec2,
    sheet_z: f32,
    direction: Vec2,
}

/// What a leg ran into.
enum LegEnd {
    Stop(Terminus),
    /// Ground rises in every direction: the hollow it is in fills, and the
    /// probe that established that is what says where the water goes next.
    Dammed(WayOut),
}

/// The landform under one point: hummocks averaged out, and the slope of what
/// is left.
#[derive(Clone, Copy)]
struct Probe {
    gradient: Vec2,
}

/// The lowest rim around a point, and how high it stands.
///
/// A pass is the highest ground on the way out, so the lowest pass is how far
/// the water can rise before it runs somewhere.
#[derive(Clone, Copy)]
struct WayOut {
    direction: Vec2,
    pass_z: f32,
}

/// What one pond-probe ray found around a dam.
#[derive(Clone, Copy)]
enum RayHit {
    /// Fell away before climbing out of the hollow.
    Open,
    /// Climbed a pond's worth, then fell — the far side of a rim.
    Crest(f32),
    /// Climbed a pond's worth and kept rising: a wall, not a spill.
    Wall,
}

/// Where a filled hollow lets go.
struct Spill {
    at: Vec2,
    sheet_z: f32,
    direction: Vec2,
    /// Steps the water spent crossing the pond.
    cost: usize,
}

struct Tracer<'s> {
    surface: &'s ContinentalSurface,
    seed: u64,
    sea: f32,
    bounds: AtlasBounds,
    centre: GlobalXZ,
    brooks: Vec<Brook>,
    ponds: Vec<Pond>,
    basins: PondGrid,
    snap: SnapGrid,
    /// Ground the chain being traced right now has already covered: the courses
    /// of its earlier legs, and the ponds it filled on the way.
    ///
    /// A leg refuses to step back onto its own course, but a chain is several
    /// legs — spring, pond, outlet, pond — and without this the outlet of a pond
    /// is free to curl round and run back into it. That draws as a closed loop of
    /// water hanging off a pond, which is the one shape a watercourse never has.
    covered: Vec<Vec2>,
    filled: Vec<usize>,
}

impl<'s> Tracer<'s> {
    fn new(surface: &'s ContinentalSurface, centre: GlobalXZ) -> Self {
        Self {
            surface,
            seed: surface.world_seed() as u32 as u64,
            sea: surface.sea_surface_z(),
            bounds: surface.bounds(),
            centre,
            brooks: Vec::new(),
            ponds: Vec::new(),
            basins: PondGrid::default(),
            snap: SnapGrid::default(),
            covered: Vec::new(),
            filled: Vec::new(),
        }
    }

    fn run(mut self) -> BrookField {
        let cell = CELL_METRES as f64;
        let lo_x = ((self.centre.x - SEED_RADIUS_M) / cell).floor() as i64;
        let hi_x = ((self.centre.x + SEED_RADIUS_M) / cell).ceil() as i64;
        let lo_z = ((self.centre.z - SEED_RADIUS_M) / cell).floor() as i64;
        let hi_z = ((self.centre.z + SEED_RADIUS_M) / cell).ceil() as i64;

        // Absolute cell order, never window order: two windows that both hold a
        // pair of springs have to trace them in the same sequence, or their
        // merge comes out differently.
        for az in lo_z..=hi_z {
            for ax in lo_x..=hi_x {
                if !self.bounds.contains_cell(ax as i32, az as i32) {
                    continue;
                }
                for spring in self.springs_of(ax, az) {
                    self.trace_chain(spring);
                }
            }
        }
        self.finish()
    }

    fn finish(self) -> BrookField {
        let brooks: Vec<Brook> = self
            .brooks
            .into_iter()
            .filter(|b| b.points.len() >= 2)
            .collect();
        let paths: Vec<Vec<Vec2>> = brooks.iter().map(|b| b.points.clone()).collect();
        let mut starts = Vec::with_capacity(brooks.len() + 1);
        let mut total = 0u32;
        for brook in &brooks {
            starts.push(total);
            total += brook.points.len() as u32;
        }
        starts.push(total);
        BrookField {
            centre: self.centre,
            index: SegmentField::build_paths(&paths),
            brooks,
            starts,
            basins: self.basins,
            ponds: self.ponds,
        }
    }

    /// Structural ground, uncarved: tracing the carved surface would have a
    /// brook chase its own groove.
    fn base(&self, p: Vec2) -> f32 {
        self.surface
            .base_ground(GlobalXZ::at(p.x as f64, p.y as f64))
    }

    /// The landform at `p`: the height with hummocks averaged out, and the slope
    /// of that averaged height.
    ///
    /// Drainage answers to the shape of the land, not to every tussock on it. Run
    /// against the raw field, a trace reads a one-metre bump at fifteen metres as
    /// a dam and stops, which is why a brook used to manage a hundred metres
    /// before it became a puddle; and its course chases noise instead of the
    /// valley. Both come from the same five samples, so this costs no more than
    /// the plain gradient it replaces.
    fn probe(&self, p: Vec2) -> Probe {
        let e = self.base(Vec2::new(p.x + LANDFORM_ARM_M, p.y));
        let w = self.base(Vec2::new(p.x - LANDFORM_ARM_M, p.y));
        let n = self.base(Vec2::new(p.x, p.y + LANDFORM_ARM_M));
        let s = self.base(Vec2::new(p.x, p.y - LANDFORM_ARM_M));
        Probe {
            gradient: Vec2::new(e - w, n - s) / (2.0 * LANDFORM_ARM_M),
        }
    }

    /// Which way the land falls over kilometres, from the atlas elevation.
    ///
    /// The landform probe reads a ninety-metre arm, which on gentle ground is
    /// mostly noise: it says which way the next hummock leans, not which way the
    /// valley goes. Without a regional fall a trace on a plateau has nothing
    /// coherent to follow and wanders until it is cut off — and the atlas has
    /// known the answer all along, one kilometre to a cell.
    fn regional(&self, p: Vec2) -> Vec2 {
        let fields = self.surface.fields();
        let at = |x: f32, z: f32| fields.sample_smooth(&fields.elevation_m, x, z);
        let e = at(p.x + REGIONAL_ARM_M, p.y);
        let w = at(p.x - REGIONAL_ARM_M, p.y);
        let n = at(p.x, p.y + REGIONAL_ARM_M);
        let s = at(p.x, p.y - REGIONAL_ARM_M);
        Vec2::new(w - e, s - n).normalize_or_zero()
    }

    /// Which way the nearest real water lies.
    ///
    /// A brook is a tributary: it is going to the river, and the atlas already
    /// knows where the river is. Steepest descent alone does not, and on a
    /// landscape whose relief is noise rather than the work of the water on it,
    /// every hollow is closed — a purely local trace fills a pond every few
    /// hundred metres and almost never arrives anywhere.
    ///
    /// [`ContinentalSurface::water_reach`] rises towards water, so its gradient
    /// is the bearing of the outlet.
    fn downstream(&self, p: Vec2) -> Vec2 {
        let reach = |q: Vec2| {
            self.surface
                .water_reach(GlobalXZ::at(q.x as f64, q.y as f64))
        };
        Vec2::new(
            reach(Vec2::new(p.x + OUTLET_ARM_M, p.y)) - reach(Vec2::new(p.x - OUTLET_ARM_M, p.y)),
            reach(Vec2::new(p.x, p.y + OUTLET_ARM_M)) - reach(Vec2::new(p.x, p.y - OUTLET_ARM_M)),
        )
        .normalize_or_zero()
    }

    fn in_atlas(&self, p: Vec2) -> bool {
        self.bounds
            .contains_point(GlobalXZ::at(p.x as f64, p.y as f64))
    }

    /// Candidate springs in one atlas cell, tested for somewhere to run.
    fn springs_of(&self, ax: i64, az: i64) -> Vec<Vec2> {
        let fields = self.surface.fields();
        let cx = (ax as f32 + 0.5) * CELL_METRES;
        let cz = (az as f32 + 0.5) * CELL_METRES;
        if fields.sample_smooth(&fields.elevation_m, cx, cz) < self.sea + SPRING_MIN_HEIGHT_M {
            return Vec::new();
        }
        let humidity = fields
            .sample_smooth(&fields.humidity01, cx, cz)
            .clamp(0.0, 1.0);
        let relief = fields
            .sample_smooth(&fields.relief01, cx, cz)
            .clamp(0.0, 1.0);
        // Rain to feed it and a slope to run down: a dry plain has neither.
        let expected =
            SPRINGS_PER_CELL * smoothstep(0.22, 0.70, humidity) * (0.35 + 0.9 * relief).min(1.0);

        let mut rng = CellRng::new(self.seed ^ SPRING_SALT, ax, az);
        let count = rng.count(expected, MAX_SPRINGS_PER_CELL);
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            // Both draws happen whether or not the spring is accepted, so a
            // rejection cannot shift the next candidate's position.
            let p = Vec2::new(
                (ax as f32 + rng.unit()) * CELL_METRES,
                (az as f32 + rng.unit()) * CELL_METRES,
            );
            if !self.in_atlas(p) {
                continue;
            }
            let column = self.surface.column(GlobalXZ::at(p.x as f64, p.y as f64));
            if column.is_wet() || column.ground() < self.sea + SPRING_MIN_HEIGHT_M {
                continue;
            }
            if self.probe(p).gradient.length() < SPRING_MIN_SLOPE {
                continue;
            }
            out.push(p);
        }
        out
    }

    /// A spring, its brook, whatever pond that brook fills, its outlet, and so
    /// on until the length is spent.
    fn trace_chain(&mut self, spring: Vec2) {
        // A spring that rises on a course already traced is that course. Tracing
        // it anyway produces a stub that steps once and snaps back to where it
        // started, which is a knot rather than a tributary.
        if self.snap.near(spring, Vec2::ZERO).is_some() {
            return;
        }
        self.covered.clear();
        self.filled.clear();
        let mut at = spring;
        let source = self.probe(spring);
        // The landform says which way to run; the sheet sits on the ground that
        // is actually there. Using the average for both is what hung a brook in
        // the air through a valley: the 90 m stencil is the height of the hills
        // around it.
        let mut sheet_z = self.base(spring) - FREEBOARD_M;
        let mut direction = (-source.gradient).normalize_or_zero();
        let mut budget = MAX_STEPS;

        while budget > 0 {
            let mut leg = Leg::new(at, sheet_z, direction);
            let end = self.trace_leg(&mut leg, budget);
            budget -= leg.points.len() - 1;
            let head = leg.head();

            let dam = match end {
                LegEnd::Stop(terminus) => {
                    self.keep(leg, terminus);
                    return;
                }
                LegEnd::Dammed(dam) => dam,
            };
            // Fill the hollow, and carry on from where it spills.
            let Some(spill) = self.fill(head, dam) else {
                self.keep(leg, Terminus::Soaks);
                return;
            };
            join_sheets(&mut leg.sheet_z, spill.sheet_z);
            self.keep(leg, Terminus::Pond);
            at = spill.at;
            sheet_z = spill.sheet_z;
            direction = spill.direction;
            budget = budget.saturating_sub(spill.cost.max(1));
        }
    }

    /// Steepest descent with inertia, until something stops it.
    fn trace_leg(&self, leg: &mut Leg, budget: usize) -> LegEnd {
        for _ in 0..budget {
            let step = match self.next_step(leg) {
                Ok(step) => step,
                Err(dam) => return LegEnd::Dammed(dam),
            };
            if !self.in_atlas(step.at) {
                leg.push(step);
                return LegEnd::Stop(Terminus::OffMap);
            }
            // A confluence snaps the mouth onto the trunk exactly, which moves
            // the last point sideways — so the chord it leaves behind has to be
            // a chord a brook could have run, or the join draws as a hook. When
            // it is not, the ordinary step stands and the next one gets to try.
            // A confluence is a meeting of two courses. Snapping onto water this
            // same chain laid down upstream is not a confluence, it is the chain
            // tying itself in a knot.
            if let Some(placed) = self
                .snap
                .near(step.at, step.direction)
                .filter(|placed| !self.covers(placed.at))
            {
                if let Some(direction) = leg.chord_to(placed.at) {
                    leg.push(Step {
                        at: placed.at,
                        direction,
                        ..step
                    });
                    join_sheets(&mut leg.sheet_z, placed.sheet_z);
                    return LegEnd::Stop(Terminus::Merge);
                }
            }
            if let Some((pond, sd)) = nearest_pond(&self.ponds, self.basins.at(step.at), step.at) {
                if sd >= 0.0 {
                    let sheet_z = pond.sheet_z;
                    leg.push(step);
                    join_sheets(&mut leg.sheet_z, sheet_z);
                    return LegEnd::Stop(Terminus::Pond);
                }
            }
            let column = self
                .surface
                .column(GlobalXZ::at(step.at.x as f64, step.at.y as f64));
            if let Some(top) = column.water_top() {
                leg.push(step);
                join_sheets(&mut leg.sheet_z, top);
                return LegEnd::Stop(Terminus::Water);
            }
            leg.push(step);
            if leg.orbits() {
                // Water that has wound this far is on ground too flat to tell it
                // where to go. That is not a course, it is a hollow — so treat it
                // as one: pool here, and leave over the lowest rim with a heading
                // that means something again.
                return LegEnd::Dammed(self.lowest_way_out(leg.head()));
            }
        }
        LegEnd::Stop(Terminus::Soaks)
    }

    /// Where the water goes next, or the rim that has it penned in.
    ///
    /// The course it is already on, wobbled, then the fall line, then a fan
    /// opening out either side of the two — because a brook that meets a rise
    /// square on runs along the foot of it rather than stopping. Only when every
    /// bearing it could turn onto climbs is it penned in, and then the probe
    /// that established that says where the water goes next.
    fn next_step(&self, leg: &Leg) -> Result<Step, WayOut> {
        let at = leg.head();
        let here = self.probe(at);
        let down = (-here.gradient).normalize_or_zero();
        // Three answers to the same question, at three scales: the slope under
        // the water, the fall of the country, and the bearing of the river it is
        // a tributary of. The last is zero until the river is within reach of the
        // hydro index, which is exactly when it should start to matter.
        let fall = (down * LOCAL_FALL
            + self.regional(at) * REGIONAL_FALL
            + self.downstream(at) * OUTLET_PULL)
            .normalize_or_zero();
        // The wander turns the fall line, never the heading. Turning the heading
        // makes the heading a random walk: each step keeps most of the last one,
        // so an offset that persists over a few steps is not corrected but
        // compounded, and the course rounds on itself. Turning the fall line
        // leaves the heading relaxing towards a bearing that is only ever a
        // wander off the true downhill, which is a meander.
        let wobble = value_noise(
            self.seed ^ WOBBLE_SALT,
            (at.x / WOBBLE_SCALE_M) as f64,
            (at.y / WOBBLE_SCALE_M) as f64,
        ) * WOBBLE_DEG.to_radians();
        let want =
            (leg.direction * INERTIA + rotate(fall, wobble) * (1.0 - INERTIA)).normalize_or_zero();

        if let Some(step) = self
            .try_step(leg, want)
            .or_else(|| self.try_step(leg, fall))
            .or_else(|| {
                FAN_DEG.iter().find_map(|off| {
                    self.try_step(leg, rotate(want, off.to_radians()))
                        .or_else(|| self.try_step(leg, rotate(want, -off.to_radians())))
                })
            })
        {
            return Ok(step);
        }
        // The probe is the expensive part of the whole trace, so its answer is
        // handed on to whoever needs it next rather than taken twice.
        let out = self.lowest_way_out(at);
        // A low bump is a step, not a basin. Try it before filling a pond —
        // in the mountains the old "is the pass low?" test used an 80 m ridge
        // as the pass and never took this, so every rise became a pane.
        if let Some(step) = self.try_over(leg, out.direction) {
            return Ok(step);
        }
        Err(out)
    }

    fn try_step(&self, leg: &Leg, want: Vec2) -> Option<Step> {
        self.step_along(leg, leg.steer(want, MAX_TURN_DEG)?)
    }

    /// A step over the rim of a hollow, allowed a sharper turn than a bend.
    fn try_over(&self, leg: &Leg, want: Vec2) -> Option<Step> {
        self.step_along(leg, leg.steer(want, OVER_RIM_TURN_DEG)?)
    }

    fn step_along(&self, leg: &Leg, direction: Vec2) -> Option<Step> {
        let at = leg.head() + direction * TRACE_STEP_M;
        if leg.revisits(at) || self.covers(at) {
            return None;
        }
        let want = self.base(at) - FREEBOARD_M;
        let sheet_z = (leg.sheet() - MIN_DROP_M).min(want);
        (want - sheet_z <= MAX_INCISION_M).then_some(Step {
            at,
            sheet_z,
            direction,
        })
    }

    /// Whether `at` is water this chain has already laid down.
    ///
    /// Its own basins are shunned with a little room to spare: an outlet that
    /// merely grazes the rim it just left still draws as a loop once the course
    /// is smoothed, and the spill starts further out than that.
    fn covers(&self, at: Vec2) -> bool {
        if self.covered.iter().any(|p| p.distance(at) < REVISIT_M) {
            return true;
        }
        self.filled.iter().any(|&i| self.ponds[i].shuns(at))
    }

    fn lowest_way_out(&self, at: Vec2) -> WayOut {
        let mut best = WayOut {
            direction: Vec2::X,
            pass_z: f32::INFINITY,
        };
        for k in 0..POND_RAYS {
            let direction = ray(k);
            // A crest is the spill; a slope or a wall is the ground a step
            // away — so a downhill is a way out, not a spur eighty metres on.
            let pass_z = match self.ray_hit(at, direction) {
                RayHit::Crest(peak) => peak,
                RayHit::Open => self.base(at + direction * TRACE_STEP_M),
                // A wall is not a way out; it only exists to enclose.
                RayHit::Wall => f32::INFINITY,
            };
            if pass_z < best.pass_z {
                best = WayOut { direction, pass_z };
            }
        }
        best
    }

    /// Fill the hollow around `at` and find where it spills.
    ///
    /// Water fills to the lowest way out, and only if every bearing has climbed
    /// out of the hollow — a slope that drops away is not a basin, however many
    /// spurs the other rays crest. Taking the min of those crests, ignoring the
    /// open downhill, hung a pane at ridge height over the mountainside.
    fn fill(&mut self, at: Vec2, dam: WayOut) -> Option<Spill> {
        let out = dam.direction;
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
        // The sheet is the pass the water would actually spill over, not the
        // lowest spur. `dam.pass_z` already picked that bearing.
        if open == 0
            && crests > 0
            && dam.pass_z >= floor + POND_MIN_DEPTH_M
            && dam.pass_z - floor <= POND_MAX_CELL_DROP_M
        {
            let sheet_z = dam.pass_z - POND_FREEBOARD_M;
            if let Some(spill) =
                self.flood(at, out, sheet_z, POND_MAX_RADIUS_M, POND_MAX_CELL_DROP_M, 2.0)
            {
                return Some(spill);
            }
        }
        // No closed basin. A small pool at the local waterline lets a lost
        // course spill and go on; it cannot become a pane because the sheet
        // sits on the ground that is here. Clip at the radius rather than
        // aborting — a slope beyond a 36 m pool is not a drain, it is the
        // hillside the pool sits on.
        let sheet_z = floor + POND_MIN_DEPTH_M * 0.75;
        self.flood(at, out, sheet_z, 36.0, 4.5, f32::INFINITY)
    }

    fn flood(
        &mut self,
        at: Vec2,
        out: Vec2,
        sheet_z: f32,
        max_r: f32,
        max_drop: f32,
        drain_m: f32,
    ) -> Option<Spill> {
        let start_cell = lattice(at);
        if self.pond_owns(start_cell) {
            return None;
        }
        let start_p = cell_centre(start_cell.0, start_cell.1);
        let start_drop = sheet_z - self.base(start_p);
        if start_drop <= 0.0 || start_drop > max_drop {
            return None;
        }

        let max_r2 = max_r * max_r;
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
                    if under && drop > drain_m {
                        return None;
                    }
                    continue;
                }
                if !under {
                    continue;
                }
                if self.pond_owns(next) {
                    return None;
                }
                cells.insert(next);
                queue.push_back(next);
            }
        }
        if cells.len() < POND_MIN_CELLS {
            return None;
        }
        if cells
            .iter()
            .any(|&(cx, cz)| sheet_z - self.base(cell_centre(cx, cz)) > max_drop)
        {
            return None;
        }

        let mut reach_m: f32 = 0.0;
        let mut spill_r: f32 = 0.0;
        for &(cx, cz) in &cells {
            let p = cell_centre(cx, cz);
            reach_m = reach_m.max(at.distance(p));
            spill_r = spill_r.max((p - at).dot(out));
        }
        reach_m += CHUNK_SAMPLE_M as f32 * std::f32::consts::SQRT_2 * 0.5;

        self.basins.add(self.ponds.len(), at, reach_m);
        self.filled.push(self.ponds.len());
        self.ponds.push(Pond {
            centre: at,
            sheet_z,
            reach_m,
            cells,
        });

        let start = at + out * (spill_r + TRACE_STEP_M * 0.5);
        Some(Spill {
            at: start,
            sheet_z,
            direction: out,
            cost: (spill_r / TRACE_STEP_M).ceil() as usize,
        })
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

    /// Put every point of a course on the ground that is actually there.
    ///
    /// The trace may have carried a sheet down from a neighbour that stood
    /// higher; Chaikin then draws chords through dips the trace never sampled.
    /// Either way the water would hang in the air. Clamped to the real ground
    /// and then made monotone, it lies in the channel it cuts.
    fn seat(&self, points: &[Vec2], sheets: &mut [f32]) {
        for (p, z) in points.iter().zip(sheets.iter_mut()) {
            *z = z.min(self.base(*p) - FREEBOARD_M);
        }
        for i in 1..sheets.len() {
            sheets[i] = sheets[i].min(sheets[i - 1] - MIN_DROP_M);
        }
    }

    fn keep(&mut self, leg: Leg, terminus: Terminus) {
        if leg.points.len() < 2 {
            return;
        }
        // A course that ends up where it began is not a watercourse.
        if leg.points[0].distance(leg.head()) < BROOK_REACH_M {
            return;
        }
        self.covered.extend_from_slice(&leg.points);
        // Twice: one pass leaves seven-metre chords, and from the bank a
        // seven-metre chord is a straight edge as long as the channel is wide.
        let (points, sheet_z) = smoothed(&leg.points, &leg.sheet_z);
        let (mut points, mut sheet_z) = smoothed(&points, &sheet_z);
        self.seat(&mut points, &mut sheet_z);
        // Smoothing inserts points the trace never stood on, and seating those
        // can drop the mouth off the water it was joined to. Lower the tail to
        // meet a lower sheet; never lift it to meet a higher one — that is how
        // a pond's freeboard used to hang the whole inlet in the air.
        if matches!(terminus, Terminus::Water | Terminus::Merge) {
            let target = *leg.sheet_z.last().expect("a kept leg has a mouth");
            let seated = *sheet_z.last().expect("a kept leg has a mouth");
            if target < seated {
                join_sheets(&mut sheet_z, target);
            } else if let Some(last) = sheet_z.last_mut() {
                *last = target;
            }
        }
        // The smoothed course is the one that gets drawn and carved, so it is
        // the one a tributary should snap onto.
        for (at, sheet) in points.iter().zip(&sheet_z) {
            self.snap.insert(*at, *sheet);
        }
        let n = points.len();
        let mut half_width_m = Vec::with_capacity(n);
        let mut run = 0.0;
        for i in 0..n {
            if i > 0 {
                run += points[i].distance(points[i - 1]);
            }
            half_width_m.push(half_width(run));
        }
        if terminus == Terminus::Soaks {
            // A channel that simply stopped mid-slope reads as a mistake, so
            // the last stretch narrows away to nothing.
            let taper = 8.min(n);
            for k in 0..taper {
                half_width_m[n - 1 - k] *= k as f32 / taper as f32;
            }
        }
        self.brooks.push(Brook {
            points,
            sheet_z,
            half_width_m,
            terminus,
        });
    }
}

/// One Chaikin pass over a traced course, with both ends pinned.
///
/// A trace is a run of fifteen-metre chords, and a chord is straight: drawn
/// directly, a brook is a chain of rectangles with a visible angle at every
/// joint. Cutting the corners halves the segment length and halves every turn,
/// which is the difference between a channel and a set of paving slabs. The ends
/// stay exactly where they were, because the mouth of a brook has to keep
/// meeting the water it was traced into.
fn smoothed(points: &[Vec2], sheets: &[f32]) -> (Vec<Vec2>, Vec<f32>) {
    let n = points.len();
    let mut out_p = Vec::with_capacity(2 * n);
    let mut out_z = Vec::with_capacity(2 * n);
    out_p.push(points[0]);
    out_z.push(sheets[0]);
    for i in 0..n - 1 {
        for w in [0.25f32, 0.75] {
            out_p.push(points[i].lerp(points[i + 1], w));
            // A weighted mean of two sheets sits between them, so a descent
            // stays a descent and nothing here can lift water uphill.
            out_z.push(lerp(sheets[i], sheets[i + 1], w));
        }
    }
    out_p.push(points[n - 1]);
    out_z.push(sheets[n - 1]);
    (out_p, out_z)
}

/// Bring the end of a run of sheets to `target` without letting any of them
/// rise downstream.
///
/// A brook meeting a river or a pond has to arrive at exactly its surface, or
/// there is a visible step where the two meet. Lifting the tail rather than
/// dropping the head keeps the descent above it intact.
fn join_sheets(sheets: &mut [f32], target: f32) {
    let Some(last) = sheets.last_mut() else {
        return;
    };
    *last = target;
    for i in (0..sheets.len() - 1).rev() {
        let need = sheets[i + 1] + MIN_DROP_M;
        if sheets[i] >= need {
            break;
        }
        sheets[i] = need;
    }
}

/// A brook widens as it runs. Length is the only measure of catchment that
/// depends on nothing but the brook's own trace, which is what keeps two
/// windows agreeing about how wide it is.
fn half_width(travelled_m: f32) -> f32 {
    let t = (travelled_m / MAX_BROOK_LEN_M as f32)
        .clamp(0.0, 1.0)
        .sqrt();
    lerp(HALF_WIDTH_SPRING_M, HALF_WIDTH_MOUTH_M, t)
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

fn rotate(v: Vec2, radians: f32) -> Vec2 {
    let (s, c) = radians.sin_cos();
    Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c)
}
