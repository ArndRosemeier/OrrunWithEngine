//! Settlements on the continent: lab packing, door-sill seating, no flattened pads.
//!
//! Atlas settlement nodes are pins. Each nearby pin is packed at its atlas
//! tier (hamlet … port), scored against the real ground, then each building
//! sits with its door at grade. Downhill air under the floor is a foundation
//! skirt. The heightfield is not rewritten — that was the Godot trap that
//! buried doors or left houses floating, and it was hard to keep the pad edge
//! honest.

use std::collections::HashMap;
use std::path::PathBuf;

use engine::color::Color;
use engine::error::{EngineError, EngineResult};
use engine::mesh::Mesh;
use engine::model::Model;
use engine::place::Place;
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use glam::{Vec2, Vec3};
use thiserror::Error;

use super::brooks::{BrookDetail, BrookField};
use super::scatter::{props_dir, ScatterError};
use super::surface::{ContinentalSurface, SettlementPin, SurfaceColumn};
use super::world_stream::WorldStream;
use crate::hamlet::{
    plan_on, spec_for, HamletError, HamletLabConfig, Plan2D, Plot, ShapeKind, SKIRT_BITE_M,
};

/// How far from the player a pin still gets a layout. Sized for a port envelope.
const REACH_M: f64 = 720.0;
/// Rebuild the standing set once the player is this far from where it was centred.
const RESEED_M: f64 = 70.0;
/// Lab ports ask for thousands of dwellings; that is a hitch and a hillside of
/// underfill. 3D uses the atlas tier's market and spread, with this cap on
/// houses until packing is off the main thread.
const MAX_3D_DWELLINGS: u32 = 80;

/// Blender authors the door on +Y; glTF Y-up maps that to −Z, which is where
/// the planner already puts the door at yaw 0. No extra turn.
const MESH_DOOR_YAW_OFFSET_DEG: f32 = 0.0;

#[derive(Debug, Error)]
pub enum SettlementError {
    #[error(transparent)]
    Scatter(#[from] ScatterError),

    #[error(
        "no house meshes under {0}; generate the Asset Lab cabins and run tools/sync_props.py"
    )]
    NoHouses(PathBuf),

    #[error("house {path} failed to load: {source}")]
    BadHouse {
        path: PathBuf,
        #[source]
        source: EngineError,
    },

    #[error(transparent)]
    Engine(#[from] EngineError),

    #[error(transparent)]
    Hamlet(#[from] HamletError),
}

struct HouseMesh {
    id: String,
    entity: EntityId,
    places: Vec<Place>,
}

#[derive(Clone, Copy)]
enum Kind {
    House(usize),
    Well,
    Skirt,
}

struct Standing {
    kind: Kind,
    at: GlobalPosition,
    yaw_deg: f32,
    stretch: Vec3,
}

/// One seated hamlet, in world metres.
#[derive(Clone, Debug)]
pub struct HamletStand {
    pub at: GlobalXZ,
    pub houses: Vec<GlobalXZ>,
}

struct GroundPlot<'a> {
    surface: &'a ContinentalSurface,
    brooks: &'a BrookField,
    stream: Option<&'a WorldStream>,
    origin: GlobalXZ,
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
        self.column(self.world(p)).wetness()
    }
}

/// Live hamlets around the player.
pub struct SettlementLayer {
    houses: Vec<HouseMesh>,
    well: EntityId,
    well_places: Vec<Place>,
    skirt: EntityId,
    skirt_places: Vec<Place>,
    seed: i32,
    centre: Option<GlobalXZ>,
    resident_chunks: usize,
    standing: Vec<Standing>,
    plans: HashMap<i32, Plan2D>,
    hamlets: Vec<HamletStand>,
}

