//! Location-driven music and water beds.
//!
//! Village theme loops quietly inside or just outside a seated hamlet. Forest
//! pieces start one at a time with long gaps. River and ocean beds fade with
//! distance to the authored shoreline.

use std::path::PathBuf;

use engine::audio::{Audio, ClipId, Play, VoiceId};
use engine::error::EngineError;
use engine::space::GlobalXZ;
use glam::Vec2;
use rand::Rng;
use thiserror::Error;

use super::combat_layer::CombatSfx;
use super::scatter::{canopy_noise, Fall, GroundCover};
use super::session::WorldSession;
use super::settlement::HamletStand;
use super::SessionState;
use crate::atlas::biomes::Biome;

/// Peak village music. Background, not a concert.
const VILLAGE_PEAK: f32 = 0.18;
/// How far past the packed-house disk the theme is still at full level.
const VILLAGE_FULL_PAD_M: f32 = 12.0;
/// Extra metres where the theme fades to silence. "Very near."
const VILLAGE_FADE_M: f32 = 40.0;

const FOREST_PEAK: f32 = 0.20;
const FOREST_FADE_S: f32 = 2.0;
const FOREST_FIRST_WAIT_S: (f32, f32) = (18.0, 48.0);
const FOREST_GAP_S: (f32, f32) = (80.0, 200.0);

const RIVER_PEAK: f32 = 0.32;
const RIVER_FADE_M: f32 = 48.0;
const OCEAN_PEAK: f32 = 0.28;
const OCEAN_FADE_M: f32 = 90.0;

const BED_FADE_S: f32 = 2.4;
const WATER_FADE_S: f32 = 1.4;

const VILLAGE_CLIP: &str = "village_theme.wav";
const FOREST_CLIPS: [&str; 3] = ["forest_01.wav", "forest_02.wav", "forest_03.wav"];
const RIVER_CLIP: &str = "river.wav";
const OCEAN_CLIP: &str = "ocean.wav";
const COMBAT_HIT_CLIP: &str = "combat/hit.wav";
const COMBAT_SWING_CLIP: &str = "combat/swing.wav";

const COMBAT_PEAK: f32 = 0.45;
/// Incoming mob strikes: softer swish, not a full outgoing hit.

/// Prevent overlapping mob strikes from becoming a continuous wall of sound.

