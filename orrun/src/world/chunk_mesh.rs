//! Seam-safe terrain chunks built from the canonical surface.
//!
//! A chunk is a square patch of the continent, sampled on a fixed global
//! lattice with a one-sample halo. Because the lattice is global, two adjacent
//! chunks evaluate *the same* columns on their shared edge and therefore agree
//! on height, normal, and water contour without any stitching pass.
//!
//! One bake produces everything that must agree about a patch of ground:
//! the land mesh, the water mesh, and the CPU contact grid the player stands on.

use std::sync::{Arc, RwLock};

use engine::chunk_stream::{ChunkBuilder, ChunkPayload};
use engine::color::{rgb, rgba, Color};
use engine::contact::ContactGrid;
use engine::error::EngineResult;
use engine::mesh::BuiltMesh;
use engine::space::{ChunkCoord, ChunkLayer, ChunkSpan, GlobalXZ};
use engine::SurfaceMeshStyle;
use glam::{Vec3, Vec4};

use super::coords::{chunk_span, CHUNK_SAMPLE_M};
use super::footprint::{self, HousePlot};
use super::ponds::SharedPonds;
use super::scatter::{canopy_noise, soil_drift, soil_patch, Fall, GroundCover};
use super::surface::{
    lerp, smoothstep, ContinentalSurface, SurfaceColumn, SurfaceMaterial, WaterBody,
};

/// Depth that a fully opaque water vertex stands for.
///
/// The sheet carries its depth in vertex alpha and the engine's water material
/// decodes it with the same scale, which is how a shallow margin stays clear
/// while a channel goes dark without any second geometry pass.
pub const WATER_DEPTH_SCALE_M: f32 = 9.0;

/// Ground seen through a closed stand: shaded, and much less yellow than the
/// open grass the same cell would otherwise be.
const CANOPY_TINT: [u8; 3] = [46, 74, 42];
/// The green strip along a bank, wetter and ranker than the ground behind it.
const RIPARIAN_TINT: [u8; 3] = [74, 126, 66];
/// Standing-wet ground: sedge and peat rather than grass.
const BOG_TINT: [u8; 3] = [98, 104, 64];

/// sRGB bytes as the linear colour the meshes carry.
fn tint(c: [u8; 3]) -> Vec4 {
    rgb(c[0], c[1], c[2]).into()
}

