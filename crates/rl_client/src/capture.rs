//! Non-interactive screenshot capture, for proof shots and visual checks.
//!
//! When the client is asked to capture (`rustlife --screenshot <path>`), this
//! plugin waits a few seconds for the map and its textures to stream in, saves
//! one frame of the primary window to `path`, and then exits. It never grabs
//! the cursor, so a capture does not disturb an interactive session.

use std::path::PathBuf;

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};

/// Where and when to capture.
#[derive(Resource, Debug, Clone)]
pub struct Capture {
    /// Output PNG path.
    pub path: PathBuf,
    /// Seconds to wait before capturing, letting assets stream in.
    pub warmup_secs: f32,
}

/// Captures one screenshot after warmup, then exits the app.
pub struct CapturePlugin(pub Capture);

impl Plugin for CapturePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.0.clone())
            .add_systems(Update, capture_then_exit);
    }
}

fn capture_then_exit(
    time: Res<Time>,
    capture: Res<Capture>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
    mut phase: Local<u8>,
) {
    let elapsed = time.elapsed_secs();
    if *phase == 0 && elapsed >= capture.warmup_secs {
        info!("capturing screenshot to {}", capture.path.display());
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(capture.path.clone()));
        *phase = 1;
    } else if *phase == 1 && elapsed >= capture.warmup_secs + 1.0 {
        // Give the save observer a moment to write the file before quitting.
        exit.write(AppExit::Success);
        *phase = 2;
    }
}