#[derive(Debug, Error)]
pub enum AmbienceError {
    #[error(transparent)]
    Engine(#[from] EngineError),

    #[error("ambience clip {name} is missing at {path}")]
    MissingClip { name: &'static str, path: PathBuf },
}

struct Bed {
    clip: ClipId,
    voice: Option<VoiceId>,
    gain: f32,
}

#[derive(Clone, Copy)]
enum ForestBed {
    Silent { wait_s: f32 },
    Playing { voice: VoiceId, gain: f32 },
}

/// Mixer policy for the walked continent.
pub struct Ambience {
    audio: Audio,
    village: Bed,
    river: Bed,
    ocean: Bed,
    forest_clips: [ClipId; 3],
    forest: ForestBed,
    last_forest: Option<usize>,
    combat_hit: ClipId,
    combat_swing: ClipId,
}

impl Ambience {
    pub fn load() -> Result<Self, AmbienceError> {
        let mut audio = Audio::open()?;
        let village = Bed {
            clip: load_clip(&mut audio, VILLAGE_CLIP)?,
            voice: None,
            gain: 0.0,
        };
        let river = Bed {
            clip: load_clip(&mut audio, RIVER_CLIP)?,
            voice: None,
            gain: 0.0,
        };
        let ocean = Bed {
            clip: load_clip(&mut audio, OCEAN_CLIP)?,
            voice: None,
            gain: 0.0,
        };
        let forest_clips = [
            load_clip(&mut audio, FOREST_CLIPS[0])?,
            load_clip(&mut audio, FOREST_CLIPS[1])?,
            load_clip(&mut audio, FOREST_CLIPS[2])?,
        ];
        let combat_hit = load_clip(&mut audio, COMBAT_HIT_CLIP)?;
        let combat_swing = load_clip(&mut audio, COMBAT_SWING_CLIP)?;
        Ok(Self {
            audio,
            village,
            river,
            ocean,
            forest_clips,
            forest: ForestBed::Silent {
                wait_s: roll_range(FOREST_FIRST_WAIT_S),
            },
            last_forest: None,
            combat_hit,
            combat_swing,
        })
    }

    /// Fade every bed toward silence (title, atlas, travel).
    pub fn silence(&mut self, dt: f32) -> Result<(), AmbienceError> {
        tick_loop(&mut self.audio, &mut self.village, 0.0, dt, BED_FADE_S)?;
        tick_loop(&mut self.audio, &mut self.river, 0.0, dt, WATER_FADE_S)?;
        tick_loop(&mut self.audio, &mut self.ocean, 0.0, dt, WATER_FADE_S)?;
        self.tick_forest(false, dt)?;
        Ok(())
    }

    pub fn play_swing(&mut self) -> Result<(), AmbienceError> {
        self.audio.play(
            self.combat_swing,
            Play {
                looped: false,
                volume: COMBAT_PEAK,
            },
        )?;
        Ok(())
    }

    pub fn play_hit(&mut self) -> Result<(), AmbienceError> {
        self.audio.play(
            self.combat_hit,
            Play {
                looped: false,
                volume: COMBAT_PEAK,
            },
        )?;
        Ok(())
    }

    pub fn play_hurt(&mut self) -> Result<(), AmbienceError> {
        // The vendored hurt.wav is a sustained synthetic tone, not a melee
        // transient. Keep it silent until a verified replacement is added.
        Ok(())
    }

    fn play_pending_combat(&mut self, session: &mut WorldSession) -> Result<(), AmbienceError> {
        for sfx in session.take_combat_sfx() {
            match sfx {
                CombatSfx::Swing => self.play_swing()?,
                CombatSfx::Hit => self.play_hit()?,
                CombatSfx::Hurt => self.play_hurt()?,
            }
        }
        Ok(())
    }

    pub fn update(&mut self, session: &mut WorldSession, dt: f32) -> Result<(), AmbienceError> {
        self.play_pending_combat(session)?;
        if session.state() != SessionState::World {
            return self.silence(dt);
        }
        let Some(pos) = session.player_position() else {
            return self.silence(dt);
        };
        let at = GlobalXZ::at(pos.x, pos.z);
        let village = village_presence(session.hamlets(), at);
        let (river, ocean) = water_presence(session, at);
        let in_woods = village < 0.05 && forest_here(session, at);

        tick_loop(
            &mut self.audio,
            &mut self.village,
            village * VILLAGE_PEAK,
            dt,
            BED_FADE_S,
        )?;
        tick_loop(
            &mut self.audio,
            &mut self.river,
            river * RIVER_PEAK,
            dt,
            WATER_FADE_S,
        )?;
        tick_loop(
            &mut self.audio,
            &mut self.ocean,
            ocean * OCEAN_PEAK,
            dt,
            WATER_FADE_S,
        )?;
        self.tick_forest(in_woods, dt)?;
        Ok(())
    }

    fn tick_forest(&mut self, in_woods: bool, dt: f32) -> Result<(), AmbienceError> {
        match self.forest {
            ForestBed::Silent { wait_s } => {
                if !in_woods {
                    self.forest = ForestBed::Silent {
                        wait_s: wait_s.max(FOREST_FIRST_WAIT_S.0),
                    };
                    return Ok(());
                }
                let wait_s = wait_s - dt;
                if wait_s > 0.0 {
                    self.forest = ForestBed::Silent { wait_s };
                    return Ok(());
                }
                let index = self.next_forest_index();
                let voice = self.audio.play(
                    self.forest_clips[index],
                    Play {
                        looped: false,
                        volume: FOREST_PEAK,
                    },
                )?;
                self.last_forest = Some(index);
                self.forest = ForestBed::Playing {
                    voice,
                    gain: FOREST_PEAK,
                };
            }
            ForestBed::Playing { voice, gain } => {
                if !in_woods {
                    let gain = approach(gain, 0.0, dt, FOREST_FADE_S);
                    if gain <= 0.0 {
                        self.audio.stop(voice)?;
                        self.forest = ForestBed::Silent {
                            wait_s: roll_range(FOREST_FIRST_WAIT_S),
                        };
                    } else {
                        self.audio.set_volume(voice, gain)?;
                        self.forest = ForestBed::Playing { voice, gain };
                    }
                    return Ok(());
                }
                if !self.audio.is_playing(voice)? {
                    self.audio.stop(voice)?;
                    self.forest = ForestBed::Silent {
                        wait_s: roll_range(FOREST_GAP_S),
                    };
                    return Ok(());
                }
                if (gain - FOREST_PEAK).abs() > f32::EPSILON {
                    self.audio.set_volume(voice, FOREST_PEAK)?;
                    self.forest = ForestBed::Playing {
                        voice,
                        gain: FOREST_PEAK,
                    };
                }
            }
        }
        Ok(())
    }

    fn next_forest_index(&self) -> usize {
        let mut rng = rand::thread_rng();
        if self.forest_clips.len() == 1 {
            return 0;
        }
        loop {
            let index = rng.gen_range(0..self.forest_clips.len());
            if self.last_forest != Some(index) {
                return index;
            }
        }
    }
}

fn tick_loop(
    audio: &mut Audio,
    bed: &mut Bed,
    target: f32,
    dt: f32,
    fade_s: f32,
) -> Result<(), AmbienceError> {
    bed.gain = approach(bed.gain, target, dt, fade_s);
    if bed.gain <= 0.0 && target <= 0.0 {
        if let Some(voice) = bed.voice.take() {
            audio.stop(voice)?;
        }
        return Ok(());
    }
    match bed.voice {
        Some(voice) => audio.set_volume(voice, bed.gain)?,
        None => {
            bed.voice = Some(audio.play(
                bed.clip,
                Play {
                    looped: true,
                    volume: bed.gain,
                },
            )?);
        }
    }
    Ok(())
}

fn load_clip(audio: &mut Audio, name: &'static str) -> Result<ClipId, AmbienceError> {
    let path = audio_clip_path(name);
    if !path.is_file() {
        return Err(AmbienceError::MissingClip { name, path });
    }
    Ok(audio.load_wav(&path)?)
}

fn audio_clip_path(name: &str) -> PathBuf {
    for dir in audio_dirs() {
        let path = dir.join(name);
        if path.is_file() {
            return path;
        }
    }
    audio_dirs()
        .last()
        .expect("at least the crate assets dir")
        .join(name)
}

fn audio_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(root) = std::env::var_os("ORRUN_ASSETS") {
        dirs.push(PathBuf::from(root).join("audio"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("assets").join("audio"));
        }
    }
    dirs.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("audio"),
    );
    dirs
}

