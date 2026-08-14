//! Draped roads and measured bridges.
//!
//! Roads follow the atlas polylines and sit on the real ground. Where a road
//! (or a split hamlet) crosses water, the gap is measured from the signed
//! field: both dry banks, the bed, the width. A kit cannot know that. The
//! continent is not graded to meet a deck.

use std::sync::Arc;
use std::thread::JoinHandle;

use engine::color::Color;
use engine::error::EngineResult;
use engine::mesh::Mesh;
use engine::place::GlobalPlace;
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use glam::{Vec2, Vec3};

use super::ponds::PondField;
use super::settlement::HamletStand;
use super::surface::{ContinentalSurface, SurfaceColumn};
use super::world_stream::NEAR;
use crate::hamlet::WATERLINE_MARGIN;

const REACH_M: f64 = NEAR.covers_m();
const RESEED_M: f64 = 80.0;
const SAMPLE_M: f32 = 6.0;
/// How far the dirt sits above sampled ground so the 4 m grass grid cannot
/// poke through a chord. Walking still uses the terrain, not this ribbon.
const ROAD_LIFT_M: f32 = 0.16;
/// Drape the visible strip this finely; bridge detection stays on `SAMPLE_M`.
const RIBBON_STEP_M: f32 = 2.0;
const PRIMARY_WIDTH: f32 = 5.2;
const SECONDARY_WIDTH: f32 = 3.6;
const FOOT_WIDTH: f32 = 2.2;
const FORD_MAX_GAP_M: f32 = 8.0;
const FORD_MAX_WET_M: f32 = 0.9;
const MIN_SPAN_M: f32 = 4.0;
const MAX_SPAN_M: f32 = 220.0;
const DECK_THICKNESS: f32 = 0.45;
const DECK_CLEARANCE: f32 = 0.12;
/// How far a gentle bank is walked inland.
const BANK_CREST_M: f32 = 28.0;
/// How far a steep face is followed to its crest. That span is the chasm, not the water.
const STEEP_BANK_M: f32 = 80.0;
/// Keep walking a cut that levels into a terrace; stop on a hillside.
const CUT_SLOPE: f32 = 0.15;
/// Gentle banks: sit this far above the water when there is no steep crest.
const BRIDGE_ABOVE_WATER: f32 = 6.0;
const MERGE_SPAN_M: f32 = 40.0;
const PARAPET_H: f32 = 0.72;
const PARAPET_T: f32 = 0.22;
const PIER_SPACING: f32 = 10.0;
const PIER_FOOT: f32 = 1.4;

fn road_dirt() -> Color {
    Color::rgb(92, 72, 52)
}
fn deck_wood() -> Color {
    Color::rgb(130, 98, 68)
}
fn pier_wood() -> Color {
    Color::rgb(72, 56, 42)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpanKind {
    Bridge,
    Ford,
}

#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub a: Vec2,
    pub b: Vec2,
    pub za: f32,
    pub zb: f32,
    pub width: f32,
    pub kind: SpanKind,
}

struct Deck {
    a: Vec2,
    b: Vec2,
    width: f32,
    za: f32,
    zb: f32,
}

struct PathBake {
    mesh: Mesh,
    decks: Vec<Deck>,
    focus: GlobalXZ,
}

struct Pending {
    focus: GlobalXZ,
    resident_chunks: usize,
    job: JoinHandle<EngineResult<PathBake>>,
}

/// Roads and bridges around the player.
pub struct PathLayer {
    entity: Option<EntityId>,
    decks: Vec<Deck>,
    centre: Option<GlobalXZ>,
    resident_chunks: usize,
    pending: Option<Pending>,
    /// Hamlets the in-flight or last bake was started against.
    hamlets: Vec<HamletStand>,
}

impl Default for PathLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl PathLayer {
    pub fn new() -> Self {
        Self {
            entity: None,
            decks: Vec::new(),
            centre: None,
            resident_chunks: 0,
            pending: None,
            hamlets: Vec::new(),
        }
    }

    pub fn clear(&mut self, world: &mut World) -> EngineResult<()> {
        self.pending = None;
        if let Some(id) = self.entity.take() {
            world.despawn(id);
        }
        self.decks.clear();
        self.centre = None;
        self.resident_chunks = 0;
        self.hamlets.clear();
        Ok(())
    }

    /// Height of a bridge deck under `p`, if any. Fords use the terrain.
    pub fn deck_height(&self, p: GlobalXZ) -> Option<f32> {
        let q = Vec2::new(p.x as f32, p.z as f32);
        let mut best: Option<f32> = None;
        for deck in &self.decks {
            if let Some(z) = on_deck(q, deck) {
                best = Some(best.map_or(z, |b| b.max(z)));
            }
        }
        best
    }

