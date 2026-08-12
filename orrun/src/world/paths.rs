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

use super::brooks::{BrookDetail, BrookField};
use super::settlement::HamletStand;
use super::surface::{ContinentalSurface, SurfaceColumn};
use crate::hamlet::WATERLINE_MARGIN;

const REACH_M: f64 = 900.0;
const RESEED_M: f64 = 80.0;
const SAMPLE_M: f32 = 6.0;
const ROAD_SINK_M: f32 = 0.05;
const PRIMARY_WIDTH: f32 = 5.2;
const SECONDARY_WIDTH: f32 = 3.6;
const FOOT_WIDTH: f32 = 2.2;
const FORD_MAX_GAP_M: f32 = 8.0;
const FORD_MAX_WET_M: f32 = 0.9;
const MIN_SPAN_M: f32 = 4.0;
const MAX_SPAN_M: f32 = 140.0;
const DECK_THICKNESS: f32 = 0.45;
const DECK_FREEBOARD: f32 = 0.85;
const BANK_CREST_M: f32 = 48.0;
const PARAPET_H: f32 = 0.72;
const PARAPET_T: f32 = 0.22;
const PIER_SPACING: f32 = 10.0;
const PIER_FOOT: f32 = 1.4;
const BANK_DRY: f32 = -0.45;

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
    pub deck_z: f32,
    pub width: f32,
    pub kind: SpanKind,
}

struct Deck {
    a: Vec2,
    b: Vec2,
    width: f32,
    z: f32,
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
}

