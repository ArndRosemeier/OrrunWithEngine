//! Swinging house leaves and the portal interior they open onto.

use engine::collision::{ActorBody, ColliderLayer, ColliderShape, StaticCollider};
use engine::error::EngineResult;
use engine::mesh::Mesh;
use engine::place::GlobalPlace;
use engine::portal::{PortalId, PortalSettings, SpaceId};
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use glam::Vec2;

use super::settlement::HouseDoor;
use crate::hamlet::interior::{self, InteriorLayout};
use crate::hamlet::kit;

const REACH_M: f32 = 2.5;
const LOOK_DOT: f32 = 0.35;
const SWING_S: f32 = 0.40;
const OPEN_YAW_DEG: f32 = 95.0;
const OPENING_H: f32 = 2.16;
const DOOR_LAYER: ColliderLayer = 3;
const INTERIOR_LAYER: ColliderLayer = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Swing {
    Closed,
    Opening,
    Open,
    Closing,
}

struct LiveHouse {
    door_id: u64,
    space: SpaceId,
    leaf: EntityId,
    door_out: EntityId,
    interior: Vec<EntityId>,
    portal: PortalId,
    swing: Swing,
    open01: f32,
    floor_y: f32,
    storeys: u32,
    stair_local: Option<glam::Vec3>,
    house_at: GlobalXZ,
    house_yaw_deg: f32,
    opening_out_at: GlobalPosition,
    opening_in_at: GlobalPosition,
}

/// One house door the player can open. At most one interior is live.
pub struct DoorLayer {
    live: Option<LiveHouse>,
    hint: Option<String>,
}

impl DoorLayer {
    pub fn new() -> Self {
        Self {
            live: None,
            hint: None,
        }
    }

    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    pub fn hidden_leaf(&self) -> Option<u64> {
        self.live.as_ref().map(|h| h.door_id)
    }

    pub fn indoor_floor_y(&self, world: &World, feet: GlobalPosition) -> Option<f32> {
        let live = self.live.as_ref()?;
        if world.living_in() != live.space {
            return None;
        }
        Some(indoor_stand_y(live, feet))
    }

    /// Start just clear of the destination jamb, then collision-test any
    /// remaining movement in the newly entered space. This prevents a long
    /// frame from teleporting through the room's far wall.
    pub fn settle_after_travel(
        &self,
        world: &mut World,
        body: &ActorBody,
        entered: SpaceId,
        feet: &mut GlobalPosition,
    ) {
        let live = self
            .live
            .as_ref()
            .unwrap_or_else(|| panic!("portal travel completed without a live house"));
        let (opening, normal_z) = if entered == live.space {
            (live.opening_in_at, 1.0)
        } else if entered == SpaceId::DEFAULT {
            (live.opening_out_at, -1.0)
        } else {
            panic!("house portal entered an unrelated space");
        };
        settle_position(world, body, opening, normal_z, live.house_yaw_deg, feet);
    }

    pub fn evict_if_missing(&mut self, world: &mut World, doors: &[HouseDoor]) -> EngineResult<()> {
        let Some(id) = self.live.as_ref().map(|h| h.door_id) else {
            return Ok(());
        };
        if doors.iter().any(|d| d.id == id) {
            return Ok(());
        }
        self.evict(world)
    }

    pub fn frame(
        &mut self,
        world: &mut World,
        doors: &[HouseDoor],
        feet: GlobalPosition,
        yaw_degrees: f32,
        interact: bool,
        dt: f32,
    ) -> EngineResult<()> {
        self.hint = None;
        let facing = Vec2::new(
            yaw_degrees.to_radians().sin(),
            yaw_degrees.to_radians().cos(),
        );
        let target = nearest_door(doors, feet, facing);
        if let Some(door) = target {
            let open = self
                .live
                .as_ref()
                .is_some_and(|h| h.door_id == door.id && !matches!(h.swing, Swing::Closed));
            self.hint = Some(if open {
                "E close".into()
            } else {
                "E open".into()
            });
            if interact {
                self.toggle(world, door)?;
            }
        }
        self.tick_swing(world, doors, dt)?;
        Ok(())
    }

    fn toggle(&mut self, world: &mut World, door: &HouseDoor) -> EngineResult<()> {
        if let Some(live) = self.live.as_mut() {
            if live.door_id == door.id {
                live.swing = match live.swing {
                    Swing::Closed | Swing::Closing => Swing::Opening,
                    Swing::Open | Swing::Opening => Swing::Closing,
                };
                return Ok(());
            }
        }
        self.evict(world)?;
        self.begin_open(world, door)
    }

