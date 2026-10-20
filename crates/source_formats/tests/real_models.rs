//! Decodes every physics model of a real install. `#[ignore]`d; run with
//! `HL2DM_DIR=<install> cargo test -p source_formats --test real_models --
//! --ignored`.

use std::path::PathBuf;

use source_formats::phy;
use source_fs::content::{ContentOptions, mount_game};

#[test]
#[ignore = "requires a Half-Life 2: Deathmatch install"]
fn decodes_every_physics_model() {
    let options = ContentOptions {
        game_dir: std::env::var_os("HL2DM_DIR").map(PathBuf::from),
        ..ContentOptions::default()
    };
    let game = match mount_game(&options) {
        Ok(game) => game,
        Err(error) => {
            eprintln!("skipping: {error}");
            return;
        }
    };
    let paths = game.vfs.list("models/**/*.phy");
    let mut failures = Vec::new();
    let mut pieces = 0;
    for path in paths.iter().map(|file| &file.path) {
        let Ok(Some(bytes)) = game.vfs.read(path) else {
            continue;
        };
        match phy::decode(&bytes) {
            Ok(model) => pieces += model.solids.iter().map(|s| s.pieces.len()).sum::<usize>(),
            Err(error) => failures.push(format!("{path}: {error}")),
        }
    }
    eprintln!(
        "{} physics models, {pieces} convex pieces, {} failed",
        paths.len(),
        failures.len()
    );
    for failure in failures.iter().take(20) {
        eprintln!("  {failure}");
    }
    assert!(!paths.is_empty(), "the install has physics models");
    assert!(
        failures.len() * 100 <= paths.len(),
        "more than 1% of physics models failed to decode"
    );
}

/// Physics hulls must line up with the rendered model: compares the bounds
/// of each single-solid `.phy` with those of its mesh. A few percent of
/// models (animated doors, turrets, cables, editor helpers) have meshes
/// whose rest positions are not the pose their hull was built in; for
/// stock props (2.4% of the 1783 checked in HL2DM) that is tolerated.
#[test]
#[ignore = "requires a Half-Life 2: Deathmatch install"]
fn physics_hulls_line_up_with_meshes() {
    let options = ContentOptions {
        game_dir: std::env::var_os("HL2DM_DIR").map(PathBuf::from),
        ..ContentOptions::default()
    };
    let Ok(game) = mount_game(&options) else {
        eprintln!("skipping: no install");
        return;
    };
    let read = |path: &str| game.vfs.read(path).ok().flatten();
    let (mut checked, mut misaligned) = (0, Vec::new());
    for file in game.vfs.list("models/**/*.phy") {
        let stem = file.path.trim_end_matches(".phy");
        let (Some(phy), Some(mdl), Some(vtx), Some(vvd)) = (
            read(&file.path),
            read(&format!("{stem}.mdl")),
            read(&format!("{stem}.dx90.vtx")),
            read(&format!("{stem}.vvd")),
        ) else {
            continue;
        };
        let (Ok(physics), Ok(model)) = (
            phy::decode(&phy),
            source_formats::mdl::load_model(&mdl, &vtx, &vvd),
        ) else {
            continue;
        };
        if physics.solids.len() != 1 {
            continue;
        }
        let Some((min, max)) = physics.bounds() else {
            continue;
        };
        let mesh = model.meshes.iter().flat_map(|mesh| &mesh.positions);
        let (mut mesh_min, mut mesh_max) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for p in mesh {
            for axis in 0..3 {
                mesh_min[axis] = mesh_min[axis].min(p[axis]);
                mesh_max[axis] = mesh_max[axis].max(p[axis]);
            }
        }
        checked += 1;
        let centre = |a: [f32; 3], b: [f32; 3]| [0, 1, 2].map(|i| f32::midpoint(a[i], b[i]));
        let (pc, mc) = (centre(min, max), centre(mesh_min, mesh_max));
        let size = (0..3)
            .map(|i| mesh_max[i] - mesh_min[i])
            .fold(1.0f32, f32::max);
        let offset = (0..3).map(|i| (pc[i] - mc[i]).abs()).fold(0.0f32, f32::max);
        if offset > 0.25 * size {
            misaligned.push(format!("{stem}: phy centre {pc:?}, mesh centre {mc:?}"));
        }
    }
    eprintln!("{checked} props checked, {} misaligned", misaligned.len());
    for line in misaligned.iter().take(15) {
        eprintln!("  {line}");
    }
    assert!(misaligned.len() * 20 <= checked, "more than 5% misaligned");
}
