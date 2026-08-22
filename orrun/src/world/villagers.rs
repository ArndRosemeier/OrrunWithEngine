//! Tier-0 hamlet people: clipped dressed humans on the cut, doors, and alley.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use engine::anim::AnimatedModel;
use engine::error::{EngineError, EngineResult};
use engine::place::Place;
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use glam::Vec2;

use super::footprint::{BuildingPlot, HousePlot};
use super::settlement::{HouseDoor, HamletStand};
use super::surface::ContinentalSurface;
use super::world_stream::WorldStream;

const FOOT_CLEARANCE_M: f32 = 0.05;
/// Human walk, not a sprint. Metres per second along the cut.
const WALK_MPS: f32 = 1.35;
/// Hitch frames must not teleport a walker; 20 Hz is still a walk.
const MAX_STEP_DT: f32 = 1.0 / 20.0;
/// Mixamo in-place Walk reads ~1.5 m/s at 1.0; scale to WALK_MPS.
const WALK_ANIM_RATE: f32 = 0.90;
const IDLE_ANIM_RATE: f32 = 1.0;
const END_PAUSE_S: f32 = 0.9;
const CORRIDOR_M: f32 = 2.5;
const MIN_PEOPLE: usize = 5;
const MAX_PEOPLE: usize = 8;
const WORKSUIT_GLB: &str = "humans/male_dressed_male_worksuit01.glb";
const CASUAL_GLB: &str = "humans/female_dressed_female_casualsuit01.glb";
const IDLE_CLIPS: &[&str] = &["Idle", "Idle_Loop"];
const WALK_CLIPS: &[&str] = &["Walk", "Walk_Loop"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Walker,
    Lingerer,
    Cutter,
}

struct Person {
    entity: EntityId,
    hamlet: GlobalXZ,
    role: Role,
    pos: GlobalPosition,
    yaw: f32,
    clip: String,
    walking: bool,
    /// 0 = rim, 1 = well. Walkers ping-pong along the cut.
    along: f32,
    dir: f32,
    pause_left: f32,
    pace_t: f32,
    speed_mps: f32,
}

/// People standing and walking in seated tier-0 hamlets.
pub struct VillagerLayer {
    people: Vec<Person>,
    spawned: HashSet<(i64, i64)>,
    worksuit: Option<Arc<AnimatedModel>>,
    casual: Option<Arc<AnimatedModel>>,
}

impl VillagerLayer {
    pub fn new() -> Self {
        Self {
            people: Vec::new(),
            spawned: HashSet::new(),
            worksuit: None,
            casual: None,
        }
    }

    pub fn clear(&mut self, world: &mut World) -> EngineResult<()> {
        for person in self.people.drain(..) {
            world.despawn(person.entity);
        }
        self.spawned.clear();
        Ok(())
    }

    pub fn human_count(&self) -> usize {
        self.people.len()
    }

    pub fn walk_mps() -> f32 {
        WALK_MPS
    }

    pub fn walker_speed_mps(&self) -> Option<f32> {
        self.people
            .iter()
            .find(|p| p.role == Role::Walker && p.walking)
            .map(|p| p.speed_mps)
    }

    pub fn entities(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.people.iter().map(|p| p.entity)
    }

    pub fn human_on_corridor(&self, cut: &[Vec2]) -> bool {
        self.corridor_human(cut).is_some()
    }

