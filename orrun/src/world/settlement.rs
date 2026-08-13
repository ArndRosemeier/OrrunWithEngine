//! Settlements on the continent: lab packing, door-sill seating, no flattened pads.
//!
//! Atlas settlement nodes are pins. Each nearby pin is packed at its atlas
//! tier (hamlet … port), scored against the real ground, then each dwelling
//! is a Modular medieval kit seated with its door at grade. Kit plinths take
//! the downhill air. The heightfield is not rewritten — that was the Godot
//! trap that buried doors or left houses floating.

use std::collections::HashMap;

use engine::color::Color;
use engine::error::{EngineError, EngineResult};
use engine::mesh::Mesh;
use engine::place::Place;
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use glam::{Vec2, Vec3};
use modular::prelude::{PieceId, PlacedMesh};
use thiserror::Error;

use super::brooks::{BrookDetail, BrookField};
use super::footprint::HousePlot;
use super::surface::{ContinentalSurface, SettlementPin, SurfaceColumn};
use super::world_stream::WorldStream;
use crate::hamlet::kit::{self, KitError};
use crate::hamlet::{plan_on, spec_for, HamletError, HamletLabConfig, Plan2D, Plot, ShapeKind, DOOR_SINK_M};

/// How far from the player a pin still gets a layout. Sized for a port envelope.
const REACH_M: f64 = 720.0;
/// Rebuild the standing set once the player is this far from where it was centred.
const RESEED_M: f64 = 70.0;
/// Lab ports ask for thousands of dwellings; that is a hitch and a hillside of
/// underfill. 3D uses the atlas tier's market and spread, with this cap on
/// houses until packing is off the main thread.
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
#[derive(Clone, Debug)]
pub struct HamletStand {
    pub at: GlobalXZ,
    /// Disk that covers the packed houses, for roads to pause inside.
    pub radius: f32,
    pub houses: Vec<GlobalXZ>,
}

struct GroundPlot<'a> {
    surface: &'a ContinentalSurface,
    brooks: &'a BrookField,
    stream: Option<&'a WorldStream>,
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
        self.brooks.carve(p, &mut column, BrookDetail::Channels);
        column
    }
}

impl Plot for GroundPlot<'_> {
    fn height(&self, p: Vec2) -> f32 {
        let at = self.world(p);
        if let Some(stream) = self.stream {
            if let Some(h) = stream.contact_height(at) {
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

/// Live hamlets around the player.
pub struct SettlementLayer {
    pieces: Vec<PieceBatch>,
    recipes: HashMap<String, Vec<PlacedMesh>>,
    well: EntityId,
    well_places: Vec<Place>,
    seed: i32,
    centre: Option<GlobalXZ>,
    resident_chunks: usize,
    standing: Vec<Standing>,
    plans: HashMap<i32, Plan2D>,
    hamlets: Vec<HamletStand>,
    plots: Vec<HousePlot>,
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
            recipes,
            well,
            well_places: Vec::new(),
            seed,
            centre: None,
            resident_chunks: 0,
            standing: Vec::new(),
            plans: HashMap::new(),
            hamlets: Vec::new(),
            plots: Vec::new(),
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

    pub fn clear(&mut self, world: &mut World) -> EngineResult<()> {
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

    /// Keep hamlets around the player. Re-seats when the player walks off the
    /// last centre, when ground streams in, or when render space rebases.
    pub fn follow(
        &mut self,
        world: &mut World,
        stream: &WorldStream,
        surface: &ContinentalSurface,
        brooks: &BrookField,
        focus: GlobalXZ,
        rebased: bool,
    ) -> EngineResult<bool> {
        let resident = stream.resident_count();
        let moved = self
            .centre
            .map(|c| ((c.x - focus.x).powi(2) + (c.z - focus.z).powi(2)).sqrt())
            .unwrap_or(f64::INFINITY);
        let wanted = moved >= RESEED_M
            || (resident != self.resident_chunks && stream.walked_pending_count() == 0);
        if !wanted && !rebased {
            return Ok(false);
        }
        if wanted {
            self.rebuild(stream, surface, brooks, focus);
            self.centre = Some(focus);
            self.resident_chunks = resident;
        }
        self.stand(world)?;
        Ok(wanted)
    }

    fn rebuild(
        &mut self,
        stream: &WorldStream,
        surface: &ContinentalSurface,
        brooks: &BrookField,
        focus: GlobalXZ,
    ) {
        let reach_sq = REACH_M * REACH_M;
        let nearby: Vec<SettlementPin> = surface
            .settlements()
            .iter()
            .copied()
            .filter(|pin| {
                let dx = pin.at.x - focus.x;
                let dz = pin.at.z - focus.z;
                dx * dx + dz * dz <= reach_sq
            })
            .collect();

        self.plans
            .retain(|id, _| nearby.iter().any(|pin| pin.id == *id));

        self.standing.clear();
        self.hamlets.clear();
        self.plots.clear();
        for pin in nearby {
            let plan = self.plans.entry(pin.id).or_insert_with(|| {
                layout_for(self.seed, pin, surface, brooks)
                    .unwrap_or_else(|err| panic!("hamlet at node {} failed: {err}", pin.id))
            });
            let mut houses = Vec::new();
            seat_plan(
                plan,
                pin,
                surface,
                brooks,
                stream,
                &self.pieces,
                &self.recipes,
                &mut self.standing,
                &mut houses,
                &mut self.plots,
            );
            let radius = hamlet_radius(pin.at, &houses);
            self.hamlets.push(HamletStand {
                at: pin.at,
                radius,
                houses,
            });
        }
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

fn layout_for(
    world_seed: i32,
    pin: SettlementPin,
    surface: &ContinentalSurface,
    brooks: &BrookField,
) -> Result<Plan2D, HamletError> {
    let mut config = layout_config(pin);
    config.seed = plan_seed(world_seed, pin.id);
    let plot = GroundPlot {
        surface,
        brooks,
        stream: None,
        origin: pin.at,
        roads: nearby_road_segs(surface, pin.at, config.max_settle_radius + 24.0),
    };
    plan_on(&config, Some(&plot))
}

fn seat_plan(
    plan: &Plan2D,
    pin: SettlementPin,
    surface: &ContinentalSurface,
    brooks: &BrookField,
    stream: &WorldStream,
    pieces: &[PieceBatch],
    recipes: &HashMap<String, Vec<PlacedMesh>>,
    out: &mut Vec<Standing>,
    houses: &mut Vec<GlobalXZ>,
    plots: &mut Vec<HousePlot>,
) {
    let plot = GroundPlot {
        surface,
        brooks,
        stream: Some(stream),
        origin: pin.at,
        roads: nearby_road_segs(
            surface,
            pin.at,
            layout_config(pin).max_settle_radius + 24.0,
        ),
    };
    for shape in &plan.shapes {
        if shape.kind != ShapeKind::House {
            continue;
        }
        let Some(spec) = spec_for(&shape.catalog_id) else {
            panic!("planned building '{}' is not in the catalog", shape.catalog_id);
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

        let recipe = recipes.get(spec.id).unwrap_or_else(|| {
            panic!("dwelling '{}' has no modular recipe", spec.id)
        });
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
            let index = pieces
                .iter()
                .position(|batch| batch.piece == item.piece)
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
    (world_seed as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
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

fn nearby_road_segs(surface: &ContinentalSurface, origin: GlobalXZ, reach: f32) -> Vec<(Vec2, Vec2)> {
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
