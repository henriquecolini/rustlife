//! End-to-end runtime test of the content pipeline, without a window or GPU.
//!
//! It mounts a real install, drives the `source://` asset source and the
//! `BspLoader` through a headless Bevy `AssetServer`, and checks a map loads
//! into a `MapAsset` with geometry. Gated on `RUSTLIFE_TEST_GAME_DIR` and
//! `#[ignore]`d so content-free CI still passes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bevy::MinimalPlugins;
use bevy::asset::{AssetPlugin, AssetServer, Assets};
use bevy::image::Image;
use bevy::mesh::Mesh;
use bevy::prelude::*;
use rl_assets::{MapAsset, SourceAssetPlugin, SourceLoadersPlugin};
use source_fs::content::{ContentOptions, mount_game};

#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn loads_a_real_map_through_bevy() {
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
    // Source must be registered before AssetPlugin builds.
    app.add_plugins(SourceAssetPlugin::new(vfs));
    app.add_plugins(MinimalPlugins);
    app.add_plugins(AssetPlugin::default());
    app.init_asset::<Mesh>();
    app.init_asset::<Image>();
    app.add_plugins(SourceLoadersPlugin);

    let handle: Handle<MapAsset> = app
        .world()
        .resource::<AssetServer>()
        .load("source://maps/dm_lockdown.bsp");

    let mut surfaces = 0;
    for _ in 0..600 {
        app.update();
        if let Some(map) = app.world().resource::<Assets<MapAsset>>().get(&handle) {
            surfaces = map.meshes.len();
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(surfaces > 0, "dm_lockdown loaded with {surfaces} surfaces");
}