    fn begin_open(&mut self, world: &mut World, door: &HouseDoor) -> EngineResult<()> {
        let leaf_mesh = kit::load_piece_mesh(
            &modular::prelude::PieceId::new(&door.leaf_piece).expect("leaf id"),
        )
        .unwrap_or_else(|err| panic!("door leaf mesh: {err}"));
        let leaf = world.spawn_anchored(leaf_mesh, door.closed_place())?;
        let space = world.space(format!("house-{}", door.id))?;
        let opening = Mesh::opening(door.opening_width, OPENING_H).expect("door opening");
        let opening_out = door.opening_out();
        let door_out = world.spawn_anchored(opening.clone(), opening_out)?;
        world.in_space(space)?;
        let layout = interior::assemble(door.brief, door.seed);
        let opening_in = house_place(door, layout.door_local);
        let door_in = world.spawn_anchored(opening, opening_in)?;
        let mut interior = spawn_layout(world, door, &layout)?;
        interior.push(door_in);
        world.in_space(SpaceId::DEFAULT)?;
        let portal = link_house_portal(world, door_out, door_in)?;
        replace_interior_colliders(world, space, door, &layout);
        self.live = Some(LiveHouse {
            door_id: door.id,
            space,
            leaf,
            door_out,
            interior,
            portal,
            swing: Swing::Opening,
            open01: 0.0,
            floor_y: door.floor_y,
            storeys: layout.storeys,
            stair_local: layout.stair_local,
            house_at: door.house_at,
            house_yaw_deg: door.house_yaw_deg,
            opening_out_at: opening_out.position,
            opening_in_at: opening_in.position,
        });
        Ok(())
    }

    fn tick_swing(&mut self, world: &mut World, doors: &[HouseDoor], dt: f32) -> EngineResult<()> {
        let Some(live) = self.live.as_mut() else {
            world.collision_mut().clear_layer(DOOR_LAYER);
            return Ok(());
        };
        let Some(door) = doors.iter().find(|d| d.id == live.door_id) else {
            return Ok(());
        };
        let dir = match live.swing {
            Swing::Opening | Swing::Open => 1.0,
            Swing::Closing | Swing::Closed => -1.0,
        };
        live.open01 = (live.open01 + dir * dt / SWING_S).clamp(0.0, 1.0);
        if live.open01 >= 1.0 {
            live.swing = Swing::Open;
        } else if live.open01 <= 0.0 && matches!(live.swing, Swing::Closing | Swing::Closed) {
            live.swing = Swing::Closed;
        } else if dir > 0.0 {
            live.swing = Swing::Opening;
        } else {
            live.swing = Swing::Closing;
        }
        let yaw = door.closed_yaw_deg + OPEN_YAW_DEG * live.open01;
        world.set_anchored_place(live.leaf, door.leaf_place(yaw))?;

        if matches!(live.swing, Swing::Closed) {
            world.set_portal_enabled(live.portal, false)?;
            replace_door_collider(world, door, yaw);
        } else {
            world.set_portal_enabled(live.portal, true)?;
            world.collision_mut().clear_layer(DOOR_LAYER);
        }
        Ok(())
    }

    pub fn evict(&mut self, world: &mut World) -> EngineResult<()> {
        let Some(live) = self.live.take() else {
            return Ok(());
        };
        world.destroy_portal(live.portal)?;
        world.despawn(live.leaf);
        world.despawn(live.door_out);
        for id in live.interior {
            world.despawn(id);
        }
        if world.living_in() == live.space {
            world.live_in(SpaceId::DEFAULT)?;
        }
        world.collision_mut().clear_layer(DOOR_LAYER);
        world.collision_mut().clear_layer(INTERIOR_LAYER);
        Ok(())
    }
}

fn link_house_portal(
    world: &mut World,
    outside: EntityId,
    inside: EntityId,
) -> EngineResult<PortalId> {
    world.create_portal(outside, inside, PortalSettings::TELEPORTING)
}

