//! World-metre road polylines from atlas cell links.
//!
//! Each cell is meandered the same way as the overlay, then unique corridors
//! are stitched at shared ports. Duplicate cell links used to be appended as
//! out-and-back traversals, which is the scribble of overlapping dirt.

use glam::Vec2;
use rustc_hash::FxHashMap;

use super::features::{edge_owner, Dir, EndpointKind, Kind};
use super::types::{Endpoint, Link};
use super::{layer_seed, ContinentAtlas, CELL_METRES};

const JOIN_M: f32 = 12.0;

#[derive(Clone, Debug)]
pub struct RoadPath {
    pub id: i32,
    pub class: i32,
    pub points: Vec<Vec2>,
}

pub fn bake_road_paths(atlas: &ContinentAtlas) -> Vec<RoadPath> {
    let seed = layer_seed(atlas.world_seed, "cell_overlay") ^ 0xA0AD;
    let mut by_id: FxHashMap<i32, (i32, Vec<Vec<Vec2>>)> = FxHashMap::default();
    let size = atlas.size as i32;
    for az in 0..size {
        for ax in 0..size {
            let cell_idx = atlas.index_of(ax, az) as i32;
            for link in unique_links(atlas.links_in_cell(ax, az, Kind::Road)) {
                let a = endpoint_local(atlas, ax, az, link.a);
                let b = endpoint_local(atlas, ax, az, link.b);
                if a.distance(b) < 1e-4 {
                    continue;
                }
                let world = meander_cell_corridor(a, b, seed, cell_idx, link.feature_id)
                    .into_iter()
                    .map(|p| {
                        Vec2::new(
                            (ax as f32 + p.x) * CELL_METRES,
                            (az as f32 + p.y) * CELL_METRES,
                        )
                    })
                    .collect::<Vec<_>>();
                if world.len() < 2 {
                    continue;
                }
                let entry = by_id
                    .entry(link.feature_id)
                    .or_insert((link.feature_class, Vec::new()));
                entry.0 = entry.0.min(link.feature_class);
                entry.1.push(world);
            }
        }
    }

    let mut out: Vec<RoadPath> = Vec::new();
    for (id, (class, pieces)) in by_id {
        for points in stitch_polylines(dedupe_pieces(pieces)) {
            if points.len() >= 2 {
                out.push(RoadPath { id, class, points });
            }
        }
    }
    out.sort_by_key(|p| (p.class, p.id, p.points.len() as i32));
    out
}

/// Same corridor the cell overlay draws, in cell-local `[0, 1]²`.
pub(crate) fn meander_cell_corridor(
    a: Vec2,
    b: Vec2,
    seed: u32,
    cell_idx: i32,
    feature_id: i32,
) -> Vec<Vec2> {
    const STEP_LOCAL: f32 = 0.04;
    const MIN_SAMPLES: usize = 10;
    const MAX_SAMPLES: usize = 36;
    const AMP_COARSE: f32 = 0.05;
    const AMP_FINE: f32 = 0.018;
    const AMP_MICRO: f32 = 0.006;

    let delta = b - a;
    let len = delta.length();
    if len < 1e-4 {
        return vec![a, b];
    }
    let dir = delta / len;
    let perp = Vec2::new(-dir.y, dir.x);
    let n = ((len / STEP_LOCAL).ceil() as usize).clamp(MIN_SAMPLES, MAX_SAMPLES);

    let mut pts = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        if i == 0 {
            pts.push(a);
            continue;
        }
        if i == n {
            pts.push(b);
            continue;
        }
        let envelope = (std::f32::consts::PI * t).sin();
        let h0 = hash_unit(seed, cell_idx as u32, feature_id as u32, i as u32);
        let h1 = hash_unit(seed ^ 0x9E37, cell_idx as u32, feature_id as u32, (i * 3) as u32);
        let h2 = hash_unit(seed ^ 0x85EB, cell_idx as u32, feature_id as u32, (i * 7) as u32);
        let h0b = hash_unit(seed, cell_idx as u32, feature_id as u32, (i + 1) as u32);
        let coarse = (h0 * 2.0 - 1.0) * 0.65 + (h0b * 2.0 - 1.0) * 0.35;
        let fine = (h1 * 2.0 - 1.0) * (0.5 + 0.5 * (t * 11.0).sin());
        let micro = (h2 * 2.0 - 1.0) * (0.5 + 0.5 * (t * 29.0).cos());
        let lateral = envelope * len * (coarse * AMP_COARSE + fine * AMP_FINE + micro * AMP_MICRO);
        let p = a + dir * (len * t) + perp * lateral;
        pts.push(Vec2::new(p.x.clamp(0.02, 0.98), p.y.clamp(0.02, 0.98)));
    }
    pts
}