/// Body tint, around the neutral 0.45 the water shader expects.
fn water_tint(body: Option<WaterBody>) -> Color {
    match body {
        Some(WaterBody::Ocean) | None => rgba(104, 116, 130, 255),
        Some(WaterBody::Lake { .. }) => rgba(104, 126, 122, 255),
        Some(WaterBody::River { .. }) => rgba(112, 122, 104, 255),
        Some(WaterBody::Pond) => rgba(100, 122, 112, 255),
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
    /// The scatter seed, so the ground tint tears its canopy edges in exactly
    /// the places the trees stand.
    seed: u64,
    span: ChunkSpan,
    sample_m: f64,
    style: SurfaceMeshStyle,
    /// Metres the ground is lowered by.
    ///
    /// Distant tiers sit deliberately low. A wide grid cuts the corners off
    /// ridges, and where its surface came out *above* the detailed one it won
    /// the depth test and pushed blunt triangles through the ground the player
    /// is standing on. Dropping the tier by more than that error keeps the
    /// finer ground on top wherever the two overlap.
    sink_m: f32,
    /// Whether this tier bakes the CPU grid the player stands on.
    contact: bool,
    /// Sub-atlas water, for the tiers close enough to resolve any of it.
    ponds: Option<SharedPonds>,
    /// Seated dwellings. Empty on distance tiers.
    plots: Arc<RwLock<Vec<HousePlot>>>,
}

impl TerrainChunkBuilder {
    /// The tier the player walks on: full detail, real collision.
    pub fn new(surface: Arc<ContinentalSurface>) -> Self {
        Self {
            seed: surface.world_seed() as u32 as u64,
            surface,
            span: chunk_span(),
            sample_m: CHUNK_SAMPLE_M,
            style: SurfaceMeshStyle {
                rock_height: super::surface::ROCK_HEIGHT,
                ..SurfaceMeshStyle::default()
            },
            sink_m: 0.0,
            contact: true,
            ponds: None,
            plots: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Interior ground caps for seated houses. Shared with [`super::WorldStream`].
    pub fn with_plots(mut self, plots: Arc<RwLock<Vec<HousePlot>>>) -> Self {
        self.plots = plots;
        self
    }

    /// Read the sub-atlas pond layer while baking.
    pub fn with_ponds(mut self, ponds: SharedPonds) -> Self {
        self.ponds = Some(ponds);
        self
    }

    /// A distance tier: bigger chunks, coarser samples, no collision.
    pub fn distant(
        surface: Arc<ContinentalSurface>,
        span: ChunkSpan,
        sample_m: f64,
        sink_m: f32,
    ) -> Self {
        Self {
            span,
            sample_m,
            sink_m,
            contact: false,
            ..Self::new(surface)
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
        // Taken once for the whole chunk, never per column: the window may be
        // swapped for a freshly scanned one at any moment, and half a chunk with
        // ponds and half without would show as a seam through the water.
        let ponds = self
            .ponds
            .as_ref()
            .map(|shared| Arc::clone(&shared.read().expect("pond window")));
        let plots = self.plots.read().expect("house plots");
        let mut columns: Vec<SurfaceColumn> = Vec::with_capacity(stride * stride);
        for sz in -1..(stride as i32 - 1) {
            for sx in -1..(stride as i32 - 1) {
                let p = GlobalXZ::at(origin.x + sx as f64 * step, origin.z + sz as f64 * step);
                let mut column = self.surface.column(p);
                if let Some(field) = &ponds {
                    field.carve(p, &mut column);
                }
                if let Some(cap) = footprint::terrain_cap(&plots, p) {
                    column.cap_ground(cap);
                }
                columns.push(column);
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
        let mut uvs = Vec::with_capacity(count);
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
                let p = GlobalXZ::at(
                    s.origin.x + ix as f64 * s.step,
                    s.origin.z + iz as f64 * s.step,
                );
                let look = self.vertex_look(column, normal, sea, p);
                positions.push(Vec3::new(lx, column.ground() - self.sink_m, lz));
                normals.push(normal);
                colors.push(look.0);
                uvs.push(look.1);
            }
        }

        // One fixed diagonal for every quad; `ContactGrid` splits the same way,
        // so the collision surface is exactly the drawn surface — except where
        // a house straddles a quad, which is split so the hillside cannot enter
        // the room without lowering the yard.
        let plots = self.plots.read().expect("house plots");
        for iz in 0..s.verts - 1 {
            for ix in 0..s.verts - 1 {
                let i00 = (iz * s.verts + ix) as u32;
                let i10 = i00 + 1;
                let i01 = i00 + s.verts as u32;
                let i11 = i01 + 1;
                if plots.is_empty() {
                    indices.extend([i00, i01, i11, i00, i11, i10]);
                    continue;
                }
                emit_land_quad(
                    &plots,
                    s,
                    self.sink_m,
                    [ix as i32, iz as i32],
                    [i00, i10, i01, i11],
                    &mut positions,
                    &mut normals,
                    &mut colors,
                    &mut uvs,
                    &mut indices,
                );
            }
        }

        let opaque_index_count = indices.len();
        BuiltMesh {
            positions,
            normals,
            colors,
            uvs,
            indices,
            opaque_index_count,
        }
    }

    /// Colour and soil splat for one land vertex.
    ///
    /// Cover terms matter far past the range anything is scattered at. Props
    /// stop at a few hundred metres; without this a forest that fills the
    /// valley is one green from the far side of it. Cover comes from the same
    /// [`GroundCover`] the props are placed from, so the dark floor and the
    /// trees are never in different places.
    ///
    /// Mesh UVs are splat weights (`dry`, `moor`); lush is the remainder. They
    /// follow climate, water, aspect, and two low-frequency fields — meadow
    /// patches and hillside lean — so the ground mottles as a place, not as
    /// noise. Gentle ground above the rock-height band still shows grass in
    /// the shader, so splat is not gated on the CPU material class.
    fn vertex_look(
        &self,
        column: SurfaceColumn,
        normal: Vec3,
        sea: f32,
        p: GlobalXZ,
    ) -> (Vec4, [f32; 2]) {
        let base = match column.material(sea) {
            SurfaceMaterial::Bed => self.style.bed,
            SurfaceMaterial::Sand => self.style.sand,
            SurfaceMaterial::Grass => self.style.grass,
            SurfaceMaterial::Rock => self.style.rock,
        };
        let mut color: Vec4 = base.into();
        // Alpha is soil, not opacity: the shader draws a bed as mud, and only
        // the columns that actually carry water are one.
        color.w = 1.0;
        if column.is_wet() {
            color.w = 0.0;
            return (color, [0.0, 0.0]);
        }
        let slope = (1.0 - normal.y).clamp(0.0, 1.0);
        let lift = ((column.ground() * 0.015).sin() * 0.04) + slope * 0.06;
        color.x = (color.x * (1.0 - slope * 0.15) + lift * 0.35).clamp(0.0, 1.0);
        color.y = (color.y * (1.0 - slope * 0.05) + lift * 0.15).clamp(0.0, 1.0);
        color.z = (color.z * (1.0 - lift * 0.2)).clamp(0.0, 1.0);

        let fall = Fall::of(normal);
        let cover = GroundCover::sample(
            &self.surface,
            p,
            column.ground(),
            fall,
            canopy_noise(self.seed, p),
        )
        .with_water(column.wetness());
        // Bare rock and sand keep their own vertex colour: a stand thins out
        // to nothing on a scree slope, and tinting one green would invent a
        // forest the props then fail to put there. Tint is light because the
        // splat textures now carry dry sward, peat, and duff.
        let soil = matches!(column.material(sea), SurfaceMaterial::Grass) as u8 as f32;
        color = color.lerp(tint(CANOPY_TINT), soil * cover.tree * 0.40);
        color = color.lerp(tint(RIPARIAN_TINT), soil * cover.bank * 0.28);
        let boggy = cover.bank * cover.moisture * (1.0 - slope / 0.08).clamp(0.0, 1.0);
        color = color.lerp(tint(BOG_TINT), soil * boggy * 0.22);

        // The shader still draws grass on gentle ground above the rock-height
        // band, so splat follows climate, not the CPU material class. Wet
        // columns already returned: the bed texture is the water shader's.
        let patch = soil_patch(self.seed, p);
        let drift = soil_drift(self.seed, p);
        // Climate chooses the place; the 48 m patch only shifts the boundary.
        let dry_signal = (1.0 - cover.moisture)
            * (1.0 - cover.bank * 0.85)
            * (0.32 + 0.68 * cover.aspect)
            * (0.70 + 0.45 * cover.alpine)
            * (0.55 + 0.60 * drift);
        let moor_signal = (cover.bank * 0.55 + cover.moisture * 0.42 + cover.tree * 0.32)
            * (1.0 - cover.alpine * 0.85)
            * (1.0 - (fall.steep / 0.22).clamp(0.0, 1.0) * 0.55)
            * (0.65 + 0.45 * (1.0 - drift));
        let dry = smoothstep(0.22, 0.58, dry_signal * 0.82 + (patch - 0.5) * 0.36);
        let moor =
            smoothstep(0.20, 0.54, moor_signal * 0.88 + (0.5 - patch) * 0.22) * (1.0 - dry * 0.62);
        let splat = [dry.clamp(0.0, 1.0), moor.clamp(0.0, 1.0)];
        (color, splat)
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
            uvs: Vec::new(),
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

fn world_of(s: &ChunkSamples, ix: i32, iz: i32) -> GlobalXZ {
    GlobalXZ::at(
        s.origin.x + ix as f64 * s.step,
        s.origin.z + iz as f64 * s.step,
    )
}

fn lerp_xz(a: GlobalXZ, b: GlobalXZ, t: f32) -> GlobalXZ {
    let t = f64::from(t);
    GlobalXZ::at(a.x + (b.x - a.x) * t, a.z + (b.z - a.z) * t)
}

fn wall_t(plots: &[HousePlot], a: GlobalXZ, b: GlobalXZ) -> f32 {
    let a_in = footprint::terrain_cap(plots, a).is_some();
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    for _ in 0..20 {
        let mid = 0.5 * (lo + hi);
        let p = lerp_xz(a, b, mid);
        if footprint::terrain_cap(plots, p).is_some() == a_in {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

fn push_wall_vert(
    plots: &[HousePlot],
    s: &ChunkSamples,
    sink_m: f32,
    a: GlobalXZ,
    b: GlobalXZ,
    ia: u32,
    ib: u32,
    positions: &mut Vec<Vec3>,
    normals: &mut Vec<Vec3>,
    colors: &mut Vec<Vec4>,
    uvs: &mut Vec<[f32; 2]>,
) -> u32 {
    let t = wall_t(plots, a, b);
    let p = lerp_xz(a, b, t);
    let cap = footprint::terrain_cap(plots, p)
        .or_else(|| footprint::terrain_cap(plots, a))
        .or_else(|| footprint::terrain_cap(plots, b))
        .expect("a split edge belongs to a house wall");
    let (lx, lz) = ((p.x - s.origin.x) as f32, (p.z - s.origin.z) as f32);
    positions.push(Vec3::new(lx, cap - sink_m, lz));
    normals.push(Vec3::Y);
    let ca = colors[ia as usize];
    let cb = colors[ib as usize];
    colors.push(ca.lerp(cb, t));
    let ua = uvs[ia as usize];
    let ub = uvs[ib as usize];
    uvs.push([ua[0] + (ub[0] - ua[0]) * t, ua[1] + (ub[1] - ua[1]) * t]);
    (positions.len() - 1) as u32
}

fn emit_land_quad(
    plots: &[HousePlot],
    s: &ChunkSamples,
    sink_m: f32,
    [ix, iz]: [i32; 2],
    [i00, i10, i01, i11]: [u32; 4],
    positions: &mut Vec<Vec3>,
    normals: &mut Vec<Vec3>,
    colors: &mut Vec<Vec4>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
) {
    let p00 = world_of(s, ix, iz);
    let p10 = world_of(s, ix + 1, iz);
    let p01 = world_of(s, ix, iz + 1);
    let p11 = world_of(s, ix + 1, iz + 1);
    emit_land_tri(
        plots, s, sink_m, i00, i01, i11, p00, p01, p11, positions, normals, colors, uvs, indices, 0,
    );
    emit_land_tri(
        plots, s, sink_m, i00, i11, i10, p00, p11, p10, positions, normals, colors, uvs, indices, 0,
    );
}

fn emit_land_tri(
    plots: &[HousePlot],
    s: &ChunkSamples,
    sink_m: f32,
    ia: u32,
    ib: u32,
    ic: u32,
    pa: GlobalXZ,
    pb: GlobalXZ,
    pc: GlobalXZ,
    positions: &mut Vec<Vec3>,
    normals: &mut Vec<Vec3>,
    colors: &mut Vec<Vec4>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
    depth: u8,
) {
    let ins = [
        footprint::terrain_cap(plots, pa).is_some(),
        footprint::terrain_cap(plots, pb).is_some(),
        footprint::terrain_cap(plots, pc).is_some(),
    ];
    let n_in = ins.iter().filter(|v| **v).count();
    if n_in == 0 || n_in == 3 {
        if n_in == 0 && depth < 2 {
            let mid = lerp_xz(pa, lerp_xz(pb, pc, 0.5), 0.5);
            if footprint::terrain_cap(plots, mid).is_some() {
                let cap = footprint::terrain_cap(plots, mid).expect("just tested");
                let (lx, lz) = ((mid.x - s.origin.x) as f32, (mid.z - s.origin.z) as f32);
                let im = positions.len() as u32;
                positions.push(Vec3::new(lx, cap - sink_m, lz));
                normals.push(Vec3::Y);
                colors.push(colors[ia as usize]);
                uvs.push(uvs[ia as usize]);
                emit_land_tri(
                    plots,
                    s,
                    sink_m,
                    ia,
                    ib,
                    im,
                    pa,
                    pb,
                    mid,
                    positions,
                    normals,
                    colors,
                    uvs,
                    indices,
                    depth + 1,
                );
                emit_land_tri(
                    plots,
                    s,
                    sink_m,
                    ib,
                    ic,
                    im,
                    pb,
                    pc,
                    mid,
                    positions,
                    normals,
                    colors,
                    uvs,
                    indices,
                    depth + 1,
                );
                emit_land_tri(
                    plots,
                    s,
                    sink_m,
                    ic,
                    ia,
                    im,
                    pc,
                    pa,
                    mid,
                    positions,
                    normals,
                    colors,
                    uvs,
                    indices,
                    depth + 1,
                );
                return;
            }
        }
        indices.extend([ia, ib, ic]);
        return;
    }
    if depth >= 3 {
        indices.extend([ia, ib, ic]);
        return;
    }
    let mut split = |a: GlobalXZ, b: GlobalXZ, ia: u32, ib: u32| {
        push_wall_vert(
            plots, s, sink_m, a, b, ia, ib, positions, normals, colors, uvs,
        )
    };
    if n_in == 1 {
        let (i_in, p_in, i0, p0, i1, p1) = if ins[0] {
            (ia, pa, ib, pb, ic, pc)
        } else if ins[1] {
            (ib, pb, ic, pc, ia, pa)
        } else {
            (ic, pc, ia, pa, ib, pb)
        };
        let s0 = split(p_in, p0, i_in, i0);
        let s1 = split(p_in, p1, i_in, i1);
        indices.extend([i_in, s0, s1, s0, i0, i1, s0, i1, s1]);
        return;
    }
    let (i_out, p_out, i0, p0, i1, p1) = if !ins[0] {
        (ia, pa, ib, pb, ic, pc)
    } else if !ins[1] {
        (ib, pb, ic, pc, ia, pa)
    } else {
        (ic, pc, ia, pa, ib, pb)
    };
    let s0 = split(p_out, p0, i_out, i0);
    let s1 = split(p_out, p1, i_out, i1);
    indices.extend([i_out, s0, s1, s0, i0, i1, s0, i1, s1]);
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

        let anchor = samples.origin.with_height(0.0)?;
        let mut payload = ChunkPayload::new(anchor).with_layer(ChunkLayer::Land, land)?;
        if self.contact {
            payload = payload.with_contact(self.build_contact(&samples)?);
        }
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
        cols[0].contour_wetness() >= 0.0,
        cols[1].contour_wetness() >= 0.0,
        cols[2].contour_wetness() >= 0.0,
        cols[3].contour_wetness() >= 0.0,
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
    // The pair is ordered by global lattice index first, so the cell on the
    // other side of an edge evaluates the identical expression and the two
    // shorelines meet bit-for-bit.
    //
    // Atlas water keeps a level sheet at the rim: lakes and rivers already
    // meet the bank by construction. A pond on a hillside does not — the wet
    // sample's sheet can sit metres above the dirt under the crossing, and the
    // contour draws a pane in the air. Drop that edge onto the land.
    let crossing = |a: usize, b: usize| -> WaterVertex {
        let (a, b) = if (CORNERS[b].1, CORNERS[b].0) < (CORNERS[a].1, CORNERS[a].0) {
            (b, a)
        } else {
            (a, b)
        };
        let wa = cols[a].contour_wetness();
        let wb = cols[b].contour_wetness();
        let t = (wa / (wa - wb)).clamp(0.0, 1.0);
        let pa = corner(a).position;
        let pb = corner(b).position;
        let inside = if wet[a] { a } else { b };
        let sheet = cols[inside].sheet_hint();
        let y = match cols[inside].body() {
            Some(WaterBody::Pond) => {
                let land = lerp(cols[a].ground(), cols[b].ground(), t);
                sheet.min(land)
            }
            _ => sheet,
        };
        WaterVertex {
            position: Vec3::new(pa.x + (pb.x - pa.x) * t, y, pa.z + (pb.z - pa.z) * t),
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
        let centre = 0.25
            * (cols[0].contour_wetness()
                + cols[1].contour_wetness()
                + cols[2].contour_wetness()
                + cols[3].contour_wetness());
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
