//! M4 overland sites: Taken Cairn and Woods Hut.
//!
//! One site per SettlementPin hinterland. Replaces empty-grass roster pin+18
//! seating. Props are static Model loads; bandits sit through the combat layer.

use crate::atlas::Biome;
use crate::combat::sheets::bandit_sheet;
use crate::combat::types::{WorldCombat, WorldHostile};
use crate::hamlet::kit;
use crate::world::settlement::HamletStand;
use crate::world::surface::{ContinentalSurface, SettlementPin};
use engine::error::{EngineError, EngineResult};
use engine::mesh::Mesh;
use engine::model::Model;
use engine::place::GlobalPlace;
use engine::space::{GlobalPosition, GlobalXZ};
use engine::world::{EntityId, World};
use std::collections::HashMap;
use std::path::PathBuf;

/// Pack patrol unstack. Same metre as combat_layer STRAFE_M.
pub const STRAFE_M: f64 = 1.8;
/// Woods Hut stays this far from its SettlementPin.
pub const HUT_MIN_M: f64 = 150.0;
/// Reject hamlet interior plus this pad.
pub const SITE_HAMLET_PAD_M: f32 = 24.0;
const HINTERLAND_MAX_M: f64 = 720.0;
const CAIRN_MIN_PIN_M: f64 = 28.0;
const SAMPLE_M: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteKind {
    TakenCairn,
    WoodsHut,
}

impl SiteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TakenCairn => "cairn",
            Self::WoodsHut => "hut",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlandSite {
    pub kind: SiteKind,
    pub pin_id: i32,
    pub at: GlobalXZ,
    pub yaw_deg: f32,
}

impl OverlandSite {
    pub fn bandit_xz(self) -> [(f64, f64); 2] {
        match self.kind {
            SiteKind::TakenCairn => {
                let (sx, sz) = strafe_axes(self.yaw_deg);
                [
                    (self.at.x + sx * -STRAFE_M, self.at.z + sz * -STRAFE_M),
                    (self.at.x + sx * STRAFE_M, self.at.z + sz * STRAFE_M),
                ]
            }
            SiteKind::WoodsHut => {
                // Door is local -Z. Stand the pack on the stoop, unstacked.
                let (fx, fz) = kit::yaw_xz(0.0, -7.2, self.yaw_deg);
                let (sx, sz) = kit::yaw_xz(STRAFE_M as f32, 0.0, self.yaw_deg);
                [
                    (
                        self.at.x + f64::from(fx - sx),
                        self.at.z + f64::from(fz - sz),
                    ),
                    (
                        self.at.x + f64::from(fx + sx),
                        self.at.z + f64::from(fz + sz),
                    ),
                ]
            }
        }
    }
}

