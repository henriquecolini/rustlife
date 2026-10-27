//! Decodes every particle file of a real install. `#[ignore]`d; run with
//! `RUSTLIFE_TEST_GAME_DIR=<install> cargo test -p source_formats --test
//! real_particles -- --ignored --nocapture`.

use std::path::PathBuf;

use source_formats::pcf;
use source_fs::content::{ContentOptions, mount_game};

fn game_dir() -> Option<PathBuf> {
    std::env::var_os("RUSTLIFE_TEST_GAME_DIR")
        .or_else(|| std::env::var_os("HL2DM_DIR"))
        .map(PathBuf::from)
}

#[test]
#[ignore = "requires a Half-Life 2: Deathmatch install"]
fn decodes_every_particle_file() {
    let options = ContentOptions {
        game_dir: game_dir(),
        ..ContentOptions::default()
    };
    let game = match mount_game(&options) {
        Ok(game) => game,
        Err(error) => {
            eprintln!("skipping: {error}");
            return;
        }
    };
    let paths = game.vfs.list("particles/*.pcf");
    assert!(!paths.is_empty(), "no particle files found");
    let mut systems = 0;
    for path in paths.iter().map(|file| &file.path) {
        let bytes = game.vfs.read(path).unwrap().unwrap();
        let file = pcf::parse(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
        let list = file.systems();
        eprintln!("{path}: {} systems", list.len());
        for s in &list {
            assert!(!s.name.is_empty(), "{path}: unnamed system");
        }
        systems += list.len();
    }
    assert!(systems > 50, "only {systems} systems");
    // The systems the plan names.
    let bytes = game.vfs.read("particles/impact_fx.pcf").unwrap().unwrap();
    let list = pcf::parse(&bytes).unwrap().systems();
    let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
    eprintln!("impact_fx: {names:?}");
    let ar2 = list.iter().find(|s| s.name == "AR2Impact");
    eprintln!("{ar2:#?}");
}

#[test]
#[ignore = "requires a Half-Life 2: Deathmatch install"]
fn lists_operators_in_use() {
    let options = ContentOptions {
        game_dir: game_dir(),
        ..ContentOptions::default()
    };
    let Ok(game) = mount_game(&options) else {
        return;
    };
    let mut uses: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for file in game.vfs.list("particles/*.pcf") {
        let bytes = game.vfs.read(&file.path).unwrap().unwrap();
        for s in pcf::parse(&bytes).unwrap().systems() {
            for (kind, ops) in [
                ("render", &s.renderers),
                ("op", &s.operators),
                ("init", &s.initializers),
                ("emit", &s.emitters),
                ("force", &s.forces),
                ("constraint", &s.constraints),
            ] {
                for o in ops {
                    *uses.entry(format!("{kind}: {}", o.function)).or_default() += 1;
                }
            }
        }
    }
    for (k, n) in &uses {
        eprintln!("{n:4} {k}");
    }
}
