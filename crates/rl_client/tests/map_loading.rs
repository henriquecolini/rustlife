//! End-to-end runtime test of the content pipeline, without a window or GPU.
//!
//! It mounts a real install, drives the `source://` asset source and the
//! `BspLoader` through a headless Bevy `AssetServer`, and checks a map loads
//! into a `MapAsset` with geometry, props and ambient light, and that a model
//! loads into a `ModelAsset` with resolved materials. Gated on
//! `RUSTLIFE_TEST_GAME_DIR` and `#[ignore]`d so content-free CI still passes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bevy::MinimalPlugins;
use bevy::asset::{AssetPlugin, AssetServer, Assets};
use bevy::image::Image;
use bevy::mesh::Mesh;
use bevy::prelude::*;
use rl_assets::{MapAsset, ModelAsset, PropKind, SourceAssetPlugin, SourceLoadersPlugin};
use source_fs::content::{ContentOptions, mount_game};

/// A headless app serving the install at `RUSTLIFE_TEST_GAME_DIR` through
/// the `source://` asset source, or `None` when the variable is unset.
#[expect(
    clippy::expect_used,
    reason = "test helper: an install that fails to mount must fail the test"
)]
fn content_app() -> Option<App> {
    let dir = std::env::var("RUSTLIFE_TEST_GAME_DIR").ok()?;
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
    Some(app)
}

/// Loads `path` and updates the app until the asset is ready (or a few
/// seconds pass), returning a clone of it.
fn load<A: Asset + Clone>(app: &mut App, path: &str) -> Option<A> {
    let handle: Handle<A> = app.world().resource::<AssetServer>().load(path.to_owned());
    for _ in 0..600 {
        app.update();
        if let Some(asset) = app.world().resource::<Assets<A>>().get(&handle) {
            return Some(asset.clone());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn loads_a_real_map_through_bevy() {
    let Some(mut app) = content_app() else {
        return;
    };
    let map: MapAsset = load(&mut app, "source://maps/dm_lockdown.bsp").expect("map loads");

    assert!(!map.meshes.is_empty(), "dm_lockdown has surfaces");
    let statics = map
        .props
        .iter()
        .filter(|prop| prop.kind == PropKind::Static)
        .count();
    assert!(statics > 0, "dm_lockdown has static props");
    assert!(
        map.props
            .iter()
            .all(|prop| prop.model.starts_with("source://models/"))
    );
    let volume = map.irradiance.expect("dm_lockdown bakes ambient light");
    assert!(volume.placement.size.min_element() > 0.0);
}

#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn loads_a_real_model_through_bevy() {
    let Some(mut app) = content_app() else {
        return;
    };
    let model: ModelAsset =
        load(&mut app, "source://models/props_c17/oildrum001.mdl").expect("model loads");

    assert!(!model.parts.is_empty(), "the drum has meshes");
    assert_eq!(
        model.material(0, model.parts[0].material),
        Some("models/props_c17/oil_drum001a"),
        "skin 0 resolves through $cdmaterials"
    );
    let (min, max) = model.bounds;
    assert!(min.cmplt(max).all(), "bounds are ordered: {min} .. {max}");
}