impl SettlementLayer {
    /// Upload house meshes and the shared skirt / well once; nothing is seated yet.
    pub fn install(world: &mut World, seed: i32) -> Result<Self, SettlementError> {
        let root = props_dir()?.join("houses");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Err(SettlementError::NoHouses(root));
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("glb")))
            .collect();
        paths.sort();
        if paths.is_empty() {
            return Err(SettlementError::NoHouses(root));
        }

        let mut houses = Vec::new();
        for path in paths {
            let id = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .ok_or_else(|| SettlementError::NoHouses(path.clone()))?;
            if spec_for(&id).is_none() {
                return Err(SettlementError::BadHouse {
                    path: path.clone(),
                    source: EngineError::InvalidValue(format!(
                        "house mesh '{id}' has no catalog spec"
                    )),
                });
            }
            let mesh = Model::load(&path).map_err(|source| SettlementError::BadHouse {
                path: path.clone(),
                source,
            })?;
            houses.push(HouseMesh {
                id,
                entity: world.spawn_instanced(mesh),
                places: Vec::new(),
            });
        }

        let well = world.spawn_instanced(well_mesh()?);
        let skirt = world.spawn_instanced(skirt_mesh()?);
        Ok(Self {
            houses,
            well,
            well_places: Vec::new(),
            skirt,
            skirt_places: Vec::new(),
            seed,
            centre: None,
            resident_chunks: 0,
            standing: Vec::new(),
            plans: HashMap::new(),
            hamlets: Vec::new(),
        })
    }

    pub fn placed_count(&self) -> usize {
        self.standing
            .iter()
            .filter(|s| !matches!(s.kind, Kind::Skirt))
            .count()
    }

    /// Seated hamlets in the current window, for footbridges across a split river.
    pub fn hamlets(&self) -> &[HamletStand] {
        &self.hamlets
    }

    pub fn clear(&mut self, world: &mut World) -> EngineResult<()> {
        for house in &mut self.houses {
            house.places.clear();
            world.set_instances(house.entity, &[])?;
        }
        self.well_places.clear();
        self.skirt_places.clear();
        world.set_instances(self.well, &[])?;
        world.set_instances(self.skirt, &[])?;
        self.standing.clear();
        self.plans.clear();
        self.hamlets.clear();
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
        Ok(true)
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
        for pin in nearby {
            let plan = self.plans.entry(pin.id).or_insert_with(|| {
                layout_for(self.seed, pin, surface, brooks)
                    .unwrap_or_else(|err| panic!("hamlet at node {} failed: {err}", pin.id))
            });
            let before = self.standing.len();
            seat_plan(
                plan,
                pin,
                surface,
                brooks,
                stream,
                &self.houses,
                &mut self.standing,
            );
            let houses = self.standing[before..]
                .iter()
                .filter(|s| matches!(s.kind, Kind::House(_) | Kind::Well))
                .map(|s| s.at.horizontal())
                .collect();
            self.hamlets.push(HamletStand { at: pin.at, houses });
        }
    }

    fn stand(&mut self, world: &mut World) -> EngineResult<()> {
        let origin = world.render_origin();
        for house in &mut self.houses {
            house.places.clear();
        }
        self.well_places.clear();
        self.skirt_places.clear();

        for item in &self.standing {
            let Ok(render) = item.at.to_render(origin) else {
                continue;
            };
            let at = render.vec3();
            let place = Place::new(at.x, at.y, at.z)
                .with_yaw_deg(item.yaw_deg)
                .with_stretch(item.stretch);
            match item.kind {
                Kind::House(i) => self.houses[i].places.push(place),
                Kind::Well => self.well_places.push(place),
                Kind::Skirt => self.skirt_places.push(place),
            }
        }
        for house in &self.houses {
            world.set_instances(house.entity, &house.places)?;
        }
        world.set_instances(self.well, &self.well_places)?;
        world.set_instances(self.skirt, &self.skirt_places)?;
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
    };
    plan_on(&config, Some(&plot))
}

fn seat_plan(
    plan: &Plan2D,
    pin: SettlementPin,
    surface: &ContinentalSurface,
    brooks: &BrookField,
    stream: &WorldStream,
    houses: &[HouseMesh],
    out: &mut Vec<Standing>,
) {
    let plot = GroundPlot {
        surface,
        brooks,
        stream: Some(stream),
        origin: pin.at,
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
        let yaw_deg = shape.yaw.to_degrees() + MESH_DOOR_YAW_OFFSET_DEG;
        let x = pin.at.x + f64::from(shape.center.x);
        let z = pin.at.z + f64::from(shape.center.y);

        if spec.id == "Well" {
            out.push(Standing {
                kind: Kind::Well,
                at: GlobalPosition::at(x, f64::from(seat.floor_z), z),
                yaw_deg,
                stretch: Vec3::ONE,
            });
            continue;
        }

        let Some(index) = houses.iter().position(|h| h.id == spec.id) else {
            continue;
        };
        out.push(Standing {
            kind: Kind::House(index),
            at: GlobalPosition::at(x, f64::from(seat.origin_y), z),
            yaw_deg,
            stretch: Vec3::ONE,
        });
        let skirt_y = seat.floor_z - seat.skirt_height * 0.5;
        out.push(Standing {
            kind: Kind::Skirt,
            at: GlobalPosition::at(x, f64::from(skirt_y), z),
            yaw_deg,
            stretch: Vec3::new(
                spec.size_x * 0.96,
                seat.skirt_height.max(SKIRT_BITE_M),
                spec.size_z * 0.96,
            ),
        });
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

fn skirt_mesh() -> EngineResult<Mesh> {
    Mesh::box_at(Vec3::ZERO, Vec3::ONE, Color::rgb(78, 68, 58))
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
}
