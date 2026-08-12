//! Terrain streaming for the walkable continent.
//!
//! A thin policy layer over the Engine scheduler: it decides the ring sizes,
//! the rebase distance, and what "ready to walk" means. All the queueing,
//! budgeting, and uploading lives in [`engine::chunk_stream::ChunkStream`].
//!
//! # The visibility ladder
//!
//! Ground is streamed at three resolutions at once, and all three read the one
//! [`ContinentalSurface::column`](super::ContinentalSurface::column): all that
//! separates them is how far apart they sample it, so no tier can invent a
//! landform another one does not have. The walked tier samples every four
//! metres and reaches barely a kilometre; a medium tier every twenty-five out
//! to six; a far tier every hundred and twenty-five out to thirty, which buys a
//! five-kilometre square for two thousand columns and is the reason a horizon
//! is affordable at all.
//!
//! Tiers **overlap** rather than meet. Each coarse tier leaves out only the one
//! chunk the player stands in — the largest hole the finer tier is guaranteed
//! to have covered — and its ground continues underneath the finer tier from
//! there. Overlapping instead of abutting means there is never a crack to fall
//! through or a gap to see sky through, and it costs only the depth test that
//! [`super::chunk_mesh::TerrainChunkBuilder`]'s sink makes sure the finer tier
//! wins.

use std::sync::Arc;

use engine::chunk_stream::ChunkStream;
use engine::error::EngineResult;
use engine::space::{ChunkLevel, ChunkSpan, GlobalXZ, RenderOrigin};
use engine::world::World;

use super::chunk_mesh::TerrainChunkBuilder;
use super::coords::{CHUNK_SAMPLE_M, CHUNK_SPAN_M};
use super::surface::ContinentalSurface;

/// Chunks that must be resident before the player may move.
pub const ENTRY_RING: i32 = 1;
/// Async visual ring around the player, in chunks of the walked tier.
pub const VISUAL_RING: i32 = 6;
/// Rebase once render space drifts this far from the origin.
pub const REBASE_DISTANCE_M: f64 = 2_000.0;
/// How far ahead of the walker to prioritise the next bake.
pub const LOOKAHEAD_M: f64 = 120.0;

/// One rung of the visibility ladder.
#[derive(Clone, Copy, Debug)]
pub struct TerrainTier {
    /// Which tier this is; part of a chunk's identity, so no two may share one.
    pub level: u8,
    pub span_m: f64,
    pub sample_m: f64,
    /// Load radius in chunks of this tier.
    pub radius: i32,
    /// How far the ground is lowered, so a finer tier always wins the overlap.
    pub sink_m: f32,
    pub max_bakes_per_frame: usize,
    pub max_uploads_per_frame: usize,
}

impl TerrainTier {
    /// Ground this tier is guaranteed to cover in every direction.
    ///
    /// The load ring is centred on the player's *chunk*, not on the player, so
    /// standing at the edge of it is the worst case and this is what is left.
    pub const fn covers_m(&self) -> f64 {
        self.radius as f64 * self.span_m
    }

    /// The furthest this tier's ground can reach.
    pub const fn reach_m(&self) -> f64 {
        (self.radius + 1) as f64 * self.span_m
    }

    /// The tiers are compile-time constants, so a bad span is a typo in this
    /// file and not something a running game can recover from.
    fn span(&self) -> ChunkSpan {
        ChunkSpan::new(self.span_m).expect("tier span")
    }
}

/// The tier the player walks on: full surface, real collision, four-metre grid.
pub const NEAR: TerrainTier = TerrainTier {
    level: 0,
    span_m: CHUNK_SPAN_M,
    sample_m: CHUNK_SAMPLE_M,
    radius: VISUAL_RING,
    sink_m: 0.0,
    max_bakes_per_frame: 4,
    max_uploads_per_frame: 2,
};

/// Middle distance: the same surface, sampled widely enough to cover kilometres.
pub const MEDIUM: TerrainTier = TerrainTier {
    level: 1,
    span_m: 1_000.0,
    sample_m: 25.0,
    radius: 6,
    // A 25 m grid shaves a few metres off a ridge; more than that, and the
    // detailed ground it overlaps would poke through it instead.
    sink_m: 6.0,
    max_bakes_per_frame: 3,
    max_uploads_per_frame: 2,
};

/// The horizon: thirty kilometres of the same landform on a 125 m grid.
///
/// Generous budgets. A chunk here is a whole 5 km square for the price of two
/// thousand columns, and an empty horizon on the first frame in the world is
/// the one thing this ladder exists to prevent.
pub const FAR: TerrainTier = TerrainTier {
    level: 2,
    span_m: 5_000.0,
    sample_m: 125.0,
    radius: 6,
    // A 125 m grid cuts the top off a ridge and bridges a gorge; this covers
    // ninety-nine hundredths of the continent, and the cliffs it cannot cover
    // no sink could without leaving the tier hanging under the world.
    sink_m: 25.0,
    max_bakes_per_frame: 8,
    max_uploads_per_frame: 6,
};

/// Coarse tiers, finest first.
pub const DISTANT: [TerrainTier; 2] = [MEDIUM, FAR];

// A tier's hole is the single chunk the player stands in, which reaches one
// full span away from them. If the finer tier does not certainly cover that
// much, the two leave a ring of missing ground around the player.
const _: () = assert!(MEDIUM.span_m <= NEAR.covers_m());
const _: () = assert!(FAR.span_m <= MEDIUM.covers_m());