fn site_hash(world_seed: i32, pin_id: i32) -> u64 {
    let mut x = (world_seed as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (pin_id as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn strafe_axes(yaw_deg: f32) -> (f64, f64) {
    let r = (yaw_deg as f64).to_radians();
    let fx = r.sin();
    let fz = r.cos();
    (-fz, fx)
}

pub fn dungeon_blocks(surface: &ContinentalSurface, p: GlobalXZ) -> bool {
    surface
        .dungeon_pins()
        .iter()
        .any(|d| d.at.distance(p) < 200.0)
}

fn pin_keepout_m(pin: &SettlementPin) -> f64 {
    let settle = match pin.tier {
        0 => 80.0,
        1 => 160.0,
        2 => 320.0,
        _ => 600.0,
    };
    settle + f64::from(SITE_HAMLET_PAD_M)
}

fn hamlet_pad_blocks(p: GlobalXZ, hamlets: &[HamletStand]) -> bool {
    hamlets.iter().any(|h| h.covers(p, SITE_HAMLET_PAD_M))
}

fn hamlet_blocks(p: GlobalXZ, hamlets: &[HamletStand], pins: &[SettlementPin]) -> bool {
    if hamlet_pad_blocks(p, hamlets) {
        return true;
    }
    pins.iter().any(|pin| pin.at.distance(p) < pin_keepout_m(pin))
}

fn dry_land(surface: &ContinentalSurface, p: GlobalXZ) -> bool {
    surface.column(p).wetness() < 0.0
}

fn pit_drop(surface: &ContinentalSurface, p: GlobalXZ) -> bool {
    let g = surface.column(p).ground();
    let mut neighbour = g;
    for (dx, dz) in [(16.0, 0.0), (-16.0, 0.0), (0.0, 16.0), (0.0, -16.0)] {
        neighbour = neighbour.max(surface.column(GlobalXZ::at(p.x + dx, p.z + dz)).ground());
    }
    neighbour - g > 3.5
}

fn in_hinterland(p: GlobalXZ, pin: &SettlementPin, min_m: f64, max_m: f64) -> bool {
    let d = pin.at.distance(p);
    d >= min_m && d <= max_m
}

fn owns_hinterland(p: GlobalXZ, pin: &SettlementPin, pins: &[SettlementPin]) -> bool {
    pins.iter().all(|other| {
        other.id == pin.id || pin.at.distance(p) <= other.at.distance(p) + 1e-6
    })
}

pub fn plan_overland_sites(
    surface: &ContinentalSurface,
    pins: &[SettlementPin],
    hamlets: &[HamletStand],
) -> Vec<OverlandSite> {
    let seed = surface.world_seed();
    let mut ordered: Vec<SettlementPin> = pins.to_vec();
    ordered.sort_by_key(|p| p.id);

    let mut planned = Vec::new();
    let mut cairn_alts = Vec::new();
    let mut hut_alts = Vec::new();

    for pin in &ordered {
        let cairn = find_cairn(surface, pin, hamlets, pins);
        let hut = find_hut(surface, pin, hamlets, pins, seed);
        if let Some(c) = cairn {
            cairn_alts.push(c);
        }
        if let Some(h) = hut {
            hut_alts.push(h);
        }
        // Plains-road qualifies -> cairn. Hut only when cairn did not win.
        let pick = match (cairn, hut) {
            (Some(c), _) => Some(c),
            (None, Some(h)) => Some(h),
            (None, None) => None,
        };
        if let Some(site) = pick {
            planned.push(site);
        }
    }

    ensure_both_kinds(&mut planned, &cairn_alts, &hut_alts);
    planned.sort_by_key(|s| (s.pin_id, s.kind as u8));
    planned
}

fn ensure_both_kinds(
    planned: &mut Vec<OverlandSite>,
    cairn_alts: &[OverlandSite],
    hut_alts: &[OverlandSite],
) {
    let has_cairn = planned.iter().any(|s| s.kind == SiteKind::TakenCairn);
    let has_hut = planned.iter().any(|s| s.kind == SiteKind::WoodsHut);
    if !has_cairn {
        assign_alt(planned, cairn_alts);
    }
    if !has_hut {
        assign_alt(planned, hut_alts);
    }
}

fn assign_alt(planned: &mut Vec<OverlandSite>, alts: &[OverlandSite]) {
    let free = alts
        .iter()
        .copied()
        .find(|a| !planned.iter().any(|s| s.pin_id == a.pin_id));
    if let Some(site) = free {
        planned.push(site);
        return;
    }
    if let Some(site) = alts.first().copied() {
        if let Some(existing) = planned.iter_mut().find(|s| s.pin_id == site.pin_id) {
            *existing = site;
        } else {
            planned.push(site);
        }
    }
}

fn find_cairn(
    surface: &ContinentalSurface,
    pin: &SettlementPin,
    hamlets: &[HamletStand],
    pins: &[SettlementPin],
) -> Option<OverlandSite> {
    let mut best: Option<(f64, i32, usize, OverlandSite)> = None;
    for road in surface.roads() {
        if road.points.len() < 2 {
            continue;
        }
        for (si, pair) in road.points.windows(2).enumerate() {
            let a = pair[0];
            let b = pair[1];
            let span = a.distance(b);
            if span < 1e-3 {
                continue;
            }
            let steps = ((span / SAMPLE_M).ceil() as i32).max(1);
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let p = a.lerp(b, t);
                let xz = GlobalXZ::at(f64::from(p.x), f64::from(p.y));
                if !in_hinterland(xz, pin, CAIRN_MIN_PIN_M, HINTERLAND_MAX_M) {
                    continue;
                }
                if !owns_hinterland(xz, pin, pins) {
                    continue;
                }
                if hamlet_blocks(xz, hamlets, pins) {
                    continue;
                }
                if dungeon_blocks(surface, xz) || pit_drop(surface, xz) {
                    continue;
                }
                if surface.fields().biome_at(p.x, p.y) != Biome::Plains {
                    continue;
                }
                if !dry_land(surface, xz) {
                    continue;
                }
                let d = pin.at.distance(xz);
                let yaw = (b.x - a.x).atan2(b.y - a.y).to_degrees();
                let cand = OverlandSite {
                    kind: SiteKind::TakenCairn,
                    pin_id: pin.id,
                    at: xz,
                    yaw_deg: yaw,
                };
                let take = match best {
                    None => true,
                    Some((bd, brid, bsi, _)) => {
                        d < bd - 1e-6 || ((d - bd).abs() < 1e-6 && (road.id, si) < (brid, bsi))
                    }
                };
                if take {
                    best = Some((d, road.id, si, cand));
                }
            }
        }
    }
    best.map(|(_, _, _, s)| s)
}

fn find_hut(
    surface: &ContinentalSurface,
    pin: &SettlementPin,
    hamlets: &[HamletStand],
    pins: &[SettlementPin],
    world_seed: i32,
) -> Option<OverlandSite> {
    let start = (site_hash(world_seed, pin.id) % 360) as f32;
    let min_m = HUT_MIN_M.max(pin_keepout_m(pin));
    let mut ring = 0i32;
    loop {
        let r = min_m + f64::from(ring) * 8.0;
        if r > HINTERLAND_MAX_M {
            return None;
        }
        for k in 0..24 {
            let ang = (start + k as f32 * 15.0).to_radians();
            let xz = GlobalXZ::at(
                pin.at.x + r * f64::from(ang.cos()),
                pin.at.z + r * f64::from(ang.sin()),
            );
            if !in_hinterland(xz, pin, min_m, HINTERLAND_MAX_M) {
                continue;
            }
            if !owns_hinterland(xz, pin, pins) {
                continue;
            }
            if hamlet_blocks(xz, hamlets, pins) {
                continue;
            }
            if dungeon_blocks(surface, xz) || pit_drop(surface, xz) {
                continue;
            }
            if surface.fields().biome_at(xz.x as f32, xz.z as f32) != Biome::Forest {
                continue;
            }
            if !dry_land(surface, xz) {
                continue;
            }
            let yaw = ((xz.x - pin.at.x) as f32)
                .atan2((xz.z - pin.at.z) as f32)
                .to_degrees();
            return Some(OverlandSite {
                kind: SiteKind::WoodsHut,
                pin_id: pin.id,
                at: xz,
                yaw_deg: yaw,
            });
        }
        ring += 1;
    }
}

pub fn is_bandit_id(mob_id: &str) -> bool {
    matches!(mob_id, "bandit" | "male_bandit")
}

fn hostile_from_sheet(idx: i32, x: f64, z: f64) -> WorldHostile {
    let sheet = bandit_sheet();
    WorldHostile {
        idx,
        x,
        z,
        hp: f64::from(sheet.hp),
        max_hp: f64::from(sheet.hp),
        armor: sheet.armor,
        alive: true,
        stun_s: 0.0,
        slow_s: 0.0,
        root_s: 0.0,
        name: sheet.name.clone(),
        mob_id: sheet.id.clone(),
        entity: None,
        damage: sheet.damage,
        swing_s: sheet.swing_s,
        swing_cd: sheet.swing_s,
        reach_m: sheet.reach_m,
    }
}

pub fn seat_overland_sites(combat: &mut WorldCombat, sites: &[OverlandSite]) {
    let mut idx = combat.hostiles.iter().map(|h| h.idx).max().unwrap_or(-1) + 1;
    for site in sites {
        for (x, z) in site.bandit_xz() {
            combat.hostiles.push(hostile_from_sheet(idx, x, z));
            idx += 1;
        }
    }
}

pub fn clear_overland_sites(combat: &mut WorldCombat) {
    combat.hostiles.retain(|h| !is_bandit_id(&h.mob_id));
    if let Some(lock) = combat.lock {
        if !combat.hostiles.iter().any(|h| h.idx == lock) {
            combat.lock = None;
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

fn load_static(rel: &str) -> EngineResult<Mesh> {
    let assets = assets_dir()?;
    let path = assets.join(rel);
    if !path.is_file() {
        return Err(EngineError::Model(format!(
            "site mesh missing at {}",
            path.display()
        )));
    }
    Model::load(&path).map_err(|source| {
        EngineError::Model(format!("site mesh {} failed: {source}", path.display()))
    })
}

const CAIRN_ROCKS: &[(&str, f32, f32, f32)] = &[
    ("props/rocks/rock_fieldstone_low.glb", 0.0, 0.0, 0.0),
    ("props/rocks/rock_cobble_worn.glb", 0.85, 0.15, 18.0),
    ("props/rocks/rock_boulder_round.glb", -0.70, 0.40, -12.0),
];
const CAIRN_CRATE: (&str, f32, f32, f32) = ("props/crate_small.glb", 1.35, -0.55, 25.0);

fn local_xz(dx: f32, dz: f32, yaw_deg: f32) -> (f64, f64) {
    let (x, z) = kit::yaw_xz(dx, dz, yaw_deg);
    (f64::from(x), f64::from(z))
}

pub fn spawn_site_props(
    world: &mut World,
    surface: &ContinentalSurface,
    sites: &[OverlandSite],
) -> EngineResult<Vec<EntityId>> {
    let mut ids = Vec::new();
    let mut statics: HashMap<&str, Mesh> = HashMap::new();
    let mut hut_pieces: HashMap<String, Mesh> = HashMap::new();
    let catalog = kit::catalog();
    let world_seed = surface.world_seed();
    for site in sites {
        let before = ids.len();
        let ground = surface.column(site.at).ground();
        let one = (|| -> EngineResult<()> {
        match site.kind {
            SiteKind::TakenCairn => {
                for (rel, dx, dz, yaw) in CAIRN_ROCKS {
                    let mesh = if let Some(m) = statics.get(rel) {
                        m.clone()
                    } else {
                        let m = load_static(rel)?;
                        statics.insert(*rel, m.clone());
                        m
                    };
                    let (wx, wz) = local_xz(*dx, *dz, site.yaw_deg);
                    let at = GlobalPosition::at(site.at.x + wx, f64::from(ground), site.at.z + wz);
                    let place = GlobalPlace::at(at).with_yaw_deg(site.yaw_deg + *yaw);
                    ids.push(world.spawn_anchored(mesh, place)?);
                }
                let (rel, dx, dz, yaw) = CAIRN_CRATE;
                let mesh = if let Some(m) = statics.get(rel) {
                    m.clone()
                } else {
                    let m = load_static(rel)?;
                    statics.insert(rel, m.clone());
                    m
                };
                let (wx, wz) = local_xz(dx, dz, site.yaw_deg);
                let at = GlobalPosition::at(site.at.x + wx, f64::from(ground), site.at.z + wz);
                let place = GlobalPlace::at(at).with_yaw_deg(site.yaw_deg + yaw);
                ids.push(world.spawn_anchored(mesh, place)?);
            }
            SiteKind::WoodsHut => {
                let seed = site_hash(world_seed, site.pin_id);
                let places = kit::assemble_dwelling(&catalog, "house_hut_thatch", seed)
                    .map_err(|err| EngineError::Model(format!("woods hut kit: {err}")))?;
                for item in places {
                    let key = item.piece.to_string();
                    let mesh = if let Some(m) = hut_pieces.get(&key) {
                        m.clone()
                    } else {
                        let m = kit::load_piece_mesh(&item.piece).map_err(|err| {
                            EngineError::Model(format!("woods hut piece {key}: {err}"))
                        })?;
                        hut_pieces.insert(key.clone(), m.clone());
                        m
                    };
                    let p = item.place.position;
                    let (dx, dz) = kit::yaw_xz(p.x, p.z, site.yaw_deg);
                    let at = GlobalPosition::at(
                        site.at.x + f64::from(dx),
                        f64::from(ground + p.y),
                        site.at.z + f64::from(dz),
                    );
                    let place =
                        GlobalPlace::at(at).with_yaw_deg(site.yaw_deg + item.place.yaw_degrees);
                    ids.push(world.spawn_anchored(mesh, place)?);
                }
            }
        }
        Ok(())
        })();
        match one {
            Ok(()) => {}
            Err(_) => {
                ids.truncate(before);
            }
        }
    }
    Ok(ids)
}

pub fn despawn_site_props(world: &mut World, ids: &mut Vec<EntityId>) {
    for id in ids.drain(..) {
        world.despawn(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::ContinentAtlas;
    use std::sync::Arc;

    fn world_of(seed: i32, size: usize) -> Arc<ContinentalSurface> {
        let atlas = ContinentAtlas::generate(seed, size);
        Arc::new(ContinentalSurface::new(&atlas).expect("canonical surface"))
    }

    fn hamlets_for(pins: &[SettlementPin]) -> Vec<HamletStand> {
        pins.iter()
            .map(|pin| HamletStand {
                at: pin.at,
                radius: 20.0,
                houses: vec![],
                cut: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn seed1_size64_has_one_cairn_and_one_hut() {
        let surface = world_of(1, 64);
        let pins = surface.settlements();
        let hamlets = hamlets_for(pins);
        let sites = plan_overland_sites(&surface, pins, &hamlets);
        let cairns = sites
            .iter()
            .filter(|s| s.kind == SiteKind::TakenCairn)
            .count();
        let huts = sites
            .iter()
            .filter(|s| s.kind == SiteKind::WoodsHut)
            .count();
        assert!(
            cairns >= 1,
            "seed 1 size 64 must stamp a Taken Cairn, got {sites:?}"
        );
        assert!(
            huts >= 1,
            "seed 1 size 64 must stamp a Woods Hut, got {sites:?}"
        );
        let mut pins_used = std::collections::BTreeSet::new();
        for site in &sites {
            assert!(
                pins_used.insert(site.pin_id),
                "pin {} has more than one site",
                site.pin_id
            );
            assert!(
                !hamlet_blocks(site.at, &hamlets, pins),
                "site {site:?} sits in a hamlet pad"
            );
            match site.kind {
                SiteKind::TakenCairn => {
                    assert_eq!(
                        surface
                            .fields()
                            .biome_at(site.at.x as f32, site.at.z as f32),
                        Biome::Plains
                    );
                    let on_road = surface.roads().iter().any(|road| {
                        road.points.windows(2).any(|w| {
                            dist_point_seg(
                                glam::Vec2::new(site.at.x as f32, site.at.z as f32),
                                w[0],
                                w[1],
                            ) <= 4.5
                        })
                    });
                    assert!(on_road, "cairn {:?} is not on a plains road", site.at);
                }
                SiteKind::WoodsHut => {
                    let pin = pins.iter().find(|p| p.id == site.pin_id).unwrap();
                    assert!(
                        pin.at.distance(site.at) + 1e-6 >= HUT_MIN_M,
                        "hut {:.1} m from pin, want >= {HUT_MIN_M}",
                        pin.at.distance(site.at)
                    );
                    assert_eq!(
                        surface
                            .fields()
                            .biome_at(site.at.x as f32, site.at.z as f32),
                        Biome::Forest
                    );
                }
            }
        }
        let again = plan_overland_sites(&surface, pins, &hamlets);
        assert_eq!(sites, again, "sites must be seed-stable");
    }

    fn dist_point_seg(p: glam::Vec2, a: glam::Vec2, b: glam::Vec2) -> f32 {
        let ab = b - a;
        let t = ((p - a).dot(ab) / ab.length_squared().max(1e-8)).clamp(0.0, 1.0);
        (a + ab * t - p).length()
    }

    #[test]
    fn hamlet_pad_rejects_site() {
        let hamlet = HamletStand {
            at: GlobalXZ::at(0.0, 0.0),
            radius: 20.0,
            houses: vec![],
            cut: Vec::new(),
        };
        let pin = SettlementPin {
            id: 1,
            at: GlobalXZ::at(0.0, 0.0),
            tier: 0,
            population: 3,
        };
        let inside = GlobalXZ::at(0.0, 10.0);
        assert!(hamlet.covers(inside, SITE_HAMLET_PAD_M));
        assert!(hamlet_blocks(inside, &[hamlet.clone()], &[pin]));
        let far = GlobalXZ::at(0.0, 200.0);
        assert!(!hamlet.covers(far, SITE_HAMLET_PAD_M));
        assert!(!hamlet_blocks(far, &[hamlet], &[pin]));
    }
}