impl PathLayer {
    pub fn new() -> Self {
        Self {
            entity: None,
            decks: Vec::new(),
            centre: None,
            resident_chunks: 0,
            pending: None,
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
        brooks: &Arc<BrookField>,
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
        let wanted = moved >= RESEED_M || (resident != self.resident_chunks && walked_pending == 0);
        if !wanted || self.pending.is_some() {
            return Ok(changed);
        }

        let surface = Arc::clone(surface);
        let brooks = Arc::clone(brooks);
        let hamlets = hamlets.to_vec();
        self.pending = Some(Pending {
            focus,
            resident_chunks: resident,
            job: std::thread::Builder::new()
                .name("paths".into())
                .spawn(move || bake_paths(&surface, &brooks, &hamlets, focus))
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
    brooks: &BrookField,
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
        for samples in sample_runs(surface, brooks, &road.points, focus2) {
            let spans = spans_along(&samples, width);
            let dry = dry_runs(&samples, &spans);
            for run in dry {
                add_ribbon(&mut mesh, origin, &run, width, road_dirt())?;
            }
            for span in spans {
                if span.kind == SpanKind::Ford {
                    continue;
                }
                if near_deck(&decks, &span) {
                    continue;
                }
                add_bridge(&mut mesh, origin, &span, surface, brooks)?;
                decks.push(Deck {
                    a: span.a,
                    b: span.b,
                    width: span.width,
                    z: span.deck_z,
                });
            }
        }
    }

    for hamlet in hamlets {
        if let Some(span) = hamlet_span(surface, brooks, hamlet) {
            if span.kind == SpanKind::Bridge && !near_deck(&decks, &span) {
                add_bridge(&mut mesh, origin, &span, surface, brooks)?;
                decks.push(Deck {
                    a: span.a,
                    b: span.b,
                    width: span.width,
                    z: span.deck_z,
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

fn mid(a: Vec2, b: Vec2) -> Vec2 {
    (a + b) * 0.5
}

fn near_deck(decks: &[Deck], span: &Span) -> bool {
    decks
        .iter()
        .any(|d| (mid(d.a, d.b) - mid(span.a, span.b)).length() < 18.0)
}

#[derive(Clone, Copy)]
struct Sample {
    at: Vec2,
    wet: f32,
    ground: f32,
}

fn column_at(surface: &ContinentalSurface, brooks: &BrookField, p: Vec2) -> SurfaceColumn {
    let g = GlobalXZ::at(f64::from(p.x), f64::from(p.y));
    let mut column = surface.column(g);
    brooks.carve(g, &mut column, BrookDetail::Channels);
    column
}

fn sample_runs(
    surface: &ContinentalSurface,
    brooks: &BrookField,
    points: &[Vec2],
    focus: Vec2,
) -> Vec<Vec<Sample>> {
    let pad = REACH_M as f32 + MAX_SPAN_M;
    densify_near(points, focus, pad, SAMPLE_M)
        .into_iter()
        .map(|run| {
            run.into_iter()
                .map(|at| {
                    let col = column_at(surface, brooks, at);
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
        let (a_at, b_at, deck_z) = if kind == SpanKind::Bridge {
            let ia = walk_crest(samples, start - 1, -1);
            let ib = walk_crest(samples, i, 1);
            let crest_gap = samples[ia].at.distance(samples[ib].at);
            if (MIN_SPAN_M..=MAX_SPAN_M).contains(&crest_gap) {
                (
                    samples[ia].at,
                    samples[ib].at,
                    samples[ia].ground.max(samples[ib].ground) + DECK_FREEBOARD,
                )
            } else {
                (a.at, b.at, a.ground.max(b.ground) + DECK_FREEBOARD)
            }
        } else {
            (a.at, b.at, a.ground.max(b.ground) + DECK_FREEBOARD)
        };
        spans.push(Span {
            a: a_at,
            b: b_at,
            deck_z,
            width,
            kind,
        });
    }
    spans
}

fn walk_crest(samples: &[Sample], from: usize, step: isize) -> usize {
    let mut i = from;
    let mut best = from;
    let mut best_h = samples[from].ground;
    let origin = samples[from].at;
    loop {
        let next = i as isize + step;
        if next < 0 || next >= samples.len() as isize {
            break;
        }
        let n = next as usize;
        if samples[n].wet >= -WATERLINE_MARGIN {
            break;
        }
        if samples[n].at.distance(origin) > BANK_CREST_M {
            break;
        }
        let h = samples[n].ground;
        if h > best_h + 0.08 {
            best = n;
            best_h = h;
        } else if h < best_h - 0.35 {
            break;
        }
        i = n;
    }
    best
}

fn dry_runs(samples: &[Sample], spans: &[Span]) -> Vec<Vec<(Vec3, Vec3)>> {
    let mut runs: Vec<Vec<(Vec3, Vec3)>> = Vec::new();
    let mut cur: Vec<(Vec3, Vec3)> = Vec::new();
    let bridge_wet = |p: Vec2| {
        spans.iter().any(|s| {
            s.kind == SpanKind::Bridge && project_t(p, s.a, s.b).is_some_and(|t| (0.02..=0.98).contains(&t))
        })
    };
    for s in samples {
        if bridge_wet(s.at) {
            if cur.len() >= 2 {
                runs.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
            continue;
        }
        cur.push((
            Vec3::new(s.at.x, s.ground - ROAD_SINK_M, s.at.y),
            Vec3::ZERO,
        ));
    }
    if cur.len() >= 2 {
        runs.push(cur);
    }
    runs
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
        Some(deck.z)
    } else {
        None
    }
}

fn add_ribbon(
    mesh: &mut Mesh,
    origin: Vec3,
    centreline: &[(Vec3, Vec3)],
    width: f32,
    color: Color,
) -> EngineResult<()> {
    if centreline.len() < 2 {
        return Ok(());
    }
    let half = width * 0.5;
    let mut left = Vec::new();
    let mut right = Vec::new();
    for i in 0..centreline.len() {
        let p = centreline[i].0;
        let tangent = if i + 1 < centreline.len() {
            let n = centreline[i + 1].0 - p;
            Vec2::new(n.x, n.z).normalize_or_zero()
        } else {
            let n = p - centreline[i - 1].0;
            Vec2::new(n.x, n.z).normalize_or_zero()
        };
        let perp = Vec3::new(-tangent.y, 0.0, tangent.x);
        left.push(p - perp * half - origin);
        right.push(p + perp * half - origin);
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
    brooks: &BrookField,
) -> EngineResult<()> {
    let along = span.b - span.a;
    let len = along.length();
    if len < MIN_SPAN_M {
        return Ok(());
    }
    let dir = along / len;
    let perp = Vec2::new(-dir.y, dir.x);
    let half = span.width * 0.5;
    let z_top = span.deck_z;
    let z_bot = z_top - DECK_THICKNESS;
    let a = span.a;
    let b = span.b;

    let corners_top = [
        Vec3::new(a.x - perp.x * half, z_top, a.y - perp.y * half),
        Vec3::new(a.x + perp.x * half, z_top, a.y + perp.y * half),
        Vec3::new(b.x + perp.x * half, z_top, b.y + perp.y * half),
        Vec3::new(b.x - perp.x * half, z_top, b.y - perp.y * half),
    ];
    let corners_bot = [
        Vec3::new(a.x - perp.x * half, z_bot, a.y - perp.y * half),
        Vec3::new(a.x + perp.x * half, z_bot, a.y + perp.y * half),
        Vec3::new(b.x + perp.x * half, z_bot, b.y + perp.y * half),
        Vec3::new(b.x - perp.x * half, z_bot, b.y - perp.y * half),
    ];
    add_box_corners(mesh, origin, &corners_bot, &corners_top, deck_wood())?;

    let rail = half - PARAPET_T * 0.5;
    for sign in [-1.0_f32, 1.0] {
        let pa = Vec3::new(
            a.x + perp.x * rail * sign,
            z_top + PARAPET_H * 0.5,
            a.y + perp.y * rail * sign,
        );
        let pb = Vec3::new(
            b.x + perp.x * rail * sign,
            z_top + PARAPET_H * 0.5,
            b.y + perp.y * rail * sign,
        );
        let mid = (pa + pb) * 0.5 - origin;
        let yaw = dir.x.atan2(dir.y).to_degrees();
        add_oriented_box(
            mesh,
            mid,
            Vec3::new(PARAPET_T, PARAPET_H, len),
            yaw,
            pier_wood(),
        )?;
    }

    let piers = ((len / PIER_SPACING).floor() as i32).max(0);
    for i in 1..=piers {
        let t = i as f32 / (piers + 1) as f32;
        let p = a.lerp(b, t);
        let col = column_at(surface, brooks, p);
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

fn add_oriented_box(
    mesh: &mut Mesh,
    centre: Vec3,
    size: Vec3,
    yaw_deg: f32,
    color: Color,
) -> EngineResult<()> {
    let yaw = yaw_deg.to_radians();
    let (s, c) = (yaw.sin(), yaw.cos());
    let hx = size.x * 0.5;
    let hy = size.y * 0.5;
    let hz = size.z * 0.5;
    let rot = |x: f32, z: f32| Vec3::new(x * c + z * s, 0.0, -x * s + z * c);
    let mut bot = [Vec3::ZERO; 4];
    let mut top = [Vec3::ZERO; 4];
    let corners = [(-hx, -hz), (hx, -hz), (hx, hz), (-hx, hz)];
    for (i, (x, z)) in corners.iter().enumerate() {
        let r = rot(*x, *z);
        bot[i] = centre + r + Vec3::new(0.0, -hy, 0.0);
        top[i] = centre + r + Vec3::new(0.0, hy, 0.0);
    }
    add_box_corners(mesh, Vec3::ZERO, &bot, &top, color)
}

pub(super) fn hamlet_span(
    surface: &ContinentalSurface,
    brooks: &BrookField,
    hamlet: &HamletStand,
) -> Option<Span> {
    if hamlet.houses.len() < 2 {
        return None;
    }
    let plaza = Vec2::new(hamlet.at.x as f32, hamlet.at.z as f32);
    let river = surface.hydro_index().nearest_river(surface.hydro(), plaza)?;
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
    let a = walk_to_crest(surface, brooks, walk_to_bank(surface, brooks, river.at, perp)?, perp);
    let b = walk_to_crest(surface, brooks, walk_to_bank(surface, brooks, river.at, -perp)?, -perp);
    let gap = a.distance(b);
    if !(MIN_SPAN_M..=MAX_SPAN_M).contains(&gap) {
        return None;
    }
    let ga = column_at(surface, brooks, a).ground();
    let gb = column_at(surface, brooks, b).ground();
    Some(Span {
        a,
        b,
        deck_z: ga.max(gb) + DECK_FREEBOARD,
        width: FOOT_WIDTH,
        kind: SpanKind::Bridge,
    })
}

fn walk_to_bank(
    surface: &ContinentalSurface,
    brooks: &BrookField,
    start: Vec2,
    dir: Vec2,
) -> Option<Vec2> {
    let step = 2.0_f32;
    let mut last_wet = start;
    for i in 1..50 {
        let p = start + dir * (step * i as f32);
        let col = column_at(surface, brooks, p);
        if col.wetness() < BANK_DRY {
            return Some(last_wet.lerp(p, 0.6));
        }
        last_wet = p;
    }
    None
}

fn walk_to_crest(
    surface: &ContinentalSurface,
    brooks: &BrookField,
    start: Vec2,
    dir: Vec2,
) -> Vec2 {
    let mut best = start;
    let mut best_h = column_at(surface, brooks, start).ground();
    for i in 1..25 {
        let p = start + dir * (2.0 * i as f32);
        if start.distance(p) > BANK_CREST_M {
            break;
        }
        let col = column_at(surface, brooks, p);
        if col.wetness() >= BANK_DRY {
            break;
        }
        let h = col.ground();
        if h > best_h + 0.08 {
            best = p;
            best_h = h;
        } else if h < best_h - 0.35 {
            break;
        }
    }
    best
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
        assert!(spans[0].deck_z > 10.0);
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
        let heights = [20.0, 20.0, 20.0, 14.0, 8.0, 3.0, 3.0, 8.0, 14.0, 20.0, 20.0, 20.0];
        let wets = [-2.0, -2.0, -2.0, -2.0, -0.2, 2.0, 2.0, -0.2, -2.0, -2.0, -2.0, -2.0];
        let samples: Vec<Sample> = (0..12)
            .map(|i| samp(i as f32 * 6.0, wets[i], heights[i]))
            .collect();
        let spans = spans_along(&samples, 5.0);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, SpanKind::Bridge);
        assert!(
            spans[0].deck_z > 20.0,
            "deck should sit on the terrace, got {}",
            spans[0].deck_z
        );
        assert!(spans[0].a.x <= 12.0, "left abutment still in the gully at {}", spans[0].a.x);
        assert!(spans[0].b.x >= 54.0, "right abutment still in the gully at {}", spans[0].b.x);
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
        assert!(run.len() < 400, "sampled the whole road: {} points", run.len());
        for p in run {
            assert!(p.distance(focus) <= pad + 1.0);
        }
    }
}