fn settle_position(
    world: &mut World,
    body: &ActorBody,
    opening: GlobalPosition,
    normal_z: f32,
    yaw_degrees: f32,
    feet: &mut GlobalPosition,
) {
    let clearance = body.radius + interior::WALL_T * 0.5 + 0.08;
    let (nx, nz) = kit::yaw_xz(0.0, normal_z * clearance, yaw_degrees);
    let safe = GlobalXZ::at(opening.x + f64::from(nx), opening.z + f64::from(nz));
    let wanted = feet.horizontal();
    // Slide from just inside the jamb toward the requested point. Starting at
    // `feet` would add the portal offset twice and can tunnel through the room.
    let start = GlobalPosition::at(safe.x, opening.y, safe.z);
    let settled = world.move_actor(body, start, wanted.x - safe.x, wanted.z - safe.z);
    feet.x = settled.x;
    feet.z = settled.z;
}

impl Default for DoorLayer {
    fn default() -> Self {
        Self::new()
    }
}

fn spawn_layout(
    world: &mut World,
    door: &HouseDoor,
    layout: &InteriorLayout,
) -> EngineResult<Vec<EntityId>> {
    let mut ids = Vec::new();
    for item in layout.pieces.iter().chain(layout.furniture.iter()) {
        if item.piece.as_str() == "plinth" {
            continue;
        }
        let (dx, dz) = crate::hamlet::kit::yaw_xz(
            item.place.position.x,
            item.place.position.z,
            door.house_yaw_deg,
        );
        let at = GlobalPosition::at(
            door.house_at.x + f64::from(dx),
            f64::from(door.floor_y + item.place.position.y),
            door.house_at.z + f64::from(dz),
        );
        let place = GlobalPlace::at(at).with_yaw_deg(door.house_yaw_deg + item.place.yaw_degrees);
        let mesh = interior::piece_mesh(item.piece.as_str());
        ids.push(world.spawn_anchored(mesh, place)?);
    }
    Ok(ids)
}

fn house_place(door: &HouseDoor, local: engine::place::Place) -> GlobalPlace {
    let (dx, dz) = kit::yaw_xz(local.position.x, local.position.z, door.house_yaw_deg);
    GlobalPlace::at(GlobalPosition::at(
        door.house_at.x + f64::from(dx),
        f64::from(door.floor_y + local.position.y),
        door.house_at.z + f64::from(dz),
    ))
    .with_yaw_deg(door.house_yaw_deg + local.yaw_degrees)
}

fn replace_door_collider(world: &mut World, door: &HouseDoor, yaw_deg: f32) {
    let collider = StaticCollider::new(
        door.at.horizontal(),
        yaw_deg.to_radians(),
        ColliderShape::Box {
            half_x: 0.06,
            half_z: door.opening_width * 0.5,
        },
    );
    world
        .collision_mut()
        .replace_layer(DOOR_LAYER, [collider])
        .expect("door collider");
}

fn replace_interior_colliders(
    world: &mut World,
    space: SpaceId,
    door: &HouseDoor,
    layout: &InteriorLayout,
) {
    let mut colliders = Vec::new();
    for item in &layout.pieces {
        let name = item.piece.as_str();
        match name {
            "wall" | "wall_b" => push_piece_box(
                &mut colliders,
                space,
                door,
                item,
                0.0,
                interior::WALL_CENTER_Z,
                interior::CELL_M * 0.5,
                interior::WALL_T * 0.5,
                0.0,
            ),
            "partition" => push_piece_box(
                &mut colliders,
                space,
                door,
                item,
                0.0,
                0.0,
                interior::CELL_M * 0.5,
                interior::WALL_T * 0.5,
                0.0,
            ),
            "door" => push_door_jambs(&mut colliders, space, door, item, interior::WALL_CENTER_Z),
            "partition_door" => push_door_jambs(&mut colliders, space, door, item, 0.0),
            "corner" => {
                push_piece_box(
                    &mut colliders,
                    space,
                    door,
                    item,
                    0.0,
                    interior::WALL_CENTER_Z,
                    interior::CELL_M * 0.5,
                    interior::WALL_T * 0.5,
                    0.0,
                );
                push_piece_box(
                    &mut colliders,
                    space,
                    door,
                    item,
                    interior::WALL_CENTER_Z,
                    0.0,
                    interior::CELL_M * 0.5,
                    interior::WALL_T * 0.5,
                    90.0,
                );
            }
            // Stair height is handled by indoor_stand_y. A horizontal box
            // over its footprint would make the stair impossible to climb.
            "stair" | "floor" | "loft_floor" | "ceiling" | "plinth" => {}
            other => panic!("indoor structural collider is undefined for '{other}'"),
        }
    }
    for item in &layout.furniture {
        let name = item.piece.as_str();
        if name == "stair" {
            continue;
        }
        let (half_x, half_z) = interior::furniture_half_xz(name);
        push_piece_box(
            &mut colliders,
            space,
            door,
            item,
            0.0,
            0.0,
            half_x,
            half_z,
            0.0,
        );
    }
    world
        .collision_mut()
        .replace_layer(INTERIOR_LAYER, colliders)
        .expect("interior colliders");
}