fn forest_here(session: &WorldSession, at: GlobalXZ) -> bool {
    let surface = session.surface();
    if surface.fields().biome_at(at.x as f32, at.z as f32) != Biome::Forest {
        return false;
    }
    let column = surface.column(at);
    if column.is_wet() {
        return false;
    }
    let seed = surface.world_seed() as u32 as u64;
    let cover = GroundCover::sample(
        seed,
        surface,
        at,
        column.ground(),
        Fall::default(),
        canopy_noise(seed, at),
    );
    // Deep glades are still forest country; only the thinnest edge falls silent.
    cover.tree > 0.12 || cover.clearing < 0.92
}

fn water_presence(session: &WorldSession, at: GlobalXZ) -> (f32, f32) {
    let surface = session.surface();
    let p = Vec2::new(at.x as f32, at.z as f32);
    let ocean = ocean_presence(surface.coast_signed(at));
    let river = match surface.hydro_index().nearest_river(surface.hydro(), p) {
        Some(hit) => river_presence(hit.dist, hit.half_width),
        None => 0.0,
    };
    (river, ocean)
}

/// 1 inside the houses (plus a short pad), then a linear fade to silence.
pub(crate) fn village_presence(hamlets: &[HamletStand], at: GlobalXZ) -> f32 {
    let mut best = 0.0_f32;
    for hamlet in hamlets {
        let dx = (at.x - hamlet.at.x) as f32;
        let dz = (at.z - hamlet.at.z) as f32;
        let distance = (dx * dx + dz * dz).sqrt();
        let full_r = hamlet.radius + VILLAGE_FULL_PAD_M;
        best = best.max(fade_by_distance(distance, full_r, full_r + VILLAGE_FADE_M));
    }
    best
}

pub(crate) fn fade_by_distance(distance: f32, full_m: f32, silent_m: f32) -> f32 {
    if !(distance.is_finite() && full_m.is_finite() && silent_m.is_finite()) {
        panic!("ambience distance fade got a non-finite value");
    }
    if silent_m <= full_m {
        panic!("ambience fade band must be wider than the full-level radius");
    }
    if distance <= full_m {
        1.0
    } else if distance >= silent_m {
        0.0
    } else {
        1.0 - (distance - full_m) / (silent_m - full_m)
    }
}

