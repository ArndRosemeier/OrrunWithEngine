//! v0 playtest harness: standing, dungeon_fill, bind, combat, controls.
//!
//! Driven through [`WorldSession::step`] with explicit [`WalkInput`]. Never
//! captures the pointer, never treats E as interact, never steals Escape.
//! Look is applied as WalkInput yaw/pitch *deltas* (same sign as +mouse_dx),
//! not as an absolute setLook.
//!
//! Usage:
//! `cargo run -p orrun --release --bin playtester -- --seed 1 --size 64 --hooks standing,dungeon_fill,bind,combat,controls`

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use engine::prelude::*;
use engine::space::GlobalXZ;
use glam::{Vec2, Vec3};
use orrun::atlas::ContinentAtlas;
use orrun::combat::CombatVerb;
use orrun::settings::Settings;
use orrun::world::{
    install_daylight, install_materials, resolve_spawn, DungeonPin, Heading, Locomotion,
    MapPoint, SessionState, WalkInput, WorldEntryRequest, WorldSession, LIVE_OPEN_M,
};
use serde_json::{json, Value};

const WALK_SPEED: f32 = 10.0;
const FLY_SPEED: f32 = 40.0;
const YAW_EPS: f32 = 1e-3;
const STAND_TIMEOUT_S: f32 = 90.0;
const DUNGEON_TIMEOUT_S: f32 = 180.0;
const EYE_HEIGHT_M: f32 = 1.7;
/// Standing camera: land + sky horizon. Not look-down floor (-70).
const HORIZON_PITCH: f32 = -15.0;
/// Indoor camera: eye-height along the hall. Not a floor texture.
const CORRIDOR_PITCH: f32 = 0.0;
const PITCH_EPS: f32 = 2.5;
/// Same sign as `+mouse_dx`: decreases `yaw_degrees` and looks screen-right.
const LOOK_DELTA_YAW: f32 = -24.0;
const HATCH_IN_VIEW_DEG: f32 = 30.0;
const MIN_FREEBOARD_M: f32 = 0.75;

fn main() {
    let args = parse_args();
    let shots = screenshot_dir();
    fs::create_dir_all(&shots).expect("shots directory");
    let appdata = shots.join("appdata");
    fs::create_dir_all(&appdata).expect("isolated APPDATA");
    std::env::set_var("APPDATA", &appdata);
    std::env::set_var("ORRUN_SAVE_DIR", &appdata);
    for name in [
        "standing.png",
        "dungeon_mouth.png",
        "dungeon_interior.png",
        "bind_overworld.png",
        "bind_dungeon.png",
        "report.json",
        "bind.json",
        "standing.json",
        "dungeon_fill.json",
        "combat.json",
        "controls.json",
    ] {
        let _ = fs::remove_file(shots.join(name));
    }

    eprintln!(
        "playtester seed={} size={} hooks={} shots={}",
        args.seed,
        args.size,
        args.hooks.join(","),
        shots.display()
    );
    eprintln!("generating atlas seed={} size={}", args.seed, args.size);
    let atlas = Arc::new(ContinentAtlas::generate(args.seed, args.size));
    let surface =
        Arc::new(orrun::world::ContinentalSurface::new(&atlas).expect("canonical surface"));
    let sea = surface.sea_surface_z();
    let session = WorldSession::new(Arc::clone(&surface)).with_instant_travel();
    let (request, entry_via) = standing_request(&atlas, &session);

    let mut driver = Driver {
        session,
        seed: args.seed,
        shots: shots.clone(),
        hooks: args.hooks,
        hook_i: 0,
        phase: Phase::Boot,
        reports: Vec::new(),
        fails: Vec::new(),
        skips: Vec::new(),
        pin: None,
        yaw_before: None,
        landing_yaw: None,
        yaw_after_entry: None,
        yaw_pre_exit: None,
        look_yaw_before: None,
        look_yaw_after: None,
        look_delta_applied: None,
        yaw_decreased: false,
        pending_yaw_delta: 0.0,
        pending_pitch_delta: 0.0,
        after_force_walk: None,
        fly_after_exit: None,
        fly_before_recross: None,
        entry_via,
        hatch_in_view: false,
        hatch_angle_degrees: None,
        phase_t0: 0.0,
        awaiting_shot: None,
        finished: false,
        request,
        sea,
        combat_tab_sent: false,
        controls_stage: ControlsStage::Tab,
    };

    driver.write_running_report();
    Engine::run("Orrun - playtester", move |world, frame| {
        driver.tick(world, frame);
    });
    eprintln!("playtester engine returned");

    let report_path = shots.join("report.json");
    let code = match fs::read_to_string(&report_path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => {
                let fails = v
                    .get("fails")
                    .and_then(|x| x.as_array())
                    .map(|a| a.len())
                    .unwrap_or(1);
                if fails == 0 {
                    0
                } else {
                    1
                }
            }
            Err(_) => 1,
        },
        Err(_) => 1,
    };
    std::process::exit(code);
}

struct Args {
    seed: i32,
    size: usize,
    hooks: Vec<String>,
}

fn parse_args() -> Args {
    let mut seed = 1i32;
    let mut size = 64usize;
    let mut hooks = Vec::new();
    let mut raw = std::env::args().skip(1);
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--seed" => {
                seed = raw
                    .next()
                    .expect("--seed needs a value")
                    .parse()
                    .expect("seed");
            }
            "--size" => {
                size = raw
                    .next()
                    .expect("--size needs a value")
                    .parse()
                    .expect("size");
            }
            "--hooks" => {
                hooks = raw
                    .next()
                    .expect("--hooks needs a value")
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            other => panic!("unknown argument {other}"),
        }
    }
    if hooks.is_empty() {
        hooks = vec!["standing".into(), "dungeon_fill".into(), "bind".into()];
    }
    Args { seed, size, hooks }
}

fn screenshot_dir() -> PathBuf {
    if let Some(raw) = std::env::var_os("ENGINE_SCREENSHOT") {
        PathBuf::from(raw)
    } else {
        PathBuf::from(r"C:\Projekte\OrrunWithEngine\shots\playtester")
    }
}

fn standing_request(_atlas: &ContinentAtlas, session: &WorldSession) -> (WorldEntryRequest, String) {
    if let Some(request) = dry_settlement_entry(session) {
        return (request, "best_settlement_entry".into());
    }
    if let Some(request) = dry_land_entry(session) {
        return (request, "dry_land".into());
    }
    panic!("no dry settlement or dry-land stand on this atlas");
}

