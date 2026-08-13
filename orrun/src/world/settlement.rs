//! Settlements on the continent: lab packing, door-sill seating, no flattened pads.
//!
//! Atlas settlement nodes are pins. Each nearby pin is packed at its atlas
//! tier (hamlet … port) on a worker thread, scored against the real ground,
//! then each dwelling is a Modular medieval kit seated with its door at grade.
//! Kit plinths take the downhill air. The heightfield is not rewritten — that
//! was the Godot trap that buried doors or left houses floating.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

use engine::color::Color;
use engine::contact::ContactSnapshot;
use engine::error::{EngineError, EngineResult};
use engine::mesh::Mesh;
use engine::place::Place;
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use glam::{Vec2, Vec3};
use modular::prelude::{PieceId, PlacedMesh};
use thiserror::Error;

use super::footprint::HousePlot;
use super::ponds::PondField;
use super::surface::{ContinentalSurface, SettlementPin, SurfaceColumn};
use super::world_stream::{WorldStream, NEAR};
use crate::hamlet::kit::{self, KitError};
use crate::hamlet::{
    plan_on, spec_for, HamletError, HamletLabConfig, Plan2D, Plot, ShapeKind, DOOR_SINK_M,
};

/// How far from the player a pin still gets a layout. Sized with the walked ring.
const REACH_M: f64 = NEAR.covers_m();
/// Rebuild the standing set once the player is this far from where it was centred.
const RESEED_M: f64 = 70.0;
/// Lab ports ask for thousands of dwellings. Packing is off the game thread;
/// this still caps instance count and scatter holes in 3D.
const MAX_3D_DWELLINGS: u32 = 80;
/// Keep house footprints off the atlas road bed (half of a primary ribbon plus a wall).
const ROAD_CLEAR_M: f32 = 4.0;
/// Extra metres past the outermost house where the dirt ribbon still pauses.
const HAMLET_ROAD_PAD_M: f32 = 10.0;

#[derive(Debug, Error)]
pub enum SettlementError {
    #[error(transparent)]
    Kit(#[from] KitError),

    #[error(transparent)]
    Engine(#[from] EngineError),

    #[error(transparent)]
    Hamlet(#[from] HamletError),
}

struct PieceBatch {
    piece: PieceId,
    entity: EntityId,
    places: Vec<Place>,
}

#[derive(Clone, Copy)]
enum Kind {
    Piece(usize),
    Well,
}

struct Standing {
    kind: Kind,
    at: GlobalPosition,
    yaw_deg: f32,
}

/// One seated hamlet, in world metres.
#[derive(Clone, Debug, PartialEq)]
pub struct HamletStand {
    pub at: GlobalXZ,
    /// Disk that covers the packed houses, for roads to pause inside.
    pub radius: f32,
    pub houses: Vec<GlobalXZ>,
}

struct GroundPlot<'a> {
    surface: &'a ContinentalSurface,
    ponds: &'a PondField,
    ground: Option<&'a ContactSnapshot>,
    origin: GlobalXZ,
    roads: Vec<(Vec2, Vec2)>,
}

impl GroundPlot<'_> {
    fn world(&self, local: Vec2) -> GlobalXZ {
        GlobalXZ::at(
            self.origin.x + f64::from(local.x),
            self.origin.z + f64::from(local.y),
        )
    }

    fn column(&self, p: GlobalXZ) -> SurfaceColumn {
        let mut column = self.surface.column(p);
        self.ponds.carve(p, &mut column);
        column
    }
}

impl Plot for GroundPlot<'_> {
    fn height(&self, p: Vec2) -> f32 {
        let at = self.world(p);
        if let Some(ground) = self.ground {
            if let Some(h) = ground.height_at(at) {
                return h;
            }
        }
        self.column(at).ground()
    }

    fn wetness(&self, p: Vec2) -> f32 {
        let at = self.world(p);
        let hydro = self.column(at).wetness();
        if on_road(
            Vec2::new(at.x as f32, at.z as f32),
            &self.roads,
            ROAD_CLEAR_M,
        ) {
            hydro.max(1.0)
        } else {
            hydro
        }
    }
}

struct Packing {
    seed: i32,
    pins: Vec<SettlementPin>,
    plans: HashMap<i32, Plan2D>,
    surface: Arc<ContinentalSurface>,
    ponds: Arc<PondField>,
    ground: ContactSnapshot,
    piece_ids: Vec<PieceId>,
    recipes: Arc<HashMap<String, Vec<PlacedMesh>>>,
}

struct PackResult {
    plans: HashMap<i32, Plan2D>,
    standing: Vec<Standing>,
    hamlets: Vec<HamletStand>,
    plots: Vec<HousePlot>,
}

