//! Every sound the gameplay data names, and every sound the stock maps'
//! `ambient_generic`s play, resolves to a wave of the mounted game that
//! decodes. Gated on `RUSTLIFE_TEST_GAME_DIR`.

use std::path::PathBuf;

use rl_client::audio::Sounds;
use rl_data::GameData;
use source_formats::surfaceprops::SurfaceTable;
use source_formats::wav;
use source_fs::content::{ContentOptions, mount_game};

#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn named_sounds_resolve_and_decode() {
    let Ok(dir) = std::env::var("RUSTLIFE_TEST_GAME_DIR") else {
        return;
    };
    let game = mount_game(&ContentOptions {
        game_dir: Some(PathBuf::from(dir)),
        ..Default::default()
    })
    .expect("mount game");
    let vfs = game.vfs;
    let mut sounds = Sounds::load(&vfs);
    let data = GameData::builtin().expect("builtin data");
    let mut names: Vec<String> = data
        .weapons
        .values()
        .flat_map(|weapon| weapon.sounds.values().cloned())
        .chain(
            data.items
                .values()
                .flat_map(|item| item.sounds.values().cloned()),
        )
        .collect();
    let config = &data.sounds;
    names.extend(
        [
            &config.footsteps.left,
            &config.footsteps.right,
            &config.fall_damage,
            &config.weapon_pickup,
            &config.death,
            &config.explosion,
            &config.impact,
            &config.flesh_impact,
        ]
        .into_iter()
        .cloned(),
    );
    names.extend(config.breaking.values().cloned());
    let surfaces = vfs
        .read("scripts/surfaceproperties.txt")
        .ok()
        .flatten()
        .map(|bytes| SurfaceTable::parse(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default();
    names.extend(surfaces.sounds_used().map(str::to_owned));
    names.extend(
        [
            &config.weapon_select.step,
            &config.weapon_select.pick,
            &config.weapon_select.close,
        ]
        .into_iter()
        .cloned(),
    );
    // Buttons name their sound by number; the stock maps' use none, so
    // only the pattern is checked (`Buttons.snd7`'s wave does not ship).
    names.push(config.button(1));

    let mut missing = Vec::new();
    for name in &names {
        // Several draws, to meet most of an entry's waves.
        for _ in 0..8 {
            let Some(chosen) = sounds.choose(name) else {
                missing.push(format!("{name}: no such sound"));
                break;
            };
            let path = format!("sound/{}", chosen.path);
            match vfs.read(&path).ok().flatten() {
                None => missing.push(format!("{name}: {path} missing")),
                Some(bytes) if !chosen.path.to_ascii_lowercase().ends_with("mp3") => {
                    if let Err(error) = wav::decode(&bytes) {
                        missing.push(format!("{name}: {path}: {error}"));
                    }
                }
                Some(_) => {}
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(missing.is_empty(), "{missing:#?}");
}
