use orrun::ContinentAtlas;

fn main() {
    let seed = 20260809i32;
    let size = 96usize;
    let atlas = ContinentAtlas::generate(seed, size);
    let errors = atlas.validate();
    println!(
        "atlas {}² seed={seed}: lakes={} nodes={} river_edges={} road_edges={} crossings={} hash={:#x}",
        atlas.size,
        atlas.lakes.len(),
        atlas.nodes.len(),
        atlas.river_ports.len(),
        atlas.road_ports.len(),
        atlas.crossings.len(),
        atlas.content_hash as u32,
    );
    let mut ocean = 0usize;
    let mut land = 0usize;
    let mut lake = 0usize;
    for &cell in &atlas.cells {
        match orrun::atlas::pack::biome(cell) {
            orrun::atlas::biomes::Biome::Ocean => ocean += 1,
            orrun::atlas::biomes::Biome::Lake => lake += 1,
            b if orrun::atlas::biomes::is_land(b) => land += 1,
            _ => {}
        }
    }
    println!("climate: ocean={ocean} land={land} lake_cells={lake}");
    if errors.is_empty() {
        println!("validate: ok");
    } else {
        println!("validate: {} errors", errors.len());
        for e in errors.iter().take(12) {
            println!("  · {e}");
        }
        std::process::exit(1);
    }
}