/// `coast_signed` is positive inland. Ocean is full on the water and fades inland.
pub(crate) fn ocean_presence(coast_signed: f32) -> f32 {
    if !coast_signed.is_finite() {
        panic!("ocean presence got a non-finite coast distance");
    }
    if coast_signed <= 0.0 {
        1.0
    } else {
        fade_by_distance(coast_signed, 0.0, OCEAN_FADE_M)
    }
}

/// Full in the channel, fading across [`RIVER_FADE_M`] past the bank.
pub(crate) fn river_presence(dist_to_centre: f32, half_width: f32) -> f32 {
    if !(dist_to_centre.is_finite() && half_width.is_finite()) {
        panic!("river presence got a non-finite channel distance");
    }
    let bank = (dist_to_centre - half_width.max(0.0)).max(0.0);
    fade_by_distance(bank, 0.0, RIVER_FADE_M)
}

fn approach(current: f32, target: f32, dt: f32, fade_s: f32) -> f32 {
    if fade_s <= 0.0 {
        panic!("ambience fade time must be positive, got {fade_s}");
    }
    let step = dt / fade_s;
    let delta = target - current;
    if delta.abs() <= step {
        target
    } else {
        current + delta.signum() * step
    }
}

fn roll_range(range: (f32, f32)) -> f32 {
    rand::thread_rng().gen_range(range.0..range.1)
}

#[cfg(test)]
mod tests {
    use super::{
        fade_by_distance, ocean_presence, river_presence, village_presence, VILLAGE_FADE_M,
        VILLAGE_FULL_PAD_M,
    };
    use crate::world::settlement::HamletStand;
    use engine::space::GlobalXZ;
    use std::path::Path;

    #[test]
    fn fade_is_full_then_linear_then_silent() {
        assert_eq!(fade_by_distance(0.0, 10.0, 20.0), 1.0);
        assert_eq!(fade_by_distance(10.0, 10.0, 20.0), 1.0);
        assert!((fade_by_distance(15.0, 10.0, 20.0) - 0.5).abs() < 1e-5);
        assert_eq!(fade_by_distance(20.0, 10.0, 20.0), 0.0);
        assert_eq!(fade_by_distance(40.0, 10.0, 20.0), 0.0);
    }

    #[test]
    fn village_covers_the_houses_and_a_short_approach() {
        let hamlet = HamletStand {
            at: GlobalXZ::at(0.0, 0.0),
            radius: 20.0,
            houses: Vec::new(),
            cut: Vec::new(),
        };
        let hamlets = [hamlet];
        let inside = GlobalXZ::at(5.0, 0.0);
        let edge = GlobalXZ::at(f64::from(20.0 + VILLAGE_FULL_PAD_M), 0.0);
        let mid = GlobalXZ::at(
            f64::from(20.0 + VILLAGE_FULL_PAD_M + VILLAGE_FADE_M * 0.5),
            0.0,
        );
        let far = GlobalXZ::at(400.0, 0.0);
        assert_eq!(village_presence(&hamlets, inside), 1.0);
        assert_eq!(village_presence(&hamlets, edge), 1.0);
        assert!((village_presence(&hamlets, mid) - 0.5).abs() < 1e-5);
        assert_eq!(village_presence(&hamlets, far), 0.0);
    }

    #[test]
    fn ocean_is_full_on_the_water_and_silent_inland() {
        assert_eq!(ocean_presence(-20.0), 1.0);
        assert_eq!(ocean_presence(0.0), 1.0);
        assert_eq!(ocean_presence(200.0), 0.0);
        assert!(ocean_presence(45.0) > 0.4);
    }

    #[test]
    fn river_is_full_in_the_channel() {
        assert_eq!(river_presence(4.0, 8.0), 1.0);
        assert_eq!(river_presence(8.0, 8.0), 1.0);
        assert_eq!(river_presence(200.0, 8.0), 0.0);
        assert!(river_presence(20.0, 8.0) > 0.5);
    }

    #[test]
    fn shipped_clips_are_on_disk() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/audio");
        for name in [
            "village_theme.wav",
            "forest_01.wav",
            "forest_02.wav",
            "forest_03.wav",
            "river.wav",
            "ocean.wav",
            "combat/hit.wav",
            "combat/swing.wav",
            "combat/hurt.wav",
        ] {
            let path = dir.join(name);
            assert!(
                path.is_file(),
                "expected shipped ambience clip at {}",
                path.display()
            );
        }
    }
}