fn push_door_jambs(
    colliders: &mut Vec<StaticCollider>,
    space: SpaceId,
    door: &HouseDoor,
    item: &modular::prelude::PlacedMesh,
    z: f32,
) {
    let side = (interior::CELL_M - interior::DOORWAY_M) * 0.5;
    let center_x = interior::DOORWAY_M * 0.5 + side * 0.5;
    for x in [-center_x, center_x] {
        push_piece_box(
            colliders,
            space,
            door,
            item,
            x,
            z,
            side * 0.5,
            interior::WALL_T * 0.5,
            0.0,
        );
    }
}

fn piece_y_span(door: &HouseDoor, item: &modular::prelude::PlacedMesh) -> (f64, f64) {
    let base = door.floor_y as f64 + f64::from(item.place.position.y);
    (base, base + f64::from(STOREY_M))
}

#[allow(clippy::too_many_arguments)]
fn push_piece_box(
    colliders: &mut Vec<StaticCollider>,
    space: SpaceId,
    door: &HouseDoor,
    item: &modular::prelude::PlacedMesh,
    local_x: f32,
    local_z: f32,
    half_x: f32,
    half_z: f32,
    yaw_offset_deg: f32,
) {
    let item_yaw = door.house_yaw_deg + item.place.yaw_degrees;
    let (piece_dx, piece_dz) = kit::yaw_xz(
        item.place.position.x,
        item.place.position.z,
        door.house_yaw_deg,
    );
    let (shape_dx, shape_dz) = interior::rotate_xz(local_x, local_z, item_yaw);
    let at = GlobalXZ::at(
        door.house_at.x + f64::from(piece_dx + shape_dx),
        door.house_at.z + f64::from(piece_dz + shape_dz),
    );
    let (min_y, max_y) = piece_y_span(door, item);
    colliders.push(
        StaticCollider::new(
            at,
            (item_yaw + yaw_offset_deg).to_radians(),
            ColliderShape::Box { half_x, half_z },
        )
        .with_y_span(min_y, max_y)
        .in_space(space),
    );
}

const STOREY_M: f32 = 2.7;

fn indoor_stand_y(live: &LiveHouse, feet: GlobalPosition) -> f32 {
    if live.storeys <= 1 {
        return live.floor_y;
    }
    let (lx, lz) = world_to_house_local(feet, live.house_at, live.house_yaw_deg);
    if let Some(stair) = live.stair_local {
        let dx = lx - stair.x;
        let dz = lz - stair.z;
        if dx.abs() < 1.2 && dz.abs() < 2.2 {
            let t = ((lz - (stair.z - 2.0)) / 4.0).clamp(0.0, 1.0);
            return live.floor_y + STOREY_M * t;
        }
    }
    if (feet.y as f32) > live.floor_y + 1.2 {
        live.floor_y + STOREY_M
    } else {
        live.floor_y
    }
}

fn world_to_house_local(feet: GlobalPosition, house_at: GlobalXZ, yaw_deg: f32) -> (f32, f32) {
    let dx = (feet.x - house_at.x) as f32;
    let dz = (feet.z - house_at.z) as f32;
    let rad = yaw_deg.to_radians();
    let (sin, cos) = (rad.sin(), rad.cos());
    (dx * cos - dz * sin, dx * sin + dz * cos)
}

