use std::path::{Path, PathBuf};
use std::sync::Arc;

use orrun::combat::WorldCombat;
use orrun::gamedata::{ActionId, GameData};
use orrun::resolution::TimedStatusKind;
use serde::Serialize;

#[derive(Serialize)]
struct ActionCheck {
    action: String,
    passed: bool,
    detail: String,
}
#[derive(Serialize)]
struct Report {
    hook: String,
    passed: bool,
    mob_roster: Vec<String>,
    checks: Vec<ActionCheck>,
}

fn game_data_path() -> PathBuf {
    let candidates = [
        Path::new("data/OrrunGameData.xml"),
        Path::new("../data/OrrunGameData.xml"),
    ];
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("cannot find data/OrrunGameData.xml"))
        .to_owned()
}
fn tick_until_idle(combat: &mut WorldCombat, limit_s: f64) {
    let mut elapsed = 0.0;
    while combat.casting_action_id().is_some() {
        assert!(elapsed < limit_s, "cast did not complete within {limit_s}s");
        combat.step_fixed(0.0, 0.0, 1.0, 0.0);
        elapsed += orrun::combat::TICK;
    }
}
fn use_action(combat: &mut WorldCombat, id: &str) {
    let action = ActionId::new(id);
    assert!(
        combat.press_action(&action, 0.0, 0.0, 1.0, 0.0),
        "{id} was rejected"
    );
    tick_until_idle(combat, 5.0);
    let cooldown = combat.player_action_cooldown_s(&action);
    let authored = combat
        .game_data()
        .action(&action)
        .expect("authored action")
        .cooldown_s();
    assert!(
        cooldown > 0.0 && cooldown <= authored,
        "{id} cooldown was not started"
    );
}
fn m5_actions(report_path: &Path) {
    let data = Arc::new(GameData::load(game_data_path()).expect("load GameData"));
    let mut combat = WorldCombat::with_game_data(Arc::clone(&data));
    let roster: Vec<String> = data
        .mobs()
        .iter()
        .map(|m| m.id().as_str().to_owned())
        .collect();
    assert!(!roster.is_empty());
    for (index, mob) in data.mobs().iter().enumerate() {
        let idx = i32::try_from(index).expect("mob roster index");
        combat.add_canonical_mob(
            mob.id(),
            idx,
            1.0,
            index as f64 * 0.1,
            1.0,
            index as f64 * 0.1,
        );
    }
    assert_eq!(
        combat.hostiles().len(),
        roster.len(),
        "full authored mob roster was not seated"
    );
    combat.clear_hostiles();
    combat.add_canonical_mob(&orrun::gamedata::MobId::new("wolf"), 0, 1.0, 0.0, 1.0, 0.0);
    combat.step_fixed(0.0, 0.0, 1.0, 0.0);
    combat.set_lock(Some(0));
    let mut checks = Vec::new();
    let hp_before = combat.hostiles()[0].hp();
    use_action(&mut combat, "slash");
    assert!(combat.hostiles()[0].hp() < hp_before);
    checks.push(ActionCheck {
        action: "slash".into(),
        passed: true,
        detail: "slash dealt canonical melee damage".into(),
    });
    while combat.player_action_cooldown_s(&ActionId::new("slash")) > 0.0 {
        combat.step_fixed(0.0, 0.0, 1.0, 0.0);
    }
    let hp_before = combat.hostiles()[0].hp();
    use_action(&mut combat, "arrow");
    assert!(combat.hostiles()[0].hp() < hp_before);
    checks.push(ActionCheck {
        action: "arrow".into(),
        passed: true,
        detail: "arrow dealt canonical ranged damage".into(),
    });
    combat.clear_hostiles();
    combat.set_player_hp(combat.player().resources.hp_max() - 20.0);
    let wounded_hp = combat.player().resources.hp();
    use_action(&mut combat, "restore");
    assert!(
        combat.player().resources.hp() > wounded_hp,
        "restore did not increase HP: {} -> {}",
        wounded_hp,
        combat.player().resources.hp()
    );
    checks.push(ActionCheck {
        action: "restore".into(),
        passed: true,
        detail: "restore restored player HP".into(),
    });
    combat.add_canonical_mob(
        &orrun::gamedata::MobId::new("wolf"),
        0,
        10.0,
        0.0,
        10.0,
        0.0,
    );
    combat.set_lock(Some(0));
    for (action, expected, detail) in [
        ("entangle", TimedStatusKind::Root, "root disables movement"),
        (
            "stasis",
            TimedStatusKind::Hold,
            "hold disables movement and actions",
        ),
        ("hobble", TimedStatusKind::Snare, "snare reduces movement"),
        (
            "befriend",
            TimedStatusKind::Charm,
            "charm changes effective faction",
        ),
    ] {
        use_action(&mut combat, action);
        let hostile = &combat.hostiles()[0];
        assert!(
            hostile.statuses().any(|s| s.kind() == expected),
            "{action} status missing"
        );
        match expected {
            TimedStatusKind::Root => {
                assert!(!hostile.can_move());
                assert!(hostile.can_act());
            }
            TimedStatusKind::Hold => {
                assert!(!hostile.can_move());
                assert!(!hostile.can_act());
            }
            TimedStatusKind::Snare => {
                assert!(hostile.effective_movement_speed_mps() < hostile.movement_speed_mps())
            }
            TimedStatusKind::Charm => {
                assert_eq!(hostile.effective_faction(), combat.player().faction())
            }
        }
        checks.push(ActionCheck {
            action: action.into(),
            passed: true,
            detail: detail.into(),
        });
        if expected == TimedStatusKind::Hold {
            for _ in 0..30 {
                combat.step_fixed(0.0, 0.0, 1.0, 0.0);
            }
        }
    }
    let report = Report {
        hook: "m5_actions".into(),
        passed: true,
        mob_roster: roster,
        checks,
    };
    std::fs::write(
        report_path,
        serde_json::to_vec_pretty(&report).expect("serialize report"),
    )
    .expect("write report");
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hook = args
        .windows(2)
        .find(|w| w[0] == "--hooks")
        .map(|w| w[1].as_str())
        .unwrap_or_else(|| panic!("usage: playtester --hooks m5_actions [--report path]"));
    let report = args
        .windows(2)
        .find(|w| w[0] == "--report")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(|| PathBuf::from("playtester-report.json"));
    match hook {
        "m5_actions" => m5_actions(&report),
        other => panic!("unknown playtester hook {other}"),
    }
}