    pub fn corridor_human(&self, cut: &[Vec2]) -> Option<GlobalPosition> {
        if cut.len() < 2 {
            return None;
        }
        // Prefer a walker in the yard (near the well end). Far spur walkers
        // made village.png a riverside skyline instead of lived-in dirt.
        const YARD_M: f32 = 16.0;
        let plaza = *cut.last().unwrap();
        self.people
            .iter()
            .filter_map(|p| {
                let q = Vec2::new(p.pos.x as f32, p.pos.z as f32);
                if dist_to_polyline(q, cut) < CORRIDOR_M {
                    let d = q.distance(plaza);
                    if d <= YARD_M {
                        Some((d, p.pos))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, pos)| pos)
    }

    pub fn mesh_count(&self, world: &World) -> usize {
        let animated: Vec<EntityId> = world.animated_entities().map(|(id, _)| *id).collect();
        self.people
            .iter()
            .filter(|p| animated.iter().any(|id| *id == p.entity) || world.entity(p.entity).is_ok())
            .count()
    }

    pub fn follow(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        surface: &ContinentalSurface,
        hamlets: &[HamletStand],
        doors: &[HouseDoor],
        plots: &[BuildingPlot],
        dt: f32,
    ) -> EngineResult<()> {
        self.ensure_models();
        self.drop_gone(world, hamlets);
        let models_ready = self.worksuit.is_some() && self.casual.is_some();
        for hamlet in hamlets {
            if !is_hamlet_pin(surface, hamlet.at) {
                continue;
            }
            if hamlet.cut.len() < 2 {
                continue;
            }
            let key = stand_key(hamlet.at);
            if self.spawned.contains(&key) {
                continue;
            }
            // Staggered GLB load: do not mark the hamlet spawned until both
            // outfits exist, or a cold spawn at the pin permanently skips people.
            if !models_ready {
                continue;
            }
            let local_doors: Vec<&HouseDoor> = doors
                .iter()
                .filter(|d| hamlet.covers(d.house_at, 0.0))
                .collect();
            if local_doors.is_empty() {
                continue;
            }
            self.spawn_hamlet(world, stream, hamlet, &local_doors, plots)?;
            self.spawned.insert(key);
        }
        self.tick(world, stream, hamlets, dt)?;
        Ok(())
    }

    fn ensure_models(&mut self) {
        // One GLB per follow: full UAL bakes are ~20 MB and a double load on the
        // first village visit was a multi-second hitch (looked like floating seats).
        if self.worksuit.is_none() {
            match load_human(WORKSUIT_GLB) {
                Ok(model) => self.worksuit = Some(model),
                Err(err) => eprintln!("villagers: worksuit skipped: {err}"),
            }
            return;
        }
        if self.casual.is_none() {
            match load_human(CASUAL_GLB) {
                Ok(model) => self.casual = Some(model),
                Err(err) => eprintln!("villagers: casual skipped: {err}"),
            }
        }
    }

    fn drop_gone(&mut self, world: &mut World, hamlets: &[HamletStand]) {
        let live: HashSet<(i64, i64)> = hamlets.iter().map(|h| stand_key(h.at)).collect();
        let mut i = 0;
        while i < self.people.len() {
            let key = stand_key(self.people[i].hamlet);
            if live.contains(&key) {
                i += 1;
                continue;
            }
            let person = self.people.swap_remove(i);
            world.despawn(person.entity);
            self.spawned.remove(&key);
        }
    }

    fn spawn_hamlet(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        hamlet: &HamletStand,
        doors: &[&HouseDoor],
        plots: &[BuildingPlot],
    ) -> EngineResult<()> {
        let casual = self.casual.clone();
        let worksuit = self.worksuit.clone();
        let mut spawned = 0usize;

        let walker_n = if hamlet.cut.len() >= 2 { 2 } else { 0 };
        let walker_ts = dry_cut_ts(&hamlet.cut, stream, hamlet, walker_n);
        for t in walker_ts {
            let Some(model) = casual.clone() else {
                break;
            };
            let Some((pos, yaw)) = point_on_cut(&hamlet.cut, t, 1.0, stream) else {
                continue;
            };
            if self.spawn_person(
                world,
                model,
                hamlet.at,
                Role::Walker,
                pos,
                yaw,
                t,
                1.0,
                true,
            )? {
                spawned += 1;
            }
        }

        let linger_n = doors.len().min(2);
        for door in doors.iter().take(linger_n) {
            let Some(model) = casual.clone() else {
                break;
            };
            let place = door.opening_out();
            let at = place.position;
            let Some(y) = stream.contact_height(GlobalXZ::at(at.x, at.z)) else {
                continue;
            };
            let pos = GlobalPosition::at(at.x, f64::from(y + FOOT_CLEARANCE_M), at.z);
            if self.spawn_person(
                world,
                model,
                hamlet.at,
                Role::Lingerer,
                pos,
                place.yaw_degrees,
                0.0,
                0.0,
                false,
            )? {
                spawned += 1;
            }
        }

        if let Some((at, yaw)) = alley_spot(hamlet, plots) {
            if let Some(model) = worksuit {
                if let Some(y) = stream.contact_height(at) {
                    let pos = GlobalPosition::at(at.x, f64::from(y + FOOT_CLEARANCE_M), at.z);
                    if self.spawn_person(
                        world,
                        model,
                        hamlet.at,
                        Role::Cutter,
                        pos,
                        yaw,
                        0.0,
                        0.0,
                        false,
                    )? {
                        spawned += 1;
                    }
                }
            }
        }

        for door in doors.iter().skip(linger_n) {
            if spawned >= MIN_PEOPLE.max(6).min(MAX_PEOPLE) {
                break;
            }
            let Some(model) = casual.clone() else {
                break;
            };
            let place = door.opening_out();
            let at = place.position;
            let Some(y) = stream.contact_height(GlobalXZ::at(at.x, at.z)) else {
                continue;
            };
            let pos = GlobalPosition::at(at.x, f64::from(y + FOOT_CLEARANCE_M), at.z);
            if self.spawn_person(
                world,
                model,
                hamlet.at,
                Role::Lingerer,
                pos,
                place.yaw_degrees,
                0.0,
                0.0,
                false,
            )? {
                spawned += 1;
            }
        }

        let mut extra_t = 0.35;
        while spawned < MIN_PEOPLE {
            let Some(model) = casual.clone() else {
                break;
            };
            let Some((pos, yaw)) = point_on_cut(&hamlet.cut, extra_t, 1.0, stream) else {
                break;
            };
            if self.spawn_person(
                world,
                model,
                hamlet.at,
                Role::Lingerer,
                pos,
                yaw,
                extra_t,
                0.0,
                false,
            )? {
                spawned += 1;
            } else {
                break;
            }
            extra_t = (extra_t + 0.18).min(0.9);
            if spawned >= MAX_PEOPLE {
                break;
            }
        }
        Ok(())
    }

    fn spawn_person(
        &mut self,
        world: &mut World,
        model: Arc<AnimatedModel>,
        hamlet: GlobalXZ,
        role: Role,
        pos: GlobalPosition,
        yaw: f32,
        along: f32,
        dir: f32,
        walking: bool,
    ) -> EngineResult<bool> {
        let want = if walking { WALK_CLIPS } else { IDLE_CLIPS };
        let Some(clip) = first_clip(&model, want) else {
            eprintln!("villagers: missing {:?} clip, skipped", role);
            return Ok(false);
        };
        let place = place_of(world, pos, yaw)?;
        let entity = world.spawn_animated_shared(model, place)?;
        world.play_animation(entity, &clip)?;
        world.set_animation_speed(entity, if walking { WALK_ANIM_RATE } else { IDLE_ANIM_RATE })?;
        self.people.push(Person {
            entity,
            hamlet,
            role,
            pos,
            yaw,
            clip,
            walking,
            along,
            dir,
            pause_left: 0.0,
            pace_t: 0.0,
            speed_mps: 0.0,
        });
        Ok(true)
    }

    fn tick(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        hamlets: &[HamletStand],
        dt: f32,
    ) -> EngineResult<()> {
        if dt <= 0.0 {
            return Ok(());
        }
        for i in 0..self.people.len() {
            let hamlet_at = self.people[i].hamlet;
            let Some(hamlet) = hamlets.iter().find(|h| near_xz(h.at, hamlet_at)) else {
                continue;
            };
            match self.people[i].role {
                Role::Walker => self.tick_walker(world, stream, i, &hamlet.cut, dt)?,
                Role::Cutter => self.tick_cutter(world, stream, i, dt)?,
                Role::Lingerer => self.tick_lingerer(world, i)?,
            }
        }
        Ok(())
    }

    fn tick_walker(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        i: usize,
        cut: &[Vec2],
        dt: f32,
    ) -> EngineResult<()> {
        if cut.len() < 2 {
            return Ok(());
        }
        let dt = dt.min(MAX_STEP_DT);
        if self.people[i].pause_left > 0.0 {
            self.people[i].pause_left -= dt;
            self.people[i].speed_mps = 0.0;
            if self.people[i].walking {
                self.set_clip(world, i, false)?;
            }
            return self.sync_place(world, i);
        }
        let len = polyline_len(cut).max(0.5);
        let step = (WALK_MPS * dt) / len;
        let prev = self.people[i].pos;
        self.people[i].along += self.people[i].dir * step;
        if self.people[i].along >= 1.0 {
            self.people[i].along = 1.0;
            self.people[i].dir = -1.0;
            self.people[i].pause_left = END_PAUSE_S;
            self.set_clip(world, i, false)?;
        } else if self.people[i].along <= 0.0 {
            self.people[i].along = 0.0;
            self.people[i].dir = 1.0;
            self.people[i].pause_left = END_PAUSE_S;
            self.set_clip(world, i, false)?;
        } else {
            self.set_clip(world, i, true)?;
        }
        if let Some((pos, yaw)) = point_on_cut(cut, self.people[i].along, self.people[i].dir, stream)
        {
            self.people[i].pos = pos;
            self.people[i].yaw = yaw;
        }
        if dt > 1e-5 {
            let dx = (self.people[i].pos.x - prev.x) as f32;
            let dz = (self.people[i].pos.z - prev.z) as f32;
            self.people[i].speed_mps = (dx * dx + dz * dz).sqrt() / dt;
        }
        self.sync_place(world, i)
    }

    fn tick_cutter(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        i: usize,
        dt: f32,
    ) -> EngineResult<()> {
        self.people[i].pace_t += dt;
        let home = self.people[i].pos;
        let yaw0 = self.people[i].yaw;
        let phase = (self.people[i].pace_t * 0.35).sin();
        let rad = yaw0.to_radians();
        let dx = f64::from(rad.sin()) * f64::from(phase) * 0.7;
        let dz = f64::from(rad.cos()) * f64::from(phase) * 0.7;
        let at = GlobalXZ::at(home.x + dx, home.z + dz);
        if let Some(y) = stream.contact_height(at) {
            self.people[i].pos = GlobalPosition::at(at.x, f64::from(y + FOOT_CLEARANCE_M), at.z);
        }
        let moving = phase.abs() > 0.25;
        self.set_clip(world, i, moving)?;
        self.sync_place(world, i)
    }

    fn tick_lingerer(&mut self, world: &mut World, i: usize) -> EngineResult<()> {
        self.set_clip(world, i, false)?;
        self.sync_place(world, i)
    }

    fn set_clip(&mut self, world: &mut World, i: usize, walking: bool) -> EngineResult<()> {
        if self.people[i].walking == walking {
            return Ok(());
        }
        let entity = self.people[i].entity;
        let model_clip = self.people[i].clip.clone();
        let want = if walking { WALK_CLIPS } else { IDLE_CLIPS };
        // Clip name on the person is the last played clip; pick a sibling from the same set.
        let clip = if walking {
            if WALK_CLIPS.contains(&model_clip.as_str()) {
                model_clip
            } else {
                want[0].to_string()
            }
        } else if IDLE_CLIPS.contains(&model_clip.as_str()) {
            model_clip
        } else {
            want[0].to_string()
        };
        world.play_animation(entity, &clip)?;
        world.set_animation_speed(
            entity,
            if walking {
                WALK_ANIM_RATE
            } else {
                IDLE_ANIM_RATE
            },
        )?;
        self.people[i].clip = clip;
        self.people[i].walking = walking;
        Ok(())
    }

    fn sync_place(&self, world: &mut World, i: usize) -> EngineResult<()> {
        let place = place_of(world, self.people[i].pos, self.people[i].yaw)?;
        world.set_place(self.people[i].entity, place)
    }
}

impl Default for VillagerLayer {
    fn default() -> Self {
        Self::new()
    }
}

fn load_human(rel: &str) -> EngineResult<Arc<AnimatedModel>> {
    let assets = assets_dir()?;
    let path = assets.join(rel);
    if !path.is_file() {
        return Err(EngineError::Model(format!(
            "human mesh missing at {}",
            path.display()
        )));
    }
    let root = path.parent().unwrap_or(&assets).to_path_buf();
    let mut model = AnimatedModel::load_with(&path, &root, &engine::EngineLimits::default())?;
    dress_human_meshes(&mut model);
    Ok(Arc::new(model))
}

/// Force opaque albedo/vertex alpha so clothes do not punch through as holes.
fn dress_human_meshes(model: &mut AnimatedModel) {
    let mut tris = Vec::new();
    for mesh in &model.meshes {
        let tri_count = mesh.indices.len() / 3;
        tris.push(tri_count);
        let (mut mn, mut mx) = (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN));
        for p in &mesh.positions {
            mn = mn.min(*p);
            mx = mx.max(*p);
        }
        let albedo = mesh
            .albedo
            .as_ref()
            .map(|a| format!("{}x{}", a.width, a.height))
            .unwrap_or_else(|| "none".into());
        eprintln!(
            "human mesh tris={tri_count} aabb=({:.3},{:.3},{:.3})-({:.3},{:.3},{:.3}) albedo={albedo}",
            mn.x, mn.y, mn.z, mx.x, mx.y, mx.z
        );
    }
    eprintln!("human meshes={} tris={:?}", model.meshes.len(), tris);

    for mesh in &mut model.meshes {
        for color in &mut mesh.colors {
            color.w = 1.0;
        }
        if let Some(albedo) = &mut mesh.albedo {
            for px in albedo.rgba.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
    }
}

fn assets_dir() -> EngineResult<PathBuf> {
    let mut tried = Vec::new();
    if let Some(dir) = std::env::var_os("ORRUN_ASSETS") {
        tried.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tried.push(dir.join("assets"));
        }
    }
    tried.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"));
    for root in &tried {
        if root.is_dir() {
            return Ok(root.clone());
        }
    }
    Err(EngineError::Model(format!(
        "no assets under {}",
        tried
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    )))
}

fn first_clip(model: &AnimatedModel, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find(|n| model.find_clip(n).is_some())
        .map(|n| (*n).to_string())
}

fn place_of(world: &World, pos: GlobalPosition, yaw: f32) -> EngineResult<Place> {
    let render = world.to_render(pos)?;
    Place::at(render.x, render.y, render.z)?
        .yaw_deg(yaw)?
        .scale(1.0)
}

fn stand_key(at: GlobalXZ) -> (i64, i64) {
    (at.x.round() as i64, at.z.round() as i64)
}

fn near_xz(a: GlobalXZ, b: GlobalXZ) -> bool {
    (a.x - b.x).abs() < 0.75 && (a.z - b.z).abs() < 0.75
}

fn is_hamlet_pin(surface: &ContinentalSurface, at: GlobalXZ) -> bool {
    surface.settlements().iter().any(|pin| {
        pin.tier <= 1 && (pin.at.x - at.x).abs() < 0.75 && (pin.at.z - at.z).abs() < 0.75
    })
}


fn dry_cut_ts(cut: &[Vec2], stream: &WorldStream, hamlet: &HamletStand, want: usize) -> Vec<f32> {
    if cut.len() < 2 || want == 0 {
        return Vec::new();
    }
    let a = cut[0];
    let b = cut[cut.len() - 1];
    let mut inside = Vec::new();
    let mut any = Vec::new();
    for i in 1..40 {
        let t = i as f32 / 40.0;
        let p = a.lerp(b, t);
        let xz = GlobalXZ::at(f64::from(p.x), f64::from(p.y));
        if stream.contact_height(xz).is_none() {
            continue;
        }
        any.push(t);
        if hamlet.covers(xz, 0.0) {
            inside.push(t);
        }
    }
    let src = if inside.len() >= want { inside } else { any };
    if src.len() >= want {
        // spread walkers along the packed street, not the forest rim
        let last = src.len() - 1;
        (0..want)
            .map(|i| src[last * (i + 1) / (want + 1)])
            .collect()
    } else if want >= 2 {
        vec![0.35, 0.65]
    } else {
        vec![0.5]
    }
}

fn point_on_cut(
    cut: &[Vec2],
    t: f32,
    dir: f32,
    stream: &WorldStream,
) -> Option<(GlobalPosition, f32)> {
    let t = t.clamp(0.0, 1.0);
    let p = sample_polyline(cut, t)?;
    let sign = if dir >= 0.0 { 1.0 } else { -1.0 };
    let look_t = (t + 0.02 * sign).clamp(0.0, 1.0);
    let ahead = sample_polyline(cut, look_t).unwrap_or(p);
    let mut d = ahead - p;
    if d.length_squared() <= 1e-6 {
        let back_t = (t - 0.02 * sign).clamp(0.0, 1.0);
        let back = sample_polyline(cut, back_t).unwrap_or(p);
        d = p - back;
    }
    let yaw = if d.length_squared() > 1e-6 {
        d.x.atan2(d.y).to_degrees()
    } else {
        0.0
    };
    let xz = GlobalXZ::at(f64::from(p.x), f64::from(p.y));
    let y = stream.contact_height(xz)?;
    Some((
        GlobalPosition::at(xz.x, f64::from(y + FOOT_CLEARANCE_M), xz.z),
        yaw,
    ))
}

fn sample_polyline(cut: &[Vec2], t: f32) -> Option<Vec2> {
    if cut.len() < 2 {
        return None;
    }
    let total = polyline_len(cut);
    if total < 1e-4 {
        return Some(cut[0]);
    }
    let mut left = total * t.clamp(0.0, 1.0);
    for w in cut.windows(2) {
        let seg = w[1] - w[0];
        let len = seg.length();
        if len < 1e-5 {
            continue;
        }
        if left <= len {
            return Some(w[0] + seg * (left / len));
        }
        left -= len;
    }
    cut.last().copied()
}

fn polyline_len(cut: &[Vec2]) -> f32 {
    cut.windows(2).map(|w| w[0].distance(w[1])).sum()
}

pub(super) fn dist_to_polyline(p: Vec2, cut: &[Vec2]) -> f32 {
    let mut best = f32::MAX;
    for w in cut.windows(2) {
        best = best.min(dist_point_seg(p, w[0], w[1]));
    }
    best
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

fn alley_spot(hamlet: &HamletStand, plots: &[BuildingPlot]) -> Option<(GlobalXZ, f32)> {
    let houses: Vec<HousePlot> = plots
        .iter()
        .filter_map(|p| match p {
            BuildingPlot::House(h) if hamlet.covers(h.at, 0.0) => Some(*h),
            _ => None,
        })
        .collect();
    let mut best: Option<(f32, GlobalXZ, f32)> = None;
    for (i, a) in houses.iter().enumerate() {
        for b in houses.iter().skip(i + 1) {
            let dx = (b.at.x - a.at.x) as f32;
            let dz = (b.at.z - a.at.z) as f32;
            let d = (dx * dx + dz * dz).sqrt();
            let gap = d - a.half_x - b.half_x;
            if !(0.4..=3.5).contains(&gap) {
                continue;
            }
            let mid = GlobalXZ::at((a.at.x + b.at.x) * 0.5, (a.at.z + b.at.z) * 0.5);
            if houses.iter().any(|h| h.contains_xz(mid)) {
                continue;
            }
            let yaw = dx.atan2(dz).to_degrees();
            if best.map(|(bd, _, _)| gap < bd).unwrap_or(true) {
                best = Some((gap, mid, yaw));
            }
        }
    }
    if let Some((_, at, yaw)) = best {
        return Some((at, yaw));
    }
    let h = houses.first()?;
    let (s, c) = h.yaw.sin_cos();
    let side = f64::from(h.half_x + 0.6 + 0.8);
    let at = GlobalXZ::at(h.at.x + f64::from(c) * side, h.at.z + f64::from(s) * side);
    if hamlet.covers(at, 0.0) && !houses.iter().any(|o| o.contains_xz(at)) {
        Some((at, h.yaw.to_degrees()))
    } else {
        Some((at, h.yaw.to_degrees()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_on_the_cut_is_on_the_corridor() {
        let cut = [Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0)];
        assert!(dist_to_polyline(Vec2::new(5.0, 1.0), &cut) < CORRIDOR_M);
        assert!(dist_to_polyline(Vec2::new(5.0, 8.0), &cut) > CORRIDOR_M);
    }

    #[test]
    fn polyline_samples_reach_the_well() {
        let cut = [Vec2::new(0.0, 4.0), Vec2::new(10.0, 0.0)];
        let well = sample_polyline(&cut, 1.0).expect("well");
        assert!((well - cut[1]).length() < 1e-4);
        let rim = sample_polyline(&cut, 0.0).expect("rim");
        assert!((rim - cut[0]).length() < 1e-4);
    }

    #[test]
    fn walker_step_is_a_human_walk() {
        assert!((1.2..=1.5).contains(&WALK_MPS));
        let cut = [Vec2::new(0.0, 0.0), Vec2::new(40.0, 0.0)];
        let len = polyline_len(&cut);
        let dt: f32 = 1.0 / 60.0;
        let step = (WALK_MPS * dt.min(MAX_STEP_DT)) / len;
        let moved = step * len;
        assert!((moved - WALK_MPS * dt).abs() < 1e-5);
    }
}