    pub fn follow(
        &mut self,
        world: &mut World,
        surface: &Arc<ContinentalSurface>,
        ponds: &Arc<PondField>,
        hamlets: &[HamletStand],
        focus: GlobalXZ,
        resident: usize,
        walked_pending: usize,
        _rebased: bool,
    ) -> EngineResult<bool> {
        let mut changed = false;
        if let Some(pending) = self.pending.take() {
            if pending.job.is_finished() {
                let bake = pending.job.join().expect("path thread")?;
                self.install(world, bake)?;
                self.centre = Some(pending.focus);
                self.resident_chunks = pending.resident_chunks;
                changed = true;
            } else {
                self.pending = Some(pending);
            }
        }

        let moved = self
            .centre
            .map(|c| ((c.x - focus.x).powi(2) + (c.z - focus.z).powi(2)).sqrt())
            .unwrap_or(f64::INFINITY);
        let hamlets_changed = self.hamlets != hamlets;
        let wanted = moved >= RESEED_M
            || (resident != self.resident_chunks && walked_pending == 0)
            || hamlets_changed;
        if !wanted || self.pending.is_some() {
            return Ok(changed);
        }

        let surface = Arc::clone(surface);
        let ponds = Arc::clone(ponds);
        let hamlets = hamlets.to_vec();
        self.hamlets = hamlets.clone();
        self.pending = Some(Pending {
            focus,
            resident_chunks: resident,
            job: std::thread::Builder::new()
                .name("paths".into())
                .spawn(move || bake_paths(&surface, &ponds, &hamlets, focus))
                .expect("path thread"),
        });
        Ok(changed)
    }

    fn install(&mut self, world: &mut World, bake: PathBake) -> EngineResult<()> {
        if let Some(id) = self.entity.take() {
            world.despawn(id);
        }
        self.decks = bake.decks;
        if bake.mesh.face_count() == 0 {
            return Ok(());
        }
        let place = GlobalPlace::at(GlobalPosition::at(bake.focus.x, 0.0, bake.focus.z));
        self.entity = Some(world.spawn_anchored(bake.mesh, place)?);
        Ok(())
    }
}

fn bake_paths(
    surface: &ContinentalSurface,
    ponds: &PondField,
    hamlets: &[HamletStand],
    focus: GlobalXZ,
) -> EngineResult<PathBake> {
    let mut mesh = Mesh::new();
    let mut decks = Vec::new();
    let origin = Vec3::new(focus.x as f32, 0.0, focus.z as f32);
    let focus2 = Vec2::new(focus.x as f32, focus.z as f32);
    let reach_sq = REACH_M * REACH_M;

    for road in surface.roads() {
        if !path_near(&road.points, focus, reach_sq) {
            continue;
        }
        let width = if road.class == 0 {
            PRIMARY_WIDTH
        } else {
            SECONDARY_WIDTH
        };
        for samples in sample_runs(surface, ponds, &road.points, focus2) {
            let spans = spans_along(&samples, width);
            let dry = dry_runs(&samples, &spans, hamlets);
            for run in dry {
                add_ribbon(&mut mesh, origin, &run, width, road_dirt(), surface, ponds)?;
            }
            for span in spans {
                if span.kind == SpanKind::Ford {
                    continue;
                }
                if near_deck(&decks, &span) {
                    continue;
                }
                add_bridge(&mut mesh, origin, &span, surface, ponds)?;
                decks.push(Deck {
                    a: span.a,
                    b: span.b,
                    width: span.width,
                    za: span.za,
                    zb: span.zb,
                });
            }
        }
    }

    for hamlet in hamlets {
        if let Some(span) = hamlet_span(surface, ponds, hamlet) {
            if span.kind == SpanKind::Bridge && !near_deck(&decks, &span) {
                add_bridge(&mut mesh, origin, &span, surface, ponds)?;
                decks.push(Deck {
                    a: span.a,
                    b: span.b,
                    width: span.width,
                    za: span.za,
                    zb: span.zb,
                });
            }
        }
    }

    Ok(PathBake { mesh, decks, focus })
}

fn path_near(points: &[Vec2], focus: GlobalXZ, reach_sq: f64) -> bool {
    points.iter().any(|p| {
        let dx = f64::from(p.x) - focus.x;
        let dz = f64::from(p.y) - focus.z;
        dx * dx + dz * dz <= reach_sq
    })
}

fn near_deck(decks: &[Deck], span: &Span) -> bool {
    decks
        .iter()
        .any(|d| dist_segments(d.a, d.b, span.a, span.b) < MERGE_SPAN_M)
}

fn dist_segments(a0: Vec2, a1: Vec2, b0: Vec2, b1: Vec2) -> f32 {
    dist_point_segment(a0, b0, b1)
        .min(dist_point_segment(a1, b0, b1))
        .min(dist_point_segment(b0, a0, a1))
        .min(dist_point_segment(b1, a0, a1))
}

fn dist_point_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let d = b - a;
    let len2 = d.length_squared();
    if len2 < 1e-8 {
        return p.distance(a);
    }
    let t = ((p - a).dot(d) / len2).clamp(0.0, 1.0);
    p.distance(a + d * t)
}

#[derive(Clone, Copy)]
struct Sample {
    at: Vec2,
    wet: f32,
    ground: f32,
}

fn column_at(surface: &ContinentalSurface, ponds: &PondField, p: Vec2) -> SurfaceColumn {
    let g = GlobalXZ::at(f64::from(p.x), f64::from(p.y));
    let mut column = surface.column(g);
    ponds.carve(g, &mut column);
    column
}

fn sample_runs(
    surface: &ContinentalSurface,
    ponds: &PondField,
    points: &[Vec2],
    focus: Vec2,
) -> Vec<Vec<Sample>> {
    let pad = REACH_M as f32 + MAX_SPAN_M;
    densify_near(points, focus, pad, SAMPLE_M)
        .into_iter()
        .map(|run| {
            run.into_iter()
                .map(|at| {
                    let col = column_at(surface, ponds, at);
                    Sample {
                        at,
                        wet: col.wetness(),
                        ground: col.ground(),
                    }
                })
                .collect()
        })
        .collect()
}