struct Pending {
    focus: GlobalXZ,
    resident_chunks: usize,
    job: JoinHandle<PackResult>,
}

/// Live hamlets around the player.
pub struct SettlementLayer {
    pieces: Vec<PieceBatch>,
    recipes: Arc<HashMap<String, Vec<PlacedMesh>>>,
    well: EntityId,
    well_places: Vec<Place>,
    seed: i32,
    centre: Option<GlobalXZ>,
    resident_chunks: usize,
    standing: Vec<Standing>,
    plans: HashMap<i32, Plan2D>,
    hamlets: Vec<HamletStand>,
    plots: Vec<HousePlot>,
    pending: Option<Pending>,
}

impl SettlementLayer {
    /// Upload kit piece meshes and the well once; nothing is seated yet.
    pub fn install(world: &mut World, seed: i32) -> Result<Self, SettlementError> {
        let catalog = kit::catalog();
        let recipes = kit::dwelling_recipes(&catalog)?;
        let mut pieces = Vec::new();
        for (id, _) in kit::PIECE_GLBS {
            let piece = PieceId::new(*id).unwrap_or_else(|err| panic!("{err}"));
            let mesh = kit::load_piece_mesh(&piece)?;
            pieces.push(PieceBatch {
                piece,
                entity: world.spawn_instanced(mesh),
                places: Vec::new(),
            });
        }

        let well = world.spawn_instanced(well_mesh()?);
        Ok(Self {
            pieces,
            recipes: Arc::new(recipes),
            well,
            well_places: Vec::new(),
            seed,
            centre: None,
            resident_chunks: 0,
            standing: Vec::new(),
            plans: HashMap::new(),
            hamlets: Vec::new(),
            plots: Vec::new(),
            pending: None,
        })
    }

    pub fn placed_count(&self) -> usize {
        self.hamlets.iter().map(|h| h.houses.len()).sum()
    }

    /// Seated hamlets in the current window, for footbridges across a split river.
    pub fn hamlets(&self) -> &[HamletStand] {
        &self.hamlets
    }

    /// Dwelling footprints, for interior ground caps and scatter.
    pub fn plots(&self) -> &[HousePlot] {
        &self.plots
    }

    /// A layout is packing on a worker. The loading screen waits; walking does not.
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn clear(&mut self, world: &mut World) -> EngineResult<()> {
        self.pending = None;
        for piece in &mut self.pieces {
            piece.places.clear();
            world.set_instances(piece.entity, &[])?;
        }
        self.well_places.clear();
        world.set_instances(self.well, &[])?;
        self.standing.clear();
        self.plans.clear();
        self.hamlets.clear();
        self.plots.clear();
        self.centre = None;
        self.resident_chunks = 0;
        Ok(())
    }

    /// Keep hamlets around the player. Packing and seating run on a worker;
    /// this frame only starts that job or installs one that has finished.
    /// Re-uploads instances when render space rebases.
    pub fn follow(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        surface: &Arc<ContinentalSurface>,
        ponds: &Arc<PondField>,
        focus: GlobalXZ,
        rebased: bool,
    ) -> EngineResult<bool> {
        let resident = stream.resident_count();
        let mut changed = false;

        if let Some(pending) = self.pending.take() {
            if pending.job.is_finished() {
                let bake = pending.job.join().expect("settlement thread");
                self.install_bake(bake);
                self.centre = Some(pending.focus);
                self.resident_chunks = pending.resident_chunks;
                self.stand(world)?;
                changed = true;
            } else {
                self.pending = Some(pending);
            }
        }

        if rebased && !self.standing.is_empty() {
            self.stand(world)?;
            changed = true;
        }

        let moved = self
            .centre
            .map(|c| ((c.x - focus.x).powi(2) + (c.z - focus.z).powi(2)).sqrt())
            .unwrap_or(f64::INFINITY);
        let wanted = moved >= RESEED_M
            || (resident != self.resident_chunks && stream.walked_pending_count() == 0);
        if !wanted || self.pending.is_some() {
            return Ok(changed);
        }

        let nearby = nearby_pins(surface, focus);
        if nearby.is_empty() {
            let had = !self.standing.is_empty();
            self.plans.clear();
            self.standing.clear();
            self.hamlets.clear();
            self.plots.clear();
            self.centre = Some(focus);
            self.resident_chunks = resident;
            if had {
                self.stand(world)?;
            }
            return Ok(changed || had);
        }

        let packing = Packing {
            seed: self.seed,
            pins: nearby,
            plans: self.plans.clone(),
            surface: Arc::clone(surface),
            ponds: Arc::clone(ponds),
            ground: stream.contact_snapshot(),
            piece_ids: self
                .pieces
                .iter()
                .map(|batch| batch.piece.clone())
                .collect(),
            recipes: Arc::clone(&self.recipes),
        };
        self.pending = Some(Pending {
            focus,
            resident_chunks: resident,
            job: std::thread::Builder::new()
                .name("settlements".into())
                .spawn(move || packing.pack())
                .expect("settlement thread"),
        });
        Ok(changed)
    }

