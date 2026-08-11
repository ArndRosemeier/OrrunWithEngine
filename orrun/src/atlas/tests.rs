use super::biomes::{self, Biome};
use super::features::{Dir, EndpointKind, Kind, NodeKind, RoadClass};
use super::pack;
use super::ContinentAtlas;

#[test]
fn elevation_mapping_endpoints() {
    assert!(pack::elevation_to_metres(0) < 0);
    assert_eq!(pack::elevation_to_metres(32), 0);
    assert!(pack::elevation_to_metres(40) > 0);
    assert!(pack::elevation_to_metres(40) <= 21);
    assert!(pack::elevation_to_metres(120) < 500);
    assert!(pack::elevation_to_metres(200) >= 1200);
    assert!(pack::elevation_to_metres(255) >= 3500);
    assert!(pack::elevation_to_metres(255) <= 4000);
    assert!((pack::elevation_to_metres(pack::metres_to_elevation(100)) - 100).abs() <= 12);
    assert!((pack::elevation_to_metres(pack::metres_to_elevation(3000)) - 3000).abs() <= 80);
}

#[test]
fn pack_roundtrip() {
    let packed = pack::pack(120, 200, Biome::Forest, 40, 3);
    assert_eq!(pack::elevation(packed), 120);
    assert_eq!(pack::humidity(packed), 200);
    assert_eq!(pack::biome(packed), Biome::Forest);
    assert_eq!(pack::relief(packed), 40);
    assert_eq!(pack::population(packed), 3);
}

#[test]
fn generate_validates_clean() {
    let atlas = ContinentAtlas::generate(20260809, 128);
    let errors = atlas.validate();
    for e in &errors {
        eprintln!("validate: {e}");
    }
    assert!(errors.is_empty(), "{} validation errors", errors.len());
}

#[test]
fn no_inland_ocean_pockets() {
    for seed in [3, 7, 20260809, 424242] {
        let atlas = ContinentAtlas::generate(seed, 96);
        assert!(
            atlas.validate().iter().all(|e| !e.contains("inland ocean")),
            "seed {seed} has inland ocean: {:?}",
            atlas
                .validate()
                .into_iter()
                .filter(|e| e.contains("inland ocean"))
                .collect::<Vec<_>>()
        );
        // Every ocean cell must reach the border; every former pocket is a lake.
        let mut ocean = 0;
        let mut lake_cells = 0;
        for &cell in &atlas.cells {
            match pack::biome(cell) {
                Biome::Ocean => ocean += 1,
                Biome::Lake => lake_cells += 1,
                _ => {}
            }
        }
        assert!(ocean > 0, "seed {seed}: expected open ocean");
        assert!(lake_cells > 0, "seed {seed}: expected lakes");
        assert_eq!(atlas.first_inland_ocean_cell(), None);
    }
}

#[test]
fn climate_sanity() {
    let atlas = ContinentAtlas::generate(20260809, 128);
    let mut ocean = 0;
    let mut land = 0;
    let mut lake = 0;
    for &cell in &atlas.cells {
        match pack::biome(cell) {
            Biome::Ocean => ocean += 1,
            Biome::Lake => lake += 1,
            b if biomes::is_land(b) => land += 1,
            _ => {}
        }
    }
    assert!(ocean > atlas.size * 4);
    assert!(land > atlas.size * 4);
    assert!(lake > 0 && !atlas.lakes.is_empty());
    assert_eq!(atlas.sea_surface_z, 0);
    assert_eq!(atlas.schema_version, super::continent::SCHEMA_VERSION);

    let mut wet_ocean = 0;
    let mut ocean_n = 0;
    for &cell in &atlas.cells {
        if pack::biome(cell) != Biome::Ocean {
            continue;
        }
        ocean_n += 1;
        if pack::humidity(cell) >= 250 {
            wet_ocean += 1;
        }
    }
    assert!(ocean_n > 0 && wet_ocean * 10 >= ocean_n * 9);
}

#[test]
fn determinism_same_seed() {
    let a = ContinentAtlas::generate(424242, 96);
    let b = ContinentAtlas::generate(424242, 96);
    let c = ContinentAtlas::generate(424243, 96);
    assert_eq!(a.content_hash, b.content_hash);
    assert_eq!(a.lakes.len(), b.lakes.len());
    assert_eq!(a.nodes.len(), b.nodes.len());
    assert_eq!(a.cells, b.cells);
    assert_ne!(a.content_hash, c.content_hash);
}

#[test]
fn edge_ports_shared() {
    let atlas = ContinentAtlas::generate(20260809, 96);
    let mut mismatches = 0;
    for az in 0..atlas.size {
        for ax in 0..atlas.size.saturating_sub(1) {
            let a = atlas.ports_on_edge(ax as i32, az as i32, Dir::East, Kind::River);
            let b = atlas.ports_on_edge(ax as i32 + 1, az as i32, Dir::West, Kind::River);
            if a.len() != b.len() {
                mismatches += 1;
            }
        }
    }
    assert_eq!(mismatches, 0);
}

#[test]
fn river_and_lake_sanity() {
    let atlas = ContinentAtlas::generate(20260809, 128);
    let mut climbs = 0;
    let mut lake_mouths = 0;
    let mut ocean_mouths = 0;
    for (&idx, links) in &atlas.river_links {
        let down = atlas.river_receiver[idx as usize];
        if down >= 0 {
            let e0 = pack::elevation(atlas.cells[idx as usize]);
            let db = pack::biome(atlas.cells[down as usize]);
            if !matches!(db, Biome::Ocean | Biome::Lake)
                && pack::elevation(atlas.cells[down as usize]) > e0
            {
                climbs += 1;
            }
        }
        for link in links {
            match link.b.kind {
                EndpointKind::Lake => lake_mouths += 1,
                EndpointKind::Ocean => ocean_mouths += 1,
                _ => {}
            }
        }
    }
    assert_eq!(climbs, 0);
    assert!(ocean_mouths > 0);
    assert!(lake_mouths > 0);
    for lake in &atlas.lakes {
        assert!(lake.spill_cell >= 0);
    }
}

#[test]
fn population_sparse() {
    let atlas = ContinentAtlas::generate(20260809, 128);
    let mut land = 0;
    let mut occupied = 0;
    let mut water_occupied = 0;
    for &cell in &atlas.cells {
        let pop = pack::population(cell);
        if biomes::is_land(pack::biome(cell)) {
            land += 1;
            if pop > 0 {
                occupied += 1;
            }
        } else if pop > 0 {
            water_occupied += 1;
        }
    }
    assert_eq!(water_occupied, 0);
    assert!(occupied > 0);
    assert!((occupied as f32) / (land as f32) < 0.35);
    let settlements = atlas
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Settlement)
        .count();
    assert!(settlements > 0 || atlas.river_links.is_empty());
    let _ = RoadClass::Primary;
}