/// How far the eye can be shown ground, in metres.
///
/// The far plane goes here rather than at the guaranteed coverage: clipping
/// ground that has already been built and uploaded is the exact mistake that
/// made the world stop at 800 m while the streamer reached 1200.
pub const FAR_VIEW_M: f32 = FAR.reach_m() as f32;

pub struct WorldStream {
    /// The walked tier: the only one with collision under it.
    near: ChunkStream,
    /// Coarse tiers, finest first.
    distant: Vec<ChunkStream>,
}

impl WorldStream {
    pub fn new(surface: Arc<ContinentalSurface>) -> Self {
        let builder = Arc::new(TerrainChunkBuilder::new(Arc::clone(&surface)));
        let near = ChunkStream::new(builder, NEAR.radius)
            .with_required_radius(ENTRY_RING)
            .with_keep_margin(2)
            .with_budgets(NEAR.max_bakes_per_frame, NEAR.max_uploads_per_frame);

        let mut distant = Vec::with_capacity(DISTANT.len());
        for tier in DISTANT {
            let builder = Arc::new(TerrainChunkBuilder::distant(
                Arc::clone(&surface),
                tier.span(),
                tier.sample_m,
                tier.sink_m,
            ));
            distant.push(
                ChunkStream::new(builder, tier.radius)
                    .with_level(ChunkLevel::new(tier.level))
                    // The ring immediately around the hole is part of entering
                    // the world: without it the player arrives inside a bubble.
                    .with_required_radius(1)
                    .with_hole_radius(0)
                    .with_keep_margin(1)
                    .with_budgets(tier.max_bakes_per_frame, tier.max_uploads_per_frame),
            );
        }
        Self { near, distant }
    }

    pub fn with_visual_ring(mut self, radius: i32) -> Self {
        self.near.radius = radius.max(ENTRY_RING);
        self
    }

    /// Resident chunks of the walked tier.
    pub fn resident_count(&self) -> usize {
        self.near.resident_count()
    }

    /// Resident chunks of the coarse tiers, nearest first.
    pub fn distant_resident_counts(&self) -> Vec<usize> {
        self.distant.iter().map(|s| s.resident_count()).collect()
    }

    pub fn pending_count(&self) -> usize {
        self.near.pending_count()
            + self
                .distant
                .iter()
                .map(|s| s.pending_count())
                .sum::<usize>()
    }

    /// Bakes running on worker threads right now.
    pub fn inflight_count(&self) -> usize {
        self.near.inflight_count()
            + self
                .distant
                .iter()
                .map(|s| s.inflight_count())
                .sum::<usize>()
    }

    /// Finished bakes waiting for an upload slot.
    pub fn ready_count(&self) -> usize {
        self.near.ready_count() + self.distant.iter().map(|s| s.ready_count()).sum::<usize>()
    }

    pub fn required_ready(&self, focus: GlobalXZ) -> bool {
        self.near.required_ready(focus) && self.distant.iter().all(|s| s.required_ready(focus))
    }

    /// Height of the drawn ground under `p`, or `None` when it is not resident.
    ///
    /// Only the walked tier bakes a contact grid: the coarse ones are lowered
    /// on purpose and standing on one would drop the player through the world.
    pub fn contact_height(&self, p: GlobalXZ) -> Option<f32> {
        self.near.contact_height(p)
    }

    /// Bake the entry ring on this thread. Used while the loading screen is up.
    pub fn prepare_entry(&mut self, world: &mut World, focus: GlobalXZ) -> EngineResult<()> {
        self.near.ensure_required_blocking(world, focus)?;
        for stream in &mut self.distant {
            stream.ensure_required_blocking(world, focus)?;
        }
        Ok(())
    }

    /// Advance streaming around the walker, favouring where they are heading.
    pub fn sync(
        &mut self,
        world: &mut World,
        focus: GlobalXZ,
        heading: Option<glam::Vec2>,
    ) -> EngineResult<()> {
        let ahead = match heading {
            Some(dir) if dir.length_squared() > 1e-6 => {
                let d = dir.normalize();
                Some(GlobalXZ::at(
                    focus.x + d.x as f64 * LOOKAHEAD_M,
                    focus.z + d.y as f64 * LOOKAHEAD_M,
                ))
            }
            _ => None,
        };
        self.near.sync(world, focus, ahead)?;
        // Ground under the player first, horizon after: a coarse tier holding
        // up the tier being walked on would stutter the walk.
        for stream in &mut self.distant {
            stream.sync(world, focus, None)?;
        }
        Ok(())
    }

    /// Re-base render space when the walker has drifted too far from the origin.
    ///
    /// Returns whether the origin moved. Snapping to the chunk grid keeps chunk
    /// vertices on exactly representable offsets across repeated rebases.
    pub fn maybe_rebase(&mut self, world: &mut World, focus: GlobalXZ) -> EngineResult<bool> {
        if world.render_offset_m(focus) < REBASE_DISTANCE_M {
            return Ok(false);
        }
        let origin = RenderOrigin::snapped(focus, CHUNK_SPAN_M)?;
        world.set_render_origin(origin)?;
        Ok(true)
    }

    /// Drop everything and invalidate in-flight bakes (leaving this world).
    pub fn reset(&mut self, world: &mut World) {
        self.near.reset(world);
        for stream in &mut self.distant {
            stream.reset(world);
        }
    }
}

impl std::fmt::Debug for WorldStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldStream")
            .field("near", &self.near)
            .field("distant", &self.distant)
            .finish()
    }
}
