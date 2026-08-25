//! Headless Orrun combat oracle port. No wgpu.
//!
//! `cargo run -p orrun --release --bin combat_sim -- --scenario all --out shots/playtester/combat-sim.json`

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use orrun::combat::math::{HARD_CAP_S, TICK};
use orrun::combat::sheets::formulas;
use orrun::combat::sim::{
    match_published_rows, print_table, run_all, run_scenario, scenario_ids, simulate_fight,
};
use orrun::combat::Discipline;
use serde_json::json;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let args = parse_args()?;
    if let Some(level) = args.player_level {
        if args.scenario.is_none() {
            let disc = Discipline::parse(&args.discipline)
                .ok_or_else(|| format!("unknown discipline: {}", args.discipline))?;
            let mut r = simulate_fight(
                level,
                disc,
                &args.mob,
                args.count,
                args.seed,
                args.potions,
                None,
                None,
                "",
            )?;
            r["band_pass"] = json!(null);
            r["title"] = json!(format!("L{level} {disc} vs {} {}", args.count, args.mob));
            print_table(&[r.clone()]);
            let text = serde_json::to_string_pretty(&json!({"formulas": formulas(), "fight": r}))
                .map_err(|e| e.to_string())?;
            if let Some(out) = args.out {
                write_out(&out, &format!("{text}\n"))?;
            } else {
                println!("{text}");
            }
            return Ok(ExitCode::SUCCESS);
        }
    }

    let scenario = args.scenario.as_deref().unwrap_or("all");
    if scenario == "all" {
        let payload = run_all(args.seed)?;
        let rows = payload["scenarios"].as_array().cloned().unwrap_or_default();
        print_table(&rows);
        println!();
        let path = &payload["leveling_path"];
        let mins = path["total_minutes"].as_f64().unwrap_or(0.0);
        let pass = if path["in_90_150"].as_bool() == Some(true) {
            "PASS"
        } else {
            "FAIL 90-150"
        };
        println!("XP path: {mins:.0} min to L10 ({}) {pass}", path["clears"]);
        let all_pass = payload["all_pass"].as_bool() == Some(true);
        println!("ALL BANDS: {}", if all_pass { "PASS" } else { "FAIL" });
        match_published_rows(&payload).map_err(|e| format!("published TTK mismatch: {e}"))?;
        println!("PUBLISHED TTKs: PASS");
        if let Some(out) = args.out {
            let mut text = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
            text.push('\n');
            write_out(&out, &text)?;
            println!("wrote {}", out.display());
        }
        return Ok(if all_pass {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        });
    }

    let r =
        run_scenario(scenario, args.seed).map_err(|e| format!("{e}. ids: {:?}", scenario_ids()))?;
    print_table(std::slice::from_ref(&r));
    println!(
        "{}",
        serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?
    );
    if let Some(out) = args.out {
        let mut text = serde_json::to_string_pretty(&json!({"formulas": formulas(), "fight": r}))
            .map_err(|e| e.to_string())?;
        text.push('\n');
        write_out(&out, &text)?;
    }
    Ok(if r["band_pass"].as_bool() == Some(true) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn write_out(path: &PathBuf, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

struct Args {
    scenario: Option<String>,
    out: Option<PathBuf>,
    player_level: Option<i32>,
    discipline: String,
    mob: String,
    count: i32,
    seed: i32,
    potions: Option<i32>,
}

fn parse_args() -> Result<Args, String> {
    let mut scenario = None;
    let mut out = None;
    let mut player_level = None;
    let mut discipline = "Martial".to_string();
    let mut mob = "crawler_spider_wolf".to_string();
    let mut count = 1i32;
    let mut seed = 1i32;
    let mut potions = None;
    let mut raw = std::env::args().skip(1);
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--scenario" => scenario = Some(need(&mut raw, "--scenario")?),
            "--out" => out = Some(PathBuf::from(need(&mut raw, "--out")?)),
            "--player-level" => {
                player_level = Some(
                    need(&mut raw, "--player-level")?
                        .parse()
                        .map_err(|_| "--player-level wants an int")?,
                )
            }
            "--discipline" => discipline = need(&mut raw, "--discipline")?,
            "--mob" => mob = need(&mut raw, "--mob")?,
            "--count" => {
                count = need(&mut raw, "--count")?
                    .parse()
                    .map_err(|_| "--count wants an int")?
            }
            "--seed" => {
                seed = need(&mut raw, "--seed")?
                    .parse()
                    .map_err(|_| "--seed wants an int")?
            }
            "--potions" => {
                potions = Some(
                    need(&mut raw, "--potions")?
                        .parse()
                        .map_err(|_| "--potions wants an int")?,
                )
            }
            "--help" | "-h" => {
                eprintln!(
                    "combat_sim --scenario all|id [--out path] tick={TICK}s cap={HARD_CAP_S}s"
                );
                return Err("help".into());
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    Ok(Args {
        scenario,
        out,
        player_level,
        discipline,
        mob,
        count,
        seed,
        potions,
    })
}

fn need(raw: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    raw.next().ok_or_else(|| format!("{flag} needs a value"))
}
