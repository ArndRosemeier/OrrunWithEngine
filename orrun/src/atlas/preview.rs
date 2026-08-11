//! CPU preview raster for the atlas map viewer.

use super::biomes::Biome;
use super::pack;
use super::ContinentAtlas;

/// Biome overview as tightly packed RGBA8 (`size * size * 4` bytes).
pub fn biome_rgba(atlas: &ContinentAtlas) -> Vec<u8> {
    let n = atlas.size * atlas.size;
    let mut out = vec![0u8; n * 4];
    for (i, &cell) in atlas.cells.iter().enumerate() {
        let rgb = pack::biome(cell).color_rgb();
        let o = i * 4;
        out[o] = rgb[0];
        out[o + 1] = rgb[1];
        out[o + 2] = rgb[2];
        out[o + 3] = 255;
    }
    out
}

/// Elevation overview (sea → peaks) as RGBA8.
pub fn elevation_rgba(atlas: &ContinentAtlas) -> Vec<u8> {
    let n = atlas.size * atlas.size;
    let mut out = vec![0u8; n * 4];
    for (i, &cell) in atlas.cells.iter().enumerate() {
        let e = pack::elevation(cell) as u8;
        let biome = pack::biome(cell);
        let o = i * 4;
        if biome == Biome::Ocean {
            out[o] = 20;
            out[o + 1] = 40;
            out[o + 2] = 70u8.saturating_add(e / 2);
        } else if biome == Biome::Lake {
            out[o] = 40;
            out[o + 1] = 90;
            out[o + 2] = 140;
        } else {
            out[o] = e;
            out[o + 1] = e.saturating_sub(20);
            out[o + 2] = e.saturating_sub(40);
        }
        out[o + 3] = 255;
    }
    out
}
