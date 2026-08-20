//! Thin combat mesh map. Id to on-disk glTF. Not FaunaLayer.

pub struct CombatMesh {
    pub id: &'static str,
    pub source: &'static str,      // relative to orrun/assets
    pub anim_idle: &'static str,   // "Idle"
    pub anim_melee: &'static str,  // "Punch"
    pub anim_weapon: Option<&'static str>, // Some("Weapon") orc+skull, None tribal
    pub weapon_node: Option<&'static str>, // Some("Orc_Weapon") orc only
}

pub fn mesh_spec(mob_id: &str) -> Option<CombatMesh> {
    Some(match mob_id {
        "orc" => CombatMesh {
            id: "orc",
            source: "monsters/big/Orc.glb",
            anim_idle: "Idle",
            anim_melee: "Punch",
            anim_weapon: Some("Weapon"),
            weapon_node: Some("Orc_Weapon"),
        },
        "tribal" => CombatMesh {
            id: "tribal",
            source: "monsters/big/Tribal.glb",
            anim_idle: "Idle",
            anim_melee: "Punch",
            anim_weapon: None,
            weapon_node: None,
        },
        "orc_skull" => CombatMesh {
            id: "orc_skull",
            source: "monsters/big/Orc_Skull.glb",
            anim_idle: "Idle",
            anim_melee: "Punch",
            anim_weapon: Some("Weapon"),
            weapon_node: None,
        },
        "crawler_spider_wolf" | "wolf" | "wolf-spider" => CombatMesh {
            id: "crawler_spider_wolf",
            source: "fauna/wolf/wolf.gltf",
            anim_idle: "Idle",
            anim_melee: "Attack",
            anim_weapon: None,
            weapon_node: None,
        },
        "skeleton_warrior" => CombatMesh {
            id: "skeleton_warrior",
            source: "monsters/kaykit/Skeleton_Warrior.glb",
            anim_idle: "Idle",
            anim_melee: "Unarmed_Melee_Attack_Punch_A",
            anim_weapon: None,
            weapon_node: None,
        },
        "skeleton_minion" => CombatMesh {
            id: "skeleton_minion",
            source: "monsters/kaykit/Skeleton_Minion.glb",
            anim_idle: "Idle",
            anim_melee: "Unarmed_Melee_Attack_Punch_A",
            anim_weapon: None,
            weapon_node: None,
        },
        "bandit" | "male_bandit" => CombatMesh {
            id: "bandit",
            source: "humans/male_bandit_01.glb",
            anim_idle: "Idle",
            anim_melee: "Attack",
            anim_weapon: None,
            weapon_node: None,
        },
        "skeleton_mage" => CombatMesh {
            id: "skeleton_mage",
            source: "monsters/kaykit/Skeleton_Mage_Staff.glb",
            anim_idle: "Idle",
            anim_melee: "Unarmed_Melee_Attack_Punch_A",
            anim_weapon: Some("Spellcast_Shoot"),
            weapon_node: None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn mesh_spec_big_roster_files_exist() {
        for id in [
            "orc",
            "tribal",
            "orc_skull",
            "skeleton_warrior",
            "skeleton_minion",
            "skeleton_mage",
            "bandit",
        ] {
            let spec = mesh_spec(id).expect(id);
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("assets")
                .join(spec.source);
            assert!(path.is_file(), "{} missing at {}", id, path.display());
        }
    }

    #[test]
    fn bandit_glb_loads_idle_and_attack() {
        let spec = mesh_spec("bandit").expect("bandit");
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(spec.source);
        let root = path.parent().unwrap();
        let model = engine::anim::AnimatedModel::load_with(
            &path,
            root,
            &engine::EngineLimits::default(),
        )
        .unwrap_or_else(|err| panic!("bandit glb load: {err}"));
        assert!(
            model.find_clip(spec.anim_idle).is_some(),
            "Idle missing on bandit"
        );
        assert!(
            model.find_clip(spec.anim_melee).is_some(),
            "Attack missing on bandit"
        );
    }

    #[test]
    fn crate_small_glb_loads() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("props")
            .join("crate_small.glb");
        engine::model::Model::load(&path)
            .unwrap_or_else(|err| panic!("crate_small glb load: {err}"));
    }
}
