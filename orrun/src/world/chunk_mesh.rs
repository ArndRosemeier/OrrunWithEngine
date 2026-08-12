//! Seam-safe terrain chunks built from the canonical surface.
//!
//! A chunk is a square patch of the continent, sampled on a fixed global
//! lattice with a one-sample halo. Because the lattice is global, two adjacent
//! chunks evaluate *the same* columns on their shared edge and therefore agree
//! on height, normal, and water contour without any stitching pass.
//!
//! One bake produces everything that must agree about a patch of ground:
//! the land mesh, the water mesh, and the CPU contact grid the player stands on.

use std::sync::Arc;

use engine::chunk_stream::{ChunkBuilder, ChunkPayload};
use engine::color::{rgba, Color};
use engine::contact::ContactGrid;
use engine::error::EngineResult;
use engine::mesh::BuiltMesh;
use engine::space::{ChunkCoord, ChunkLayer, ChunkSpan, GlobalXZ};
use engine::SurfaceMeshStyle;
use glam::{Vec3, Vec4};

use super::coords::{chunk_span, CHUNK_SAMPLE_M};
use super::surface::{ContinentalSurface, SurfaceColumn, SurfaceMaterial, WaterBody};

/// Depth that a fully opaque water vertex stands for.
///
/// The sheet carries its depth in vertex alpha and the engine's water material
/// decodes it with the same scale, which is how a shallow margin stays clear
/// while a channel goes dark without any second geometry pass.
pub const WATER_DEPTH_SCALE_M: f32 = 9.0;

/// Body tint, around the neutral 0.45 the water shader expects.
fn water_tint(body: Option<WaterBody>) -> Color {
    match body {
        Some(WaterBody::Ocean) | None => rgba(104, 116, 130, 255),
        Some(WaterBody::Lake { .. }) => rgba(104, 126, 122, 255),
        Some(WaterBody::River { .. }) => rgba(112, 122, 104, 255),
    }
}

/// A water vertex: where the sheet sits, and how deep it is there.
#[derive(Clone, Copy, Debug)]
struct WaterVertex {
    position: Vec3,
    depth_m: f32,
    body: Option<WaterBody>,
}

impl WaterVertex {
    fn color(self) -> Vec4 {
        let mut c: Vec4 = water_tint(self.body).into();
        c.w = (self.depth_m / WATER_DEPTH_SCALE_M).clamp(0.0, 1.0);
        c
    }
}

/// Samples of one chunk plus its halo, on the global lattice.
struct ChunkSamples {
    /// Samples per axis including both halo rings.
    stride: usize,
    /// Vertices per axis of the drawn mesh.
    verts: usize,
    step: f64,
    origin: GlobalXZ,
    columns: Vec<SurfaceColumn>,
}

impl ChunkSamples {
    /// `ix`/`iz` are mesh vertex indices; `-1` and `verts` address the halo.
    #[inline]
    fn column(&self, ix: i32, iz: i32) -> SurfaceColumn {
        let sx = (ix + 1) as usize;
        let sz = (iz + 1) as usize;
        self.columns[sz * self.stride + sx]
    }

    #[inline]
    fn local(&self, ix: i32, iz: i32) -> (f32, f32) {
        (
            (ix as f64 * self.step) as f32,
            (iz as f64 * self.step) as f32,
        )
    }
}

/// Builds land, water, and contact for one chunk from [`ContinentalSurface`].
pub struct TerrainChunkBuilder {
    surface: Arc<ContinentalSurface>,
    span: ChunkSpan,
    sample_m: f64,
    style: SurfaceMeshStyle,
}

impl TerrainChunkBuilder {
    pub fn new(surface: Arc<ContinentalSurface>) -> Self {
        Self {
            surface,
            span: chunk_span(),
            sample_m: CHUNK_SAMPLE_M,
            style: SurfaceMeshStyle {
                rock_height: super::surface::ROCK_HEIGHT,
                ..SurfaceMeshStyle::default()
            },
        }
    }

