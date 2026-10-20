//! Local play in a headless app over a real install: the player spawns on
//! the map, lands on its floor and walks when forward is held. Gated on
//! `RUSTLIFE_TEST_GAME_DIR` and `#[ignore]`d so content-free CI still passes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bevy::MinimalPlugins;
use bevy::asset::AssetPlugin;
use bevy::image::Image;
use bevy::input::InputPlugin;
use bevy::mesh::Mesh;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use rl_assets::{SourceAssetPlugin, SourceLoadersPlugin};
use rl_client::player::{LocalPlayer, PlayerPlugin, ViewMode};
use rl_client::world::WorldPlugin;
use rl_client::{AppState, CurrentMap, GameVfs};
use rl_data::GameData;
use source_fs::content::{ContentOptions, mount_game};

/// Updates the app for `seconds` of real time, so fixed ticks accumulate.
fn run_for(app: &mut App, seconds: f32) {
    let steps = (seconds / 0.005) as usize;
    for _ in 0..steps {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn player(app: &App) -> Option<LocalPlayer> {
    app.world().get_resource::<LocalPlayer>().copied()
}

#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn the_player_spawns_lands_and_walks() {
    let Ok(dir) = std::env::var("RUSTLIFE_TEST_GAME_DIR") else {
        return;
    };
    let game = mount_game(&ContentOptions {
        game_dir: Some(PathBuf::from(dir)),
        ..Default::default()
    })
    .expect("mount game");
    let vfs = Arc::new(game.vfs);

    let mut app = App::new();
    app.insert_resource(GameVfs(vfs.clone()));
    app.add_plugins(SourceAssetPlugin::new(vfs));
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin::default(),
        StatesPlugin,
        InputPlugin,
    ));
    app.init_asset::<Mesh>();
    app.init_asset::<Image>();
    app.init_asset::<StandardMaterial>();
    app.add_plugins(SourceLoadersPlugin);
    app.insert_resource(CurrentMap("dm_lockdown".into()));
    app.insert_state(AppState::InGame);
    app.add_plugins((
        WorldPlugin { spawn: 6 },
        PlayerPlugin {
            data: GameData::builtin().expect("data"),
        },
    ));

    for _ in 0..2000 {
        app.update();
        if player(&app).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(player(&app).is_some(), "the player spawned");
    assert_eq!(*app.world().resource::<ViewMode>(), ViewMode::Walk);

    run_for(&mut app, 0.5);
    let standing = player(&app).expect("player").current;
    assert!(standing.on_ground, "landed on the floor: {standing:?}");

    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::KeyW);
    run_for(&mut app, 0.5);
    let walking = player(&app).expect("player").current;
    let moved = (walking.origin - standing.origin).truncate().length();
    assert!(moved > 50.0, "walked {moved} units: {walking:?}");
    assert!(walking.speed_2d() > 200.0);
}