fn unique_links(links: &[Link]) -> Vec<&Link> {
    let mut best: FxHashMap<(EndpointKey, EndpointKey), &Link> = FxHashMap::default();
    for link in links {
        let mut ka = endpoint_key(link.a);
        let mut kb = endpoint_key(link.b);
        if kb < ka {
            std::mem::swap(&mut ka, &mut kb);
        }
        best.entry((ka, kb))
            .and_modify(|prev| {
                if link.feature_class < prev.feature_class {
                    *prev = link;
                }
            })
            .or_insert(link);
    }
    best.into_values().collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct EndpointKey {
    kind: u8,
    ref_id: i32,
    port_id: i32,
}

fn endpoint_key(ep: Endpoint) -> EndpointKey {
    EndpointKey {
        kind: ep.kind as u8,
        ref_id: ep.ref_id,
        port_id: ep.port_id,
    }
}

fn endpoint_local(atlas: &ContinentAtlas, ax: i32, az: i32, ep: Endpoint) -> Vec2 {
    match ep.kind {
        EndpointKind::Node => Vec2::new(0.5, 0.5),
        EndpointKind::EdgePort => {
            let (ox, oz, dir) = edge_owner(ep.ref_id);
            let ports = atlas
                .road_ports
                .get(&ep.ref_id)
                .map(|p| p.as_slice())
                .unwrap_or(&[]);
            let mut t = 0.5_f32;
            for p in ports {
                if p.id == ep.port_id {
                    t = p.t;
                    break;
                }
            }
            let (wx, wz) = match dir {
                Dir::East => (ox as f32 + 1.0, oz as f32 + t),
                Dir::South => (ox as f32 + t, oz as f32 + 1.0),
                _ => (ax as f32 + 0.5, az as f32 + 0.5),
            };
            Vec2::new(
                (wx - ax as f32).clamp(0.0, 1.0),
                (wz - az as f32).clamp(0.0, 1.0),
            )
        }
        EndpointKind::Ocean | EndpointKind::Lake => Vec2::new(0.5, 0.5),
    }
}

fn hash_unit(a: u32, b: u32, c: u32, d: u32) -> f32 {
    hash_u32(a ^ d.wrapping_mul(0x27D4_EB2D), b, c) as f32 / u32::MAX as f32
}

fn hash_u32(a: u32, b: u32, c: u32) -> u32 {
    let mut x = a
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(b)
        .wrapping_mul(0x85EB_CA6B)
        .wrapping_add(c);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x
}

fn dedupe_pieces(pieces: Vec<Vec<Vec2>>) -> Vec<Vec<Vec2>> {
    let mut out = Vec::new();
    for p in pieces {
        if p.len() < 2 {
            continue;
        }
        let a = p[0];
        let b = *p.last().expect("len >= 2");
        let dup = out.iter().any(|q: &Vec<Vec2>| {
            let qa = q[0];
            let qb = *q.last().expect("len >= 2");
            (qa.distance(a) < JOIN_M && qb.distance(b) < JOIN_M)
                || (qa.distance(b) < JOIN_M && qb.distance(a) < JOIN_M)
        });
        if !dup {
            out.push(p);
        }
    }
    out
}

fn stitch_polylines(mut unused: Vec<Vec<Vec2>>) -> Vec<Vec<Vec2>> {
    let mut chains = Vec::new();
    while !unused.is_empty() {
        let mut chain = unused.swap_remove(0);
        let mut grew = true;
        while grew {
            grew = false;
            let head = *chain.first().expect("chain");
            let tail = *chain.last().expect("chain");
            let mut take = None;
            for (i, piece) in unused.iter().enumerate() {
                let a = piece[0];
                let b = *piece.last().expect("piece");
                if a.distance(tail) < JOIN_M {
                    take = Some((i, true, false));
                    break;
                }
                if b.distance(tail) < JOIN_M {
                    take = Some((i, true, true));
                    break;
                }
                if b.distance(head) < JOIN_M {
                    take = Some((i, false, false));
                    break;
                }
                if a.distance(head) < JOIN_M {
                    take = Some((i, false, true));
                    break;
                }
            }
            let Some((i, at_tail, rev)) = take else {
                break;
            };
            let mut piece = unused.swap_remove(i);
            if rev {
                piece.reverse();
            }
            if reverses_onto(&chain, &piece, at_tail) {
                continue;
            }
            join_piece(&mut chain, piece, at_tail);
            grew = true;
        }
        if chain.len() >= 2 {
            chains.push(chain);
        }
    }
    chains
}

fn reverses_onto(chain: &[Vec2], piece: &[Vec2], at_tail: bool) -> bool {
    if chain.len() < 2 || piece.len() < 2 {
        return false;
    }
    if at_tail {
        chain[chain.len() - 2].distance(piece[1]) < JOIN_M
    } else {
        chain[1].distance(piece[piece.len() - 2]) < JOIN_M
    }
}

fn join_piece(chain: &mut Vec<Vec2>, piece: Vec<Vec2>, at_tail: bool) {
    if at_tail {
        let skip = usize::from(piece[0].distance(*chain.last().expect("chain")) < 2.0);
        chain.extend(piece.into_iter().skip(skip));
    } else {
        let skip_last = piece.last().expect("piece").distance(*chain.first().expect("chain")) < 2.0;
        let mut prefix = piece;
        if skip_last {
            prefix.pop();
        }
        prefix.append(chain);
        *chain = prefix;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContinentAtlas;

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y)
    }

    #[test]
    fn duplicate_corridors_do_not_scribble() {
        let a = vec![v(0.0, 0.0), v(500.0, 0.0), v(1000.0, 0.0)];
        let b = vec![v(1000.0, 0.0), v(1500.0, 0.0), v(2000.0, 0.0)];
        let chains = stitch_polylines(dedupe_pieces(vec![
            a.clone(),
            b.clone(),
            a,
            b.clone(),
            {
                let mut rev = b;
                rev.reverse();
                rev
            },
        ]));
        assert_eq!(chains.len(), 1, "one road, not a bundle of copies");
        let p = &chains[0];
        assert_eq!(p.len(), 5);
        assert!(p[0].distance(v(0.0, 0.0)) < 1.0 || p[0].distance(v(2000.0, 0.0)) < 1.0);
        assert!(p.last().unwrap().distance(v(2000.0, 0.0)) < 1.0 || p.last().unwrap().distance(v(0.0, 0.0)) < 1.0);
    }

    #[test]
    fn baked_roads_do_not_fold_back_on_themselves() {
        let atlas = ContinentAtlas::generate(20260809, 64);
        let roads = bake_road_paths(&atlas);
        assert!(!roads.is_empty(), "this seed has roads");
        for road in &roads {
            let mut len = 0.0_f32;
            for w in road.points.windows(2) {
                len += w[0].distance(w[1]);
            }
            let mut min = road.points[0];
            let mut max = road.points[0];
            for p in &road.points {
                min = min.min(*p);
                max = max.max(*p);
            }
            let extent = (max - min).length().max(1.0);
            assert!(
                len < extent * 3.5,
                "road {} scribbles: path {len:.0} m in a {extent:.0} m box",
                road.id
            );
        }
    }
}