    pub fn surface(&self) -> &ContinentalSurface {
        &self.surface
    }

    pub fn sample_metres(&self) -> f64 {
        self.sample_m
    }

    /// Vertices per axis of one chunk's land mesh.
    pub fn verts_per_axis(&self) -> usize {
        (self.span.metres() / self.sample_m).round() as usize + 1
    }

    /// True when the chunk lies completely outside the generated atlas.
    fn outside_atlas(&self, coord: ChunkCoord) -> bool {
        let bounds = self.surface.bounds();
        let origin = coord.origin(self.span);
        let span = self.span.metres();
        let world = bounds.metres();
        origin.x + span <= 0.0 || origin.z + span <= 0.0 || origin.x >= world || origin.z >= world
    }

    /// Samples the chunk plus its halo.
    ///
    /// Deliberately serial: whole chunks are already baked in parallel on the
    /// scheduler's worker pool, and nesting a parallel iterator inside one of
    /// those jobs lets rayon stack recursive stolen work until the worker
    /// thread overflows.
    fn sample_chunk(&self, coord: ChunkCoord) -> ChunkSamples {
        let verts = self.verts_per_axis();
        let stride = verts + 2;
        let step = self.sample_m;
        let origin = coord.origin(self.span);
        let mut columns: Vec<SurfaceColumn> = Vec::with_capacity(stride * stride);
        for sz in -1..(stride as i32 - 1) {
            for sx in -1..(stride as i32 - 1) {
                let p = GlobalXZ::at(origin.x + sx as f64 * step, origin.z + sz as f64 * step);
                columns.push(self.surface.column(p));
            }
        }
        ChunkSamples {
            stride,
            verts,
            step,
            origin,
            columns,
        }
    }

    fn build_land(&self, s: &ChunkSamples) -> BuiltMesh {
        let n = s.verts as i32;
        let count = s.verts * s.verts;
        let sea = self.surface.sea_surface_z();
        let mut positions = Vec::with_capacity(count);
        let mut normals = Vec::with_capacity(count);
        let mut colors = Vec::with_capacity(count);
        let mut indices = Vec::with_capacity((s.verts - 1) * (s.verts - 1) * 6);

        let step = s.step as f32;
        for iz in 0..n {
            for ix in 0..n {
                let column = s.column(ix, iz);
                // Halo samples make edge normals identical in both chunks that
                // share the vertex, so lighting has no visible seam.
                let normal = Vec3::new(
                    s.column(ix - 1, iz).ground() - s.column(ix + 1, iz).ground(),
                    2.0 * step,
                    s.column(ix, iz - 1).ground() - s.column(ix, iz + 1).ground(),
                )
                .normalize_or_zero();
                let normal = if normal.length_squared() > 0.0 {
                    normal
                } else {
                    Vec3::Y
                };
                let (lx, lz) = s.local(ix, iz);
                positions.push(Vec3::new(lx, column.ground(), lz));
                normals.push(normal);
                colors.push(self.vertex_color(column, normal, sea));
            }
        }

        // One fixed diagonal for every quad; `ContactGrid` splits the same way,
        // so the collision surface is exactly the drawn surface.
        for iz in 0..s.verts - 1 {
            for ix in 0..s.verts - 1 {
                let i00 = (iz * s.verts + ix) as u32;
                let i10 = i00 + 1;
                let i01 = i00 + s.verts as u32;
                let i11 = i01 + 1;
                indices.extend([i00, i01, i11, i00, i11, i10]);
            }
        }

        let opaque_index_count = indices.len();
        BuiltMesh {
            positions,
            normals,
            colors,
            indices,
            opaque_index_count,
        }
    }

