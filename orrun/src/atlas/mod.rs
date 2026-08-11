//! Continent atlas — climate, lakes, rivers, roads.

pub mod biomes;
mod classify;
mod continent;
pub mod features;
mod lakes;
mod landmask;
mod nodes;
mod orogen;
pub mod pack;
mod population;
pub mod preview;
mod rivers;
mod roads;
pub mod types;

pub use biomes::Biome;
pub use continent::{
    AtlasError, ContinentAtlas, CELL_METRES, SCHEMA_VERSION, SEA_SURFACE_Z, SIZE,
};
pub use features::{edge_owner, Dir, EndpointKind, Kind, NodeKind, RoadClass};
pub use pack::{elevation_to_metres, metres_to_elevation, pack, population as cell_population};
pub use types::{Crossing, Endpoint, GraphNode, Lake, Link, Port};

use rustc_hash::FxHasher;
use std::hash::{Hash, Hasher};

/// Deterministic layer seed from world seed + layer name.
pub fn layer_seed(world_seed: i32, name: &str) -> u32 {
    let mut h = FxHasher::default();
    world_seed.hash(&mut h);
    b':'.hash(&mut h);
    name.hash(&mut h);
    h.finish() as u32
}

/// Stable positive feature id from seed material.
pub fn feature_hash(parts: &[&str]) -> i32 {
    let mut h = FxHasher::default();
    for p in parts {
        p.hash(&mut h);
        b'|'.hash(&mut h);
    }
    (h.finish() as i32) & 0x7fff_ffff
}

#[inline]
pub(crate) fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
pub(crate) fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Cardinal then diagonal neighbours (matches Godot NEIGHBOR_DX/DZ).
pub(crate) const NEIGHBOR_DX: [i32; 8] = [1, 1, 0, -1, -1, -1, 0, 1];
pub(crate) const NEIGHBOR_DZ: [i32; 8] = [0, 1, 1, 1, 0, -1, -1, -1];

#[inline]
pub(crate) fn cardinal(k: usize) -> (i32, i32) {
    (NEIGHBOR_DX[k * 2], NEIGHBOR_DZ[k * 2])
}

#[cfg(test)]
mod tests;
