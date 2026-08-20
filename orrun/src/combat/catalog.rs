//! Thin combat mesh map. Id to on-disk glTF. Not FaunaLayer.

pub struct CombatMesh {
    pub id: &'static str,
    pub source: &'static str,      // relative to orrun/assets
    pub anim_idle: &'static str,   // "Idle"
    pub anim_melee: &'static str,  // "Punch"
    pub anim_death: Option<&'static str>, // Some("Death") / Some("Death_A"); None bandit
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
            anim_death: Some("Death"),
            anim_weapon: Some("Weapon"),
            weapon_node: Some("Orc_Weapon"),
        },
        "tribal" => CombatMesh {
            id: "tribal",
            source: "monsters/big/Tribal.glb",
            anim_idle: "Idle",
            anim_melee: "Punch",
            anim_death: Some("Death"),
            anim_weapon: None,
            weapon_node: None,
        },
        "orc_skull" => CombatMesh {
            id: "orc_skull",
            source: "monsters/big/Orc_Skull.glb",
            anim_idle: "Idle",
            anim_melee: "Punch",
            anim_death: Some("Death"),
            anim_weapon: Some("Weapon"),
            weapon_node: None,
        },
        "crawler_spider_wolf" | "wolf" | "wolf-spider" => CombatMesh {
            id: "crawler_spider_wolf",
            source: "fauna/wolf/wolf.gltf",
            anim_idle: "Idle",
            anim_melee: "Attack",
            anim_death: Some("Death"),
            anim_weapon: None,
            weapon_node: None,
        },
        "skeleton_warrior" => CombatMesh {
            id: "skeleton_warrior",
            source: "monsters/kaykit/Skeleton_Warrior.glb",
            anim_idle: "Idle",
            anim_melee: "Unarmed_Melee_Attack_Punch_A",
            anim_death: Some("Death_A"),
            anim_weapon: None,
            weapon_node: None,
        },
        "skeleton_minion" => CombatMesh {
            id: "skeleton_minion",
            source: "monsters/kaykit/Skeleton_Minion.glb",
            anim_idle: "Idle",
            anim_melee: "Unarmed_Melee_Attack_Punch_A",
            anim_death: Some("Death_A"),
            anim_weapon: None,
            weapon_node: None,
        },
        "bandit" | "male_bandit" => CombatMesh {
            id: "bandit",
            source: "humans/male_bandit_01.glb",
            anim_idle: "Idle",
            anim_melee: "Attack",
            anim_death: None,
            anim_weapon: None,
            weapon_node: None,
        },
        "skeleton_mage" => CombatMesh {
            id: "skeleton_mage",
            source: "monsters/kaykit/Skeleton_Mage_Staff.glb",
            anim_idle: "Idle",
            anim_melee: "Unarmed_Melee_Attack_Punch_A",
            anim_death: Some("Death_A"),
            anim_weapon: Some("Spellcast_Shoot"),
            weapon_node: None,
        },
        "yeti" => CombatMesh {
            id: "yeti",
            source: "monsters/big/Yeti.glb",
            anim_idle: "Idle",
            anim_melee: "Punch",
            anim_death: Some("Death"),
            anim_weapon: None,
            weapon_node: None,
        },
        "demon" => CombatMesh {
            id: "demon",
            source: "monsters/big/Demon.glb",
            anim_idle: "Idle",
            anim_melee: "Punch",
            anim_death: Some("Death"),
            anim_weapon: Some("Weapon"),
            weapon_node: Some("Trident"),
        },
        "blue_demon" | "BlueDemon" => CombatMesh {
            id: "blue_demon",
            source: "monsters/big/BlueDemon.glb",
            anim_idle: "Idle",
            anim_melee: "Punch",
            anim_death: Some("Death"),
            anim_weapon: None,
            weapon_node: None,
        },
        "tribal_veteran" | "TribalVeteran" => CombatMesh {
            id: "tribal_veteran",
            source: "monsters/big/Tribal_Veteran.glb",
            anim_idle: "Idle",
            anim_melee: "Punch",
            anim_death: Some("Death"),
            anim_weapon: None,
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
            "yeti",
            "demon",
            "blue_demon",
            "tribal_veteran",
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
        assert!(spec.anim_death.is_none(), "bandit has no Death clip");
    }

    #[test]
    fn demon_glb_loads_idle_punch_and_trident() {
        let spec = mesh_spec("demon").expect("demon");
        assert_eq!(spec.anim_idle, "Idle");
        assert_eq!(spec.anim_melee, "Punch");
        assert_eq!(spec.anim_death, Some("Death"));
        assert_eq!(spec.anim_weapon, Some("Weapon"));
        assert_eq!(spec.weapon_node, Some("Trident"));
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(spec.source);
        let root = path.parent().unwrap();
        let model = engine::anim::AnimatedModel::load_with(
            &path,
            root,
            &engine::EngineLimits::default(),
        )
        .unwrap_or_else(|err| panic!("demon glb load: {err}"));
        assert!(
            model.find_clip(spec.anim_idle).is_some(),
            "Idle missing on demon"
        );
        assert!(
            model.find_clip(spec.anim_melee).is_some(),
            "Punch missing on demon"
        );
        let bytes = std::fs::read(&path).expect("read Demon.glb");
        let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let json = std::str::from_utf8(&bytes[20..20 + json_len]).expect("glb json");
        assert!(
            json.contains("\"name\":\"Trident\""),
            "Trident node missing in Demon.glb"
        );
    }

    #[test]
    fn blue_demon_glb_loads_idle_and_punch() {
        let spec = mesh_spec("blue_demon").expect("blue_demon");
        assert_eq!(spec.source, "monsters/big/BlueDemon.glb");
        assert_eq!(spec.anim_idle, "Idle");
        assert_eq!(spec.anim_melee, "Punch");
        assert_eq!(spec.anim_death, Some("Death"));
        assert_eq!(spec.anim_weapon, None);
        assert_eq!(spec.weapon_node, None);
        let alias = mesh_spec("BlueDemon").expect("BlueDemon");
        assert_eq!(alias.id, "blue_demon");
        assert_eq!(alias.weapon_node, None);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(spec.source);
        let root = path.parent().unwrap();
        let model = engine::anim::AnimatedModel::load_with(
            &path,
            root,
            &engine::EngineLimits::default(),
        )
        .unwrap_or_else(|err| panic!("blue_demon glb load: {err}"));
        assert!(
            model.find_clip(spec.anim_idle).is_some(),
            "Idle missing on blue_demon"
        );
        assert!(
            model.find_clip(spec.anim_melee).is_some(),
            "Punch missing on blue_demon"
        );
        let bytes = std::fs::read(&path).expect("read BlueDemon.glb");
        let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let json = std::str::from_utf8(&bytes[20..20 + json_len]).expect("glb json");
        assert!(
            json.contains("\"name\":\"BlueDemon\""),
            "BlueDemon node missing in BlueDemon.glb"
        );
        assert!(
            !json.contains("\"name\":\"Trident\""),
            "BlueDemon.glb must not carry a Trident node"
        );
    }

    #[test]
    fn tribal_veteran_glb_loads_idle_and_punch() {
        let spec = mesh_spec("tribal_veteran").expect("tribal_veteran");
        assert_eq!(spec.source, "monsters/big/Tribal_Veteran.glb");
        assert_eq!(spec.anim_idle, "Idle");
        assert_eq!(spec.anim_melee, "Punch");
        assert_eq!(spec.anim_death, Some("Death"));
        assert_eq!(spec.anim_weapon, None);
        assert_eq!(spec.weapon_node, None);
        let alias = mesh_spec("TribalVeteran").expect("TribalVeteran");
        assert_eq!(alias.id, "tribal_veteran");
        assert_eq!(alias.weapon_node, None);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(spec.source);
        let root = path.parent().unwrap();
        let model = engine::anim::AnimatedModel::load_with(
            &path,
            root,
            &engine::EngineLimits::default(),
        )
        .unwrap_or_else(|err| panic!("tribal_veteran glb load: {err}"));
        assert!(
            model.find_clip(spec.anim_idle).is_some(),
            "Idle missing on tribal_veteran"
        );
        assert!(
            model.find_clip(spec.anim_melee).is_some(),
            "Punch missing on tribal_veteran"
        );
        let bytes = std::fs::read(&path).expect("read Tribal_Veteran.glb");
        let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let json = std::str::from_utf8(&bytes[20..20 + json_len]).expect("glb json");
        assert!(
            json.contains("\"name\":\"Tribal_Veteran\""),
            "Tribal_Veteran node missing in Tribal_Veteran.glb"
        );
        assert!(
            json.contains("\"name\":\"VeteranBones\""),
            "VeteranBones node missing in Tribal_Veteran.glb"
        );
        assert!(
            json.contains("\"name\":\"VeteranPelt\""),
            "VeteranPelt node missing in Tribal_Veteran.glb"
        );
        assert!(
            !json.contains("\"name\":\"Trident\""),
            "Tribal_Veteran.glb must not carry a Trident node"
        );
    }

    #[test]
    fn yeti_glb_loads_idle_and_punch() {
        let spec = mesh_spec("yeti").expect("yeti");
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(spec.source);
        let root = path.parent().unwrap();
        let model = engine::anim::AnimatedModel::load_with(
            &path,
            root,
            &engine::EngineLimits::default(),
        )
        .unwrap_or_else(|err| panic!("yeti glb load: {err}"));
        assert!(
            model.find_clip(spec.anim_idle).is_some(),
            "Idle missing on yeti"
        );
        assert!(
            model.find_clip(spec.anim_melee).is_some(),
            "Punch missing on yeti"
        );
    }

    #[test]
    fn anim_death_names_match_glb_or_none() {
        for (id, want) in [
            ("orc", Some("Death")),
            ("tribal", Some("Death")),
            ("orc_skull", Some("Death")),
            ("wolf", Some("Death")),
            ("skeleton_warrior", Some("Death_A")),
            ("skeleton_minion", Some("Death_A")),
            ("skeleton_mage", Some("Death_A")),
            ("yeti", Some("Death")),
            ("demon", Some("Death")),
            ("blue_demon", Some("Death")),
            ("tribal_veteran", Some("Death")),
            ("bandit", None),
        ] {
            let spec = mesh_spec(id).expect(id);
            assert_eq!(spec.anim_death, want, "{id}");
            let Some(clip) = spec.anim_death else {
                continue;
            };
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("assets")
                .join(spec.source);
            let root = path.parent().unwrap();
            let model = engine::anim::AnimatedModel::load_with(
                &path,
                root,
                &engine::EngineLimits::default(),
            )
            .unwrap_or_else(|err| panic!("{id} glb load: {err}"));
            assert!(
                model.find_clip(clip).is_some(),
                "{id} missing death clip {clip}"
            );
        }
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