    fn vertex_color(&self, column: SurfaceColumn, normal: Vec3, sea: f32) -> Vec4 {
        let base = match column.material(sea) {
            SurfaceMaterial::Bed => self.style.bed,
            SurfaceMaterial::Sand => self.style.sand,
            SurfaceMaterial::Grass => self.style.grass,
            SurfaceMaterial::Rock => self.style.rock,
        };
        let mut color: Vec4 = base.into();
        if !column.is_wet() {
            let slope = (1.0 - normal.y).clamp(0.0, 1.0);
            let lift = ((column.ground() * 0.015).sin() * 0.04) + slope * 0.06;
            color.x = (color.x * (1.0 - slope * 0.15) + lift * 0.35).clamp(0.0, 1.0);
            color.y = (color.y * (1.0 - slope * 0.05) + lift * 0.15).clamp(0.0, 1.0);
            color.z = (color.z * (1.0 - lift * 0.2)).clamp(0.0, 1.0);
        }
        color
    }

    /// Water is the `wetness >= 0` region of the surface, contoured with
    /// marching squares. Ocean, lake, and river sheets all come out of this one
    /// field, so there is no second ribbon path to drift out of alignment.
    fn build_water(&self, s: &ChunkSamples) -> Option<BuiltMesh> {
        let mut positions: Vec<Vec3> = Vec::new();
        let mut normals: Vec<Vec3> = Vec::new();
        let mut colors: Vec<Vec4> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();

        let n = s.verts as i32;
        for iz in 0..n - 1 {
            for ix in 0..n - 1 {
                for poly in cell_water_polygons(s, ix, iz) {
                    push_fan(
                        &poly,
                        &mut positions,
                        &mut normals,
                        &mut colors,
                        &mut indices,
                    );
                }
            }
        }

        if indices.is_empty() {
            return None;
        }
        Some(BuiltMesh {
            positions,
            normals,
            colors,
            indices,
            // Fully translucent layer.
            opaque_index_count: 0,
        })
    }

    fn build_contact(&self, s: &ChunkSamples) -> EngineResult<ContactGrid> {
        let n = s.verts as i32;
        let mut heights = Vec::with_capacity(s.verts * s.verts);
        for iz in 0..n {
            for ix in 0..n {
                heights.push(s.column(ix, iz).ground());
            }
        }
        ContactGrid::new(s.origin, s.step, s.verts, heights)
    }
}

impl ChunkBuilder for TerrainChunkBuilder {
    fn span(&self) -> ChunkSpan {
        self.span
    }

    fn build(&self, coord: ChunkCoord) -> EngineResult<Option<ChunkPayload>> {
        if self.outside_atlas(coord) {
            return Ok(None);
        }
        let samples = self.sample_chunk(coord);
        let land = self.build_land(&samples);
        let water = self.build_water(&samples);
        let contact = self.build_contact(&samples)?;

        let anchor = samples.origin.with_height(0.0)?;
        let mut payload = ChunkPayload::new(anchor)
            .with_layer(ChunkLayer::Land, land)?
            .with_contact(contact);
        if let Some(water) = water {
            payload = payload.with_layer(ChunkLayer::Water, water)?;
        }
        Ok(Some(payload))
    }
}

/// A water polygon inside one lattice cell, in chunk-local metres.
type WaterPoly = Vec<WaterVertex>;

/// Corner walk `(0,0) → (0,1) → (1,1) → (1,0)`, matching the land winding.
const CORNERS: [(i32, i32); 4] = [(0, 0), (0, 1), (1, 1), (1, 0)];

