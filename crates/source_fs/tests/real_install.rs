//! Integration tests against a real Half-Life 2: Deathmatch install.
//!
//! These are `#[ignore]`d so the default test run never needs game content.
//! Run them with `cargo test -p source_fs -- --ignored`; set `HL2DM_DIR` to
//! the install folder if it is not in a standard Steam library. When no
//! install is found the tests pass vacuously with a note on stderr.

use std::path::PathBuf;

use source_fs::MountOrigin;
use source_fs::content::{ContentOptions, mount_game};

fn options() -> ContentOptions {
    ContentOptions {
        game_dir: std::env::var_os("HL2DM_DIR").map(PathBuf::from),
        ..ContentOptions::default()
    }
}

macro_rules! mount_or_skip {
    () => {
        match mount_game(&options()) {
            Ok(game) => game,
            Err(error) => {
                eprintln!("skipping: {error}");
                return;
            }
        }
    };
}

#[test]
#[ignore = "requires a Half-Life 2: Deathmatch install"]
fn mounts_stock_maps_from_base_game() {
    let game = mount_or_skip!();
    let maps = game.vfs.list("maps/*.bsp");
    for stock in [
        "dm_lockdown",
        "dm_overwatch",
        "dm_powerhouse",
        "dm_resistance",
        "dm_runoff",
        "dm_steamlab",
        "dm_underpass",
    ] {
        let path = format!("maps/{stock}.bsp");
        let listed = maps
            .iter()
            .find(|m| m.path == path)
            .unwrap_or_else(|| panic!("{path} not listed"));
        assert_eq!(listed.origin, MountOrigin::BaseGame);
    }
}

#[test]
#[ignore = "requires a Half-Life 2: Deathmatch install"]
fn reads_files_from_vpks_and_loose_dirs() {
    let game = mount_or_skip!();
    // A map: loose file in hl2mp/maps; starts with the BSP magic.
    let bsp = game.vfs.read("maps/dm_lockdown.bsp").unwrap().unwrap();
    assert_eq!(&bsp[..4], b"VBSP");
    // Something that only exists in the shared HL2 VPKs.
    let textures = game.vfs.list("materials/concrete/*.vtf");
    assert!(
        !textures.is_empty(),
        "expected concrete textures in hl2 VPKs"
    );
    let vtf = game.vfs.read(&textures[0].path).unwrap().unwrap();
    assert_eq!(&vtf[..4], b"VTF\0");
}