/// Highest-tier settlement whose resolved stand is dry underfoot *and* in the
/// neighbourhood, facing inland so the PNG is land + sky rather than a river.
fn dry_settlement_entry(session: &WorldSession) -> Option<WorldEntryRequest> {
    let surface = session.surface();
    let ponds = session.ponds();
    let bounds = surface.bounds();
    let mut pins: Vec<_> = surface.settlements().iter().copied().collect();
    pins.sort_by(|a, b| (b.tier, b.population, b.id).cmp(&(a.tier, a.population, a.id)));
    for pin in pins {
        let Ok(point) = MapPoint::from_global(bounds, pin.at) else {
            continue;
        };
        let request = WorldEntryRequest::at(point);
        let Ok(spawn) = resolve_spawn(surface, ponds.as_ref(), request) else {
            continue;
        };
        let stand = spawn.ground();
        if pose_too_wet(session, stand) {
            continue;
        }
        return Some(request.facing(inland_heading(session, stand)));
    }
    None
}

fn dry_land_entry(session: &WorldSession) -> Option<WorldEntryRequest> {
    let bounds = session.surface().bounds();
    let metres = bounds.metres();
    let step = 2000.0;
    let mut x = 750.0;
    while x < metres {
        let mut z = 750.0;
        while z < metres {
            let at = GlobalXZ::at(x, z);
            if let Ok(request) = WorldEntryRequest::at_global(bounds, at) {
                if let Ok(spawn) = resolve_spawn(session.surface(), session.ponds().as_ref(), request)
                {
                    let stand = spawn.ground();
                    if !pose_too_wet(session, stand) {
                        return Some(request.facing(inland_heading(session, stand)));
                    }
                }
            }
            z += step;
        }
        x += step;
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Boot,
    WaitWorld,
    Standing,
    StandingLook,
    DungeonTravel,
    DungeonWaitReady,
    DungeonApproach,
    DungeonWaitLive,
    DungeonLookHatch,
    DungeonArmAway,
    DungeonArmBack,
    DungeonEnter,
    DungeonInteriorLook,
    BindArmAway,
    BindArmBack,
    BindLookDelta,
    BindOverworldShot,
    BindEnter,
    BindLookAfterEntry,
    BindExitPrep,
    BindExit,
    BindForceWalk,
    BindWrite,
    CombatLive,
    ControlsLive,
    NextHook,
    Done,
}

struct Driver {
    session: WorldSession,
    seed: i32,
    shots: PathBuf,
    hooks: Vec<String>,
    hook_i: usize,
    phase: Phase,
    reports: Vec<Value>,
    fails: Vec<String>,
    skips: Vec<String>,
    pin: Option<DungeonPin>,
    yaw_before: Option<f32>,
    landing_yaw: Option<f32>,
    yaw_after_entry: Option<f32>,
    yaw_pre_exit: Option<f32>,
    look_yaw_before: Option<f32>,
    look_yaw_after: Option<f32>,
    look_delta_applied: Option<f32>,
    yaw_decreased: bool,
    pending_yaw_delta: f32,
    pending_pitch_delta: f32,
    after_force_walk: Option<Phase>,
    fly_after_exit: Option<bool>,
    fly_before_recross: Option<bool>,
    entry_via: String,
    hatch_in_view: bool,
    hatch_angle_degrees: Option<f32>,
    phase_t0: f32,
    awaiting_shot: Option<String>,
    finished: bool,
    request: WorldEntryRequest,
    sea: f32,
    combat_tab_sent: bool,
    controls_stage: ControlsStage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlsStage {
    Tab,
    Strike,
    WaitHit,
    Ember,
    Potion,
}

impl Driver {
    fn tick(&mut self, world: &mut World, frame: &Frame) {
        if self.finished {
            return;
        }
        world.set_pointer_lock(false);

        if frame.first {
            install_daylight(world);
            install_materials(world, self.seed, self.sea);
            self.session
                .begin_entry(world, self.request)
                .expect("entry point must resolve to walkable ground");
            self.phase = Phase::WaitWorld;
            self.phase_t0 = frame.time;
        }

        let input = self.input_for_phase(frame);
        if let Err(err) = self.session.step(world, input) {
            self.fail_current(&format!("session step failed: {err}"));
            self.finish(world);
            return;
        }
        self.pending_yaw_delta = 0.0;
        self.pending_pitch_delta = 0.0;

        if let Some(name) = self.awaiting_shot.clone() {
            let path = self.shots.join(format!("{name}.png"));
            if path.is_file() {
                self.awaiting_shot = None;
            } else if frame.time - self.phase_t0 > 8.0 {
                self.fail_current(&format!("screenshot {name}.png was never written"));
                self.awaiting_shot = None;
                self.advance_after_fail(world, frame);
            }
            return;
        }

        self.advance(world, frame);
    }

    fn current_hook(&self) -> Option<&str> {
        self.hooks.get(self.hook_i).map(String::as_str)
    }

    fn input_for_phase(&self, frame: &Frame) -> WalkInput {
        let dt = frame.dt.max(0.0);
        let mut input = WalkInput {
            dt,
            yaw_delta_degrees: self.pending_yaw_delta,
            pitch_delta_degrees: self.pending_pitch_delta,
            ..WalkInput::IDLE
        };
        match self.phase {
            Phase::WaitWorld | Phase::DungeonTravel => {
                input.skip_travel = true;
            }
            Phase::DungeonApproach
            | Phase::DungeonArmBack
            | Phase::BindArmBack
            | Phase::DungeonEnter
            | Phase::BindEnter => {
                if let (Some(pin), Some(pos)) = (self.pin, self.session.player_position()) {
                    input = walk_toward(pos.horizontal(), pin.at, dt);
                    input.yaw_delta_degrees = self.pending_yaw_delta;
                    input.pitch_delta_degrees = self.pending_pitch_delta;
                    if pos.horizontal().distance(pin.at) <= 6.0 {
                        input.jump = true;
                    }
                }
                input.toggle_fly = self.session.locomotion() == Some(Locomotion::Fly);
            }
            Phase::DungeonArmAway | Phase::BindArmAway => {
                if let (Some(pin), Some(pos)) = (self.pin, self.session.player_position()) {
                    input = walk_away(pos.horizontal(), pin.at, dt);
                    input.yaw_delta_degrees = self.pending_yaw_delta;
                    input.pitch_delta_degrees = self.pending_pitch_delta;
                }
                input.toggle_fly = self.session.locomotion() == Some(Locomotion::Fly);
            }
            Phase::CombatLive => {
                if !self.combat_tab_sent {
                    input.tab = true;
                }
            }
            Phase::ControlsLive => {
                match self.controls_stage {
                    ControlsStage::Tab => input.tab = true,
                    ControlsStage::Strike => {
                        input.verb = self.session.key_binds().verb_for(engine::Key::Digit1)
                    }
                    ControlsStage::WaitHit => {}
                    ControlsStage::Ember => {
                        input.verb = self.session.key_binds().verb_for(engine::Key::Digit5)
                    }
                    ControlsStage::Potion => {
                        input.verb = self.session.key_binds().verb_for(engine::Key::R)
                    }
                }
            }
            Phase::BindForceWalk => {
                if self.session.locomotion() == Some(Locomotion::Fly) {
                    input.toggle_fly = true;
                }
            }
            Phase::BindExitPrep => {
                if self.session.locomotion() != Some(Locomotion::Fly) {
                    input.toggle_fly = true;
                }
            }
            Phase::BindExit => {
                if self.session.locomotion() != Some(Locomotion::Fly) {
                    input.toggle_fly = true;
                }
                input.direction = Vec3::Y;
                input.step_m = FLY_SPEED * dt;
                if let (Some(pin), Some(pos)) = (self.pin, self.session.player_position()) {
                    let horiz = walk_toward(pos.horizontal(), pin.at, dt);
                    input.direction =
                        Vec3::new(horiz.direction.x, 1.0, horiz.direction.z).normalize_or_zero();
                    if input.direction.y < 0.5 {
                        input.direction = Vec3::Y;
                    }
                    input.step_m = FLY_SPEED * dt;
                }
            }
            _ => {}
        }
        input
    }

    fn advance(&mut self, world: &mut World, frame: &Frame) {
        match self.phase {
            Phase::Boot => {}
            Phase::WaitWorld => {
                if self.session.state() == SessionState::World {
                    self.start_current_hook(world, frame);
                    return;
                }
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("timed out waiting for SessionState::World");
                    self.advance_after_fail(world, frame);
                }
            }
            Phase::Standing => self.tick_standing(world, frame),
            Phase::StandingLook => self.tick_standing_look(world, frame),
            Phase::DungeonTravel => self.tick_dungeon_travel(world, frame),
            Phase::DungeonWaitReady => self.tick_dungeon_wait_ready(world, frame),
            Phase::DungeonApproach => self.tick_dungeon_approach(world, frame),
            Phase::DungeonWaitLive => self.tick_dungeon_wait_live(world, frame),
            Phase::DungeonLookHatch => self.tick_dungeon_look_hatch(world, frame),
            Phase::DungeonArmAway => self.tick_arm_away(world, frame, Phase::DungeonArmBack),
            Phase::DungeonArmBack => self.tick_arm_back(world, frame, Phase::DungeonEnter),
            Phase::DungeonEnter => self.tick_enter(world, frame),
            Phase::DungeonInteriorLook => self.tick_dungeon_interior_look(world, frame),
            Phase::BindArmAway => self.tick_arm_away(world, frame, Phase::BindArmBack),
            Phase::BindArmBack => self.tick_arm_back(world, frame, Phase::BindOverworldShot),
            Phase::BindLookDelta => self.tick_bind_look_delta(world, frame),
            Phase::BindOverworldShot => self.tick_bind_overworld_shot(world, frame),
            Phase::BindEnter => self.tick_bind_enter(world, frame),
            Phase::BindLookAfterEntry => self.tick_bind_look_after_entry(world, frame),
            Phase::BindExitPrep => {
                self.yaw_pre_exit = self.session.player_yaw_degrees();
                self.phase = Phase::BindExit;
                self.phase_t0 = frame.time;
            }
            Phase::BindExit => self.tick_bind_exit(world, frame),
            Phase::BindForceWalk => self.tick_force_walk(world, frame),
            Phase::BindWrite => self.tick_bind_write(world, frame),
            Phase::CombatLive => self.tick_combat_live(world, frame),
            Phase::ControlsLive => self.tick_controls(world, frame),
            Phase::NextHook => {
                self.hook_i += 1;
                self.start_current_hook(world, frame);
            }
            Phase::Done => self.finish(world),
        }
    }

    fn start_current_hook(&mut self, world: &mut World, frame: &Frame) {
        let Some(name) = self.current_hook().map(str::to_string) else {
            self.phase = Phase::Done;
            self.finish(world);
            return;
        };
        self.phase_t0 = frame.time;
        match name.as_str() {
            "standing" => {
                self.phase = Phase::Standing;
                self.tick_standing(world, frame);
            }
            "dungeon_fill" => self.start_dungeon_fill(world, frame),
            "bind" => self.start_bind(world, frame),
            "combat" => self.start_combat(world, frame),
            "controls" => self.start_controls(world, frame),
            "faction_overlay" => {
                self.write_json(&name, json!({ "status": "absent" }));
                self.reports
                    .push(json!({ "name": name, "status": "absent" }));
                self.fails.push(name);
                self.phase = Phase::NextHook;
                self.advance(world, frame);
            }
            other => {
                self.write_json(other, json!({ "status": "absent" }));
                self.reports
                    .push(json!({ "name": other, "status": "absent" }));
                self.fails.push(other.to_string());
                self.phase = Phase::NextHook;
                self.advance(world, frame);
            }
        }
    }

    fn tick_standing(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("standing never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(pos) = self.session.player_position() else {
            return;
        };
        let stand = pos.horizontal();
        let Some(contact) = self.session.contact_height(stand) else {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("standing: no contact height under the feet");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        let feet_y = pos.y as f32;
        if (feet_y - contact).abs() > 1.5 || !self.session.feet_on_ground() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!(
                    "standing: feet y={feet_y} contact={contact} on_ground={}",
                    self.session.feet_on_ground()
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let required_ready = self.session.stream().required_ready(stand);
        if !required_ready {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("standing: required_ready stayed false");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if world.pointer_lock() {
            self.fail_current("standing: pointer_lock is true");
            self.advance_after_fail(world, frame);
            return;
        }
        if column_is_wet(&self.session, stand) {
            self.fail_current("standing: spawn/pose is mid-river or wet");
            self.advance_after_fail(world, frame);
            return;
        }
        self.aim_pitch(HORIZON_PITCH);
        self.phase = Phase::StandingLook;
        self.phase_t0 = frame.time;
    }

    fn tick_standing_look(&mut self, world: &mut World, frame: &Frame) {
        let Some(pos) = self.session.player_position() else {
            return;
        };
        let stand = pos.horizontal();
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if !pitch_near(pitch, HORIZON_PITCH) {
            self.aim_pitch(HORIZON_PITCH);
            if frame.time - self.phase_t0 > 5.0 {
                self.fail_current(&format!(
                    "standing: horizon look failed, pitch={pitch} (want {HORIZON_PITCH} for land+sky)"
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if column_is_wet(&self.session, stand) {
            self.fail_current("standing: pose is mid-river or wet after look");
            self.advance_after_fail(world, frame);
            return;
        }
        let contact = self.session.contact_height(stand).unwrap_or(pos.y as f32);
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let fly = self.session.locomotion() == Some(Locomotion::Fly);
        self.write_json(
            "standing",
            json!({
                "status": "ok",
                "state": "World",
                "shot_after": "World",
                "entry_via": self.entry_via,
                "pose": {
                    "x": pos.x,
                    "y": pos.y,
                    "z": pos.z,
                    "yaw_degrees": yaw,
                    "pitch_degrees": pitch,
                },
                "contact_height": contact,
                "feet_on_ground": self.session.feet_on_ground(),
                "look_down": false,
                "horizon": true,
                "wet": false,
                "mid_river": false,
                "required_ready": true,
                "pointer_lock": false,
                "locomotion": locomotion_name(self.session.locomotion()),
                "fly": fly,
            }),
        );
        self.ok_hook("standing");
        world.mark_ready();
        self.queue_shot(world, frame, "standing");
        self.phase = Phase::NextHook;
    }

    fn start_dungeon_fill(&mut self, world: &mut World, frame: &Frame) {
        let from = self
            .session
            .player_position()
            .map(|p| p.horizontal())
            .or_else(|| self.session.spawn().map(|s| s.ground()))
            .unwrap_or(GlobalXZ::at(0.0, 0.0));
        let Some(pin) = self.session.surface().nearest_dungeon(from) else {
            self.fail_current("no DungeonPin on this atlas");
            self.advance_after_fail(world, frame);
            return;
        };
        self.pin = Some(pin);
        let dist = pin.at.distance(from);
        if dist > 80.0 || self.session.state() != SessionState::World {
            match WorldEntryRequest::at_global(self.session.surface().bounds(), pin.at) {
                Ok(request) => match self.session.begin_entry(world, request) {
                    Ok(()) => {
                        self.phase = Phase::DungeonTravel;
                        self.phase_t0 = frame.time;
                    }
                    Err(err) => {
                        self.fail_current(&format!("dungeon entry failed: {err}"));
                        self.advance_after_fail(world, frame);
                    }
                },
                Err(err) => {
                    self.fail_current(&format!("dungeon pin is not a valid entry: {err}"));
                    self.advance_after_fail(world, frame);
                }
            }
        } else {
            self.phase = Phase::DungeonWaitReady;
            self.phase_t0 = frame.time;
        }
    }

    fn tick_dungeon_travel(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() == SessionState::World {
            self.phase = Phase::DungeonWaitReady;
            self.phase_t0 = frame.time;
            return;
        }
        if frame.time - self.phase_t0 > DUNGEON_TIMEOUT_S {
            self.fail_current("timed out travelling to the dungeon pin");
            self.advance_after_fail(world, frame);
        }
    }

    fn tick_dungeon_wait_ready(&mut self, world: &mut World, frame: &Frame) {
        let Some(pin) = self.pin else {
            return;
        };
        if self.session.dungeon_pin_failed(pin.id) {
            self.fail_current("dungeon generate Failed");
            self.advance_after_fail(world, frame);
            return;
        }
        if self.session.dungeon_pin_ready(pin.id) {
            self.phase = Phase::DungeonApproach;
            self.phase_t0 = frame.time;
            return;
        }
        if frame.time - self.phase_t0 > DUNGEON_TIMEOUT_S {
            self.fail_current(&format!(
                "dungeon {} never became ready (seated={} generating={})",
                pin.id,
                self.session.dungeon_seated_count(),
                self.session.dungeon_generating()
            ));
            self.advance_after_fail(world, frame);
        }
    }

    fn tick_dungeon_approach(&mut self, world: &mut World, frame: &Frame) {
        let Some(pin) = self.pin else {
            return;
        };
        if let Some(pos) = self.session.player_position() {
            if pos.horizontal().distance(pin.at) <= LIVE_OPEN_M {
                self.phase = Phase::DungeonWaitLive;
                self.phase_t0 = frame.time;
                return;
            }
        }
        if frame.time - self.phase_t0 > DUNGEON_TIMEOUT_S {
            self.fail_current("never reached LIVE_OPEN_M of the dungeon mouth");
            self.advance_after_fail(world, frame);
        }
    }

    fn tick_dungeon_wait_live(&mut self, world: &mut World, frame: &Frame) {
        if self.session.dungeon_has_live() {
            self.aim_at_hatch();
            self.phase = Phase::DungeonLookHatch;
            self.phase_t0 = frame.time;
            return;
        }
        if frame.time - self.phase_t0 > DUNGEON_TIMEOUT_S {
            self.fail_current("dungeon never went live");
            self.advance_after_fail(world, frame);
        }
    }

    fn aim_pitch(&mut self, target: f32) {
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        self.pending_pitch_delta = target - pitch;
    }

    fn aim_corridor(&mut self) {
        if let Some(landing) = self.session.dungeon_landing_yaw() {
            let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
            self.pending_yaw_delta = shortest_delta(yaw, landing);
        }
        self.aim_pitch(CORRIDOR_PITCH);
    }

    fn aim_at_hatch(&mut self) {
        let Some(pin) = self.pin else {
            return;
        };
        let Some(pos) = self.session.player_position() else {
            return;
        };
        let hatch_y = self
            .session
            .contact_height(pin.at)
            .unwrap_or(pos.y as f32)
            + 0.25;
        let eye = Vec3::new(pos.x as f32, pos.y as f32 + EYE_HEIGHT_M, pos.z as f32);
        let target = Vec3::new(pin.at.x as f32, hatch_y, pin.at.z as f32);
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let (dyaw, dpitch) = look_deltas(eye, target, yaw, pitch);
        self.pending_yaw_delta = dyaw;
        self.pending_pitch_delta = dpitch;
    }

    fn tick_dungeon_look_hatch(&mut self, world: &mut World, frame: &Frame) {
        let Some(pin) = self.pin else {
            return;
        };
        let Some(pos) = self.session.player_position() else {
            return;
        };
        let hatch_y = self
            .session
            .contact_height(pin.at)
            .unwrap_or(pos.y as f32)
            + 0.25;
        let eye = Vec3::new(pos.x as f32, pos.y as f32 + EYE_HEIGHT_M, pos.z as f32);
        let target = Vec3::new(pin.at.x as f32, hatch_y, pin.at.z as f32);
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let angle = view_angle_degrees(eye, yaw, pitch, target);
        self.hatch_angle_degrees = Some(angle);
        self.hatch_in_view = angle <= HATCH_IN_VIEW_DEG;
        if !self.hatch_in_view {
            if frame.time - self.phase_t0 > 5.0 {
                self.fail_current(&format!(
                    "dungeon_mouth: hatch not in view (angle={angle:.1} deg, yaw={yaw:.1}, pitch={pitch:.1})"
                ));
                self.advance_after_fail(world, frame);
            } else {
                self.aim_at_hatch();
            }
            return;
        }
        world.mark_ready();
        self.queue_shot(world, frame, "dungeon_mouth");
        self.phase = Phase::DungeonArmAway;
        self.phase_t0 = frame.time;
    }

    fn tick_arm_away(&mut self, world: &mut World, frame: &Frame, next: Phase) {
        if !self.session.near_hatch() {
            self.phase = next;
            self.phase_t0 = frame.time;
            return;
        }
        if frame.time - self.phase_t0 > 30.0 {
            self.fail_current("could not step off the hatch to arm it");
            self.advance_after_fail(world, frame);
        }
    }

    fn tick_arm_back(&mut self, world: &mut World, frame: &Frame, next: Phase) {
        if self.session.hatch_armed() {
            self.phase = next;
            self.phase_t0 = frame.time;
            return;
        }
        if frame.time - self.phase_t0 > 30.0 {
            self.fail_current("hatch never armed after stepping off");
            self.advance_after_fail(world, frame);
        }
    }

    fn tick_enter(&mut self, world: &mut World, frame: &Frame) {
        if self.session.in_dungeon(world) {
            let pin = self.pin.expect("dungeon pin");
            if self.session.dungeon_pin_failed(pin.id) {
                self.fail_current("dungeon generate Failed");
                self.advance_after_fail(world, frame);
                return;
            }
            self.aim_corridor();
            self.phase = Phase::DungeonInteriorLook;
            self.phase_t0 = frame.time;
            return;
        }
        if frame.time - self.phase_t0 > 45.0 {
            self.fail_current("did not fall through the hatch into the dungeon");
            self.advance_after_fail(world, frame);
        }
    }

    fn start_bind(&mut self, world: &mut World, frame: &Frame) {
        if self.pin.is_none() {
            let from = self
                .session
                .player_position()
                .map(|p| p.horizontal())
                .or_else(|| self.session.spawn().map(|s| s.ground()))
                .unwrap_or(GlobalXZ::at(0.0, 0.0));
            self.pin = self.session.surface().nearest_dungeon(from);
        }
        if self.pin.is_none() || !self.session.dungeon_has_live() {
            self.fail_current("bind needs a live dungeon");
            self.advance_after_fail(world, frame);
            return;
        }
        if self.session.in_dungeon(world) {
            self.phase = Phase::BindExitPrep;
            self.phase_t0 = frame.time;
        } else if self.session.hatch_armed() {
            self.phase = Phase::BindOverworldShot;
            self.phase_t0 = frame.time;
            self.tick_bind_overworld_shot(world, frame);
        } else {
            self.phase = Phase::BindArmAway;
            self.phase_t0 = frame.time;
        }
    }

    fn tick_bind_look_delta(&mut self, world: &mut World, frame: &Frame) {
        let after = self.session.player_yaw_degrees().unwrap_or(0.0);
        let before = self.look_yaw_before.unwrap_or(after);
        self.look_yaw_after = Some(after);
        if !yaw_decreased(before, after) {
            self.fail_current(&format!(
                "bind look-delta: +mouse_dx / yaw_delta={LOOK_DELTA_YAW} did not decrease yaw (before={before} after={after})"
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        self.yaw_decreased = true;
        self.aim_pitch(CORRIDOR_PITCH);
        self.phase = Phase::BindLookAfterEntry;
        self.phase_t0 = frame.time;
    }

    fn tick_bind_overworld_shot(&mut self, world: &mut World, frame: &Frame) {
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        self.yaw_before = Some(yaw);
        world.mark_ready();
        self.queue_shot(world, frame, "bind_overworld");
        self.phase = Phase::BindEnter;
        self.phase_t0 = frame.time;
    }

    fn tick_bind_enter(&mut self, world: &mut World, frame: &Frame) {
        if self.session.locomotion() == Some(Locomotion::Fly) {
            if frame.time - self.phase_t0 > 4.0 {
                self.fail_current("bind leftover fly is on before recrossing the hatch");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.fly_before_recross = Some(false);
        if self.session.in_dungeon(world) {
            let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
            let landing = self.session.dungeon_landing_yaw().unwrap_or(yaw);
            self.yaw_after_entry = Some(yaw);
            self.landing_yaw = Some(landing);
            if !yaw_near(yaw, landing) {
                self.fail_current(&format!(
                    "after entry yaw_degrees={yaw} landing_yaw={landing}"
                ));
                self.advance_after_fail(world, frame);
                return;
            }
            self.look_yaw_before = Some(yaw);
            self.pending_yaw_delta = LOOK_DELTA_YAW;
            self.look_delta_applied = Some(LOOK_DELTA_YAW);
            self.phase = Phase::BindLookDelta;
            self.phase_t0 = frame.time;
            return;
        }
        if frame.time - self.phase_t0 > 45.0 {
            let dist = match (self.pin, self.session.player_position()) {
                (Some(pin), Some(pos)) => pos.horizontal().distance(pin.at),
                _ => -1.0,
            };
            self.fail_current(&format!(
                "bind did not cross the hatch (dist={dist:.1} near={} armed={} live={} fly={:?})",
                self.session.near_hatch(),
                self.session.hatch_armed(),
                self.session.dungeon_has_live(),
                self.session.locomotion()
            ));
            self.advance_after_fail(world, frame);
        }
    }

    fn tick_bind_look_after_entry(&mut self, world: &mut World, frame: &Frame) {
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if !pitch_near(pitch, CORRIDOR_PITCH) {
            self.aim_pitch(CORRIDOR_PITCH);
            if frame.time - self.phase_t0 > 5.0 {
                self.fail_current(&format!(
                    "bind_dungeon: corridor pitch failed, pitch={pitch} (want {CORRIDOR_PITCH})"
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        world.mark_ready();
        self.queue_shot(world, frame, "bind_dungeon");
        self.phase = Phase::BindExitPrep;
        self.phase_t0 = frame.time;
    }

    fn tick_dungeon_interior_look(&mut self, world: &mut World, frame: &Frame) {
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if !pitch_near(pitch, CORRIDOR_PITCH) {
            self.aim_corridor();
            if frame.time - self.phase_t0 > 5.0 {
                self.fail_current(&format!(
                    "dungeon_interior: corridor look failed, pitch={pitch} (want {CORRIDOR_PITCH})"
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if !self.session.feet_on_ground() {
            if frame.time - self.phase_t0 > 8.0 {
                self.fail_current("dungeon_interior: feet never settled on the hall floor");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let pin = self.pin.expect("dungeon pin");
        if self.session.dungeon_pin_failed(pin.id) {
            self.fail_current("dungeon generate Failed");
            self.advance_after_fail(world, frame);
            return;
        }
        let fly = self.session.locomotion() == Some(Locomotion::Fly);
        self.write_json(
            "dungeon_fill",
            json!({
                "status": "ok",
                "pin": {
                    "id": pin.id,
                    "tier": pin.tier,
                    "seed": pin.seed,
                    "x": pin.at.x,
                    "z": pin.at.z,
                },
                "ready": self.session.dungeon_pin_ready(pin.id),
                "seated": self.session.dungeon_pin_seated(pin.id),
                "live": self.session.dungeon_has_live(),
                "failed": false,
                "hatch_in_view": self.hatch_in_view,
                "hatch_angle_degrees": self.hatch_angle_degrees,
                "locomotion": locomotion_name(self.session.locomotion()),
                "fly": fly,
            }),
        );
        self.ok_hook("dungeon_fill");
        world.mark_ready();
        self.queue_shot(world, frame, "dungeon_interior");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn tick_bind_exit(&mut self, world: &mut World, frame: &Frame) {
        if self.session.in_dungeon(world) {
            self.yaw_pre_exit = self.session.player_yaw_degrees();
            if frame.time - self.phase_t0 > 45.0 {
                self.fail_current("bind did not return through the ceiling hatch");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let after_exit = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pre_exit = self.yaw_pre_exit.unwrap_or(after_exit);
        if !yaw_near(after_exit, pre_exit) {
            self.fail_current(&format!(
                "after exit yaw_degrees={after_exit} pre_exit={pre_exit}"
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        if self.yaw_before.is_none() {
            self.start_force_walk(Phase::BindArmAway, frame);
        } else {
            self.start_force_walk(Phase::BindWrite, frame);
        }
    }

    fn start_force_walk(&mut self, next: Phase, frame: &Frame) {
        self.after_force_walk = Some(next);
        self.phase = Phase::BindForceWalk;
        self.phase_t0 = frame.time;
    }

    fn tick_force_walk(&mut self, world: &mut World, frame: &Frame) {
        if self.session.locomotion() == Some(Locomotion::Fly) {
            if frame.time - self.phase_t0 > 5.0 {
                self.fail_current("BindExit leftover fly did not turn off");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if self.session.locomotion() != Some(Locomotion::Walk) {
            if frame.time - self.phase_t0 > 5.0 {
                self.fail_current(&format!(
                    "expected walk after BindExit, got {:?}",
                    self.session.locomotion()
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.fly_after_exit = Some(false);
        let next = self.after_force_walk.unwrap_or(Phase::BindWrite);
        if next == Phase::BindArmAway {
            self.fly_before_recross = Some(false);
        }
        self.phase = next;
        self.phase_t0 = frame.time;
        if next == Phase::BindWrite {
            self.tick_bind_write(world, frame);
        }
    }

    fn tick_bind_write(&mut self, world: &mut World, frame: &Frame) {
        let after_exit = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pre_exit = self.yaw_pre_exit.unwrap_or(after_exit);
        let Some(before) = self.yaw_before else {
            self.phase = Phase::BindArmAway;
            self.phase_t0 = frame.time;
            return;
        };
        let landing = self.landing_yaw.unwrap_or(0.0);
        let after_entry = self.yaw_after_entry.unwrap_or(0.0);
        let yaws = [before, landing, after_entry, after_exit, pre_exit];
        if yaws.iter().all(|y| y.abs() < YAW_EPS) {
            self.fail_current("bind: all yaw_degrees are 0.0 (default-yaw hole)");
            self.advance_after_fail(world, frame);
            return;
        }
        if !self.yaw_decreased || self.look_delta_applied.is_none() {
            self.fail_current("bind: look-delta was not applied");
            self.advance_after_fail(world, frame);
            return;
        }
        if self.fly_after_exit != Some(false) || self.session.locomotion() == Some(Locomotion::Fly) {
            self.fail_current("bind: fly is not false after BindExit");
            self.advance_after_fail(world, frame);
            return;
        }
        let interior = self.shots.join("dungeon_interior.png");
        let bind_shot = self.shots.join("bind_dungeon.png");
        let interior_bytes = fs::read(&interior).unwrap_or_default();
        let bind_bytes = fs::read(&bind_shot).unwrap_or_default();
        if interior_bytes.is_empty() || bind_bytes.is_empty() {
            self.fail_current("bind: missing dungeon_interior.png or bind_dungeon.png");
            self.advance_after_fail(world, frame);
            return;
        }
        if interior_bytes == bind_bytes {
            self.fail_current("bind: bind_dungeon.png is byte-identical to dungeon_interior.png");
            self.advance_after_fail(world, frame);
            return;
        }
        let fly = self.session.locomotion() == Some(Locomotion::Fly);
        self.write_json(
            "bind",
            json!({
                "status": "ok",
                "yaw_degrees": before,
                "landing_yaw": self.landing_yaw,
                "after_entry": self.yaw_after_entry,
                "after_exit": after_exit,
                "pre_exit": pre_exit,
                "pointer_lock": false,
                "look_delta": {
                    "applied": true,
                    "yaw_delta_degrees": self.look_delta_applied,
                    "yaw_before": self.look_yaw_before,
                    "yaw_after": self.look_yaw_after,
                    "decreased": self.yaw_decreased,
                    "plus_mouse_dx_sign": true,
                },
                "bind_dungeon_bytes": bind_bytes.len(),
                "dungeon_interior_bytes": interior_bytes.len(),
                "bind_dungeon_differs": true,
                "locomotion": locomotion_name(self.session.locomotion()),
                "fly": fly,
                "fly_after_exit": self.fly_after_exit,
                "fly_before_recross": self.fly_before_recross,
            }),
        );
        self.ok_hook("bind");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn queue_shot(&mut self, world: &mut World, frame: &Frame, name: &str) {
        let path = self.shots.join(format!("{name}.png"));
        let _ = fs::remove_file(&path);
        world.queue_screenshot(&path);
        self.awaiting_shot = Some(name.to_string());
        self.phase_t0 = frame.time;
        eprintln!("playtester queue shot {name} phase={:?}", self.phase);
    }

    fn write_running_report(&self) {
        let report = json!({
            "hooks": self.reports,
            "skips": self.skips,
            "fails": ["incomplete"],
            "status": "running",
        });
        fs::write(
            self.shots.join("report.json"),
            serde_json::to_string_pretty(&report).expect("report") + "\n",
        )
        .expect("report.json");
    }

    fn start_combat(&mut self, world: &mut World, frame: &Frame) {
        match orrun::combat::fixture_l1_martial_wolf() {
            Ok(fight) => {
                let ttk = fight.get("time_to_kill_s").and_then(|v| v.as_f64());
                let hp = fight.get("hp_remaining").and_then(|v| v.as_i64());
                let winner = fight.get("winner").and_then(|v| v.as_str()).unwrap_or("");
                let sim_ok = ttk == Some(10.8)
                    && hp == Some(55)
                    && winner == "player"
                    && fight.get("band_pass").and_then(|v| v.as_bool()) == Some(true);
                let walk = self.session.combat_walk_speed();
                if !sim_ok {
                    self.write_json(
                        "combat",
                        json!({
                            "status": "fail",
                            "why": "sim inspect TTK 10.8 / HP 55 mismatch",
                            "time_to_kill_s": ttk,
                            "hp_remaining": hp,
                            "winner": winner,
                        }),
                    );
                    self.fail_current("combat sim inspect did not match TTK 10.8 / HP 55");
                    self.advance_after_fail(world, frame);
                    return;
                }
                if (walk - 4.5).abs() > 1e-4 {
                    self.fail_current(&format!(
                        "combat walk is {walk}, want 4.5 (not the playtester inject 10)"
                    ));
                    self.advance_after_fail(world, frame);
                    return;
                }
                self.session.rearm_combat_fixtures();
                self.combat_tab_sent = false;
                self.phase = Phase::CombatLive;
                self.phase_t0 = frame.time;
            }
            Err(e) => {
                self.fail_current(&format!("combat fixture: {e}"));
                self.advance_after_fail(world, frame);
            }
        }
    }

    fn tick_combat_live(&mut self, world: &mut World, frame: &Frame) {
        if !self.combat_tab_sent {
            self.combat_tab_sent = true;
        }
        let walk = self.session.combat_walk_speed();
        let live = self.session.first_auto_hit();
        if live == Some(11) && (walk - 4.5).abs() <= 1e-4 {
            let fight = orrun::combat::fixture_l1_martial_wolf().expect("sim inspect");
            self.write_json(
                "combat",
                json!({
                    "status": "ok",
                    "scenario_id": "1_l1_martial_1wolf",
                    "winner": fight.get("winner"),
                    "time_to_kill_s": fight.get("time_to_kill_s"),
                    "time_to_die_s": fight.get("time_to_die_s"),
                    "hp_remaining": fight.get("hp_remaining"),
                    "hp_max": fight.get("hp_max"),
                    "hp_pct": fight.get("hp_pct"),
                    "mana_spent": fight.get("mana_spent"),
                    "min_mana": fight.get("min_mana"),
                    "mana_decision": fight.get("mana_decision"),
                    "lock": fight.get("lock"),
                    "player_sprinted": fight.get("player_sprinted"),
                    "used_pin_or_bind": fight.get("used_pin_or_bind"),
                    "damage_taken": fight.get("damage_taken"),
                    "swings": fight.get("swings"),
                    "spells_used": fight.get("spells_used"),
                    "band_pass": fight.get("band_pass"),
                    "oneshot": fight.get("oneshot"),
                    "first_mitigated_auto": 11,
                    "live_first_auto": live,
                    "combat_walk_mps": walk,
                }),
            );
            self.ok_hook("combat");
            self.phase = Phase::NextHook;
            self.phase_t0 = frame.time;
            return;
        }
        if frame.time - self.phase_t0 > 5.0 {
            self.fail_current(&format!(
                "live lock+auto want first mitigated 11, got {live:?}; walk {walk}"
            ));
            self.advance_after_fail(world, frame);
        }
    }

    fn start_controls(&mut self, world: &mut World, frame: &Frame) {
        let loaded = Settings::load().unwrap_or_else(|err| panic!("{err}"));
        let missing = loaded.keys.missing();
        if !missing.is_empty() {
            let names: Vec<_> = missing.iter().map(|v| v.label()).collect();
            self.write_json(
                "controls",
                json!({
                    "status": "fail",
                    "why": format!("unbound verbs: {}", names.join(",")),
                    "keys": loaded.keys.inspect_map(),
                }),
            );
            self.fail_current(&format!(
                "controls: unbound verbs: {}",
                names.join(",")
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        let binds = loaded.keys;
        if binds.verb_for(engine::Key::Digit1) != Some(CombatVerb::Strike) {
            self.fail_current("controls: default Strike key 1 is not bound to Strike");
            self.advance_after_fail(world, frame);
            return;
        }
        if binds.verb_for(engine::Key::Digit5) != Some(CombatVerb::Ember) {
            self.fail_current("controls: default Ember key 5 is not bound to Ember");
            self.advance_after_fail(world, frame);
            return;
        }
        if binds.verb_for(engine::Key::R) != Some(CombatVerb::Potion) {
            self.fail_current("controls: potion default is R (Q is sidestep) and must be bound");
            self.advance_after_fail(world, frame);
            return;
        }
        self.session.set_key_binds(binds);
        self.session.rearm_combat_fixtures();
        self.controls_stage = ControlsStage::Tab;
        self.phase = Phase::ControlsLive;
        self.phase_t0 = frame.time;
    }

    fn tick_controls(&mut self, world: &mut World, frame: &Frame) {
        if frame.time - self.phase_t0 > 8.0 {
            self.fail_current(&format!(
                "controls timed out in {:?} lock={:?} auto={:?} ember={} potions={}",
                self.controls_stage,
                self.session.lock_id(),
                self.session.first_auto_hit(),
                self.session.combat().ember_started,
                self.session.combat().player.potions
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        match self.controls_stage {
            ControlsStage::Tab => {
                if self.session.lock_id().is_some() {
                    self.controls_stage = ControlsStage::Strike;
                }
            }
            ControlsStage::Strike => {
                if !self.session.combat().strike_armed {
                    self.fail_current("controls: pressing default Strike key 1 did not arm Strike");
                    self.advance_after_fail(world, frame);
                    return;
                }
                self.controls_stage = ControlsStage::WaitHit;
            }
            ControlsStage::WaitHit => {
                match self.session.first_auto_hit() {
                    Some(16) => {
                        self.session.combat_mut().player.stats.ranks.arcane = 1;
                        self.controls_stage = ControlsStage::Ember;
                    }
                    Some(got) => {
                        self.fail_current(&format!(
                            "controls: Strike next swing want mitigated 16, got {got}"
                        ));
                        self.advance_after_fail(world, frame);
                    }
                    None => {}
                }
            }
            ControlsStage::Ember => {
                if !self.session.combat().ember_started {
                    self.fail_current(
                        "controls: pressing default Ember key 5 did not start Ember",
                    );
                    self.advance_after_fail(world, frame);
                    return;
                }
                self.session.combat_mut().player.resources.hp = 50.0;
                self.controls_stage = ControlsStage::Potion;
            }
            ControlsStage::Potion => {
                let hp = self.session.combat().player.resources.hp;
                let potions = self.session.combat().player.potions;
                let heal = self.session.combat().last_potion_heal;
                if (hp - 90.0).abs() > 1e-6 || potions != 0 || heal != 40 {
                    self.fail_current(&format!(
                        "controls: potion want hp 90 potions 0 heal 40, got hp {hp} potions {potions} heal {heal}"
                    ));
                    self.advance_after_fail(world, frame);
                    return;
                }
                let keys = self.session.key_binds().inspect_map();
                self.write_json(
                    "controls",
                    json!({
                        "status": "ok",
                        "keys": keys,
                        "strike_first_auto": 16,
                        "ember_started": true,
                        "potion_hp": hp,
                        "potion_heal": heal,
                        "potion_key": "R",
                    }),
                );
                self.ok_hook("controls");
                self.phase = Phase::NextHook;
                self.phase_t0 = frame.time;
            }
        }
    }

    fn write_json(&self, name: &str, value: Value) {
        let path = self.shots.join(format!("{name}.json"));
        fs::write(
            &path,
            serde_json::to_string_pretty(&value).expect("json") + "\n",
        )
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }

    fn ok_hook(&mut self, name: &str) {
        self.reports.push(json!({ "name": name, "status": "ok" }));
        eprintln!("playtester hook {name} ok");
        self.write_running_report();
    }

    fn fail_current(&mut self, why: &str) {
        let name = self.current_hook().unwrap_or("unknown").to_string();
        eprintln!("playtester hook {name} failed: {why}");
        self.write_json(
            &name,
            json!({
                "status": "fail",
                "why": why,
            }),
        );
        self.reports.push(json!({
            "name": name,
            "status": "fail",
            "why": why,
        }));
        self.fails.push(name);
    }

    fn advance_after_fail(&mut self, world: &mut World, frame: &Frame) {
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
        self.advance(world, frame);
    }

    fn finish(&mut self, world: &mut World) {
        if self.finished {
            return;
        }
        self.finished = true;
        let report = json!({
            "hooks": self.reports,
            "skips": self.skips,
            "fails": self.fails,
        });
        fs::write(
            self.shots.join("report.json"),
            serde_json::to_string_pretty(&report).expect("report") + "\n",
        )
        .expect("report.json");
        world.request_exit();
    }
}

fn walk_toward(from: GlobalXZ, to: GlobalXZ, dt: f32) -> WalkInput {
    let dx = (to.x - from.x) as f32;
    let dz = (to.z - from.z) as f32;
    let direction = Vec3::new(dx, 0.0, dz).normalize_or_zero();
    WalkInput {
        direction,
        dt,
        step_m: WALK_SPEED * dt,
        ..WalkInput::IDLE
    }
}

fn walk_away(from: GlobalXZ, origin: GlobalXZ, dt: f32) -> WalkInput {
    let dx = (from.x - origin.x) as f32;
    let dz = (from.z - origin.z) as f32;
    let mut direction = Vec3::new(dx, 0.0, dz).normalize_or_zero();
    if direction.length_squared() < 1e-6 {
        direction = Vec3::Z;
    }
    WalkInput {
        direction,
        dt,
        step_m: WALK_SPEED * dt,
        ..WalkInput::IDLE
    }
}

fn column_is_wet(session: &WorldSession, p: GlobalXZ) -> bool {
    let mut column = session.surface().column(p);
    session.ponds().carve(p, &mut column);
    column.is_wet() || column.wetness() > -MIN_FREEBOARD_M
}

fn pose_too_wet(session: &WorldSession, p: GlobalXZ) -> bool {
    if column_is_wet(session, p) {
        return true;
    }
    const RADII: [f64; 2] = [12.0, 24.0];
    const DIRS: usize = 8;
    for radius in RADII {
        for k in 0..DIRS {
            let angle = std::f64::consts::TAU * k as f64 / DIRS as f64;
            let q = GlobalXZ::at(p.x + radius * angle.cos(), p.z + radius * angle.sin());
            if column_is_wet(session, q) {
                return true;
            }
        }
    }
    false
}

fn inland_heading(session: &WorldSession, p: GlobalXZ) -> Heading {
    const LOOK_M: f64 = 60.0;
    const SAMPLES: usize = 24;
    let mut best = f32::INFINITY;
    let mut dir = Vec2::new(0.0, 1.0);
    for k in 0..SAMPLES {
        let angle = std::f64::consts::TAU * k as f64 / SAMPLES as f64;
        let d = Vec2::new(angle.cos() as f32, angle.sin() as f32);
        let q = GlobalXZ::at(p.x + LOOK_M * angle.cos(), p.z + LOOK_M * angle.sin());
        let mut column = session.surface().column(q);
        session.ponds().carve(q, &mut column);
        let wetness = column.wetness();
        if wetness < best {
            best = wetness;
            dir = d;
        }
    }
    Heading::towards(dir).unwrap_or(Heading::NORTH)
}

fn pitch_near(value: f32, target: f32) -> bool {
    (value - target).abs() <= PITCH_EPS
}

fn locomotion_name(mode: Option<Locomotion>) -> &'static str {
    match mode {
        Some(Locomotion::Walk) => "walk",
        Some(Locomotion::Fly) => "fly",
        None => "none",
    }
}

fn shortest_delta(from: f32, to: f32) -> f32 {
    (to - from + 180.0).rem_euclid(360.0) - 180.0
}

fn yaw_near(a: f32, b: f32) -> bool {
    shortest_delta(a, b).abs() < YAW_EPS
}

fn yaw_decreased(from: f32, to: f32) -> bool {
    shortest_delta(from, to) < -YAW_EPS
}

fn look_deltas(eye: Vec3, target: Vec3, yaw: f32, pitch: f32) -> (f32, f32) {
    let to = target - eye;
    let horiz = (to.x * to.x + to.z * to.z).sqrt();
    let desired_yaw = to.x.atan2(to.z).to_degrees();
    let desired_pitch = to.y.atan2(horiz).to_degrees().clamp(-89.0, 89.0);
    (shortest_delta(yaw, desired_yaw), desired_pitch - pitch)
}

fn view_angle_degrees(eye: Vec3, yaw: f32, pitch: f32, target: Vec3) -> f32 {
    let look = Camera::direction(yaw, pitch);
    let to = (target - eye).normalize_or_zero();
    if to.length_squared() < 1e-8 {
        return 0.0;
    }
    look.dot(to).clamp(-1.0, 1.0).acos().to_degrees()
}