fn nearest_door(doors: &[HouseDoor], feet: GlobalPosition, facing: Vec2) -> Option<&HouseDoor> {
    let mut best: Option<(&HouseDoor, f32)> = None;
    for door in doors {
        let dx = (door.at.x - feet.x) as f32;
        let dz = (door.at.z - feet.z) as f32;
        let dist = (dx * dx + dz * dz).sqrt();
        if dist > REACH_M {
            continue;
        }
        let to = Vec2::new(dx, dz).normalize_or_zero();
        if facing.dot(to) < LOOK_DOT {
            continue;
        }
        if best.is_none_or(|(_, d)| dist < d) {
            best = Some((door, dist));
        }
    }
    best.map(|(d, _)| d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::place::Place;

    fn door_at(x: f64, z: f64, yaw: f32) -> HouseDoor {
        HouseDoor {
            id: 1,
            brief: crate::hamlet::DwellingBrief::new(3, 2, 1, crate::hamlet::HouseTheme::Any),
            seed: 1,
            leaf_piece: "door_plank".into(),
            at: GlobalPosition::at(x, 1.0, z),
            closed_yaw_deg: yaw,
            opening_width: 1.1,
            house_at: GlobalXZ::at(x, z + 4.0),
            house_yaw_deg: 0.0,
            floor_y: 0.0,
            half_z: 4.0,
        }
    }

    #[test]
    fn closed_leaf_blocks_the_opening() {
        let door = door_at(0.0, 0.0, 180.0);
        let c = StaticCollider::new(
            door.at.horizontal(),
            door.closed_yaw_deg.to_radians(),
            ColliderShape::Box {
                half_x: 0.06,
                half_z: door.opening_width * 0.5,
            },
        );
        let mut world = engine::collision::CollisionWorld::new();
        world.insert(1, c).expect("insert");
        let body = engine::collision::ActorBody::player();
        let to = world.move_xz(&body, GlobalXZ::at(0.0, -2.0), 0.0, 4.0);
        assert!(to.z < 0.2, "closed door should stop the walk, z={}", to.z);
    }

    #[test]
    fn open_leaf_leaves_the_opening_clear() {
        let world = engine::collision::CollisionWorld::new();
        let body = engine::collision::ActorBody::player();
        let to = world.move_xz(&body, GlobalXZ::at(0.0, -2.0), 0.0, 4.0);
        assert!(
            to.z > 1.5,
            "an open door must not block the walk, z={}",
            to.z
        );
    }

    #[test]
    fn hinge_yaw_opens_inward() {
        let closed = Place::new(0.0, 0.0, 0.0).with_yaw_deg(180.0);
        let open = closed.yaw_degrees + OPEN_YAW_DEG;
        assert!((open - 275.0).abs() < 1e-3);
    }

    #[test]
    fn indoor_doorway_collider_leaves_a_walkable_gap() {
        let door = door_at(0.0, 0.0, 180.0);
        let layout = interior::assemble(door.brief, door.seed);
        let mut world = World::new();
        let space = world.space("test-house").expect("space");
        replace_interior_colliders(&mut world, space, &door, &layout);
        world.live_in(space).expect("live indoors");

        let body = ActorBody::player();
        let to = world.move_actor(&body, GlobalPosition::at(0.0, 0.0, -1.0), 0.0, 3.0);
        assert!(
            to.z > 1.0,
            "door jamb colliders closed the opening, z={}",
            to.z
        );
    }

    #[test]
    fn two_storey_wall_piece_heights() {
        let brief = crate::hamlet::DwellingBrief::new(4, 3, 2, crate::hamlet::HouseTheme::Any);
        let layout = interior::assemble(brief, 7);
        let ground = layout
            .pieces
            .iter()
            .filter(|p| {
                matches!(
                    p.piece.as_str(),
                    "wall" | "wall_b" | "partition" | "door" | "corner"
                ) && p.place.position.y < 0.5
            })
            .count();
        let upper = layout
            .pieces
            .iter()
            .filter(|p| {
                matches!(
                    p.piece.as_str(),
                    "wall" | "wall_b" | "partition" | "door" | "corner"
                ) && (p.place.position.y - STOREY_M).abs() < 0.1
            })
            .count();
        assert_eq!(
            ground, upper,
            "each ring cell should have one wall per storey"
        );
        assert!(
            layout
                .pieces
                .iter()
                .any(|p| p.piece.as_str() == "door" && p.place.position.y < 0.5),
            "ground door must stay on the lower storey"
        );
        assert!(
            layout.pieces.iter().any(|p| {
                matches!(
                    p.piece.as_str(),
                    "wall" | "wall_b" | "partition" | "door" | "corner"
                ) && (p.place.position.y - STOREY_M).abs() < 0.1
            }),
            "upper wall over the door must be tagged to the loft"
        );
    }

    #[test]
    fn two_storey_upper_walls_do_not_block_the_doorway_at_ground_level() {
        let mut door = door_at(0.0, 0.0, 180.0);
        door.brief = crate::hamlet::DwellingBrief::new(4, 3, 2, crate::hamlet::HouseTheme::Any);
        let layout = interior::assemble(door.brief, door.seed);
        assert!(
            layout.pieces.iter().any(|p| {
                matches!(p.piece.as_str(), "wall" | "wall_b" | "partition")
                    && (p.place.position.y - STOREY_M).abs() < 0.1
            }),
            "expected an upper-storey wall ring"
        );
        let mut world = World::new();
        let space = world.space("test-house").expect("space");
        replace_interior_colliders(&mut world, space, &door, &layout);
        world.live_in(space).expect("live indoors");

        let body = ActorBody::player();
        let to = world.move_actor(&body, GlobalPosition::at(0.0, 0.0, 2.0), 0.0, -3.0);
        assert!(
            to.z < 0.5,
            "upper-storey wall colliders blocked the doorway at ground level, z={}",
            to.z
        );
    }

    #[test]
    fn portal_lands_on_the_room_side_of_the_door_wall() {
        let door = door_at(0.0, 0.0, 180.0);
        let layout = interior::assemble(door.brief, door.seed);
        let mut world = World::new();
        let out = world
            .spawn_anchored(
                Mesh::opening(door.opening_width, OPENING_H).expect("opening"),
                door.opening_out(),
            )
            .expect("outside opening");
        let space = world.space("test-house").expect("space");
        world.in_space(space).expect("spawn indoors");
        let inside_place = house_place(&door, layout.door_local);
        let inside = world
            .spawn_anchored(
                Mesh::opening(door.opening_width, OPENING_H).expect("opening"),
                inside_place,
            )
            .expect("inside opening");
        world.in_space(SpaceId::DEFAULT).expect("spawn outside");
        link_house_portal(&mut world, out, inside).expect("link");

        let mut yaw = 0.0;
        let mut position = world
            .to_render(GlobalPosition::at(0.0, 0.05, -1.0))
            .expect("outside point");
        assert!(world.travel(&mut position, &mut yaw).is_none());
        position = world
            .to_render(GlobalPosition::at(0.0, 0.05, 0.5))
            .expect("crossed point");
        assert_eq!(world.travel(&mut position, &mut yaw), Some(space));
        let landed = world.to_global(position).expect("landed point");
        assert!(
            landed.z > inside_place.position.z && landed.z < inside_place.position.z + 2.0,
            "portal landed outside the room: wall={} landed={}",
            inside_place.position.z,
            landed.z
        );
    }

    #[test]
    fn two_storey_portal_round_trip() {
        let mut door = door_at(0.0, 0.0, 180.0);
        door.brief = crate::hamlet::DwellingBrief::new(4, 3, 2, crate::hamlet::HouseTheme::Any);
        let layout = interior::assemble(door.brief, door.seed);
        let mut world = World::new();
        let out = world
            .spawn_anchored(
                Mesh::opening(door.opening_width, OPENING_H).expect("opening"),
                door.opening_out(),
            )
            .expect("outside opening");
        let space = world.space("test-house").expect("space");
        world.in_space(space).expect("spawn indoors");
        let inside = world
            .spawn_anchored(
                Mesh::opening(door.opening_width, OPENING_H).expect("opening"),
                house_place(&door, layout.door_local),
            )
            .expect("inside opening");
        world.in_space(SpaceId::DEFAULT).expect("spawn outside");
        link_house_portal(&mut world, out, inside).expect("link");

        let mut yaw = 0.0;
        let mut position = world
            .to_render(GlobalPosition::at(0.0, 0.05, -1.0))
            .expect("outside point");
        assert!(world.travel(&mut position, &mut yaw).is_none());
        position = world
            .to_render(GlobalPosition::at(0.0, 0.05, 0.5))
            .expect("entered point");
        assert_eq!(world.travel(&mut position, &mut yaw), Some(space));
    }

    #[test]
    fn long_portal_step_stops_at_the_far_wall() {
        let door = door_at(0.0, 0.0, 180.0);
        let layout = interior::assemble(door.brief, door.seed);
        let mut world = World::new();
        let space = world.space("test-house").expect("space");
        replace_interior_colliders(&mut world, space, &door, &layout);
        world.live_in(space).expect("live indoors");
        let opening = house_place(&door, layout.door_local).position;
        let mut wanted = GlobalPosition::at(0.0, 0.05, 20.0);

        settle_position(
            &mut world,
            &ActorBody::player(),
            opening,
            1.0,
            door.house_yaw_deg,
            &mut wanted,
        );
        assert!(
            wanted.z < 7.5,
            "one long frame escaped through the room, z={}",
            wanted.z
        );
    }
}
