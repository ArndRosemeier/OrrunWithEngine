//! Terrain streaming for the walkable continent.
//!
//! A thin policy layer over the Engine scheduler: it decides the ring sizes,
//! the rebase distance, and what "ready to walk" means. All the queueing,
//! budgeting, and uploading lives in [`engine::chunk_stream::ChunkStream`].

use std::sync::Arc;

use engine::chunk_stream::ChunkStream;
use engine::error::EngineResult;
use engine::space::{GlobalXZ, RenderOrigin};
use engine::world::World;

use super::chunk_mesh::TerrainChunkBuilder;
use super::coords::CHUNK_SPAN_M;
use super::surface::ContinentalSurface;

/// Chunks that must be resident before the player may move.
pub const ENTRY_RING: i32 = 1;
/// Async visual ring around the player.
pub const VISUAL_RING: i32 = 6;
/// Rebase once render space drifts this far from the origin.
pub const REBASE_DISTANCE_M: f64 = 2_000.0;
/// How far ahead of the walker to prioritise the next bake.
pub const LOOKAHEAD_M: f64 = 120.0;

pub struct WorldStream {
    stream: ChunkStream,
}

impl WorldStream {
    pub fn new(surface: Arc<ContinentalSurface>) -> Self {
        let builder = Arc::new(TerrainChunkBuilder::new(surface));
        let stream = ChunkStream::new(builder, VISUAL_RING)
            .with_required_radius(ENTRY_RING)
            .with_keep_margin(2)
            .with_budgets(4, 2);
        Self { stream }
    }

    pub fn with_visual_ring(mut self, radius: i32) -> Self {
        self.stream.radius = radius.max(ENTRY_RING);
        self
    }

    pub fn resident_count(&self) -> usize {
        self.stream.resident_count()
    }

    pub fn pending_count(&self) -> usize {
        self.stream.pending_count()
    }

    /// Bakes running on worker threads right now.
    pub fn inflight_count(&self) -> usize {
        self.stream.inflight_count()
    }

    /// Finished bakes waiting for an upload slot.
    pub fn ready_count(&self) -> usize {
        self.stream.ready_count()
    }

    pub fn required_ready(&self, focus: GlobalXZ) -> bool {
        self.stream.required_ready(focus)
    }

    /// Height of the drawn ground under `p`, or `None` when it is not resident.
    pub fn contact_height(&self, p: GlobalXZ) -> Option<f32> {
        self.stream.contact_height(p)
    }

    /// Bake the entry ring on this thread. Used while the loading screen is up.
    pub fn prepare_entry(&mut self, world: &mut World, focus: GlobalXZ) -> EngineResult<()> {
        self.stream.ensure_required_blocking(world, focus)
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
        self.stream.sync(world, focus, ahead)
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
        self.stream.reset(world);
    }
}

impl std::fmt::Debug for WorldStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.stream.fmt(f)
    }
}