/// Keep only the part of a polyline that can affect the path window.
///
/// Atlas roads run for tens of kilometres. Sampling every six metres of a
/// continent-spanning road on the game thread is what froze travel for seconds
/// whenever one of those roads grazed the window.
fn densify_near(points: &[Vec2], focus: Vec2, pad: f32, step: f32) -> Vec<Vec<Vec2>> {
    let mut runs = Vec::new();
    let mut cur: Vec<Vec2> = Vec::new();
    for pair in points.windows(2) {
        let a = pair[0];
        let b = pair[1];
        if !segment_hits_disk(a, b, focus, pad) {
            if cur.len() >= 2 {
                runs.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
            continue;
        }
        let len = a.distance(b);
        let n = ((len / step).ceil() as usize).max(1);
        for i in 0..=n {
            let t = i as f32 / n as f32;
            let p = a.lerp(b, t);
            if p.distance(focus) <= pad {
                if cur.last().is_none_or(|q| q.distance(p) > 0.01) {
                    cur.push(p);
                }
            } else if cur.len() >= 2 {
                runs.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() >= 2 {
        runs.push(cur);
    }
    runs
}

fn segment_hits_disk(a: Vec2, b: Vec2, c: Vec2, r: f32) -> bool {
    if a.distance(c) <= r || b.distance(c) <= r {
        return true;
    }
    let d = b - a;
    let len2 = d.length_squared();
    if len2 < 1e-8 {
        return false;
    }
    let t = ((c - a).dot(d) / len2).clamp(0.0, 1.0);
    a.lerp(b, t).distance(c) <= r
}

/// Wet runs along a sampled road. Used by tests with a fake sample list.
fn spans_along(samples: &[Sample], width: f32) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut i = 0;
    while i < samples.len() {
        if samples[i].wet < -WATERLINE_MARGIN {
            i += 1;
            continue;
        }
        let start = i;
        let mut max_wet = samples[i].wet;
        while i < samples.len() && samples[i].wet >= -WATERLINE_MARGIN {
            max_wet = max_wet.max(samples[i].wet);
            i += 1;
        }
        if start == 0 || i == samples.len() {
            continue;
        }
        let a = samples[start - 1];
        let b = samples[i];
        let gap = a.at.distance(b.at);
        if !(MIN_SPAN_M..=MAX_SPAN_M).contains(&gap) {
            continue;
        }
        let kind = if gap <= FORD_MAX_GAP_M && max_wet <= FORD_MAX_WET_M {
            SpanKind::Ford
        } else {
            SpanKind::Bridge
        };
        let (a_at, b_at, za, zb) = if kind == SpanKind::Bridge {
            let water_z = water_surface(&samples[start..i]);
            let ia = walk_to_abutment(samples, start - 1, -1, water_z);
            let ib = walk_to_abutment(samples, i, 1, water_z);
            let crest_gap = samples[ia].at.distance(samples[ib].at);
            if (MIN_SPAN_M..=MAX_SPAN_M).contains(&crest_gap) {
                let seat = samples[ia].ground.min(samples[ib].ground);
                let ia = walk_until_height(samples, start - 1, ia, seat);
                let ib = walk_until_height(samples, i, ib, seat);
                let deck = seat + DECK_CLEARANCE;
                (samples[ia].at, samples[ib].at, deck, deck)
            } else {
                let deck = a.ground.min(b.ground) + DECK_CLEARANCE;
                (a.at, b.at, deck, deck)
            }
        } else {
            (
                a.at,
                b.at,
                a.ground + DECK_CLEARANCE,
                b.ground + DECK_CLEARANCE,
            )
        };
        spans.push(Span {
            a: a_at,
            b: b_at,
            za,
            zb,
            width,
            kind,
        });
    }
    spans
}

fn water_surface(wet: &[Sample]) -> f32 {
    let underwater = wet.iter().filter(|s| s.wet > 0.0);
    let z = underwater
        .map(|s| s.ground + s.wet)
        .fold(f32::NEG_INFINITY, f32::max);
    if z.is_finite() {
        z
    } else {
        wet.iter()
            .map(|s| s.ground + s.wet.max(0.0))
            .fold(f32::NEG_INFINITY, f32::max)
    }
}

/// Walk from the waterline to the abutment.
///
/// Steep banks are a chasm: follow the face until it crests, so the deck
/// covers the whole gap, not just the water. Gentle banks have no crest;
/// sit a few metres above the water and stop.
fn walk_to_abutment(samples: &[Sample], from: usize, step: isize, water_z: f32) -> usize {
    let target = water_z + BRIDGE_ABOVE_WATER;
    let mut i = from;
    loop {
        let next = i as isize + step;
        if next < 0 || next >= samples.len() as isize {
            break;
        }
        let n = next as usize;
        if samples[n].wet >= -WATERLINE_MARGIN {
            break;
        }
        let dist = samples[n].at.distance(samples[i].at).max(0.1);
        let slope = (samples[n].ground - samples[i].ground) / dist;
        let climbing = slope >= CUT_SLOPE;
        let cap = if climbing { STEEP_BANK_M } else { BANK_CREST_M };
        if samples[n].at.distance(samples[from].at) > cap {
            break;
        }
        if climbing {
            i = n;
            continue;
        }
        if samples[i].ground >= target {
            break;
        }
        i = n;
    }
    i
}

/// Stop where the ground first reaches `height`, walking from the water toward the crest.
fn walk_until_height(samples: &[Sample], from: usize, toward: usize, height: f32) -> usize {
    if from == toward {
        return from;
    }
    let step: isize = if toward > from { 1 } else { -1 };
    let mut i = from;
    if samples[i].ground >= height {
        return i;
    }
    loop {
        let next = i as isize + step;
        if next < 0 || next >= samples.len() as isize {
            break;
        }
        let n = next as usize;
        if step > 0 && n > toward {
            break;
        }
        if step < 0 && n < toward {
            break;
        }
        i = n;
        if samples[i].ground >= height {
            break;
        }
    }
    i
}

fn dry_runs(samples: &[Sample], spans: &[Span], hamlets: &[HamletStand]) -> Vec<Vec<Vec3>> {
    let mut runs: Vec<Vec<Vec3>> = Vec::new();
    let mut cur: Vec<Vec3> = Vec::new();
    let pause = |p: Vec2| {
        let on_bridge = spans.iter().any(|s| {
            s.kind == SpanKind::Bridge
                && project_t(p, s.a, s.b).is_some_and(|t| (0.02..=0.98).contains(&t))
        });
        on_bridge || in_hamlet(p, hamlets)
    };
    for s in samples {
        if pause(s.at) {
            if cur.len() >= 2 {
                runs.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
            continue;
        }
        cur.push(Vec3::new(s.at.x, s.ground + ROAD_LIFT_M, s.at.y));
    }
    if cur.len() >= 2 {
        runs.push(cur);
    }
    runs
}

fn in_hamlet(p: Vec2, hamlets: &[HamletStand]) -> bool {
    hamlets.iter().any(|h| {
        let c = Vec2::new(h.at.x as f32, h.at.z as f32);
        p.distance(c) < h.radius
    })
}

fn project_t(p: Vec2, a: Vec2, b: Vec2) -> Option<f32> {
    let d = b - a;
    let len2 = d.length_squared();
    if len2 < 1e-6 {
        return None;
    }
    Some((p - a).dot(d) / len2)
}

fn on_deck(p: Vec2, deck: &Deck) -> Option<f32> {
    let d = deck.b - deck.a;
    let len2 = d.length_squared();
    if len2 < 1e-4 {
        return None;
    }
    let t = (p - deck.a).dot(d) / len2;
    if !(0.0..=1.0).contains(&t) {
        return None;
    }
    let closest = deck.a + d * t;
    let dist = p.distance(closest);
    if dist <= deck.width * 0.5 + 0.15 {
        Some(deck.za + (deck.zb - deck.za) * t)
    } else {
        None
    }
}

/// Seat both edges on the real ground. A centreline ridge that would poke
/// through a flat strip lifts the whole cross-section just enough to clear it.
fn drape_edges(left_ground: f32, centre_ground: f32, right_ground: f32) -> (f32, f32) {
    let mid = (left_ground + right_ground) * 0.5;
    let crown = (centre_ground - mid).max(0.0);
    (
        left_ground + crown + ROAD_LIFT_M,
        right_ground + crown + ROAD_LIFT_M,
    )
}

fn densify_xz(points: &[Vec3], step: f32) -> Vec<Vec3> {
    let mut out = Vec::new();
    if points.is_empty() {
        return out;
    }
    out.push(points[0]);
    for pair in points.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let dx = b.x - a.x;
        let dz = b.z - a.z;
        let len = (dx * dx + dz * dz).sqrt();
        let n = ((len / step).ceil() as usize).max(1);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            out.push(Vec3::new(a.x + dx * t, 0.0, a.z + dz * t));
        }
    }
    out
}

fn add_ribbon(
    mesh: &mut Mesh,
    origin: Vec3,
    centreline: &[Vec3],
    width: f32,
    color: Color,
    surface: &ContinentalSurface,
    ponds: &PondField,
) -> EngineResult<()> {
    let points = densify_xz(centreline, RIBBON_STEP_M);
    if points.len() < 2 {
        return Ok(());
    }
    let half = width * 0.5;
    let mut left = Vec::new();
    let mut right = Vec::new();
    for i in 0..points.len() {
        let p = points[i];
        let tangent = if i + 1 < points.len() {
            Vec2::new(points[i + 1].x - p.x, points[i + 1].z - p.z).normalize_or_zero()
        } else {
            Vec2::new(p.x - points[i - 1].x, p.z - points[i - 1].z).normalize_or_zero()
        };
        let perp = Vec2::new(-tangent.y, tangent.x);
        let centre = Vec2::new(p.x, p.z);
        let left_at = centre - perp * half;
        let right_at = centre + perp * half;
        let lg = column_at(surface, ponds, left_at).ground();
        let cg = column_at(surface, ponds, centre).ground();
        let rg = column_at(surface, ponds, right_at).ground();
        let (ly, ry) = drape_edges(lg, cg, rg);
        left.push(Vec3::new(left_at.x, ly, left_at.y) - origin);
        right.push(Vec3::new(right_at.x, ry, right_at.y) - origin);
    }
    for i in 0..left.len() - 1 {
        let a = mesh.add_point(left[i])?;
        mesh.set_point_color(a, color)?;
        let b = mesh.add_point(right[i])?;
        mesh.set_point_color(b, color)?;
        let c = mesh.add_point(right[i + 1])?;
        mesh.set_point_color(c, color)?;
        let d = mesh.add_point(left[i + 1])?;
        mesh.set_point_color(d, color)?;
        mesh.add_quad(a, b, c, d)?;
    }
    Ok(())
}

fn add_bridge(
    mesh: &mut Mesh,
    origin: Vec3,
    span: &Span,
    surface: &ContinentalSurface,
    ponds: &PondField,
) -> EngineResult<()> {
    let along = span.b - span.a;
    let len = along.length();
    if len < MIN_SPAN_M {
        return Ok(());
    }
    let dir = along / len;
    let perp = Vec2::new(-dir.y, dir.x);
    let half = span.width * 0.5;
    let za = span.za;
    let zb = span.zb;
    let a = span.a;
    let b = span.b;

    let corners_top = [
        Vec3::new(a.x - perp.x * half, za, a.y - perp.y * half),
        Vec3::new(a.x + perp.x * half, za, a.y + perp.y * half),
        Vec3::new(b.x + perp.x * half, zb, b.y + perp.y * half),
        Vec3::new(b.x - perp.x * half, zb, b.y - perp.y * half),
    ];
    let corners_bot = [
        Vec3::new(
            a.x - perp.x * half,
            za - DECK_THICKNESS,
            a.y - perp.y * half,
        ),
        Vec3::new(
            a.x + perp.x * half,
            za - DECK_THICKNESS,
            a.y + perp.y * half,
        ),
        Vec3::new(
            b.x + perp.x * half,
            zb - DECK_THICKNESS,
            b.y + perp.y * half,
        ),
        Vec3::new(
            b.x - perp.x * half,
            zb - DECK_THICKNESS,
            b.y - perp.y * half,
        ),
    ];
    add_box_corners(mesh, origin, &corners_bot, &corners_top, deck_wood())?;

    let rail = half - PARAPET_T * 0.5;
    for sign in [-1.0_f32, 1.0] {
        let pa = Vec3::new(
            a.x + perp.x * rail * sign,
            za + PARAPET_H * 0.5,
            a.y + perp.y * rail * sign,
        );
        let pb = Vec3::new(
            b.x + perp.x * rail * sign,
            zb + PARAPET_H * 0.5,
            b.y + perp.y * rail * sign,
        );
        let along_h = Vec3::new(perp.x, 0.0, perp.y) * (PARAPET_T * 0.5);
        let up_a = Vec3::Y * (PARAPET_H * 0.5);
        let up_b = Vec3::Y * (PARAPET_H * 0.5);
        let bot = [
            pa - along_h - up_a,
            pa + along_h - up_a,
            pb + along_h - up_b,
            pb - along_h - up_b,
        ];
        let top = [
            pa - along_h + up_a,
            pa + along_h + up_a,
            pb + along_h + up_b,
            pb - along_h + up_b,
        ];
        add_box_corners(mesh, origin, &bot, &top, pier_wood())?;
    }

    let piers = ((len / PIER_SPACING).floor() as i32).max(0);
    for i in 1..=piers {
        let t = i as f32 / (piers + 1) as f32;
        let p = a.lerp(b, t);
        let z_bot = za + (zb - za) * t - DECK_THICKNESS;
        let col = column_at(surface, ponds, p);
        let foot = col.ground() - PIER_FOOT;
        let height = (z_bot - foot).max(0.6);
        let centre = Vec3::new(p.x, foot + height * 0.5, p.y) - origin;
        mesh.add_box(centre, Vec3::new(0.45, height, 0.45), pier_wood())?;
    }
    Ok(())
}

fn add_box_corners(
    mesh: &mut Mesh,
    origin: Vec3,
    bot: &[Vec3; 4],
    top: &[Vec3; 4],
    color: Color,
) -> EngineResult<()> {
    let mut ids = Vec::with_capacity(8);
    for p in bot.iter().chain(top.iter()) {
        let id = mesh.add_point(*p - origin)?;
        mesh.set_point_color(id, color)?;
        ids.push(id);
    }
    mesh.add_quad(ids[0], ids[1], ids[2], ids[3])?;
    mesh.add_quad(ids[4], ids[7], ids[6], ids[5])?;
    mesh.add_quad(ids[0], ids[4], ids[5], ids[1])?;
    mesh.add_quad(ids[1], ids[5], ids[6], ids[2])?;
    mesh.add_quad(ids[2], ids[6], ids[7], ids[3])?;
    mesh.add_quad(ids[3], ids[7], ids[4], ids[0])?;
    Ok(())
}

pub(super) fn hamlet_span(
    surface: &ContinentalSurface,
    ponds: &PondField,
    hamlet: &HamletStand,
) -> Option<Span> {
    if hamlet.houses.len() < 2 {
        return None;
    }
    let plaza = Vec2::new(hamlet.at.x as f32, hamlet.at.z as f32);
    let river = surface
        .hydro_index()
        .nearest_river(surface.hydro(), plaza)?;
    if river.dist > 220.0 {
        return None;
    }
    if river.tangent.length_squared() < 1e-6 {
        return None;
    }
    let mut left = 0u32;
    let mut right = 0u32;
    for h in &hamlet.houses {
        let p = Vec2::new(h.x as f32, h.z as f32);
        let side = (p - river.at).perp_dot(river.tangent);
        if side >= 0.0 {
            left += 1;
        } else {
            right += 1;
        }
    }
    if left == 0 || right == 0 {
        return None;
    }
    let perp = Vec2::new(-river.tangent.y, river.tangent.x);
    let mid = column_at(surface, ponds, river.at);
    let water_z = mid.ground() + mid.wetness().max(0.0);
    let bank_a = walk_to_bank(surface, ponds, river.at, perp)?;
    let bank_b = walk_to_bank(surface, ponds, river.at, -perp)?;
    let crest_a = walk_to_crest(surface, ponds, bank_a, perp, water_z);
    let crest_b = walk_to_crest(surface, ponds, bank_b, -perp, water_z);
    let ga = column_at(surface, ponds, crest_a).ground();
    let gb = column_at(surface, ponds, crest_b).ground();
    let seat = ga.min(gb);
    let a = walk_to_height(surface, ponds, bank_a, perp, crest_a, seat);
    let b = walk_to_height(surface, ponds, bank_b, -perp, crest_b, seat);
    let gap = a.distance(b);
    if !(MIN_SPAN_M..=MAX_SPAN_M).contains(&gap) {
        return None;
    }
    let deck = seat + DECK_CLEARANCE;
    Some(Span {
        a,
        b,
        za: deck,
        zb: deck,
        width: FOOT_WIDTH,
        kind: SpanKind::Bridge,
    })
}

fn walk_to_bank(
    surface: &ContinentalSurface,
    ponds: &PondField,
    start: Vec2,
    dir: Vec2,
) -> Option<Vec2> {
    let step = 2.0_f32;
    let mut last_wet = start;
    for i in 1..50 {
        let p = start + dir * (step * i as f32);
        let col = column_at(surface, ponds, p);
        if col.wetness() < -WATERLINE_MARGIN {
            return Some(last_wet.lerp(p, 0.6));
        }
        last_wet = p;
    }
    None
}

fn walk_to_crest(
    surface: &ContinentalSurface,
    ponds: &PondField,
    start: Vec2,
    dir: Vec2,
    water_z: f32,
) -> Vec2 {
    let target = water_z + BRIDGE_ABOVE_WATER;
    let mut cur = start;
    let mut h = column_at(surface, ponds, start).ground();
    for i in 1..40 {
        let p = start + dir * (2.0 * i as f32);
        let col = column_at(surface, ponds, p);
        if col.wetness() >= -WATERLINE_MARGIN {
            break;
        }
        let dist = p.distance(cur).max(0.1);
        let slope = (col.ground() - h) / dist;
        let climbing = slope >= CUT_SLOPE;
        let cap = if climbing { STEEP_BANK_M } else { BANK_CREST_M };
        if start.distance(p) > cap {
            break;
        }
        if climbing {
            cur = p;
            h = col.ground();
            continue;
        }
        if h >= target {
            break;
        }
        cur = p;
        h = col.ground();
    }
    cur
}

fn walk_to_height(
    surface: &ContinentalSurface,
    ponds: &PondField,
    start: Vec2,
    dir: Vec2,
    crest: Vec2,
    height: f32,
) -> Vec2 {
    if column_at(surface, ponds, start).ground() >= height {
        return start;
    }
    let max_d = start.distance(crest);
    let mut cur = start;
    for i in 1..40 {
        let p = start + dir * (2.0 * i as f32);
        if start.distance(p) > max_d + 0.5 {
            break;
        }
        cur = p;
        if column_at(surface, ponds, p).ground() >= height {
            break;
        }
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samp(x: f32, wet: f32, ground: f32) -> Sample {
        Sample {
            at: Vec2::new(x, 0.0),
            wet,
            ground,
        }
    }

    #[test]
    fn a_wet_gap_on_a_road_becomes_a_bridge() {
        let mut samples = Vec::new();
        for i in 0..8 {
            samples.push(samp(i as f32 * 6.0, -2.0, 10.0));
        }
        for i in 8..14 {
            samples.push(samp(i as f32 * 6.0, 2.0, 4.0));
        }
        for i in 14..20 {
            samples.push(samp(i as f32 * 6.0, -2.0, 10.0));
        }
        let spans = spans_along(&samples, 5.0);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, SpanKind::Bridge);
        assert!(spans[0].a.x < spans[0].b.x);
        assert!(spans[0].za.max(spans[0].zb) > 10.0);
    }

    #[test]
    fn a_shallow_narrow_gap_is_a_ford() {
        let mut samples = Vec::new();
        for i in 0..4 {
            samples.push(samp(i as f32 * 2.0, -1.0, 5.0));
        }
        for i in 4..7 {
            samples.push(samp(i as f32 * 2.0, 0.4, 4.6));
        }
        for i in 7..10 {
            samples.push(samp(i as f32 * 2.0, -1.0, 5.0));
        }
        let spans = spans_along(&samples, 3.6);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, SpanKind::Ford);
    }

    #[test]
    fn a_bridge_sits_on_the_high_banks_not_the_gully() {
        let heights = [
            20.0, 20.0, 20.0, 14.0, 8.0, 3.0, 3.0, 8.0, 14.0, 20.0, 20.0, 20.0,
        ];
        let wets = [
            -2.0, -2.0, -2.0, -2.0, -0.2, 2.0, 2.0, -0.2, -2.0, -2.0, -2.0, -2.0,
        ];
        let samples: Vec<Sample> = (0..12)
            .map(|i| samp(i as f32 * 6.0, wets[i], heights[i]))
            .collect();
        let spans = spans_along(&samples, 5.0);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, SpanKind::Bridge);
        assert!(
            spans[0].za.max(spans[0].zb) > 20.0,
            "deck should sit on the terrace, got {} / {}",
            spans[0].za,
            spans[0].zb
        );
        assert!(
            spans[0].za < 21.0 && spans[0].zb < 21.0,
            "deck climbed the hill behind the bank: {} / {}",
            spans[0].za,
            spans[0].zb
        );
        assert!(
            spans[0].a.x <= 12.0,
            "left abutment still in the gully at {}",
            spans[0].a.x
        );
        assert!(
            spans[0].b.x >= 54.0,
            "right abutment still in the gully at {}",
            spans[0].b.x
        );
    }

    #[test]
    fn a_bridge_does_not_climb_the_hill_behind_the_bank() {
        let heights = [
            28.0, 28.0, 20.0, 20.0, 12.0, 4.0, 4.0, 12.0, 20.0, 20.0, 28.0, 28.0,
        ];
        let wets = [
            -2.0, -2.0, -2.0, -2.0, -0.2, 2.0, 2.0, -0.2, -2.0, -2.0, -2.0, -2.0,
        ];
        let samples: Vec<Sample> = (0..12)
            .map(|i| samp(i as f32 * 6.0, wets[i], heights[i]))
            .collect();
        let spans = spans_along(&samples, 5.0);
        assert_eq!(spans.len(), 1);
        assert!(
            spans[0].za < 21.0 && spans[0].zb < 21.0,
            "deck sat on the hilltop: {} / {}",
            spans[0].za,
            spans[0].zb
        );
        assert!(spans[0].za > 19.0 && spans[0].zb > 19.0);
    }

    #[test]
    fn a_gentle_bank_is_not_the_waterline() {
        let heights = [7.0, 6.8, 6.5, 6.2, 5.9, 2.0, 2.0, 5.9, 6.2, 6.5, 6.8, 7.0];
        let wets = [
            -2.0, -1.6, -1.0, -0.5, -0.2, 2.0, 2.0, -0.2, -0.5, -1.0, -1.6, -2.0,
        ];
        let samples: Vec<Sample> = (0..12)
            .map(|i| samp(i as f32 * 6.0, wets[i], heights[i]))
            .collect();
        let spans = spans_along(&samples, 5.0);
        assert_eq!(spans.len(), 1);
        assert!(
            spans[0].za > 6.6 && spans[0].zb > 6.6,
            "deck sat on the waterline mud: {} / {}",
            spans[0].za,
            spans[0].zb
        );
        assert!(
            spans[0].a.x <= 12.0,
            "left abutment still at the edge at {}",
            spans[0].a.x
        );
        assert!(
            spans[0].b.x >= 54.0,
            "right abutment still at the edge at {}",
            spans[0].b.x
        );
    }

    #[test]
    fn a_steep_bank_bridge_covers_the_chasm_not_just_the_water() {
        let heights = [
            30.0, 26.0, 22.0, 18.0, 8.0, 3.0, 3.0, 8.0, 18.0, 22.0, 26.0, 30.0,
        ];
        let wets = [
            -3.0, -2.5, -2.0, -1.5, -0.2, 2.0, 2.0, -0.2, -1.5, -2.0, -2.5, -3.0,
        ];
        let samples: Vec<Sample> = (0..12)
            .map(|i| samp(i as f32 * 6.0, wets[i], heights[i]))
            .collect();
        let spans = spans_along(&samples, 5.0);
        assert_eq!(spans.len(), 1);
        let span = spans[0].a.distance(spans[0].b);
        assert!(
            span > 40.0,
            "deck only covered the water ({span:.0} m), not the chasm"
        );
        assert!(
            spans[0].za > 28.0,
            "deck stopped mid-slope: {}",
            spans[0].za
        );
    }

    #[test]
    fn a_valley_bridge_sits_on_the_bank_crests() {
        let heights = [
            16.0, 14.0, 12.0, 8.0, 3.0, 2.0, 2.0, 3.0, 8.0, 12.0, 14.0, 16.0,
        ];
        let wets = [
            -8.0, -6.0, -4.0, -1.0, 2.0, 3.0, 3.0, 2.0, -1.0, -4.0, -6.0, -8.0,
        ];
        let samples: Vec<Sample> = (0..12)
            .map(|i| samp(i as f32 * 6.0, wets[i], heights[i]))
            .collect();
        let spans = spans_along(&samples, 5.0);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, SpanKind::Bridge);
        assert!(
            (spans[0].za - spans[0].zb).abs() < 0.01,
            "a high bridge is a level deck, got {} / {}",
            spans[0].za,
            spans[0].zb
        );
        assert!(
            spans[0].za > 15.5,
            "deck stopped mid-slope: {}",
            spans[0].za
        );
        assert!(
            spans[0].a.distance(spans[0].b) > 40.0,
            "span was only river-wide: {}",
            spans[0].a.distance(spans[0].b)
        );
    }

    #[test]
    fn an_uneven_crossing_sits_at_the_lower_bank() {
        let heights = [
            24.0, 20.0, 16.0, 10.0, 4.0, 2.0, 2.0, 4.0, 8.0, 12.0, 12.0, 12.0,
        ];
        let wets = [
            -6.0, -5.0, -3.0, -1.5, -0.2, 2.0, 2.0, -0.2, -1.0, -4.0, -4.0, -4.0,
        ];
        let samples: Vec<Sample> = (0..12)
            .map(|i| samp(i as f32 * 6.0, wets[i], heights[i]))
            .collect();
        let spans = spans_along(&samples, 5.0);
        assert_eq!(spans.len(), 1);
        assert!(
            spans[0].za < 13.0,
            "deck followed the high bank: {}",
            spans[0].za
        );
        assert!(
            spans[0].za > 11.5,
            "deck sat below the lower crest: {}",
            spans[0].za
        );
        assert!(
            spans[0].a.x > 6.0,
            "high-side abutment walked to the peak at {}",
            spans[0].a.x
        );
    }

    #[test]
    fn parallel_spans_are_the_same_crossing() {
        let road = Span {
            a: Vec2::new(0.0, 0.0),
            b: Vec2::new(40.0, 0.0),
            za: 10.0,
            zb: 10.0,
            width: 5.0,
            kind: SpanKind::Bridge,
        };
        let foot = Span {
            a: Vec2::new(2.0, 12.0),
            b: Vec2::new(38.0, 12.0),
            za: 10.0,
            zb: 10.0,
            width: 2.2,
            kind: SpanKind::Bridge,
        };
        let decks = [Deck {
            a: road.a,
            b: road.b,
            width: road.width,
            za: road.za,
            zb: road.zb,
        }];
        assert!(near_deck(&decks, &foot));
    }

    #[test]
    fn a_continent_road_is_only_sampled_near_the_player() {
        let mut points = Vec::new();
        for i in 0..=200 {
            points.push(Vec2::new(i as f32 * 200.0, 0.0));
        }
        let focus = Vec2::new(20_000.0, 0.0);
        let pad = 990.0;
        let runs = densify_near(&points, focus, pad, 6.0);
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert!(run.len() > 10);
        assert!(
            run.len() < 400,
            "sampled the whole road: {} points",
            run.len()
        );
        for p in run {
            assert!(p.distance(focus) <= pad + 1.0);
        }
    }

    #[test]
    fn a_road_pauses_inside_a_hamlet() {
        let samples: Vec<Sample> = (0..20).map(|i| samp(i as f32 * 6.0, -2.0, 10.0)).collect();
        let hamlet = HamletStand {
            at: GlobalXZ::at(54.0, 0.0),
            radius: 18.0,
            houses: Vec::new(),
        };
        let runs = dry_runs(&samples, &[], &[hamlet]);
        assert!(
            runs.len() >= 2,
            "the ribbon should break through the village, got {} run(s)",
            runs.len()
        );
        for run in &runs {
            for p in run {
                assert!(
                    (p.x - 54.0).abs() >= 18.0 - 0.01,
                    "ribbon still inside the hamlet at x={}",
                    p.x
                );
            }
        }
    }

    #[test]
    fn a_road_sits_above_the_grass() {
        let samples: Vec<Sample> = (0..4).map(|i| samp(i as f32 * 6.0, -2.0, 10.0)).collect();
        let runs = dry_runs(&samples, &[], &[]);
        assert_eq!(runs.len(), 1);
        for p in &runs[0] {
            assert!(p.y > 10.0, "ribbon still sunk into the ground at {}", p.y);
            assert!(
                (p.y - (10.0 + ROAD_LIFT_M)).abs() < 1e-5,
                "ribbon hover was {}, expected {}",
                p.y,
                10.0 + ROAD_LIFT_M
            );
        }
    }

    #[test]
    fn a_side_slope_road_follows_both_edges() {
        let (left, right) = drape_edges(10.0, 8.0, 6.0);
        assert!((left - (10.0 + ROAD_LIFT_M)).abs() < 1e-5);
        assert!((right - (6.0 + ROAD_LIFT_M)).abs() < 1e-5);
    }

    #[test]
    fn a_crowned_road_clears_the_centre_bump() {
        let (left, right) = drape_edges(6.0, 10.0, 6.0);
        assert!(
            left >= 10.0 + ROAD_LIFT_M - 1e-5,
            "left edge still under the bump: {left}"
        );
        assert!(
            right >= 10.0 + ROAD_LIFT_M - 1e-5,
            "right edge still under the bump: {right}"
        );
    }

    #[test]
    fn a_ribbon_is_sampled_finer_than_the_terrain_grid() {
        let line = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(6.0, 0.0, 0.0)];
        let dense = densify_xz(&line, RIBBON_STEP_M);
        assert!(
            dense.len() >= 4,
            "6 m chord was not split onto the 4 m grass grid: {} points",
            dense.len()
        );
        assert!((dense[0].x - 0.0).abs() < 1e-5);
        assert!((dense[dense.len() - 1].x - 6.0).abs() < 1e-5);
    }
}
