//! The rustlife client: a Bevy app that mounts the player's game content and
//! renders a map.
//!
//! In M1 this is a world viewer — it loads a BSP through the `source://` asset
//! source and flies a free camera around it. Later milestones add the player
//! simulation, networking, HUD and audio on top of the same app.

#![forbid(unsafe_code)]

use std::sync::Arc;

use bevy::prelude::*;
use rl_assets::{SourceAssetPlugin, SourceLoadersPlugin};
use source_fs::Vfs;

use crate::camera::FlyCamPlugin;
use crate::world::WorldPlugin;

pub mod camera;
pub mod world;

/// Builds and runs the client, rendering `map_name` from the content mounted in
/// `vfs`.
///
/// The `source://` asset source is registered before `DefaultPlugins` (as Bevy
/// requires for custom sources) and the loaders after it.
pub fn run(vfs: Arc<Vfs>, map_name: String) -> AppExit {
    let mut app = App::new();
    app.add_plugins(SourceAssetPlugin::new(vfs));
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: format!("rustlife — {map_name}"),
            ..default()
        }),
        ..default()
    }));
    app.add_plugins(SourceLoadersPlugin);
    app.add_plugins(FlyCamPlugin);
    app.add_plugins(WorldPlugin { map_name });
    app.run()
}
