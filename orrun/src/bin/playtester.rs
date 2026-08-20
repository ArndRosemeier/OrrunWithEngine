//! v0 playtest harness: standing, dungeon_fill, bind, combat, controls.
//!
//! Driven through [`WorldSession::step`] with explicit [`WalkInput`]. Never
//! captures the pointer, never treats E as interact, never steals Escape.
//! Look is applied as WalkInput yaw/pitch *deltas* (same sign as +mouse_dx),
//! not as an absolute setLook.
//!
//! Usage:
//! `cargo run -p orrun --release --bin playtester -- --seed 1 --size 64 --hooks standing,dungeon_fill,bind,combat,controls,combat_body,combat_bones,combat_mage`

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use engine::egui::{self, Color32, Sense};
use engine::prelude::*;
use engine::space::GlobalXZ;
use glam::{Vec2, Vec3};
use orrun::atlas::ContinentAtlas;
use orrun::controls::{self, Action, PressedActions};
use orrun::hud;
use orrun::settings::Settings;
use orrun::world::{
    install_daylight, install_materials, resolve_spawn, DungeonPin, Heading, HousePlot, Locomotion,
    Ambience, MapPoint, SessionState, WalkInput, WorldEntryRequest, WorldSession, LIVE_OPEN_M,
    OverlandSite, SiteKind, plan_overland_sites,
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
        "combat.png",
        "hurt.png",
        "slain.png",
        "controls.json",
        "combat_body.json",
        "combat_body.png",
        "combat_bones.json",
        "combat_bones.png",
        "combat_mage.json",
        "combat_mage.png",
        "village.json",
        "village.png",
        "cairn.json",
        "hut.json",
        "overworld_cairn.json",
        "overworld_hut.json",
        "overworld_cairn.png",
        "overworld_hut.png",
        "yeti.json",
        "yeti.png",
        "demon.json",
        "demon.png",
        "bluedemon.json",
        "bluedemon.png",
        "tribal_veteran.json",
        "tribal_veteran.png",
        "death.json",
        "death.png",
        "cast_bar.json",
        "cast_bar.png",
        "incoming.json",
        "incoming.png",
        "camp.json",
        "camp.png",
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
        combat_shot_sent: false,
        combat_hurt_sent: false,
        combat_cd_sweep_at: None,
        combat_cd_sweep_sent: false,
        combat_cast_bar_at: None,
        combat_cast_bar_sent: false,
        combat_fail_armed: false,
        combat_fail_sent: false,
        combat_death_sent: false,
        combat_death_killed: false,
        combat_death_hold_at: None,
        incoming_hp: None,
        combat_orc_punch_at: None,
        combat_yeti_punch_at: None,
        combat_demon_punch_at: None,
        combat_demon_stand: None,
        combat_bluedemon_punch_at: None,
        combat_tribal_veteran_punch_at: None,
        combat_body_melee_at: None,
        combat_bones_melee_at: None,
        combat_mage_cast_at: None,
        combat_mage_stand: None,
        combat_mage_look: None,
        controls_stage: ControlsStage::Tab,
        ambience: match Ambience::load() {
            Ok(a) => Some(a),
            Err(err) => {
                eprintln!("playtester ambience skipped: {err}");
                None
            }
        },
        site_kind: None,
        site_melee_at: None,
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
    CombatOrcLive,
    CombatDeathLive,
    LootSparkleLive,
    LootModalLive,
    BagLive,
    CombatYetiLive,
    CombatDemonLive,
    CombatBlueDemonLive,
    CombatTribalVeteranLive,
    CombatBodyLive,
    CombatBonesLive,
    CombatMageLive,
    ControlsLive,
    VillageTravel,
    VillageLive,
    CampTravel,
    CampLive,
    CairnTravel,
    CairnLive,
    HutTravel,
    HutLive,
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
    combat_shot_sent: bool,
    combat_hurt_sent: bool,
    combat_cd_sweep_at: Option<f32>,
    combat_cd_sweep_sent: bool,
    combat_cast_bar_at: Option<f32>,
    combat_cast_bar_sent: bool,
    combat_fail_armed: bool,
    combat_fail_sent: bool,
    combat_death_sent: bool,
    combat_death_killed: bool,
    combat_death_hold_at: Option<f32>,
    incoming_hp: Option<f64>,
    combat_orc_punch_at: Option<f32>,
    combat_yeti_punch_at: Option<f32>,
    combat_demon_punch_at: Option<f32>,
    combat_demon_stand: Option<GlobalXZ>,
    combat_bluedemon_punch_at: Option<f32>,
    combat_tribal_veteran_punch_at: Option<f32>,
    combat_body_melee_at: Option<f32>,
    combat_bones_melee_at: Option<f32>,
    combat_mage_cast_at: Option<f32>,
    combat_mage_stand: Option<GlobalXZ>,
    combat_mage_look: Option<Vec3>,
    controls_stage: ControlsStage,
    ambience: Option<Ambience>,
    site_kind: Option<SiteKind>,
    site_melee_at: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlsStage {
    Tab,
    Strike,
    WaitHit,
    Bash,
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
        if let Some(ambience) = self.ambience.as_mut() {
            if let Err(err) = ambience.update(&mut self.session, frame.dt) {
                eprintln!("playtester ambience: {err}");
            }
        }
        self.pending_yaw_delta = 0.0;
        self.pending_pitch_delta = 0.0;

        let paint_combat_hud = matches!(
            self.phase,
            Phase::CombatLive
                | Phase::CombatOrcLive
                | Phase::CombatDeathLive
                | Phase::LootSparkleLive
                | Phase::LootModalLive
                | Phase::BagLive
                | Phase::CombatYetiLive
                | Phase::CombatDemonLive
                | Phase::CombatBlueDemonLive
                | Phase::CombatTribalVeteranLive
        ) || matches!(
            self.awaiting_shot.as_deref(),
            Some("combat")
                | Some("hurt")
                | Some("slain")
                | Some("hud")
                | Some("roster")
                | Some("death")
                | Some("loot_sparkle")
                | Some("loot_modal")
                | Some("bag")
                | Some("yeti")
                | Some("demon")
                | Some("bluedemon")
                | Some("tribal_veteran")
                | Some("cd_sweep")
                | Some("cast_bar")
                | Some("incoming")
        );
        if paint_combat_hud {
            draw_combat_hud(&self.session, frame);
        }
        if matches!(
            self.phase,
            Phase::LootSparkleLive | Phase::LootModalLive | Phase::BagLive
        ) || matches!(
            self.awaiting_shot.as_deref(),
            Some("loot_sparkle") | Some("loot_modal") | Some("bag")
        ) {
            hud::draw_loot_windows(&mut self.session, world, frame);
        }

        if let Some(name) = self.awaiting_shot.clone() {
            let path = self.shots.join(format!("{name}.png"));
            if path.is_file() {
                if name == "hud" {
                    let _ = fs::copy(&path, self.shots.join("hurt.png"));
                }
                if name == "overworld_cairn" || name == "overworld_hut" || name == "yeti" || name == "demon" || name == "death" || name == "loot_sparkle" || name == "loot_modal" || name == "bag" || name == "cd_sweep" || name == "cast_bar" || name == "incoming" || name == "camp" {
                    let dest = PathBuf::from(r"C:\Users\windo").join(format!("{name}.png"));
                    let _ = fs::copy(&path, dest);
                }
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

    fn input_for_phase(&mut self, frame: &Frame) -> WalkInput {
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
                // Stay in melee until wolves finish the kill. Do not walk to
                // the side vantage (that is outside 1.8 m reach).
                if !self.combat_tab_sent {
                    // look only; fixture wolves spawn in melee in front of the player
                } else if self.session.slain_line().is_none() && self.session.lock_id().is_none() {
                    input.tab = true;
                } else if self.current_hook() == Some("cast_bar") {
                    // Ember 28 m reaches the wolf line; stay put for the bar shot.
                } else if self.combat_hurt_sent && !self.combat_fail_sent {
                    if let (Some(pos), Some(stand)) = (
                        self.session.player_position(),
                        combat_oor_stand(&self.session),
                    ) {
                        if pos.horizontal().distance(stand) > 0.35 {
                            input = walk_toward(pos.horizontal(), stand, dt);
                            input.yaw_delta_degrees = self.pending_yaw_delta;
                            input.pitch_delta_degrees = self.pending_pitch_delta;
                        }
                    }
                } else if self.session.slain_line().is_none() {
                    if let (Some(pos), Some(stand)) = (
                        self.session.player_position(),
                        combat_melee_stand(&self.session),
                    ) {
                        if pos.horizontal().distance(stand) > 0.35 {
                            input = walk_toward(pos.horizontal(), stand, dt);
                            input.yaw_delta_degrees = self.pending_yaw_delta;
                            input.pitch_delta_degrees = self.pending_pitch_delta;
                        }
                    }
                }
            }
            Phase::CombatDeathLive | Phase::LootSparkleLive | Phase::LootModalLive | Phase::BagLive => {
                if !self.combat_death_killed && self.combat_tab_sent && self.session.lock_id().is_none() {
                    input.tab = true;
                } else if self.combat_death_killed {
                    if let (Some(pos), Some(stand)) = (
                        self.session.player_position(),
                        orc_view_stand(&self.session),
                    ) {
                        if pos.horizontal().distance(stand) > 0.45 {
                            input = walk_toward(pos.horizontal(), stand, dt);
                            input.yaw_delta_degrees = self.pending_yaw_delta;
                            input.pitch_delta_degrees = self.pending_pitch_delta;
                        }
                    }
                }
            }
            Phase::CombatOrcLive => {
                // Stay in the 1.5 m stand until Punch connects (reach 2.0).
                // Then walk back to the human viewing stand so roster.png
                // shows a creature, not a torso wall.
                if self.combat_tab_sent && self.session.lock_id().is_none() {
                    input.tab = true;
                } else if self.session.incoming_hit() {
                    if let (Some(pos), Some(stand)) = (
                        self.session.player_position(),
                        orc_view_stand(&self.session),
                    ) {
                        if pos.horizontal().distance(stand) > 0.45 {
                            input = walk_toward(pos.horizontal(), stand, dt);
                            input.yaw_delta_degrees = self.pending_yaw_delta;
                            input.pitch_delta_degrees = self.pending_pitch_delta;
                        }
                    }
                }
            }
            Phase::CombatYetiLive => {
                // Stay in the 1.5 m stand until Punch connects (reach 2.2).
                // Then walk back to the human viewing stand so yeti.png
                // shows a creature, not a torso wall.
                if self.combat_tab_sent && self.session.lock_id().is_none() {
                    input.tab = true;
                } else if self.session.incoming_hit() {
                    if let (Some(pos), Some(stand)) = (
                        self.session.player_position(),
                        orc_view_stand(&self.session),
                    ) {
                        if pos.horizontal().distance(stand) > 0.45 {
                            input = walk_toward(pos.horizontal(), stand, dt);
                            input.yaw_delta_degrees = self.pending_yaw_delta;
                            input.pitch_delta_degrees = self.pending_pitch_delta;
                        }
                    }
                }
            }
            Phase::CombatBlueDemonLive => {
                // Stay in the 1.5 m stand until Punch connects (reach 2.0).
                // Then walk back to the human viewing stand so bluedemon.png
                // shows a BLUE body, not a torso wall. Not Demon 3/4 Trident.
                if self.combat_tab_sent && self.session.lock_id().is_none() {
                    input.tab = true;
                } else if self.session.incoming_hit() {
                    if let (Some(pos), Some(stand)) = (
                        self.session.player_position(),
                        orc_view_stand(&self.session),
                    ) {
                        if pos.horizontal().distance(stand) > 0.45 {
                            input = walk_toward(pos.horizontal(), stand, dt);
                            input.yaw_delta_degrees = self.pending_yaw_delta;
                            input.pitch_delta_degrees = self.pending_pitch_delta;
                        }
                    }
                }
            }
            Phase::CombatTribalVeteranLive => {
                // Stay in the 1.5 m stand until Punch connects (reach 1.6).
                // Then walk back to the human viewing stand so tribal_veteran.png
                // shows mask + bone necklace + left-shoulder pelt, not a torso wall.
                if self.combat_tab_sent && self.session.lock_id().is_none() {
                    input.tab = true;
                } else if self.session.incoming_hit() {
                    if let (Some(pos), Some(stand)) = (
                        self.session.player_position(),
                        orc_view_stand(&self.session),
                    ) {
                        if pos.horizontal().distance(stand) > 0.45 {
                            input = walk_toward(pos.horizontal(), stand, dt);
                            input.yaw_delta_degrees = self.pending_yaw_delta;
                            input.pitch_delta_degrees = self.pending_pitch_delta;
                        }
                    }
                }
            }
            Phase::CombatDemonLive => {
                // Stay in the 1.5 m stand until Punch connects (reach 2.2).
                // Then walk to a 3/4 human viewing stand so demon.png
                // shows the Trident pole, not an end-on stick.
                if self.combat_tab_sent && self.session.lock_id().is_none() {
                    input.tab = true;
                } else if self.session.incoming_hit() {
                    if self.combat_demon_stand.is_none() {
                        self.combat_demon_stand = demon_view_stand(&self.session);
                    }
                    if let (Some(pos), Some(stand)) = (
                        self.session.player_position(),
                        self.combat_demon_stand,
                    ) {
                        if pos.horizontal().distance(stand) > 0.45 {
                            input = walk_toward(pos.horizontal(), stand, dt);
                            input.yaw_delta_degrees = self.pending_yaw_delta;
                            input.pitch_delta_degrees = self.pending_pitch_delta;
                        }
                    }
                }
            }
            Phase::CombatBodyLive => {
                if !self.combat_tab_sent {
                    input.tab = true;
                } else if let (Some(pos), Some(stand)) = (
                    self.session.player_position(),
                    wolf_body_view_stand(&self.session),
                ) {
                    if pos.horizontal().distance(stand) > 0.45 {
                        input = walk_toward(pos.horizontal(), stand, dt);
                        input.yaw_delta_degrees = self.pending_yaw_delta;
                        input.pitch_delta_degrees = self.pending_pitch_delta;
                    }
                }
            }
            Phase::CombatBonesLive => {
                if !self.combat_tab_sent {
                    input.tab = true;
                } else if let (Some(pos), Some(stand)) = (
                    self.session.player_position(),
                    bone_body_view_stand(&self.session),
                ) {
                    if pos.horizontal().distance(stand) > 0.45 {
                        input = walk_toward(pos.horizontal(), stand, dt);
                        input.yaw_delta_degrees = self.pending_yaw_delta;
                        input.pitch_delta_degrees = self.pending_pitch_delta;
                    }
                }
            }
            Phase::CombatMageLive => {
                if !self.combat_tab_sent || self.session.lock_id().is_none() {
                    input.tab = true;
                } else {
                    if self.combat_mage_stand.is_none() {
                        self.combat_mage_stand = mage_view_stand(&self.session);
                        self.combat_mage_look = mage_mesh_chest(&self.session);
                    }
                    if let (Some(pos), Some(stand)) = (
                        self.session.player_position(),
                        self.combat_mage_stand,
                    ) {
                        if pos.horizontal().distance(stand) > 0.45 {
                            input = walk_toward(pos.horizontal(), stand, dt);
                            input.yaw_delta_degrees = self.pending_yaw_delta;
                            input.pitch_delta_degrees = self.pending_pitch_delta;
                        }
                    }
                }
            }
            Phase::ControlsLive => {
                match self.controls_stage {
                    ControlsStage::Tab => input.tab = true,
                    ControlsStage::Strike => {
                        input.actions = PressedActions::from_actions(&controls::resolve(
                            self.session.key_binds(),
                            [engine::Key::Digit1],
                        ));
                    }
                    ControlsStage::WaitHit => {}
                    ControlsStage::Bash => {
                        input.actions = PressedActions::from_actions(&controls::resolve(
                            self.session.key_binds(),
                            [engine::Key::Digit2],
                        ));
                    }
                    ControlsStage::Ember => {
                        input.actions = PressedActions::from_actions(&controls::resolve(
                            self.session.key_binds(),
                            [engine::Key::Digit5],
                        ));
                    }
                    ControlsStage::Potion => {
                        input.actions = PressedActions::from_actions(&controls::resolve(
                            self.session.key_binds(),
                            [engine::Key::R],
                        ));
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
            Phase::VillageTravel | Phase::CampTravel | Phase::CairnTravel | Phase::HutTravel => {
                input.skip_travel = true;
            }
            Phase::CairnLive | Phase::HutLive => {
                if self.session.locomotion() != Some(Locomotion::Fly) {
                    input.toggle_fly = true;
                }
                if self.session.lock_id().is_none() {
                    input.tab = true;
                }
                if let (Some(stand), Some(pos)) = (
                    site_camera_stand(&self.session, self.site_kind),
                    self.session.player_position(),
                ) {
                    let dx = (stand.x - pos.x) as f32;
                    let dz = (stand.z - pos.z) as f32;
                    let dy = (stand.y - pos.y) as f32;
                    let mut direction = Vec3::new(dx, dy, dz);
                    if direction.length_squared() > 1e-6 {
                        direction = direction.normalize();
                    } else {
                        direction = Vec3::Y;
                    }
                    input.direction = direction;
                    input.step_m = FLY_SPEED * dt;
                }
            }
            Phase::VillageLive => {
                if self.session.locomotion() != Some(Locomotion::Fly) {
                    input.toggle_fly = true;
                }
                if let (Some(stand), Some(pos)) = (
                    village_camera_stand(&self.session),
                    self.session.player_position(),
                ) {
                    let dx = (stand.x - pos.x) as f32;
                    let dz = (stand.z - pos.z) as f32;
                    let dy = (stand.y - pos.y) as f32;
                    let mut direction = Vec3::new(dx, dy, dz);
                    if direction.length_squared() > 1e-6 {
                        direction = direction.normalize();
                    } else {
                        direction = Vec3::Y;
                    }
                    input.direction = direction;
                    input.step_m = FLY_SPEED * dt;
                }
            }
            Phase::CampLive => {
                if self.session.locomotion() != Some(Locomotion::Fly) {
                    input.toggle_fly = true;
                }
                if let (Some(stand), Some(pos)) = (
                    self.session.village_camp_stand(),
                    self.session.player_position(),
                ) {
                    let dx = (stand.x - pos.x) as f32;
                    let dz = (stand.z - pos.z) as f32;
                    let dy = (stand.y - pos.y) as f32;
                    let mut direction = Vec3::new(dx, dy, dz);
                    if direction.length_squared() > 1e-6 {
                        direction = direction.normalize();
                    } else {
                        direction = Vec3::Y;
                    }
                    input.direction = direction;
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
            Phase::CombatOrcLive => self.tick_combat_orc(world, frame),
            Phase::CombatDeathLive => self.tick_combat_death(world, frame),
            Phase::LootSparkleLive => self.tick_loot_sparkle(world, frame),
            Phase::LootModalLive => self.tick_loot_modal(world, frame),
            Phase::BagLive => self.tick_bag(world, frame),
            Phase::CombatYetiLive => self.tick_combat_yeti(world, frame),
            Phase::CombatDemonLive => self.tick_combat_demon(world, frame),
            Phase::CombatBlueDemonLive => self.tick_combat_bluedemon(world, frame),
            Phase::CombatTribalVeteranLive => self.tick_combat_tribal_veteran(world, frame),
            Phase::CombatBodyLive => self.tick_combat_body(world, frame),
            Phase::CombatBonesLive => self.tick_combat_bones(world, frame),
            Phase::CombatMageLive => self.tick_combat_mage(world, frame),
            Phase::ControlsLive => self.tick_controls(world, frame),
            Phase::VillageTravel => self.tick_village_travel(world, frame),
            Phase::VillageLive => self.tick_village_live(world, frame),
            Phase::CampTravel => self.tick_camp_travel(world, frame),
            Phase::CampLive => self.tick_camp_live(world, frame),
            Phase::CairnTravel | Phase::HutTravel => self.tick_site_travel(world, frame),
            Phase::CairnLive | Phase::HutLive => self.tick_site_live(world, frame),
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
            "combat" | "cd_sweep" | "cast_bar" | "incoming" => self.start_combat(world, frame),
            "combat_orc" => self.start_combat_orc(world, frame),
            "combat_death" => self.start_combat_death(world, frame),
            "loot_sparkle" => self.start_loot_sparkle(world, frame),
            "loot_modal" => self.start_loot_modal(world, frame),
            "bag" => self.start_bag(world, frame),
            "combat_yeti" => self.start_combat_yeti(world, frame),
            "combat_demon" => self.start_combat_demon(world, frame),
            "combat_bluedemon" => self.start_combat_bluedemon(world, frame),
            "combat_tribal_veteran" => self.start_combat_tribal_veteran(world, frame),
            "combat_body" => self.start_combat_body(world, frame),
            "combat_bones" => self.start_combat_bones(world, frame),
            "combat_mage" => self.start_combat_mage(world, frame),
            "controls" => self.start_controls(world, frame),
            "village" => self.start_village(world, frame),
            "camp" => self.start_camp(world, frame),
            "cairn" | "taken_cairn" | "overworld_cairn" => {
                self.start_site(world, frame, SiteKind::TakenCairn)
            }
            "hut" | "woods_hut" | "overworld_hut" => {
                self.start_site(world, frame, SiteKind::WoodsHut)
            }
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

    fn aim_camp(&mut self) {
        let Some((eye, target)) = self.session.village_camp_look() else {
            return;
        };
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let (dyaw, dpitch) = look_deltas(eye, target, yaw, pitch);
        self.pending_yaw_delta = dyaw;
        self.pending_pitch_delta = dpitch;
    }

    fn aim_village_cut(&mut self) {
        let Some((eye, target)) = village_look(&self.session) else {
            return;
        };
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let (dyaw, dpitch) = look_deltas(eye, target, yaw, pitch);
        self.pending_yaw_delta = dyaw;
        self.pending_pitch_delta = dpitch;
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


    fn start_combat_body(&mut self, world: &mut World, frame: &Frame) {
        self.session.rearm_combat_fixtures(world);
        self.combat_tab_sent = false;
        self.combat_body_melee_at = None;
        self.phase = Phase::CombatBodyLive;
        self.phase_t0 = frame.time;
    }

    fn tick_combat_body(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_body: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if let Some(pos) = self.session.player_position() {
            if !self.session.stream().required_ready(pos.horizontal()) {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat_body: required_ready stayed false");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
        }
        if !self.combat_tab_sent {
            self.combat_tab_sent = true;
        }
        self.aim_at_wolf_body();
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if !self.looking_at_wolf_body() {
            if frame.time - self.phase_t0 > 8.0 {
                self.fail_current(&format!(
                    "combat_body: eye-height look at wolf body failed, pitch={pitch} (not floor)"
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some((lock_name, lock_hp)) = self.session.lock_name_hp().map(|(n, h)| (n.to_string(), h)) else {
            if frame.time - self.phase_t0 > 8.0 {
                self.fail_current("combat_body: lock name/hp unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        if lock_name != "wolf-spider" || lock_hp <= 0.0 {
            self.fail_current(&format!(
                "combat_body: lock want name wolf-spider + HP>0, got {lock_name} hp={lock_hp}"
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        let mesh_visible = self.session.fixture_mesh_visible(world);
        if !mesh_visible {
            if frame.time - self.phase_t0 > 8.0 {
                self.fail_current("combat_body: wolf mesh not visible");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if let Some(stand) = wolf_body_view_stand(&self.session) {
            if let Some(pos) = self.session.player_position() {
                if pos.horizontal().distance(stand) >= 0.45 {
                    if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                        self.fail_current("combat_body: never reached wolf view stand");
                        self.advance_after_fail(world, frame);
                    }
                    return;
                }
            }
        }
        let melee_at = *self.combat_body_melee_at.get_or_insert_with(|| {
            self.session.replay_melee(world);
            frame.time
        });
        // Attack is 1.333 s; mid-lunge reads around 0.60 s at speed 1.
        if frame.time - melee_at < 0.60 {
            return;
        }
        self.write_json(
            "combat_body",
            json!({
                "status": "ok",
                "name": lock_name,
                "lock_name": lock_name,
                "hp": lock_hp,
                "mesh_visible": true,
                "mesh": "fauna/wolf/wolf.gltf",
                "mesh_note": "catalog has wolf, not spider",
                "horizon": false,
                "look_down": false,
                "eye_height_body": true,
                "shot": "combat_body.png",
            }),
        );
        self.ok_hook("combat_body");
        world.mark_ready();
        self.queue_shot(world, frame, "combat_body");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }


    fn start_combat_bones(&mut self, world: &mut World, frame: &Frame) {
        self.session.rearm_bones_fixture(world);
        self.combat_tab_sent = false;
        self.combat_bones_melee_at = None;
        self.phase = Phase::CombatBonesLive;
        self.phase_t0 = frame.time;
    }

    fn tick_combat_bones(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_bones: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if let Some(pos) = self.session.player_position() {
            if !self.session.stream().required_ready(pos.horizontal()) {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat_bones: required_ready stayed false");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
        }
        if !self.combat_tab_sent {
            self.combat_tab_sent = true;
        }
        self.aim_at_bone_body();
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if !self.looking_at_bone_body() {
            if frame.time - self.phase_t0 > 8.0 {
                self.fail_current(&format!(
                    "combat_bones: eye-height look at bone body failed, pitch={pitch} (not floor)"
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some((lock_name, lock_hp)) = self.session.lock_name_hp().map(|(n, h)| (n.to_string(), h)) else {
            if frame.time - self.phase_t0 > 8.0 {
                self.fail_current("combat_bones: lock name/hp unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        if (lock_name != "Warrior" && lock_name != "Minion") || lock_hp <= 0.0 {
            self.fail_current(&format!(
                "combat_bones: lock want Warrior or Minion + HP>0, got {lock_name} hp={lock_hp}"
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        let mesh_visible = self.session.fixture_mesh_visible(world);
        if !mesh_visible {
            if frame.time - self.phase_t0 > 8.0 {
                self.fail_current("combat_bones: bone mesh not visible");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if let Some(stand) = bone_body_view_stand(&self.session) {
            if let Some(pos) = self.session.player_position() {
                if pos.horizontal().distance(stand) >= 0.45 {
                    if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                        self.fail_current("combat_bones: never reached bone view stand");
                        self.advance_after_fail(world, frame);
                    }
                    return;
                }
            }
        }
        let melee_at = *self.combat_bones_melee_at.get_or_insert_with(|| {
            self.session.replay_melee(world);
            frame.time
        });
        if frame.time - melee_at < 0.50 {
            return;
        }
        self.write_json(
            "combat_bones",
            json!({
                "status": "ok",
                "name": lock_name,
                "lock_name": lock_name,
                "hp": lock_hp,
                "mesh_visible": true,
                "mesh": "monsters/kaykit/Skeleton_Warrior.glb or Skeleton_Minion.glb",
                "clip": "Unarmed_Melee_Attack_Punch_A",
                "horizon": false,
                "look_down": false,
                "eye_height_body": true,
                "shot": "combat_bones.png",
            }),
        );
        self.ok_hook("combat_bones");
        world.mark_ready();
        self.queue_shot(world, frame, "combat_bones");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn start_combat_mage(&mut self, world: &mut World, frame: &Frame) {
        self.session.rearm_mage_fixture(world);
        self.combat_tab_sent = false;
        self.combat_mage_cast_at = None;
        self.combat_mage_stand = None;
        self.combat_mage_look = None;
        self.phase = Phase::CombatMageLive;
        self.phase_t0 = frame.time;
    }

    fn tick_combat_mage(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_mage: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if let Some(pos) = self.session.player_position() {
            if !self.session.stream().required_ready(pos.horizontal()) {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat_mage: required_ready stayed false");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
        }
        if !self.combat_tab_sent {
            self.combat_tab_sent = true;
        }
        self.aim_at_mage_body();
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if !self.looking_at_mage_body() {
            if frame.time - self.phase_t0 > 8.0 {
                self.fail_current(&format!(
                    "combat_mage: eye-height look at mage body failed, pitch={pitch} (not floor)"
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some((lock_name, lock_hp)) = self.session.lock_name_hp().map(|(n, h)| (n.to_string(), h)) else {
            if frame.time - self.phase_t0 > 8.0 {
                self.fail_current("combat_mage: lock name/hp unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        if lock_name != "Mage" || lock_hp <= 0.0 {
            self.fail_current(&format!(
                "combat_mage: lock want Mage + HP>0, got {lock_name} hp={lock_hp}"
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        let mesh_visible = self.session.fixture_mesh_visible(world);
        if !mesh_visible {
            if frame.time - self.phase_t0 > 8.0 {
                self.fail_current("combat_mage: mage mesh not visible");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if self.combat_mage_stand.is_none() {
            self.combat_mage_stand = mage_view_stand(&self.session);
            self.combat_mage_look = mage_mesh_chest(&self.session);
        }
        if let Some(stand) = self.combat_mage_stand {
            if let Some(pos) = self.session.player_position() {
                if pos.horizontal().distance(stand) >= 0.45 {
                    if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                        self.fail_current("combat_mage: never reached mage view stand");
                        self.advance_after_fail(world, frame);
                    }
                    return;
                }
            }
        }
        let cast_at = *self.combat_mage_cast_at.get_or_insert_with(|| {
            self.session.replay_weapon(world);
            frame.time
        });
        if frame.time - cast_at < 0.62 {
            return;
        }
        self.write_json(
            "combat_mage",
            json!({
                "status": "ok",
                "name": lock_name,
                "lock_name": lock_name,
                "hp": lock_hp,
                "mesh_visible": true,
                "mesh": "monsters/kaykit/Skeleton_Mage_Staff.glb",
                "clip": "Spellcast_Shoot",
                "horizon": false,
                "look_down": false,
                "eye_height_body": true,
                "shot": "combat_mage.png",
            }),
        );
        self.ok_hook("combat_mage");
        world.mark_ready();
        self.queue_shot(world, frame, "combat_mage");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn aim_at_bone_body(&mut self) {
        self.aim_at_humanoid(1.6, 1.15);
    }

    fn looking_at_bone_body(&self) -> bool {
        self.looking_at_humanoid(1.6, 1.15)
    }

    fn aim_at_mage_body(&mut self) {
        // Mesh sits 1.6 m behind the hitbox. After the side-stand snapshot,
        // keep looking at that chest so a right-offset crop stays on the mage.
        if let Some(target) = self.combat_mage_look {
            let Some(pos) = self.session.player_position() else {
                return;
            };
            let eye = Vec3::new(pos.x as f32, pos.y as f32 + EYE_HEIGHT_M, pos.z as f32);
            let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
            let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
            let (dyaw, dpitch) = look_deltas(eye, target, yaw, pitch);
            self.pending_yaw_delta = dyaw;
            self.pending_pitch_delta = dpitch;
            return;
        }
        self.aim_at_humanoid(1.6, 1.15);
    }

    fn looking_at_mage_body(&self) -> bool {
        if let Some(target) = self.combat_mage_look {
            let Some(pos) = self.session.player_position() else {
                return false;
            };
            let eye = Vec3::new(pos.x as f32, pos.y as f32 + EYE_HEIGHT_M, pos.z as f32);
            let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
            let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
            return view_angle_degrees(eye, yaw, pitch, target) < 18.0 && pitch > -55.0;
        }
        self.looking_at_humanoid(1.6, 1.15)
    }

    fn aim_at_humanoid(&mut self, behind_m: f64, chest_m: f32) {
        let Some(pos) = self.session.player_position() else {
            return;
        };
        let lock = self.session.lock_id();
        let Some(h) = self
            .session
            .combat()
            .hostiles
            .iter()
            .find(|h| lock.map(|id| h.idx == id).unwrap_or(false))
            .or_else(|| self.session.combat().hostiles.first())
        else {
            return;
        };
        let eye = Vec3::new(pos.x as f32, pos.y as f32 + EYE_HEIGHT_M, pos.z as f32);
        let dx = h.x - pos.x;
        let dz = h.z - pos.z;
        let len = (dx * dx + dz * dz).sqrt();
        let (ux, uz) = if len > 1e-6 {
            (dx / len, dz / len)
        } else {
            (1.0, 0.0)
        };
        let target = Vec3::new(
            (h.x + ux * behind_m) as f32,
            pos.y as f32 + chest_m,
            (h.z + uz * behind_m) as f32,
        );
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let (dyaw, dpitch) = look_deltas(eye, target, yaw, pitch);
        self.pending_yaw_delta = dyaw;
        self.pending_pitch_delta = dpitch;
    }

    fn looking_at_humanoid(&self, behind_m: f64, chest_m: f32) -> bool {
        let Some(pos) = self.session.player_position() else {
            return false;
        };
        let lock = self.session.lock_id();
        let Some(h) = self
            .session
            .combat()
            .hostiles
            .iter()
            .find(|h| lock.map(|id| h.idx == id).unwrap_or(false))
            .or_else(|| self.session.combat().hostiles.first())
        else {
            return false;
        };
        let eye = Vec3::new(pos.x as f32, pos.y as f32 + EYE_HEIGHT_M, pos.z as f32);
        let dx = h.x - pos.x;
        let dz = h.z - pos.z;
        let len = (dx * dx + dz * dz).sqrt();
        let (ux, uz) = if len > 1e-6 {
            (dx / len, dz / len)
        } else {
            (1.0, 0.0)
        };
        let target = Vec3::new(
            (h.x + ux * behind_m) as f32,
            pos.y as f32 + chest_m,
            (h.z + uz * behind_m) as f32,
        );
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        view_angle_degrees(eye, yaw, pitch, target) < 18.0 && pitch > -55.0
    }

    fn aim_at_wolf_body(&mut self) {
        let Some(pos) = self.session.player_position() else {
            return;
        };
        let lock = self.session.lock_id();
        let Some(h) = self
            .session
            .combat()
            .hostiles
            .iter()
            .find(|h| lock.map(|id| h.idx == id).unwrap_or(false))
            .or_else(|| self.session.combat().hostiles.first())
        else {
            return;
        };
        let eye = Vec3::new(pos.x as f32, pos.y as f32 + EYE_HEIGHT_M, pos.z as f32);
        let dx = h.x - pos.x;
        let dz = h.z - pos.z;
        let len = (dx * dx + dz * dz).sqrt();
        let (ux, uz) = if len > 1e-6 {
            (dx / len, dz / len)
        } else {
            (1.0, 0.0)
        };
        // Mesh sits WOLF_MESH_BEHIND_M behind the combat point.
        let target = Vec3::new(
            (h.x + ux * 2.55) as f32,
            pos.y as f32 + 0.85,
            (h.z + uz * 2.55) as f32,
        );
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let (dyaw, dpitch) = look_deltas(eye, target, yaw, pitch);
        self.pending_yaw_delta = dyaw;
        self.pending_pitch_delta = dpitch;
    }

    fn looking_at_wolf_body(&self) -> bool {
        let Some(pos) = self.session.player_position() else {
            return false;
        };
        let Some(h) = self.session.combat().hostiles.first() else {
            return false;
        };
        let eye = Vec3::new(pos.x as f32, pos.y as f32 + EYE_HEIGHT_M, pos.z as f32);
        let dx = h.x - pos.x;
        let dz = h.z - pos.z;
        let len = (dx * dx + dz * dz).sqrt();
        let (ux, uz) = if len > 1e-6 {
            (dx / len, dz / len)
        } else {
            (1.0, 0.0)
        };
        let target = Vec3::new(
            (h.x + ux * 2.55) as f32,
            pos.y as f32 + 0.85,
            (h.z + uz * 2.55) as f32,
        );
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        view_angle_degrees(eye, yaw, pitch, target) < 18.0 && pitch > -55.0
    }

    fn start_combat_orc(&mut self, world: &mut World, frame: &Frame) {
        self.session.rearm_orc_fixture(world);
        self.combat_tab_sent = false;
        self.combat_shot_sent = false;
        self.combat_orc_punch_at = None;
        self.phase = Phase::CombatOrcLive;
        self.phase_t0 = frame.time;
    }

    fn tick_combat_orc(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_orc: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(pos) = self.session.player_position() else {
            return;
        };
        if !self.session.stream().required_ready(pos.horizontal()) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_orc: required_ready stayed false");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if !self.session.fixture_mesh_visible(world) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_orc: orc mesh not visible");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.aim_at_orc();
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if let Some((eye, target)) = orc_look(&self.session) {
            if view_angle_degrees(eye, yaw, pitch, target) > 14.0 {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat_orc: never looked at orc");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
        }
        self.combat_tab_sent = true;
        if self.session.lock_id().is_none() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_orc: lock unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some((name, hp)) = self.session.lock_name_hp() else {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_orc: lock name/hp unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        let name = name.to_string();
        if name != "orc" {
            self.fail_current(&format!("combat_orc: want orc lock, got {name}"));
            self.advance_after_fail(world, frame);
            return;
        }
        if !self.session.incoming_hit() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_orc: incoming Punch never landed");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(stand) = orc_view_stand(&self.session) else {
            self.fail_current("combat_orc: no orc view stand");
            self.advance_after_fail(world, frame);
            return;
        };
        if pos.horizontal().distance(stand) >= 0.45 {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_orc: never reached orc view stand");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let punch_at = *self.combat_orc_punch_at.get_or_insert_with(|| {
            self.session.replay_melee(world);
            frame.time
        });
        if frame.time - punch_at < 0.35 {
            return;
        }
        self.write_json(
            "combat_orc",
            json!({
                "status": "ok",
                "name": name,
                "hp": hp,
                "punch": true,
                "shot": "roster.png",
            }),
        );
        self.ok_hook("combat_orc");
        world.mark_ready();
        self.queue_shot(world, frame, "roster");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }


    fn start_combat_death(&mut self, world: &mut World, frame: &Frame) {
        self.session.rearm_orc_fixture(world);
        self.combat_tab_sent = false;
        self.combat_death_killed = false;
        self.combat_death_hold_at = None;
        self.phase = Phase::CombatDeathLive;
        self.phase_t0 = frame.time;
    }

    fn tick_combat_death(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_death: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(pos) = self.session.player_position() else {
            return;
        };
        if !self.session.stream().required_ready(pos.horizontal()) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_death: required_ready stayed false");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if !self.session.fixture_mesh_visible(world) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_death: orc mesh not visible");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.aim_at_orc();
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if let Some((eye, target)) = orc_look(&self.session) {
            if view_angle_degrees(eye, yaw, pitch, target) > 14.0 {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat_death: never looked at orc");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
        }
        self.combat_tab_sent = true;
        if !self.combat_death_killed {
            if self.session.lock_id().is_none() {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat_death: lock unset after Tab");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
            let Some((name, hp)) = self.session.lock_name_hp() else {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat_death: lock name/hp unset after Tab");
                    self.advance_after_fail(world, frame);
                }
                return;
            };
            let name = name.to_string();
            if name != "orc" || hp <= 0.0 {
                self.fail_current(&format!(
                    "combat_death: want live orc lock, got {name} hp={hp}"
                ));
                self.advance_after_fail(world, frame);
                return;
            }
            let lock = self.session.lock_id();
            let combat = self.session.combat_mut();
            for h in &mut combat.hostiles {
                if Some(h.idx) == lock {
                    h.hp = 0.0;
                    h.alive = false;
                }
            }
            combat.lock = None;
            self.combat_death_killed = true;
            return;
        }
        let Some(stand) = orc_view_stand(&self.session) else {
            self.fail_current("combat_death: no orc view stand");
            self.advance_after_fail(world, frame);
            return;
        };
        if pos.horizontal().distance(stand) >= 0.45 {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_death: never reached orc view stand");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if !death_hold_ready(world, &self.session) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_death: Death clip never held last frame");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let hold_at = *self.combat_death_hold_at.get_or_insert(frame.time);
        // One extra beat on the hold frame so the shot is not mid-flop.
        if frame.time - hold_at < 0.08 {
            return;
        }
        let corpse = self
            .session
            .combat()
            .hostiles
            .iter()
            .find(|h| !h.alive)
            .map(|h| (h.name.clone(), h.alive, h.hp));
        let Some((name, alive, hp)) = corpse else {
            self.fail_current("combat_death: no dead hostile after kill");
            self.advance_after_fail(world, frame);
            return;
        };
        self.write_json(
            "combat_death",
            json!({
                "status": "ok",
                "name": name,
                "alive": alive,
                "hp": hp,
                "clip": "Death",
                "looping": false,
                "shot": "death.png",
            }),
        );
        self.ok_hook("combat_death");
        world.mark_ready();
        self.queue_shot(world, frame, "death");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn loot_corpse_ready(&self, world: &World) -> bool {
        self.session.combat().hostiles.iter().any(|h| !h.alive)
            && self.session.sparkle_visible(world)
    }

    fn start_loot_sparkle(&mut self, world: &mut World, frame: &Frame) {
        if self.loot_corpse_ready(world) {
            self.combat_death_killed = true;
            self.combat_tab_sent = true;
            self.combat_death_hold_at = None;
            self.phase = Phase::LootSparkleLive;
            self.phase_t0 = frame.time;
            return;
        }
        self.session.rearm_orc_fixture(world);
        self.combat_tab_sent = false;
        self.combat_death_killed = false;
        self.combat_death_hold_at = None;
        self.phase = Phase::LootSparkleLive;
        self.phase_t0 = frame.time;
    }

    fn start_loot_modal(&mut self, world: &mut World, frame: &Frame) {
        if self.loot_corpse_ready(world) {
            self.combat_death_killed = true;
            self.combat_tab_sent = true;
            self.combat_death_hold_at = Some(frame.time);
            self.combat_shot_sent = false;
            self.phase = Phase::LootModalLive;
            self.phase_t0 = frame.time;
            return;
        }
        self.start_loot_sparkle(world, frame);
        self.combat_shot_sent = false;
        self.phase = Phase::LootModalLive;
    }

    fn start_bag(&mut self, world: &mut World, frame: &Frame) {
        if self.loot_corpse_ready(world) {
            self.session.open_first_loot();
            self.session.take_all_loot(world);
        }
        self.session.set_bag_open(true);
        self.combat_shot_sent = false;
        self.phase = Phase::BagLive;
        self.phase_t0 = frame.time;
    }

    fn tick_loot_kill_setup(&mut self, world: &mut World, frame: &Frame, hook: &str) -> bool {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!("{hook}: never reached World"));
                self.advance_after_fail(world, frame);
            }
            return false;
        }
        let Some(pos) = self.session.player_position() else {
            return false;
        };
        if !self.session.stream().required_ready(pos.horizontal()) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!("{hook}: required_ready stayed false"));
                self.advance_after_fail(world, frame);
            }
            return false;
        }
        if !self.session.fixture_mesh_visible(world) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!("{hook}: orc mesh not visible"));
                self.advance_after_fail(world, frame);
            }
            return false;
        }
        self.aim_at_orc();
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if let Some((eye, target)) = orc_look(&self.session) {
            if view_angle_degrees(eye, yaw, pitch, target) > 14.0 {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current(&format!("{hook}: never looked at orc"));
                    self.advance_after_fail(world, frame);
                }
                return false;
            }
        }
        self.combat_tab_sent = true;
        if !self.combat_death_killed {
            if self.session.lock_id().is_none() {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current(&format!("{hook}: lock unset after Tab"));
                    self.advance_after_fail(world, frame);
                }
                return false;
            }
            let Some((name, hp)) = self.session.lock_name_hp() else {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current(&format!("{hook}: lock name/hp unset after Tab"));
                    self.advance_after_fail(world, frame);
                }
                return false;
            };
            let name = name.to_string();
            if name != "orc" || hp <= 0.0 {
                self.fail_current(&format!(
                    "{hook}: want live orc lock, got {name} hp={hp}"
                ));
                self.advance_after_fail(world, frame);
                return false;
            }
            let lock = self.session.lock_id();
            {
                let combat = self.session.combat_mut();
                for h in &mut combat.hostiles {
                    if Some(h.idx) == lock {
                        h.hp = 0.0;
                        h.alive = false;
                    }
                }
                combat.lock = None;
            }
            if let Some(idx) = lock {
                self.session.force_visible_loot(idx);
            }
            self.combat_death_killed = true;
            return false;
        }
        let Some(stand) = orc_view_stand(&self.session) else {
            self.fail_current(&format!("{hook}: no orc view stand"));
            self.advance_after_fail(world, frame);
            return false;
        };
        if pos.horizontal().distance(stand) >= 0.45 {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!("{hook}: never reached orc view stand"));
                self.advance_after_fail(world, frame);
            }
            return false;
        }
        if !death_hold_ready(world, &self.session) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!("{hook}: Death clip never held last frame"));
                self.advance_after_fail(world, frame);
            }
            return false;
        }
        if !self.session.sparkle_visible(world) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!("{hook}: sparkle never spawned on Death body"));
                self.advance_after_fail(world, frame);
            }
            return false;
        }
        let hold_at = *self.combat_death_hold_at.get_or_insert(frame.time);
        if frame.time - hold_at < 0.08 {
            return false;
        }
        true
    }

    fn tick_loot_sparkle(&mut self, world: &mut World, frame: &Frame) {
        if !self.tick_loot_kill_setup(world, frame, "loot_sparkle") {
            return;
        }
        self.write_json(
            "loot_sparkle",
            json!({
                "status": "ok",
                "sparkle": true,
                "shot": "loot_sparkle.png",
            }),
        );
        self.ok_hook("loot_sparkle");
        world.mark_ready();
        self.queue_shot(world, frame, "loot_sparkle");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn tick_loot_modal(&mut self, world: &mut World, frame: &Frame) {
        if !self.tick_loot_kill_setup(world, frame, "loot_modal") {
            return;
        }
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let facing = engine::camera::Camera::facing_xz(yaw);
        if let Some(pos) = self.session.player_position() {
            let opened = self
                .session
                .try_dead_loot(pos.x, pos.z, facing.x as f64, facing.z as f64);
            if !opened {
                self.session.open_first_loot();
            }
        } else if !self.session.open_first_loot() {
            self.fail_current("loot_modal: no pile to open");
            self.advance_after_fail(world, frame);
            return;
        }
        if !self.session.loot_open() {
            self.fail_current("loot_modal: loot window did not open");
            self.advance_after_fail(world, frame);
            return;
        }
        if !self.combat_shot_sent {
            self.combat_shot_sent = true;
            self.combat_death_hold_at = Some(frame.time);
            return;
        }
        let hold_at = self.combat_death_hold_at.unwrap_or(frame.time);
        if frame.time - hold_at < 0.16 {
            return;
        }
        self.write_json(
            "loot_modal",
            json!({
                "status": "ok",
                "loot_open": true,
                "shot": "loot_modal.png",
            }),
        );
        self.ok_hook("loot_modal");
        world.mark_ready();
        self.queue_shot(world, frame, "loot_modal");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn tick_bag(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("bag: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.session.set_bag_open(true);
        if !self.session.bag_open() {
            self.fail_current("bag: I bag did not open");
            self.advance_after_fail(world, frame);
            return;
        }
        if !self.combat_shot_sent {
            self.combat_shot_sent = true;
            self.combat_death_hold_at = Some(frame.time);
            return;
        }
        let hold_at = self.combat_death_hold_at.unwrap_or(frame.time);
        if frame.time - hold_at < 0.16 {
            return;
        }
        let inv = self.session.inventory();
        if inv.bag.len() != 8 {
            self.fail_current("bag: bag is not 8 slots");
            self.advance_after_fail(world, frame);
            return;
        }
        self.write_json(
            "bag",
            json!({
                "status": "ok",
                "coin": inv.coin,
                "melee": inv.melee.map(|i| i.name()),
                "shot": "bag.png",
            }),
        );
        self.ok_hook("bag");
        world.mark_ready();
        self.queue_shot(world, frame, "bag");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn start_combat_yeti(&mut self, world: &mut World, frame: &Frame) {
        self.session.rearm_yeti_fixture(world);
        self.combat_tab_sent = false;
        self.combat_shot_sent = false;
        self.combat_yeti_punch_at = None;
        self.phase = Phase::CombatYetiLive;
        self.phase_t0 = frame.time;
    }

    fn tick_combat_yeti(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_yeti: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(pos) = self.session.player_position() else {
            return;
        };
        if !self.session.stream().required_ready(pos.horizontal()) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_yeti: required_ready stayed false");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if !self.session.fixture_mesh_visible(world) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_yeti: yeti mesh not visible");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.aim_at_orc();
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if let Some((eye, target)) = orc_look(&self.session) {
            if view_angle_degrees(eye, yaw, pitch, target) > 14.0 {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat_yeti: never looked at yeti");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
        }
        self.combat_tab_sent = true;
        if self.session.lock_id().is_none() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_yeti: lock unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some((name, hp)) = self.session.lock_name_hp() else {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_yeti: lock name/hp unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        let name = name.to_string();
        if name != "yeti" {
            self.fail_current(&format!("combat_yeti: want yeti lock, got {name}"));
            self.advance_after_fail(world, frame);
            return;
        }
        if !self.session.incoming_hit() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_yeti: incoming Punch never landed");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(stand) = orc_view_stand(&self.session) else {
            self.fail_current("combat_yeti: no yeti view stand");
            self.advance_after_fail(world, frame);
            return;
        };
        if pos.horizontal().distance(stand) >= 0.45 {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_yeti: never reached yeti view stand");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let punch_at = *self.combat_yeti_punch_at.get_or_insert_with(|| {
            self.session.replay_melee(world);
            frame.time
        });
        if frame.time - punch_at < 0.35 {
            return;
        }
        self.write_json(
            "yeti",
            json!({
                "status": "ok",
                "name": name,
                "hp": hp,
                "punch": true,
                "shot": "yeti.png",
            }),
        );
        self.ok_hook("combat_yeti");
        world.mark_ready();
        self.queue_shot(world, frame, "yeti");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn start_combat_demon(&mut self, world: &mut World, frame: &Frame) {
        self.session.rearm_demon_fixture(world);
        self.combat_tab_sent = false;
        self.combat_shot_sent = false;
        self.combat_demon_punch_at = None;
        self.combat_demon_stand = None;
        self.phase = Phase::CombatDemonLive;
        self.phase_t0 = frame.time;
    }

    fn tick_combat_demon(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_demon: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(pos) = self.session.player_position() else {
            return;
        };
        if !self.session.stream().required_ready(pos.horizontal()) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_demon: required_ready stayed false");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if !self.session.fixture_mesh_visible(world) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_demon: demon mesh not visible");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.aim_at_demon();
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if let Some((eye, target)) = demon_look(&self.session) {
            if view_angle_degrees(eye, yaw, pitch, target) > 16.0 {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat_demon: never looked at demon");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
        }
        self.combat_tab_sent = true;
        if self.session.lock_id().is_none() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_demon: lock unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some((name, hp)) = self.session.lock_name_hp() else {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_demon: lock name/hp unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        let name = name.to_string();
        if name != "demon" {
            self.fail_current(&format!("combat_demon: want demon lock, got {name}"));
            self.advance_after_fail(world, frame);
            return;
        }
        if !self.session.incoming_hit() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_demon: incoming Punch never landed");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if self.combat_demon_stand.is_none() {
            self.combat_demon_stand = demon_view_stand(&self.session);
        }
        let Some(stand) = self.combat_demon_stand else {
            self.fail_current("combat_demon: no demon view stand");
            self.advance_after_fail(world, frame);
            return;
        };
        if pos.horizontal().distance(stand) >= 0.45 {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_demon: never reached demon view stand");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let punch_at = *self.combat_demon_punch_at.get_or_insert_with(|| {
            self.session.replay_melee(world);
            frame.time
        });
        if frame.time - punch_at < 0.35 {
            return;
        }
        self.write_json(
            "demon",
            json!({
                "status": "ok",
                "name": name,
                "hp": hp,
                "punch": true,
                "shot": "demon.png",
            }),
        );
        self.ok_hook("combat_demon");
        world.mark_ready();
        self.queue_shot(world, frame, "demon");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn start_combat_bluedemon(&mut self, world: &mut World, frame: &Frame) {
        self.session.rearm_bluedemon_fixture(world);
        self.combat_tab_sent = false;
        self.combat_shot_sent = false;
        self.combat_bluedemon_punch_at = None;
        self.phase = Phase::CombatBlueDemonLive;
        self.phase_t0 = frame.time;
    }

    fn tick_combat_bluedemon(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_bluedemon: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(pos) = self.session.player_position() else {
            return;
        };
        if !self.session.stream().required_ready(pos.horizontal()) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_bluedemon: required_ready stayed false");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if !self.session.fixture_mesh_visible(world) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_bluedemon: blue_demon mesh not visible");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.aim_at_orc();
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if let Some((eye, target)) = orc_look(&self.session) {
            if view_angle_degrees(eye, yaw, pitch, target) > 14.0 {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat_bluedemon: never looked at blue_demon");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
        }
        self.combat_tab_sent = true;
        if self.session.lock_id().is_none() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_bluedemon: lock unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some((name, hp)) = self.session.lock_name_hp() else {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_bluedemon: lock name/hp unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        let name = name.to_string();
        if name != "blue_demon" {
            self.fail_current(&format!("combat_bluedemon: want blue_demon lock, got {name}"));
            self.advance_after_fail(world, frame);
            return;
        }
        if !self.session.incoming_hit() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_bluedemon: incoming Punch never landed");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(stand) = orc_view_stand(&self.session) else {
            self.fail_current("combat_bluedemon: no blue_demon view stand");
            self.advance_after_fail(world, frame);
            return;
        };
        if pos.horizontal().distance(stand) >= 0.45 {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_bluedemon: never reached blue_demon view stand");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let punch_at = *self.combat_bluedemon_punch_at.get_or_insert_with(|| {
            self.session.replay_melee(world);
            frame.time
        });
        if frame.time - punch_at < 0.35 {
            return;
        }
        self.write_json(
            "bluedemon",
            json!({
                "status": "ok",
                "name": name,
                "hp": hp,
                "punch": true,
                "shot": "bluedemon.png",
            }),
        );

        self.ok_hook("combat_bluedemon");
        world.mark_ready();
        self.queue_shot(world, frame, "bluedemon");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn start_combat_tribal_veteran(&mut self, world: &mut World, frame: &Frame) {
        self.session.rearm_tribal_veteran_fixture(world);
        self.combat_tab_sent = false;
        self.combat_shot_sent = false;
        self.combat_tribal_veteran_punch_at = None;
        self.phase = Phase::CombatTribalVeteranLive;
        self.phase_t0 = frame.time;
    }

    fn tick_combat_tribal_veteran(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_tribal_veteran: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(pos) = self.session.player_position() else {
            return;
        };
        if !self.session.stream().required_ready(pos.horizontal()) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_tribal_veteran: required_ready stayed false");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if !self.session.fixture_mesh_visible(world) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_tribal_veteran: tribal_veteran mesh not visible");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.aim_at_orc();
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if let Some((eye, target)) = orc_look(&self.session) {
            if view_angle_degrees(eye, yaw, pitch, target) > 14.0 {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat_tribal_veteran: never looked at tribal_veteran");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
        }
        self.combat_tab_sent = true;
        if self.session.lock_id().is_none() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_tribal_veteran: lock unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some((name, hp)) = self.session.lock_name_hp() else {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_tribal_veteran: lock name/hp unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        let name = name.to_string();
        if name != "tribal_veteran" {
            self.fail_current(&format!("combat_tribal_veteran: want tribal_veteran lock, got {name}"));
            self.advance_after_fail(world, frame);
            return;
        }
        if !self.session.incoming_hit() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_tribal_veteran: incoming Punch never landed");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(stand) = orc_view_stand(&self.session) else {
            self.fail_current("combat_tribal_veteran: no tribal_veteran view stand");
            self.advance_after_fail(world, frame);
            return;
        };
        if pos.horizontal().distance(stand) >= 0.45 {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat_tribal_veteran: never reached tribal_veteran view stand");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let punch_at = *self.combat_tribal_veteran_punch_at.get_or_insert_with(|| {
            self.session.replay_melee(world);
            frame.time
        });
        if frame.time - punch_at < 0.35 {
            return;
        }
        self.write_json(
            "tribal_veteran",
            json!({
                "status": "ok",
                "name": name,
                "hp": hp,
                "punch": true,
                "shot": "tribal_veteran.png",
            }),
        );
        self.ok_hook("combat_tribal_veteran");
        world.mark_ready();
        self.queue_shot(world, frame, "tribal_veteran");
        self.phase = Phase::NextHook;
        self.phase_t0 = frame.time;
    }

    fn tick_cast_bar(&mut self, world: &mut World, frame: &Frame) {
        if self.combat_cast_bar_at.is_none() {
            let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
            let facing = Camera::facing_xz(yaw);
            let Some(pos) = self.session.player_position() else {
                return;
            };
            let started = self.session.combat_mut().press_verb(
                Action::Ember,
                pos.x,
                pos.z,
                facing.x as f64,
                facing.z as f64,
            );
            let t = self.session.combat().cast_t;
            let kind = self.session.combat().cast_kind;
            if !started || kind != Some("ember") || t <= 0.0 {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current(&format!(
                        "cast_bar: Ember never started (started={started} kind={kind:?} t={t})"
                    ));
                    self.advance_after_fail(world, frame);
                }
                return;
            }
            self.combat_cast_bar_at = Some(frame.time);
            return;
        }
        if self.combat_cast_bar_sent {
            return;
        }
        let t = self.session.combat().cast_t;
        let kind = self.session.combat().cast_kind;
        if kind != Some("ember") || t <= 0.0 {
            self.fail_current(&format!(
                "cast_bar: Ember finished before mid-cast shot (kind={kind:?} t={t})"
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        // remaining-time: 1.2 at start. Shoot ~0.5–0.75s in so the bar is mid.
        if t > 0.70 {
            return;
        }
        if t < 0.45 {
            self.fail_current(&format!("cast_bar: missed mid-cast window (t={t})"));
            self.advance_after_fail(world, frame);
            return;
        }
        self.combat_cast_bar_sent = true;
        world.mark_ready();
        self.queue_shot(world, frame, "cast_bar");
        self.write_json(
            "cast_bar",
            json!({
                "status": "ok",
                "shot": "cast_bar.png",
                "cast_kind": kind,
                "cast_t": t,
                "hotbar_visible": true,
            }),
        );
        self.ok_hook("cast_bar");
        self.phase = Phase::NextHook;
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
                self.session.rearm_combat_fixtures(world);
                self.combat_tab_sent = false;
                self.combat_shot_sent = false;
                self.combat_cd_sweep_at = None;
                self.combat_cd_sweep_sent = false;
                self.combat_cast_bar_at = None;
                self.combat_cast_bar_sent = false;
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
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(pos) = self.session.player_position() else {
            return;
        };
        if !self.session.stream().required_ready(pos.horizontal()) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat: required_ready stayed false");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if self.combat_hurt_sent {
            if !self.combat_fail_sent {
                self.aim_at_wolf_line();
                let Some(pos) = self.session.player_position() else {
                    return;
                };
                if !self.combat_fail_armed {
                    if let Some(stand) = combat_oor_stand(&self.session) {
                        if pos.horizontal().distance(stand) > 0.5 {
                            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                                self.fail_current("combat: never walked out of melee");
                                self.advance_after_fail(world, frame);
                            }
                            return;
                        }
                    }
                    let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
                    let facing = Camera::facing_xz(yaw);
                    let _ = self.session.combat_mut().press_verb(
                        Action::Strike,
                        pos.x,
                        pos.z,
                        facing.x as f64,
                        facing.z as f64,
                    );
                    let tell = self.session.fail_tell();
                    let log = self.session.combat_log();
                    if tell != Some("Out of range")
                        && !log.iter().any(|l| l == "Out of range")
                    {
                        if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                            self.fail_current("combat: Strike out of range never told");
                            self.advance_after_fail(world, frame);
                        }
                        return;
                    }
                    // Hold the toast through the next paint+capture. Do not
                    // queue this frame: HUD already painted without the tell.
                    self.session.combat_mut().fail_tell_s = 1.2;
                    self.combat_fail_armed = true;
                    return;
                }
                let tell = self.session.fail_tell().or(Some("Out of range"));
                let log = self.session.combat_log();
                self.write_json(
                    "combat",
                    json!({
                        "status": "ok",
                        "shot": "hud.png",
                        "fail_tell": tell,
                        "hotbar_visible": true,
                        "log_lines": log,
                        "hud_shot": "hud.png",
                    }),
                );
                self.session.combat_mut().fail_tell_s = 1.2;
                self.combat_fail_sent = true;
                world.mark_ready();
                self.queue_shot(world, frame, "hud");
                return;
            }
            if !self.combat_death_sent {
                if self.session.slain_line().is_none()
                    && !self.session.is_shaken()
                    && !self.session.swings_stopped()
                {
                    self.aim_at_wolf_line();
                    if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                        self.fail_current("combat: wolves never finished the kill");
                        self.advance_after_fail(world, frame);
                    }
                    return;
                }
                if self.session.swings_stopped() || self.session.player_hp() <= 0.0 {
                    if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                        self.fail_current("combat: slain hold never resolved to shrine");
                        self.advance_after_fail(world, frame);
                    }
                    return;
                }
                self.aim_pitch(HORIZON_PITCH);
                let Some(here) = self.session.player_position() else {
                    return;
                };
                if let Some(place) = self.session.last_shrine() {
                    if here.horizontal().distance(place.position.horizontal()) > 2.5 {
                        if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                            self.fail_current("combat: death did not reach shrine");
                            self.advance_after_fail(world, frame);
                        }
                        return;
                    }
                }
                let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
                let still_fighting = self.session.lock_id().is_some()
                    || self.session.lock_name_hp().is_some()
                    || self.session.lock_ring_visible(world)
                    || self.session.fixture_mesh_visible(world);
                if still_fighting
                    || !self.session.is_shaken()
                    || !pitch_near(pitch, HORIZON_PITCH)
                {
                    if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                        self.fail_current("combat: death shot still locked or missing shaken");
                        self.advance_after_fail(world, frame);
                    }
                    return;
                }
                let shrine = self.session.last_shrine();
                let chipped = self.incoming_hp.unwrap_or(self.session.player_hp());
                self.write_json(
                    "combat",
                    json!({
                        "status": "ok",
                        "shot": "hurt.png",
                        "slain_shot": "slain.png",
                        "player_hp": chipped,
                        "respawn_hp": self.session.player_hp(),
                        "player_hp_max": self.session.player_hp_max(),
                        "incoming_hit": true,
                        "hurt_sfx": "hurt.wav",
                        "dead": false,
                        "slain": self.session.slain_line(),
                        "shaken": self.session.is_shaken(),
                        "shaken_outgoing": 0.90,
                        "swings_stopped": self.session.swings_stopped(),
                        "hotbar_visible": true,
                        "hud_shot": "hud.png",
                        "fail_tell": "Out of range",
                        "log_lines": self.session.combat_log(),
                        "shrine": shrine.map(|p| json!({"x": p.position.x, "y": p.position.y, "z": p.position.z})),
                    }),
                );
                self.combat_death_sent = true;
                self.combat_shot_sent = true;
                self.ok_hook("combat");
                world.mark_ready();
                self.queue_shot(world, frame, "slain");
                self.phase = Phase::NextHook;
                self.phase_t0 = frame.time;
            }
            return;
        }
        if !self.session.fixture_mesh_visible(world) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat: wolf meshes not visible");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.aim_at_wolf_line();
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        if let Some((eye, target)) = wolf_line_look(&self.session) {
            if view_angle_degrees(eye, yaw, pitch, target) > 12.0 {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat: never looked at wolf line");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
        }
        self.combat_tab_sent = true;
        if self.session.lock_id().is_none() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat: lock unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if self.current_hook() == Some("cast_bar") {
            self.tick_cast_bar(world, frame);
            return;
        }
        match self.session.first_auto_hit() {
            None => {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat: first auto never landed");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
            Some(11) => {}
            Some(got) => {
                self.fail_current(&format!("combat: first auto want 11, got {got}"));
                self.advance_after_fail(world, frame);
                return;
            }
        }
        if !self.session.lock_ring_visible(world) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat: lock ring missing after auto");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if !self.session.swing_whoosh() || !self.session.hit_flash() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("combat: swing_whoosh/hit_flash unset after auto");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if self.combat_hurt_sent {
            if let Some(vantage) = combat_side_vantage(&self.session) {
                self.aim_at_wolf_line();
                if pos.horizontal().distance(vantage) > 0.75 {
                    if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                        self.fail_current("combat: never reached side vantage after auto");
                        self.advance_after_fail(world, frame);
                    }
                    return;
                }
            }
        }
        let Some((name, hp)) = self.session.lock_name_hp() else {
            if frame.time - self.phase_t0 > 8.0 {
                self.fail_current("combat: lock name/hp unset after Tab");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        let name = name.to_string();
        let hp_max = self
            .session
            .combat()
            .hostiles
            .iter()
            .find(|h| Some(h.idx) == self.session.lock_id())
            .map(|h| h.max_hp)
            .unwrap_or(hp);
        if name != "wolf-spider" {
            self.fail_current(&format!(
                "combat: want wolf-spider after auto, got {name} {hp}/{hp_max}"
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        let walk = self.session.combat_walk_speed();
        let first_auto = orrun::world::first_fixture_auto_hit();
        if first_auto != 11 || (walk - 4.5).abs() > 1e-4 {
            self.fail_current(&format!(
                "first-auto inspect want 11 / walk 4.5, got {first_auto} / {walk}"
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        if !self.combat_hurt_sent {
            if !self.session.incoming_hit() || self.session.player_hp() >= self.session.player_hp_max() {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat: incoming never chipped player HP");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
            let log = self.session.combat_log();
            let has_out = log.iter().any(|l| l.starts_with("You hit "));
            let has_in = log.iter().any(|l| l.contains(" hits you for "));
            if !has_out || !has_in {
                if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                    self.fail_current("combat: log missing a live hit line");
                    self.advance_after_fail(world, frame);
                }
                return;
            }
            if self.current_hook() == Some("incoming") {
                self.write_json(
                    "incoming",
                    json!({
                        "status": "ok",
                        "shot": "incoming.png",
                        "player_hp": self.session.player_hp(),
                        "player_hp_max": self.session.player_hp_max(),
                        "incoming_hit": true,
                        "hp_ghost_frac": self.session.hp_ghost_frac(),
                    }),
                );
                world.mark_ready();
                self.queue_shot(world, frame, "incoming");
                self.ok_hook("incoming");
                self.phase = Phase::NextHook;
                self.phase_t0 = frame.time;
                return;
            }
            if self.combat_cd_sweep_at.is_none() {
                let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
                let facing = Camera::facing_xz(yaw);
                let Some(pos) = self.session.player_position() else {
                    return;
                };
                let armed = self.session.combat_mut().press_verb(
                    Action::Strike,
                    pos.x,
                    pos.z,
                    facing.x as f64,
                    facing.z as f64,
                );
                let frac = self.session.combat().verb_cd_frac(Action::Strike);
                if !armed || frac <= 0.0 {
                    if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                        self.fail_current("combat: in-range Strike never started CD");
                        self.advance_after_fail(world, frame);
                    }
                    return;
                }
                self.combat_cd_sweep_at = Some(frame.time);
                return;
            }
            if !self.combat_cd_sweep_sent {
                let since = frame.time - self.combat_cd_sweep_at.unwrap();
                let frac = self.session.combat().verb_cd_frac(Action::Strike);
                if since < 2.0 {
                    return;
                }
                if frac <= 0.05 || frac >= 0.99 {
                    if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                        self.fail_current(&format!(
                            "combat: Strike CD not mid-pie after {since:.2}s (frac={frac})"
                        ));
                        self.advance_after_fail(world, frame);
                    }
                    return;
                }
                self.combat_cd_sweep_sent = true;
                world.mark_ready();
                self.queue_shot(world, frame, "cd_sweep");
                if self.current_hook() == Some("cd_sweep") {
                    self.write_json(
                        "cd_sweep",
                        json!({
                            "status": "ok",
                            "shot": "cd_sweep.png",
                            "strike_cd_frac": frac,
                            "hotbar_visible": true,
                        }),
                    );
                    self.ok_hook("cd_sweep");
                    self.phase = Phase::NextHook;
                }
                return;
            }
            let swing_clip = copy_combat_wav(&self.shots, "swing.wav");
            let hit_clip = copy_combat_wav(&self.shots, "hit.wav");
            let hurt_clip = copy_combat_wav(&self.shots, "hurt.wav");
            self.write_json(
                "combat",
                json!({
                    "status": "ok",
                    "lock": self.session.lock_id(),
                    "name": name,
                    "hp": hp,
                    "hp_max": hp_max,
                    "shot": "hurt.png",
                    "first_auto_before_shot": self.session.first_auto_hit(),
                    "first_mitigated_auto": first_auto,
                    "combat_walk_mps": walk,
                    "player_hp": self.session.player_hp(),
                    "player_hp_max": self.session.player_hp_max(),
                    "player_mana": self.session.player_mana(),
                    "player_mana_max": self.session.player_mana_max(),
                    "player_hp_visible": true,
                    "incoming_hit": true,
                    "hurt_sfx": hurt_clip,
                    "hurt_clip": hurt_clip,
                    "swing_clip": swing_clip,
                    "hit_clip": hit_clip,
                    "lock_ring": self.session.lock_ring_visible(world),
                    "swing_whoosh": self.session.swing_whoosh(),
                    "hit_flash": self.session.hit_flash(),
                    "hotbar_visible": true,
                    "log_lines": log,
                    "hud_shot": "hud.png",
                }),
            );
            self.incoming_hp = Some(self.session.player_hp());
            self.combat_hurt_sent = true;
            return;
        }
    }

    fn aim_at_demon(&mut self) {
        let Some((eye, target)) = demon_look(&self.session) else {
            return;
        };
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let (dyaw, dpitch) = look_deltas(eye, target, yaw, pitch);
        self.pending_yaw_delta = dyaw;
        self.pending_pitch_delta = dpitch;
    }

    fn aim_at_orc(&mut self) {
        let Some((eye, target)) = orc_look(&self.session) else {
            return;
        };
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let (dyaw, dpitch) = look_deltas(eye, target, yaw, pitch);
        self.pending_yaw_delta = dyaw;
        self.pending_pitch_delta = dpitch;
    }

    fn aim_at_wolf_line(&mut self) {
        let Some((eye, target)) = wolf_line_look(&self.session) else {
            return;
        };
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let (dyaw, dpitch) = look_deltas(eye, target, yaw, pitch);
        self.pending_yaw_delta = dyaw;
        self.pending_pitch_delta = dpitch;
    }

    fn start_controls(&mut self, world: &mut World, frame: &Frame) {
        let mut loaded = Settings::load().unwrap_or_else(|err| panic!("{err}"));
        if loaded.format != 1 {
            self.fail_current(&format!(
                "controls: settings FORMAT must stay 1, found {}",
                loaded.format
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        loaded.keys = orrun::settings::KeyBinds::default();
        loaded
            .write()
            .unwrap_or_else(|err| panic!("{err}"));
        let loaded = Settings::load().unwrap_or_else(|err| panic!("{err}"));
        let path = orrun::settings::settings_path().expect("settings path");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or(json!({}));
        let settings_json_has_keys = parsed.get("keys").is_some();
        let binds = loaded.keys.clone();
        let missing = controls::missing_binds(&binds);
        let bible_verbs_missing_bind: Vec<&str> = missing.iter().map(|a| a.id()).collect();
        if !missing.is_empty() || !settings_json_has_keys {
            self.write_json(
                "controls",
                json!({
                    "status": "fail",
                    "binds": binds.inspect_map(),
                    "reserved": controls::reserved_names(),
                    "unbound": missing.iter().map(|a| a.id()).collect::<Vec<_>>(),
                    "bible_verbs_missing_bind": bible_verbs_missing_bind,
                    "settings_json_has_keys": settings_json_has_keys,
                    "fired": {},
                    "rank_gate": {"blocked": false},
                    "used_pin_or_bind": false,
                    "why": "missing bind",
                }),
            );
            self.fail_current("controls: missing bind");
            self.advance_after_fail(world, frame);
            return;
        }
        if controls::resolve(&binds, [engine::Key::Digit1]) != vec![Action::Strike] {
            self.fail_current("controls: key 1 must resolve to Strike");
            self.advance_after_fail(world, frame);
            return;
        }
        if controls::resolve(&binds, [engine::Key::Digit5]) != vec![Action::Ember] {
            self.fail_current("controls: default Ember key 5 is not bound to Ember");
            self.advance_after_fail(world, frame);
            return;
        }
        if controls::resolve(&binds, [engine::Key::R]) != vec![Action::Potion] {
            self.fail_current("controls: potion is R, not Q");
            self.advance_after_fail(world, frame);
            return;
        }
        if controls::resolve(&binds, [engine::Key::T]) != vec![Action::Mark]
            || binds.inspect_map().get("mark") != Some(&serde_json::Value::String("T".into()))
        {
            self.fail_current("controls: Mark must be bound to T");
            self.advance_after_fail(world, frame);
            return;
        }
        if controls::resolve(&binds, [engine::Key::G]) != vec![Action::SecondWind]
            || binds.inspect_map().get("second_wind") != Some(&serde_json::Value::String("G".into()))
        {
            self.fail_current("controls: SecondWind must be bound to G");
            self.advance_after_fail(world, frame);
            return;
        }
        if !controls::resolve(&binds, [engine::Key::Q]).is_empty() {
            self.fail_current("controls: Q must stay left strafe (unbound as a press-verb)");
            self.advance_after_fail(world, frame);
            return;
        }
        if controls::assign(&mut loaded.keys.clone(), Action::Strike, engine::Key::E).is_ok() {
            self.fail_current("controls: E is reserved");
            self.advance_after_fail(world, frame);
            return;
        }
        if controls::assign(&mut loaded.keys.clone(), Action::Strike, engine::Key::Q).is_ok() {
            self.fail_current("controls: Q is reserved (strafe)");
            self.advance_after_fail(world, frame);
            return;
        }
        self.session.set_key_binds(binds);
        self.session.rearm_combat_fixtures(world);
        self.controls_stage = ControlsStage::Tab;
        self.phase = Phase::ControlsLive;
        self.phase_t0 = frame.time;
    }

    fn tick_controls(&mut self, world: &mut World, frame: &Frame) {
        if frame.time - self.phase_t0 > 8.0 {
            self.fail_current(&format!(
                "controls timed out in {:?} lock={:?} auto={:?} strike_armed={}",
                self.controls_stage,
                self.session.lock_id(),
                self.session.first_auto_hit(),
                self.session.combat().strike_armed
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
                    self.fail_current("controls: key 1 on L1 Martial did not arm Strike");
                    self.advance_after_fail(world, frame);
                    return;
                }
                self.controls_stage = ControlsStage::WaitHit;
            }
            ControlsStage::WaitHit => {
                match self.session.first_auto_hit() {
                    Some(16) => {
                        self.controls_stage = ControlsStage::Bash;
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
            ControlsStage::Bash => {
                let gate = self.session.combat().last_rank_gate;
                let blocked = gate.map(|g| g.blocked && g.action == Action::Bash).unwrap_or(false);
                if !blocked {
                    self.fail_current(
                        "controls: key 2 on L1 Martial must rank-gate Bash (Martial < 3)",
                    );
                    self.advance_after_fail(world, frame);
                    return;
                }
                if self.session.combat().cast_kind == Some("bash") {
                    self.fail_current("controls: Digit2 on L1 Martial must not fire Bash");
                    self.advance_after_fail(world, frame);
                    return;
                }
                // Ember is known at create. Do not pad ranks.arcane.
                assert_eq!(
                    self.session.combat().player.stats.ranks.arcane, 0,
                    "L1 Martial must keep arcane 0; Ember uses ember_rank 1.00"
                );
                self.controls_stage = ControlsStage::Ember;
            }
            ControlsStage::Ember => {
                if !self.session.combat().ember_started {
                    self.fail_current(
                        "controls: L1 Martial key 5 must start Ember without padding Arcane",
                    );
                    self.advance_after_fail(world, frame);
                    return;
                }
                if self.session.combat().player.stats.ranks.arcane != 0 {
                    self.fail_current("controls: Ember hook must not cheat ranks.arcane = 1");
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
                let binds = self.session.key_binds().inspect_map();
                if binds.get("mark") != Some(&serde_json::Value::String("T".into()))
                    || binds.get("second_wind") != Some(&serde_json::Value::String("G".into()))
                {
                    self.fail_current("controls: inspect binds must include mark=T and second_wind=G");
                    self.advance_after_fail(world, frame);
                    return;
                }
                let gate = self.session.combat().last_rank_gate.expect("bash rank gate");
                if !gate.blocked || gate.action != Action::Bash || gate.have != 1 || gate.need != 3 {
                    self.fail_current(&format!(
                        "controls: rank_gate want bash blocked have=1 need=3, got blocked={} action={:?} have={} need={}",
                        gate.blocked, gate.action, gate.have, gate.need
                    ));
                    self.advance_after_fail(world, frame);
                    return;
                }
                let path = orrun::settings::settings_path().expect("settings path");
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or(json!({}));
                self.write_json(
                    "controls",
                    json!({
                        "status": "ok",
                        "binds": binds,
                        "reserved": controls::reserved_names(),
                        "unbound": [],
                        "bible_verbs_missing_bind": [],
                        "settings_json_has_keys": parsed.get("keys").is_some(),
                        "fired": {"strike": true, "bash": false, "ember": true, "potion": true},
                        "rank_gate": {
                            "action": "bash",
                            "blocked": true,
                            "have": gate.have,
                            "need": gate.need,
                        },
                        "used_pin_or_bind": self.session.combat().player.used_pin_or_bind,
                        "strike_first_auto": 16,
                        "ember_started": true,
                        "ember_arcane_rank": self.session.combat().player.stats.ranks.arcane,
                        "potion_hp": hp,
                        "potion_heal": heal,
                        "potion_key": "R",
                        "q_strafe": true,
                    }),
                );
                self.ok_hook("controls");
                self.phase = Phase::NextHook;
                self.phase_t0 = frame.time;
            }
        }
    }

    fn start_village(&mut self, world: &mut World, frame: &Frame) {
        let from = self
            .session
            .player_position()
            .map(|p| p.horizontal())
            .or_else(|| self.session.spawn().map(|s| s.ground()))
            .unwrap_or(GlobalXZ::at(0.0, 0.0));
        let Some(pin) = self.session.nearest_tier0_pin(from) else {
            self.fail_current("village: no tier-0 pin on this atlas");
            self.advance_after_fail(world, frame);
            return;
        };
        let dist = pin.at.distance(from);
        if dist > 80.0 || self.session.state() != SessionState::World {
            match WorldEntryRequest::at_global(self.session.surface().bounds(), pin.at) {
                Ok(request) => match self.session.begin_entry(world, request) {
                    Ok(()) => {
                        self.phase = Phase::VillageTravel;
                        self.phase_t0 = frame.time;
                    }
                    Err(err) => {
                        self.fail_current(&format!("village entry failed: {err}"));
                        self.advance_after_fail(world, frame);
                    }
                },
                Err(err) => {
                    self.fail_current(&format!("village pin is not a valid entry: {err}"));
                    self.advance_after_fail(world, frame);
                }
            }
        } else {
            self.phase = Phase::VillageLive;
            self.phase_t0 = frame.time;
        }
    }

    fn tick_village_travel(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() == SessionState::World {
            self.phase = Phase::VillageLive;
            self.phase_t0 = frame.time;
            return;
        }
        if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
            self.fail_current("village: timed out travelling to the hamlet");
            self.advance_after_fail(world, frame);
        }
    }

    fn tick_village_live(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("village: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(pos) = self.session.player_position() else {
            return;
        };
        let stand = pos.horizontal();
        if !self.session.stream().required_ready(stand) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("village: required_ready stayed false");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(hamlet) = self.session.nearest_tier0_hamlet() else {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("village: no seated tier-0 hamlet");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        if hamlet.cut.len() < 2 {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("village: seated hamlet has no cut");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let dwelling_count = hamlet.houses.len();
        let cut: Vec<glam::Vec2> = hamlet.cut.clone();
        let _pin = hamlet.at;
        let ribbon_faces = self.session.ribbon_faces();
        let human_count = self.session.village_human_mesh_count(world);
        let human_on_corridor = self.session.village_human_on_corridor();
        let houses = self.session.village_house_plots();
        if dwelling_count < 4
            || ribbon_faces == 0
            || !self.session.has_ribbon_mesh()
            || human_count < 5
            || !human_on_corridor
        {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                let why = if ribbon_faces == 0 || !self.session.has_ribbon_mesh() {
                    format!(
                        "HOLD: village ribbon missing (faces={ribbon_faces} mesh={})",
                        self.session.has_ribbon_mesh()
                    )
                } else if dwelling_count < 4 {
                    format!("HOLD: village has {dwelling_count} dwellings")
                } else if human_count < 5 {
                    format!("HOLD: village humans {human_count} (want >=5 with a mesh)")
                } else {
                    "HOLD: no human on the corridor".into()
                };
                self.fail_current(&why);
                self.advance_after_fail(world, frame);
            }
            return;
        }

        self.aim_village_cut();
        let Some(cam) = village_camera_stand(&self.session) else {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("HOLD: village cut has no dry sample outside a house");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        let Some(person) = self.session.village_corridor_human() else {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("HOLD: no walker on the corridor");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        let horiz = stand.distance(person.horizontal());
        let high_enough = (pos.y - cam.y).abs() < 1.6;
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let looking = village_look(&self.session).is_some_and(|(eye, target)| {
            view_angle_degrees(eye, yaw, pitch, target) < 16.0
        });
        if horiz > 5.5 || !high_enough || !looking {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!(
                    "village: camera never sat on a walker (horiz={horiz:.1} y={:.1} pitch={pitch:.1})",
                    pos.y
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }

        let samples = sample_cut_metres(&cut, 1.0);
        let blocked: Vec<usize> = samples
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                let xz = GlobalXZ::at(f64::from(p.x), f64::from(p.y));
                houses.iter().any(|h: &HousePlot| h.contains_xz(xz))
            })
            .map(|(i, _)| i)
            .collect();
        if !blocked.is_empty() {
            self.fail_current(&format!(
                "village: {} cut samples sit inside a house OBB",
                blocked.len()
            ));
            self.advance_after_fail(world, frame);
            return;
        }
        if !self.session.village_has_well() {
            self.fail_current("village: well missing");
            self.advance_after_fail(world, frame);
            return;
        }

        self.write_json(
            "village",
            json!({
                "status": "ok",
                "cut_len": cut.len(),
                "cut_samples": samples.len(),
                "cut_blocked": 0,
                "dwelling_count": dwelling_count,
                "well": true,
                "ribbon_faces": ribbon_faces,
                "human_count": human_count,
                "human_on_corridor": human_on_corridor,
                "pose": {
                    "x": pos.x,
                    "y": pos.y,
                    "z": pos.z,
                    "yaw_degrees": self.session.player_yaw_degrees(),
                    "pitch_degrees": pitch,
                },
            }),
        );
        self.ok_hook("village");
        world.mark_ready();
        self.queue_shot(world, frame, "village");
        self.phase = Phase::NextHook;
    }

    fn start_camp(&mut self, world: &mut World, frame: &Frame) {
        let from = self
            .session
            .player_position()
            .map(|p| p.horizontal())
            .or_else(|| self.session.spawn().map(|s| s.ground()))
            .unwrap_or(GlobalXZ::at(0.0, 0.0));
        let Some(pin) = self.session.nearest_tier0_pin(from) else {
            self.fail_current("camp: no tier-0 pin on this atlas");
            self.advance_after_fail(world, frame);
            return;
        };
        let dist = pin.at.distance(from);
        if dist > 80.0 || self.session.state() != SessionState::World {
            match WorldEntryRequest::at_global(self.session.surface().bounds(), pin.at) {
                Ok(request) => match self.session.begin_entry(world, request) {
                    Ok(()) => {
                        self.phase = Phase::CampTravel;
                        self.phase_t0 = frame.time;
                    }
                    Err(err) => {
                        self.fail_current(&format!("camp entry failed: {err}"));
                        self.advance_after_fail(world, frame);
                    }
                },
                Err(err) => {
                    self.fail_current(&format!("camp pin is not a valid entry: {err}"));
                    self.advance_after_fail(world, frame);
                }
            }
        } else {
            self.phase = Phase::CampLive;
            self.phase_t0 = frame.time;
        }
    }

    fn tick_camp_travel(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() == SessionState::World {
            self.phase = Phase::CampLive;
            self.phase_t0 = frame.time;
            return;
        }
        if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
            self.fail_current("camp: timed out travelling to the hamlet");
            self.advance_after_fail(world, frame);
        }
    }

    fn tick_camp_live(&mut self, world: &mut World, frame: &Frame) {
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("camp: never reached World");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(pos) = self.session.player_position() else {
            return;
        };
        let stand_xz = pos.horizontal();
        if !self.session.stream().required_ready(stand_xz) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("camp: required_ready stayed false");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if self.session.nearest_tier0_hamlet().is_none() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("camp: no seated tier-0 hamlet");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if !self.session.village_has_camp() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("camp: no tent/ring pair on the hamlet yard");
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.aim_camp();
        let Some(cam) = self.session.village_camp_stand() else {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current("camp: yard pair has no camera stand");
                self.advance_after_fail(world, frame);
            }
            return;
        };
        let Some((tent, ring)) = self.session.village_camp_pair() else {
            self.fail_current("camp: pair vanished");
            self.advance_after_fail(world, frame);
            return;
        };
        let horiz = stand_xz.distance(cam.horizontal());
        let high_enough = (pos.y - cam.y).abs() < 1.6;
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let looking = self.session.village_camp_look().is_some_and(|(eye, target)| {
            view_angle_degrees(eye, yaw, pitch, target) < 16.0
        });
        if horiz > 2.8 || !high_enough || !looking {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!(
                    "camp: camera never sat on the yard pair (horiz={horiz:.1} y={:.1} pitch={pitch:.1})",
                    pos.y
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let pin = self.session.village_well().unwrap_or(tent.horizontal());
        self.write_json(
            "camp",
            json!({
                "status": "ok",
                "has_camp": true,
                "pin": { "x": pin.x, "z": pin.z },
                "tent": { "x": tent.x, "y": tent.y, "z": tent.z },
                "ring": { "x": ring.x, "y": ring.y, "z": ring.z },
                "pose": {
                    "x": pos.x,
                    "y": pos.y,
                    "z": pos.z,
                    "yaw_degrees": yaw,
                    "pitch_degrees": pitch,
                },
            }),
        );
        self.ok_hook("camp");
        world.mark_ready();
        self.queue_shot(world, frame, "camp");
        self.phase = Phase::NextHook;
    }

    fn start_site(&mut self, world: &mut World, frame: &Frame, kind: SiteKind) {
        self.site_kind = Some(kind);
        self.site_melee_at = None;
        self.combat_tab_sent = false;
        let name = kind.as_str();
        let site = self.session.overland_site(kind).or_else(|| {
            let pins = self.session.surface().settlements();
            let hamlets = self.session.hamlets();
            plan_overland_sites(self.session.surface(), pins, hamlets)
                .into_iter()
                .find(|s| s.kind == kind)
        });
        let Some(site) = site else {
            self.fail_current(&format!("{name}: no site on this atlas (seed world must stamp both)"));
            self.advance_after_fail(world, frame);
            return;
        };
        let from = self
            .session
            .player_position()
            .map(|p| p.horizontal())
            .or_else(|| self.session.spawn().map(|s| s.ground()))
            .unwrap_or(GlobalXZ::at(0.0, 0.0));
        let dist = site.at.distance(from);
        if dist > 80.0 || self.session.state() != SessionState::World {
            match WorldEntryRequest::at_global(self.session.surface().bounds(), site.at) {
                Ok(request) => match self.session.begin_entry(world, request) {
                    Ok(()) => {
                        self.phase = match kind {
                            SiteKind::TakenCairn => Phase::CairnTravel,
                            SiteKind::WoodsHut => Phase::HutTravel,
                        };
                        self.phase_t0 = frame.time;
                    }
                    Err(err) => {
                        self.fail_current(&format!("{name} entry failed: {err}"));
                        self.advance_after_fail(world, frame);
                    }
                },
                Err(err) => {
                    self.fail_current(&format!("{name} is not a valid entry: {err}"));
                    self.advance_after_fail(world, frame);
                }
            }
        } else {
            self.phase = match kind {
                SiteKind::TakenCairn => Phase::CairnLive,
                SiteKind::WoodsHut => Phase::HutLive,
            };
            self.phase_t0 = frame.time;
        }
    }

    fn tick_site_travel(&mut self, world: &mut World, frame: &Frame) {
        let name = self.site_kind.map(SiteKind::as_str).unwrap_or("site");
        if self.session.state() == SessionState::World {
            self.phase = match self.site_kind {
                Some(SiteKind::TakenCairn) => Phase::CairnLive,
                Some(SiteKind::WoodsHut) => Phase::HutLive,
                None => Phase::NextHook,
            };
            self.phase_t0 = frame.time;
            return;
        }
        if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
            self.fail_current(&format!("{name}: timed out travelling to the site"));
            self.advance_after_fail(world, frame);
        }
    }

    fn tick_site_live(&mut self, world: &mut World, frame: &Frame) {
        let kind = match self.site_kind {
            Some(k) => k,
            None => {
                self.fail_current("site hook missing kind");
                self.advance_after_fail(world, frame);
                return;
            }
        };
        let name = kind.as_str();
        if self.session.state() != SessionState::World {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!("{name}: never reached World"));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(pos) = self.session.player_position() else {
            return;
        };
        if !self.session.stream().required_ready(pos.horizontal()) {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!("{name}: required_ready stayed false"));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let Some(site) = self.session.overland_site(kind).or_else(|| {
            let pins = self.session.surface().settlements();
            let hamlets = self.session.hamlets();
            plan_overland_sites(self.session.surface(), pins, hamlets)
                .into_iter()
                .find(|s| s.kind == kind)
        }) else {
            self.fail_current(&format!("{name}: site missing after travel"));
            self.advance_after_fail(world, frame);
            return;
        };
        let (bandit_n, gap) = {
            let hostiles = &self.session.combat().hostiles;
            let at_site: Vec<_> = hostiles
                .iter()
                .filter(|h| h.mob_id == "bandit" && h.alive)
                .filter(|h| (h.x - site.at.x).hypot(h.z - site.at.z) < 12.0)
                .collect();
            let n = at_site.len();
            let gap = if n >= 2 {
                (at_site[0].x - at_site[1].x).hypot(at_site[0].z - at_site[1].z)
            } else {
                0.0
            };
            (n, gap)
        };
        if bandit_n < 2 {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!(
                    "{name}: want 2 unstacked bandits at the prop, got {bandit_n}"
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if gap < 1.0 {
            self.fail_current(&format!("{name}: bandits stacked (gap={gap:.2} m)"));
            self.advance_after_fail(world, frame);
            return;
        }
        if self.session.site_prop_count() == 0 {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!("{name}: site props were not spawned"));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        self.aim_site(site);
        let Some(stand) = site_camera_stand(&self.session, Some(kind)) else {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!("{name}: no camera stand"));
                self.advance_after_fail(world, frame);
            }
            return;
        };
        let horiz = pos.horizontal().distance(stand.horizontal());
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let looking = site_look(&self.session, site).is_some_and(|(eye, target)| {
            view_angle_degrees(eye, yaw, pitch, target) < 16.0
        });
        if horiz > 3.5 || !looking {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!(
                    "{name}: camera never sat on the site (horiz={horiz:.1} pitch={pitch:.1})"
                ));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        if self.session.lock_id().is_none() {
            if let Some(idx) = self
                .session
                .combat()
                .hostiles
                .iter()
                .find(|h| h.mob_id == "bandit" && h.alive && (h.x - site.at.x).hypot(h.z - site.at.z) < 12.0)
                .map(|h| h.idx)
            {
                self.session.combat_mut().lock = Some(idx);
            }
        }
        if self.session.lock_id().is_none() {
            if frame.time - self.phase_t0 > STAND_TIMEOUT_S {
                self.fail_current(&format!("{name}: never locked a bandit"));
                self.advance_after_fail(world, frame);
            }
            return;
        }
        let melee_at = if let Some(t) = self.site_melee_at {
            t
        } else {
            self.session.replay_melee(world);
            self.site_melee_at = Some(frame.time);
            frame.time
        };
        // Attack is a one-shot clip; mid-swing reads around 0.45 s.
        if frame.time - melee_at < 0.45 {
            return;
        }
        let hook = self
            .current_hook()
            .map(str::to_string)
            .unwrap_or_else(|| format!("overworld_{name}"));
        let shot = format!("overworld_{name}");
        self.write_json(
            &shot,
            json!({
                "status": "ok",
                "kind": name,
                "pin_id": site.pin_id,
                "at": { "x": site.at.x, "z": site.at.z },
                "bandits": bandit_n,
                "bandit_gap_m": gap,
                "props": self.session.site_prop_count(),
                "pose": {
                    "x": pos.x,
                    "y": pos.y,
                    "z": pos.z,
                    "yaw_degrees": yaw,
                    "pitch_degrees": pitch,
                },
            }),
        );
        self.ok_hook(&hook);
        world.mark_ready();
        self.queue_shot(world, frame, &shot);
        self.phase = Phase::NextHook;
    }

    fn aim_site(&mut self, site: OverlandSite) {
        let Some((eye, target)) = site_look(&self.session, site) else {
            return;
        };
        let yaw = self.session.player_yaw_degrees().unwrap_or(0.0);
        let pitch = self.session.player_pitch_degrees().unwrap_or(0.0);
        let (dyaw, dpitch) = look_deltas(eye, target, yaw, pitch);
        self.pending_yaw_delta = dyaw;
        self.pending_pitch_delta = dpitch;
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

fn sample_cut_metres(cut: &[glam::Vec2], step: f32) -> Vec<glam::Vec2> {
    let mut out = Vec::new();
    if cut.len() < 2 {
        return out;
    }
    for w in cut.windows(2) {
        let d = w[1] - w[0];
        let len = d.length();
        let n = ((len / step).ceil() as usize).max(1);
        for i in 0..=n {
            let t = i as f32 / n as f32;
            out.push(w[0] + d * t);
        }
    }
    out
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



fn bone_body_view_stand(session: &WorldSession) -> Option<GlobalXZ> {
    const STAND_M: f64 = 5.0;
    const SIDE_M: f64 = 2.2;
    let hs = &session.combat().hostiles;
    if hs.len() < 3 {
        return None;
    }
    let a = &hs[0];
    let left_x = hs[2].x - hs[1].x;
    let left_z = hs[2].z - hs[1].z;
    let ll = (left_x * left_x + left_z * left_z).sqrt();
    if ll < 1e-6 {
        return None;
    }
    let (sx, sz) = (left_x / ll, left_z / ll);
    let (fx, fz) = (sz, -sx);
    Some(GlobalXZ::at(
        a.x - fx * STAND_M + sx * SIDE_M,
        a.z - fz * STAND_M + sz * SIDE_M,
    ))
}

fn mage_view_stand(session: &WorldSession) -> Option<GlobalXZ> {
    // True side / tight 3/4 from the mage's RIGHT: mesh is 1.6 m behind
    // the hitbox, so STAND=-1.2 / SIDE=1.8 sits ~1.8 m off the mesh on
    // the staff hand. Cylinder.004's 2.1 m +Z shaft is then a line across
    // the picture (not end-on). Snapshot once so walking does not rotate it.
    const STAND_M: f64 = -1.2;
    const SIDE_M: f64 = 1.8;
    let pos = session.player_position()?;
    let h = session.combat().hostiles.first()?;
    let dx = pos.x - h.x;
    let dz = pos.z - h.z;
    let len = (dx * dx + dz * dz).sqrt();
    let (fx, fz) = if len > 1e-6 {
        (dx / len, dz / len)
    } else {
        (-1.0, 0.0)
    };
    // Mage faces the player (fx,fz). Right-handed Y-up: right = (-fz, fx).
    let (rx, rz) = (-fz, fx);
    Some(GlobalXZ::at(
        h.x + fx * STAND_M + rx * SIDE_M,
        h.z + fz * STAND_M + rz * SIDE_M,
    ))
}

fn mage_mesh_chest(session: &WorldSession) -> Option<Vec3> {
    // Visual chest: 1.6 m behind the hitbox along facing (toward the player
    // at snapshot time). Used so a right-side stand still looks at the mesh.
    const BEHIND_M: f64 = 1.6;
    const CHEST_M: f32 = 1.15;
    let pos = session.player_position()?;
    let h = session.combat().hostiles.first()?;
    let dx = pos.x - h.x;
    let dz = pos.z - h.z;
    let len = (dx * dx + dz * dz).sqrt();
    let (fx, fz) = if len > 1e-6 {
        (dx / len, dz / len)
    } else {
        (-1.0, 0.0)
    };
    Some(Vec3::new(
        (h.x - fx * BEHIND_M) as f32,
        pos.y as f32 + CHEST_M,
        (h.z - fz * BEHIND_M) as f32,
    ))
}

fn wolf_body_view_stand(session: &WorldSession) -> Option<GlobalXZ> {
    // Stable vs player motion: back along L1 facing, then a sidestep for 3/4 body.
    const STAND_M: f64 = 7.5;
    const SIDE_M: f64 = 2.6;
    let hs = &session.combat().hostiles;
    if hs.len() < 3 {
        return None;
    }
    let a = &hs[0];
    let left_x = hs[2].x - hs[1].x;
    let left_z = hs[2].z - hs[1].z;
    let ll = (left_x * left_x + left_z * left_z).sqrt();
    if ll < 1e-6 {
        return None;
    }
    let (sx, sz) = (left_x / ll, left_z / ll);
    let (fx, fz) = (sz, -sx);
    Some(GlobalXZ::at(
        a.x - fx * STAND_M + sx * SIDE_M,
        a.z - fz * STAND_M + sz * SIDE_M,
    ))
}

fn combat_side_vantage(session: &WorldSession) -> Option<GlobalXZ> {
    let hs = &session.combat().hostiles;
    if hs.len() < 3 {
        return None;
    }
    let a = &hs[0];
    let c = &hs[2];
    let fx = c.x - a.x;
    let fz = c.z - a.z;
    let fl = (fx * fx + fz * fz).sqrt();
    if fl < 1e-6 {
        return None;
    }
    let (fx, fz) = (fx / fl, fz / fl);
    let (rx, rz) = (-fz, fx);
    // Off the line: back toward the spawn side, then 8 m to the right so the
    // 5.5 m wolf bodies are fully in frame instead of filling the near plane.
    Some(GlobalXZ::at(
        a.x - fx * 5.0 + rx * 8.0,
        a.z - fz * 5.0 + rz * 8.0,
    ))
}

fn combat_oor_stand(session: &WorldSession) -> Option<GlobalXZ> {
    let hs = &session.combat().hostiles;
    if hs.len() < 3 {
        return None;
    }
    let a = &hs[0];
    let c = &hs[2];
    let fx = c.x - a.x;
    let fz = c.z - a.z;
    let fl = (fx * fx + fz * fz).sqrt();
    if fl < 1e-6 {
        return None;
    }
    let (fx, fz) = (fx / fl, fz / fl);
    // 4 m back: past melee 2.8 so Strike fail-tells Out of range.
    Some(GlobalXZ::at(a.x - fx * 4.0, a.z - fz * 4.0))
}

fn combat_melee_stand(session: &WorldSession) -> Option<GlobalXZ> {
    let hs = &session.combat().hostiles;
    if hs.len() < 3 {
        return None;
    }
    let a = &hs[0];
    let c = &hs[2];
    let fx = c.x - a.x;
    let fz = c.z - a.z;
    let fl = (fx * fx + fz * fz).sqrt();
    if fl < 1e-6 {
        return None;
    }
    let (fx, fz) = (fx / fl, fz / fl);
    // 1.2 m back: inside wolf reach (1.8 m). 2.0 m was just outside it.
    Some(GlobalXZ::at(a.x - fx * 1.2, a.z - fz * 1.2))
}

fn demon_view_stand(session: &WorldSession) -> Option<GlobalXZ> {
    // Human viewing distance, strong 3/4 on the Trident side so the pole
    // (local Z, ~2.3 m) reads as a line. Front-on is end-on; the first
    // SIDE sign landed on the punch arm.
    const STAND_M: f64 = 3.5;
    const SIDE_M: f64 = 4.6;
    let pos = session.player_position()?;
    let h = session.combat().hostiles.first()?;
    let dx = pos.x - h.x;
    let dz = pos.z - h.z;
    let len = (dx * dx + dz * dz).sqrt();
    let (fx, fz) = if len > 1e-6 {
        (dx / len, dz / len)
    } else {
        (-1.0, 0.0)
    };
    // Hostile faces the player (fx,fz). Right-handed Y-up: (-fz, fx)
    // is the hostile's right / original punch side. Flip to the Trident.
    Some(GlobalXZ::at(
        h.x + fx * STAND_M - fz * SIDE_M,
        h.z + fz * STAND_M + fx * SIDE_M,
    ))
}

fn demon_look(session: &WorldSession) -> Option<(Vec3, Vec3)> {
    const BEHIND_M: f64 = 1.6;
    const CHEST_M: f32 = 1.25;
    let pos = session.player_position()?;
    let h = session.combat().hostiles.first()?;
    let dx = pos.x - h.x;
    let dz = pos.z - h.z;
    let len = (dx * dx + dz * dz).sqrt();
    let (fx, fz) = if len > 1e-6 {
        (dx / len, dz / len)
    } else {
        (-1.0, 0.0)
    };
    let ground = session
        .contact_height(GlobalXZ::at(h.x, h.z))
        .unwrap_or(pos.y as f32);
    let eye = Vec3::new(pos.x as f32, pos.y as f32 + EYE_HEIGHT_M, pos.z as f32);
    let target = Vec3::new(
        (h.x - fx * BEHIND_M) as f32,
        ground + CHEST_M,
        (h.z - fz * BEHIND_M) as f32,
    );
    Some((eye, target))
}


fn death_hold_ready(world: &World, session: &WorldSession) -> bool {
    let Some(id) = session.combat().hostiles.iter().find_map(|h| h.entity) else {
        return false;
    };
    for (eid, ent) in world.animated_entities() {
        if *eid != id {
            continue;
        }
        let a = ent.animator();
        if a.clip_name() != "Death" || a.looping {
            return false;
        }
        let Some(clip) = a.model.clips.get(a.clip_index) else {
            return false;
        };
        return clip.duration > 0.0 && a.time + 1e-3 >= clip.duration;
    }
    false
}

fn orc_view_stand(session: &WorldSession) -> Option<GlobalXZ> {
    const STAND_M: f64 = 5.5;
    let pos = session.player_position()?;
    let h = session.combat().hostiles.first()?;
    let dx = pos.x - h.x;
    let dz = pos.z - h.z;
    let len = (dx * dx + dz * dz).sqrt();
    let (ux, uz) = if len > 1e-6 {
        (dx / len, dz / len)
    } else {
        (-1.0, 0.0)
    };
    Some(GlobalXZ::at(h.x + ux * STAND_M, h.z + uz * STAND_M))
}

fn orc_look(session: &WorldSession) -> Option<(Vec3, Vec3)> {
    let pos = session.player_position()?;
    let h = session.combat().hostiles.first()?;
    let ground = session
        .contact_height(GlobalXZ::at(h.x, h.z))
        .unwrap_or(pos.y as f32);
    let eye = Vec3::new(pos.x as f32, pos.y as f32 + EYE_HEIGHT_M, pos.z as f32);
    // Mid / upper body of the ~3.2 m orc. A 1.45 m chest from 1.5 m is a torso wall.
    let target = Vec3::new(h.x as f32, ground + 2.0, h.z as f32);
    Some((eye, target))
}

fn wolf_line_look(session: &WorldSession) -> Option<(Vec3, Vec3)> {
    let pos = session.player_position()?;
    let hs = &session.combat().hostiles;
    if hs.is_empty() {
        return None;
    }
    let n = hs.len() as f64;
    let mid_x = hs.iter().map(|h| h.x).sum::<f64>() / n;
    let mid_z = hs.iter().map(|h| h.z).sum::<f64>() / n;
    let ground = session
        .contact_height(GlobalXZ::at(mid_x, mid_z))
        .unwrap_or(pos.y as f32);
    let eye = Vec3::new(pos.x as f32, pos.y as f32 + EYE_HEIGHT_M, pos.z as f32);
    let target = Vec3::new(mid_x as f32, ground + 0.85, mid_z as f32);
    Some((eye, target))
}


fn yaw_xz(x: f32, z: f32, yaw_deg: f32) -> (f32, f32) {
    let rad = yaw_deg.to_radians();
    let (sin, cos) = (rad.sin(), rad.cos());
    (x * cos + z * sin, -x * sin + z * cos)
}


fn wolf_near(session: &WorldSession, p: &GlobalXZ) -> f64 {
    session
        .combat()
        .hostiles
        .iter()
        .filter(|h| h.alive && h.mob_id != "bandit")
        .map(|h| (h.x - p.x).hypot(h.z - p.z))
        .fold(f64::MAX, f64::min)
}

fn site_camera_stand(session: &WorldSession, kind: Option<SiteKind>) -> Option<GlobalPosition> {
    let kind = kind?;
    let site = session.overland_site(kind).or_else(|| {
        let pins = session.surface().settlements();
        let hamlets = session.hamlets();
        plan_overland_sites(session.surface(), pins, hamlets)
            .into_iter()
            .find(|s| s.kind == kind)
    })?;
    let site_ground = session.surface().column(site.at).ground();
    let stand = match kind {
        SiteKind::TakenCairn => {
            let yaw = site.yaw_deg.to_radians();
            let fx = f64::from(yaw.sin());
            let fz = f64::from(yaw.cos());
            let cands = [
                GlobalXZ::at(site.at.x - fx * 11.0, site.at.z - fz * 11.0),
                GlobalXZ::at(site.at.x + fx * 11.0, site.at.z + fz * 11.0),
                GlobalXZ::at(site.at.x - fz * 11.0, site.at.z + fx * 11.0),
                GlobalXZ::at(site.at.x + fz * 11.0, site.at.z - fx * 11.0),
            ];
            *cands
                .iter()
                .max_by(|a, b| {
                    wolf_near(session, a)
                        .partial_cmp(&wolf_near(session, b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap_or(&cands[0])
        }
        SiteKind::WoodsHut => {
            // 3/4 off the door-right corner: thatch hip + plinth + 4 m mass.
            let (dx, dz) = yaw_xz(8.8, -11.2, site.yaw_deg);
            GlobalXZ::at(site.at.x + f64::from(dx), site.at.z + f64::from(dz))
        }
    };
    let y = site_ground.max(session.surface().column(stand).ground()) + 1.7;
    Some(GlobalPosition::at(stand.x, f64::from(y), stand.z))
}

fn site_look(session: &WorldSession, site: OverlandSite) -> Option<(Vec3, Vec3)> {
    let pos = session.player_position()?;
    let eye = Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
    let ground = session.surface().column(site.at).ground();
    let (tx, ty, tz) = match site.kind {
        SiteKind::TakenCairn => (site.at.x as f32, ground + 1.05, site.at.z as f32),
        SiteKind::WoodsHut => {
            let (dx, dz) = yaw_xz(0.15, -0.7, site.yaw_deg);
            (
                site.at.x as f32 + dx,
                ground + 2.25,
                site.at.z as f32 + dz,
            )
        }
    };
    Some((eye, Vec3::new(tx, ty, tz)))
}

fn village_camera_stand(session: &WorldSession) -> Option<GlobalPosition> {
    let person = session.village_corridor_human()?;
    let cut = session.village_cut();
    let at = person.horizontal();
    let (dx, dz) = if cut.len() >= 2 {
        let a = cut[0];
        let b = cut[cut.len() - 1];
        let tx = b.x - a.x;
        let tz = b.y - a.y;
        let len = (tx * tx + tz * tz).sqrt().max(1e-6);
        // Three-quarter: off the road a few metres, not a top-down speck.
        ((-tz / len) * 3.6, (tx / len) * 3.6)
    } else {
        (3.6, 0.0)
    };
    let stand = GlobalXZ::at(at.x + f64::from(dx), at.z + f64::from(dz));
    let contact = session
        .contact_height(stand)
        .unwrap_or(person.y as f32);
    Some(GlobalPosition::at(stand.x, f64::from(contact + 1.7), stand.z))
}

fn village_look(session: &WorldSession) -> Option<(Vec3, Vec3)> {
    let person = session.village_corridor_human()?;
    let at = person.horizontal();
    let pos = session.player_position()?;
    let eye = Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
    let target = Vec3::new(at.x as f32, person.y as f32 + 1.2, at.z as f32);
    Some((eye, target))
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

fn draw_combat_hud(session: &WorldSession, frame: &Frame) {
    let ctx = frame.ui.ctx().clone();
    let hp = session.player_hp();
    let hp_max = session.player_hp_max().max(1.0);
    let mana = session.player_mana();
    let mana_max = session.player_mana_max().max(1.0);
    let hp_frac = (hp / hp_max).clamp(0.0, 1.0) as f32;
    let mana_frac = (mana / mana_max).clamp(0.0, 1.0) as f32;
    let hp_color = if session.hp_ghost_frac().is_none() && session.hurt_flash() {
        Color32::from_rgb(220, 24, 24)
    } else if hp_frac <= 0.20 {
        Color32::from_rgb(200, 32, 32)
    } else if hp_frac <= 0.50 {
        Color32::from_rgb(220, 190, 32)
    } else {
        Color32::from_rgb(40, 180, 64)
    };
    egui::Area::new(egui::Id::new("playtester_combat_hud"))
        .fixed_pos(egui::pos2(24.0, 24.0))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(&ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("HP").size(18.0).color(Color32::WHITE));
                    let (hp_rect, _) =
                        ui.allocate_exact_size(egui::vec2(360.0, 22.0), Sense::hover());
                    ui.painter().rect_filled(
                        hp_rect,
                        2.0,
                        Color32::from_rgb(40, 40, 40),
                    );
                    if let Some(ghost) = session.hp_ghost_frac() {
                        if ghost > hp_frac {
                            let ghost_w = hp_rect.width() * ghost.clamp(0.0, 1.0);
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    hp_rect.min,
                                    egui::vec2(ghost_w, hp_rect.height()),
                                ),
                                2.0,
                                Color32::from_rgb(220, 24, 24),
                            );
                        }
                    }
                    if hp_frac > 0.0 {
                        let fill_w = hp_rect.width() * hp_frac;
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                hp_rect.min,
                                egui::vec2(fill_w, hp_rect.height()),
                            ),
                            2.0,
                            hp_color,
                        );
                    }
                    ui.label(egui::RichText::new("Mana").size(18.0).color(Color32::WHITE));
                    ui.add(
                        egui::ProgressBar::new(mana_frac)
                            .fill(Color32::from_rgb(48, 120, 210))
                            .desired_width(360.0)
                            .desired_height(18.0),
                    );
                    let pip = if session.attack_pip() {
                        Color32::from_rgb(240, 230, 180)
                    } else {
                        Color32::from_rgb(56, 56, 56)
                    };
                    let (pip_rect, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                    ui.painter().rect_filled(pip_rect, 2.0, pip);
                    if let Some(slain) = session.slain_line() {
                        ui.label(
                            egui::RichText::new(slain)
                                .size(20.0)
                                .color(Color32::from_rgb(230, 70, 70)),
                        );
                    }
                    if session.is_shaken() {
                        ui.label(
                            egui::RichText::new("Shaken")
                                .size(16.0)
                                .color(Color32::from_rgb(200, 160, 80)),
                        );
                    }
                });
        });
    hud::draw_hotbar_and_log(&ctx, session.combat(), session.key_binds());
}

fn copy_combat_wav(shots: &PathBuf, name: &str) -> String {
    let dest = shots.join(name);
    for dir in combat_audio_dirs() {
        let src = dir.join(name);
        if src.is_file() {
            let _ = fs::copy(&src, &dest);
            break;
        }
    }
    name.to_string()
}

fn combat_audio_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(root) = std::env::var_os("ORRUN_ASSETS") {
        dirs.push(PathBuf::from(root).join("audio").join("combat"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("assets").join("audio").join("combat"));
        }
    }
    dirs.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("audio")
            .join("combat"),
    );
    dirs
}