fn cell_water_polygons(s: &ChunkSamples, ix: i32, iz: i32) -> Vec<WaterPoly> {
    let cols: [SurfaceColumn; 4] = [
        s.column(ix + CORNERS[0].0, iz + CORNERS[0].1),
        s.column(ix + CORNERS[1].0, iz + CORNERS[1].1),
        s.column(ix + CORNERS[2].0, iz + CORNERS[2].1),
        s.column(ix + CORNERS[3].0, iz + CORNERS[3].1),
    ];
    let wet: [bool; 4] = [
        cols[0].wetness() >= 0.0,
        cols[1].wetness() >= 0.0,
        cols[2].wetness() >= 0.0,
        cols[3].wetness() >= 0.0,
    ];
    let inside = wet.iter().filter(|w| **w).count();
    if inside == 0 {
        return Vec::new();
    }

    let corner = |k: usize| -> WaterVertex {
        let (dx, dz) = CORNERS[k];
        let (lx, lz) = s.local(ix + dx, iz + dz);
        let sheet = cols[k].sheet_hint();
        WaterVertex {
            position: Vec3::new(lx, sheet, lz),
            depth_m: (sheet - cols[k].ground()).max(0.0),
            body: cols[k].body(),
        }
    };
    // The crossing sits at zero depth, so it takes the wet side's sheet height:
    // interpolating towards a dry column would tilt the water surface.
    //
    // The pair is ordered by global lattice index first, so the cell on the
    // other side of an edge evaluates the identical expression and the two
    // shorelines meet bit-for-bit.
    let crossing = |a: usize, b: usize| -> WaterVertex {
        let (a, b) = if (CORNERS[b].1, CORNERS[b].0) < (CORNERS[a].1, CORNERS[a].0) {
            (b, a)
        } else {
            (a, b)
        };
        let wa = cols[a].wetness();
        let wb = cols[b].wetness();
        let t = (wa / (wa - wb)).clamp(0.0, 1.0);
        let pa = corner(a).position;
        let pb = corner(b).position;
        let inside = if wet[a] { a } else { b };
        WaterVertex {
            position: Vec3::new(
                pa.x + (pb.x - pa.x) * t,
                cols[inside].sheet_hint(),
                pa.z + (pb.z - pa.z) * t,
            ),
            // The contour is the waterline, so the sheet has run out here.
            depth_m: 0.0,
            body: cols[inside].body(),
        }
    };

    // Saddle: opposite corners wet. The bilinear centre decides whether the two
    // wet lobes are joined; without it the choice would flip between neighbours
    // and tear the shoreline.
    let saddle = inside == 2 && ((wet[0] && wet[2]) || (wet[1] && wet[3]));
    if saddle {
        let centre =
            0.25 * (cols[0].wetness() + cols[1].wetness() + cols[2].wetness() + cols[3].wetness());
        if centre < 0.0 {
            let lobe = |k: usize| -> WaterPoly {
                let prev = (k + 3) % 4;
                let next = (k + 1) % 4;
                vec![crossing(k, prev), corner(k), crossing(k, next)]
            };
            let first = if wet[0] { 0 } else { 1 };
            return vec![lobe(first), lobe(first + 2)];
        }
    }

    let mut poly: WaterPoly = Vec::with_capacity(6);
    for k in 0..4 {
        let next = (k + 1) % 4;
        if wet[k] {
            poly.push(corner(k));
        }
        if wet[k] != wet[next] {
            poly.push(crossing(k, next));
        }
    }
    if poly.len() < 3 {
        return Vec::new();
    }
    vec![poly]
}

/// Fan from the centroid: works for the concave joined-saddle polygon too.
fn push_fan(
    poly: &WaterPoly,
    positions: &mut Vec<Vec3>,
    normals: &mut Vec<Vec3>,
    colors: &mut Vec<Vec4>,
    indices: &mut Vec<u32>,
) {
    if poly.len() < 3 {
        return;
    }
    let inv = 1.0 / poly.len() as f32;
    let mut centre = WaterVertex {
        position: Vec3::ZERO,
        depth_m: 0.0,
        body: poly[0].body,
    };
    for v in poly {
        centre.position += v.position * inv;
        centre.depth_m += v.depth_m * inv;
        centre.body = centre.body.or(v.body);
    }

    let base = positions.len() as u32;
    positions.push(centre.position);
    normals.push(Vec3::Y);
    colors.push(centre.color());
    for v in poly {
        positions.push(v.position);
        normals.push(Vec3::Y);
        colors.push(v.color());
    }
    for k in 0..poly.len() {
        let a = base + 1 + k as u32;
        let b = base + 1 + ((k + 1) % poly.len()) as u32;
        indices.extend([base, a, b]);
    }
}
