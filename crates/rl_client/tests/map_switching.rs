//! Headless test of the browser <-> map cycle against a real install: entering
//! a map spawns its geometry and mounts its pakfile; leaving tears both down;
//! a second map then loads cleanly. Gated on `RUSTLIFE_TEST_GAME_DIR`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bevy::MinimalPlugins;
use bevy::asset::AssetPlugin;
use bevy::image::Image;
use bevy::input::InputPlugin;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::StandardMaterial;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use rl_assets::{SourceAssetPlugin, SourceLoadersPlugin};
use rl_client::effect_material::EffectMaterial;
use rl_client::world::WorldPlugin;
use rl_client::{AppState, CurrentMap, GameVfs};
use source_fs::MountOrigin;
use source_fs::content::{ContentOptions, mount_game};

fn surface_count(app: &mut App) -> usize {
    app.world_mut().query::<&Mesh3d>().iter(app.world()).count()
}

fn pakfile_mounted(app: &App) -> bool {
    app.world()
        .resource::<GameVfs>()
        .0
        .mount_list()
        .iter()
        .any(|mount| matches!(mount.origin, MountOrigin::MapPakfile { .. }))
}

/// Updates until the current map's surfaces are spawned (or gives up).
fn wait_for_surfaces(app: &mut App) -> usize {
    for _ in 0..1000 {
        app.update();
        let count = surface_count(app);
        if count > 0 {
            return count;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    0
}

#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn switching_maps_unloads_the_previous_one() {
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
    app.init_asset::<EffectMaterial>();
    app.add_plugins(SourceLoadersPlugin);
    app.insert_resource(CurrentMap("dm_lockdown".into()));
    app.insert_state(AppState::InGame);
    app.add_plugins(WorldPlugin { spawn: None });

    let first = wait_for_surfaces(&mut app);
    assert!(first > 0, "dm_lockdown spawned surfaces");
    assert!(pakfile_mounted(&app), "dm_lockdown's pakfile is mounted");

    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::MainMenu);
    app.update();
    app.update();
    assert_eq!(surface_count(&mut app), 0, "leaving despawns the map");
    assert!(!pakfile_mounted(&app), "leaving unmounts the pakfile");

    app.world_mut().resource_mut::<CurrentMap>().0 = "dm_runoff".into();
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::InGame);
    let second = wait_for_surfaces(&mut app);
    assert!(second > 0, "dm_runoff spawned surfaces");
    assert_ne!(first, second, "the second map's own geometry was loaded");
}
