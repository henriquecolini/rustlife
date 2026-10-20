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

use crate::camera::{CursorGrab, FlyCamPlugin};
use crate::capture::{Capture, CapturePlugin};
use crate::world::WorldPlugin;

pub mod camera;
pub mod capture;
pub mod world;

/// The mounted game content, shared with systems (e.g. to mount a map's
/// pakfile on load). Cloning is cheap — it is an `Arc`.
#[derive(Resource, Clone)]
pub struct GameVfs(pub Arc<Vfs>);

/// Builds and runs the client, rendering `map_name` from the content mounted in
/// `vfs`, with the camera starting at spawn point `spawn`.
///
/// When `capture` is `Some`, the client runs non-interactively: it does not
/// grab the cursor, saves one screenshot after a warmup, and exits (see
/// [`capture`]). Otherwise it runs as the normal interactive viewer.
///
/// The `source://` asset source is registered before `DefaultPlugins` (as Bevy
/// requires for custom sources) and the loaders after it.
pub fn run(vfs: Arc<Vfs>, map_name: String, spawn: usize, capture: Option<Capture>) -> AppExit {
    let mut app = App::new();
    app.insert_resource(GameVfs(vfs.clone()));
    app.add_plugins(SourceAssetPlugin::new(vfs));
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: format!("rustlife — {map_name}"),
            ..default()
        }),
        ..default()
    }));
    app.add_plugins(SourceLoadersPlugin);
    app.insert_resource(CursorGrab(capture.is_none()));
    app.add_plugins(FlyCamPlugin);
    app.add_plugins(WorldPlugin { map_name, spawn });
    if let Some(capture) = capture {
        app.add_plugins(CapturePlugin(capture));
    }
    app.run()
}