    fn install_bake(&mut self, bake: PackResult) {
        self.plans = bake.plans;
        self.standing = bake.standing;
        self.hamlets = bake.hamlets;
        self.plots = bake.plots;
    }

    fn stand(&mut self, world: &mut World) -> EngineResult<()> {
        let origin = world.render_origin();
        for piece in &mut self.pieces {
            piece.places.clear();
        }
        self.well_places.clear();

        for item in &self.standing {
            let Ok(render) = item.at.to_render(origin) else {
                continue;
            };
            let at = render.vec3();
            let place = Place::new(at.x, at.y, at.z).with_yaw_deg(item.yaw_deg);
            match item.kind {
                Kind::Piece(i) => self.pieces[i].places.push(place),
                Kind::Well => self.well_places.push(place),
            }
        }
        for piece in &self.pieces {
            world.set_instances(piece.entity, &piece.places)?;
        }
        world.set_instances(self.well, &self.well_places)?;
        Ok(())
    }
}

impl Packing {
    fn pack(self) -> PackResult {
        let mut plans = self.plans;
        plans.retain(|id, _| self.pins.iter().any(|pin| pin.id == *id));
        let mut standing = Vec::new();
        let mut hamlets = Vec::new();
        let mut plots = Vec::new();
        for pin in self.pins {
            let plan = plans.entry(pin.id).or_insert_with(|| {
                layout_for(self.seed, pin, &self.surface, &self.ponds)
                    .unwrap_or_else(|err| panic!("hamlet at node {} failed: {err}", pin.id))
            });
            let mut houses = Vec::new();
            seat_plan(
                plan,
                pin,
                &self.surface,
                &self.ponds,
                &self.ground,
                &self.piece_ids,
                &self.recipes,
                &mut standing,
                &mut houses,
                &mut plots,
            );
            let radius = hamlet_radius(pin.at, &houses);
            hamlets.push(HamletStand {
                at: pin.at,
                radius,
                houses,
            });
        }
        PackResult {
            plans,
            standing,
            hamlets,
            plots,
        }
    }
}

fn nearby_pins(surface: &ContinentalSurface, focus: GlobalXZ) -> Vec<SettlementPin> {
    let reach_sq = REACH_M * REACH_M;
    surface
        .settlements()
        .iter()
        .copied()
        .filter(|pin| {
            let dx = pin.at.x - focus.x;
            let dz = pin.at.z - focus.z;
            dx * dx + dz * dz <= reach_sq
        })
        .collect()
}

fn layout_for(
    world_seed: i32,
    pin: SettlementPin,
    surface: &ContinentalSurface,
    ponds: &PondField,
) -> Result<Plan2D, HamletError> {
    let mut config = layout_config(pin);
    config.seed = plan_seed(world_seed, pin.id);
    let plot = GroundPlot {
        surface,
        ponds,
        ground: None,
        origin: pin.at,
        roads: nearby_road_segs(surface, pin.at, config.max_settle_radius + 24.0),
    };
    plan_on(&config, Some(&plot))
}

