//! How much texture memory each stock map asks for when textures are decoded
//! to RGBA8 (what a phone's GPU, which cannot sample BC, needs), at each
//! texture quality. `#[ignore]`d; run with `RUSTLIFE_TEST_GAME_DIR=<install>
//! cargo test --release -p source_formats --test real_texture_budget --
//! --ignored --nocapture`.
//!
//! It adds up the base textures of the materials a map's surfaces use, each
//! as a full mip chain with the top `drop` levels left out, 4 bytes a pixel.
//! (Textures of props and effects are not counted: the maps' own surfaces
//! are most of it, and the same budget is being measured at every quality.)

use std::collections::BTreeSet;
use std::path::PathBuf;

use source_formats::{collision, vmt, vtf};
use source_fs::Vfs;
use source_fs::content::{ContentOptions, mount_game};

fn game_dir() -> Option<PathBuf> {
    std::env::var_os("RUSTLIFE_TEST_GAME_DIR")
        .or_else(|| std::env::var_os("HL2DM_DIR"))
        .map(PathBuf::from)
}

/// Bytes of a full RGBA8 mip chain of `width`×`height` without its top `drop`
/// levels.
fn chain_bytes(mut width: u64, mut height: u64, drop: u32) -> u64 {
    let mut total = 0;
    let mut level = 0;
    loop {
        if level >= drop {
            total += width * height * 4;
        }
        if width == 1 && height == 1 {
            return total;
        }
        width = (width / 2).max(1);
        height = (height / 2).max(1);
        level += 1;
    }
}

fn dimensions(vfs: &Vfs, texture: &str) -> Option<(u64, u64)> {
    let path = format!("materials/{}.vtf", texture.replace('\\', "/"));
    let bytes = vfs.read(&path).ok()??;
    if let Some(compressed) = vtf::compressed(&bytes) {
        return Some((u64::from(compressed.width), u64::from(compressed.height)));
    }
    let decoded = vtf::decode(&bytes).ok()?;
    Some((u64::from(decoded.width), u64::from(decoded.height)))
}

#[test]
#[ignore = "requires a Half-Life 2: Deathmatch install"]
fn stock_map_texture_budgets() {
    let Ok(game) = mount_game(&ContentOptions {
        game_dir: game_dir(),
        ..ContentOptions::default()
    }) else {
        eprintln!("skipping: no install");
        return;
    };
    let vfs = &game.vfs;
    for map in vfs.list("maps/dm_*.bsp") {
        let Some(bytes) = vfs.read(&map.path).ok().flatten() else {
            continue;
        };
        let Ok(collision) = collision::load(&bytes) else {
            continue;
        };
        let mut textures = BTreeSet::new();
        for material in collision.materials.iter().collect::<BTreeSet<_>>() {
            let path = format!("materials/{}.vmt", material.replace('\\', "/"));
            let Some(text) = vfs.read(&path).ok().flatten() else {
                continue;
            };
            let text = String::from_utf8_lossy(&text);
            let desc = match vmt::patch_include(&text) {
                Ok(Some(base)) => {
                    vfs.read(&base.replace('\\', "/"))
                        .ok()
                        .flatten()
                        .and_then(|base| {
                            let base = String::from_utf8_lossy(&base).into_owned();
                            vmt::parse_resolved(&text, |_| Ok(base.clone())).ok()
                        })
                }
                _ => vmt::parse(&text).ok(),
            };
            if let Some(desc) = desc {
                textures.extend(desc.base_texture);
                textures.extend(desc.base_texture2);
            }
        }
        let sizes: Vec<(u64, u64)> = textures
            .iter()
            .filter_map(|texture| dimensions(vfs, texture))
            .collect();
        let mb = |drop| {
            sizes
                .iter()
                .map(|&(w, h)| chain_bytes(w, h, drop))
                .sum::<u64>()
                / 1_048_576
        };
        eprintln!(
            "{}: {} textures; RGBA8 at full {} MB, one mip dropped {} MB, two {} MB, three {} MB",
            map.path,
            sizes.len(),
            mb(0),
            mb(1),
            mb(2),
            mb(3)
        );
    }
}