fn seat_plan(
    plan: &Plan2D,
    pin: SettlementPin,
    surface: &ContinentalSurface,
    ponds: &PondField,
    ground: &ContactSnapshot,
    piece_ids: &[PieceId],
    recipes: &HashMap<String, Vec<PlacedMesh>>,
    out: &mut Vec<Standing>,
    houses: &mut Vec<GlobalXZ>,
    plots: &mut Vec<HousePlot>,
) {
    let plot = GroundPlot {
        surface,
        ponds,
        ground: Some(ground),
        origin: pin.at,
        roads: nearby_road_segs(surface, pin.at, layout_config(pin).max_settle_radius + 24.0),
    };
    for shape in &plan.shapes {
        if shape.kind != ShapeKind::House {
            continue;
        }
        let Some(spec) = spec_for(&shape.catalog_id) else {
            panic!(
                "planned building '{}' is not in the catalog",
                shape.catalog_id
            );
        };
        let sample = crate::hamlet::sample_footprint(
            &plot,
            shape.center,
            shape.half_size.x,
            shape.half_size.y,
            shape.yaw,
        );
        let Some(seat) = crate::hamlet::seat_building(&sample, spec.foundation_m) else {
            continue;
        };
        let yaw_deg = shape.yaw.to_degrees();
        let x = pin.at.x + f64::from(shape.center.x);
        let z = pin.at.z + f64::from(shape.center.y);
        houses.push(GlobalXZ::at(x, z));

        if spec.id == "Well" {
            out.push(Standing {
                kind: Kind::Well,
                at: GlobalPosition::at(x, f64::from(seat.floor_z), z),
                yaw_deg,
            });
            continue;
        }
        if spec.is_civic() {
            continue;
        }

        let recipe = recipes
            .get(spec.id)
            .unwrap_or_else(|| panic!("dwelling '{}' has no modular recipe", spec.id));
        let floor_y = seat.floor_z - DOOR_SINK_M;
        plots.push(HousePlot {
            at: GlobalXZ::at(x, z),
            half_x: shape.half_size.x,
            half_z: shape.half_size.y,
            yaw: shape.yaw,
            floor_y,
        });
        for item in recipe {
            let p = item.place.position;
            let (dx, dz) = kit::yaw_xz(p.x, p.z, yaw_deg);
            let index = piece_ids
                .iter()
                .position(|id| *id == item.piece)
                .unwrap_or_else(|| panic!("kit piece {} was not uploaded", item.piece));
            out.push(Standing {
                kind: Kind::Piece(index),
                at: GlobalPosition::at(
                    x + f64::from(dx),
                    f64::from(floor_y + p.y),
                    z + f64::from(dz),
                ),
                yaw_deg: yaw_deg + item.place.yaw_degrees,
            });
        }
    }
}

fn layout_config(pin: SettlementPin) -> HamletLabConfig {
    let mut config = HamletLabConfig::default();
    config.apply_tier_defaults(pin.tier);
    config.dwelling_max = config.dwelling_max.min(MAX_3D_DWELLINGS);
    config.dwelling_min = config.dwelling_min.min(config.dwelling_max);
    config
}

fn plan_seed(world_seed: i32, node_id: i32) -> u64 {
    (world_seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (node_id as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
}

fn well_mesh() -> EngineResult<Mesh> {
    Mesh::box_at(
        Vec3::new(0.0, 0.5, 0.0),
        Vec3::new(1.6, 1.0, 1.6),
        Color::rgb(118, 110, 98),
    )
}

fn hamlet_radius(at: GlobalXZ, houses: &[GlobalXZ]) -> f32 {
    let mut r = 20.0_f32;
    for h in houses {
        let dx = (h.x - at.x) as f32;
        let dz = (h.z - at.z) as f32;
        r = r.max((dx * dx + dz * dz).sqrt());
    }
    r + HAMLET_ROAD_PAD_M
}

fn nearby_road_segs(
    surface: &ContinentalSurface,
    origin: GlobalXZ,
    reach: f32,
) -> Vec<(Vec2, Vec2)> {
    let o = Vec2::new(origin.x as f32, origin.z as f32);
    let mut segs = Vec::new();
    for road in surface.roads() {
        for w in road.points.windows(2) {
            if dist_point_seg(o, w[0], w[1]) <= reach {
                segs.push((w[0], w[1]));
            }
        }
    }
    segs
}

fn on_road(p: Vec2, segs: &[(Vec2, Vec2)], clear: f32) -> bool {
    segs.iter().any(|&(a, b)| dist_point_seg(p, a, b) < clear)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(tier: u8) -> SettlementPin {
        SettlementPin {
            id: 1,
            at: GlobalXZ::at(0.0, 0.0),
            tier,
            population: 12,
        }
    }

    #[test]
    fn a_town_layout_asks_for_more_houses_than_a_hamlet() {
        let hamlet = layout_config(pin(0));
        let town = layout_config(pin(2));
        assert_eq!(hamlet.tier, 0);
        assert_eq!(town.tier, 2);
        assert!(town.dwelling_min > hamlet.dwelling_max);
        assert!(town.max_settle_radius > hamlet.max_settle_radius);
        assert!(town.market_radius > hamlet.market_radius);
    }

    #[test]
    fn a_point_on_a_road_centreline_is_blocked() {
        let segs = [(Vec2::ZERO, Vec2::new(100.0, 0.0))];
        assert!(on_road(Vec2::new(50.0, 0.0), &segs, ROAD_CLEAR_M));
        assert!(!on_road(Vec2::new(50.0, 8.0), &segs, ROAD_CLEAR_M));
    }

    #[test]
    fn hamlet_radius_covers_the_outer_house() {
        let at = GlobalXZ::at(0.0, 0.0);
        let houses = vec![GlobalXZ::at(30.0, 0.0), GlobalXZ::at(0.0, 10.0)];
        let r = hamlet_radius(at, &houses);
        assert!(r >= 40.0);
        assert!(r < 50.0);
    }
}
