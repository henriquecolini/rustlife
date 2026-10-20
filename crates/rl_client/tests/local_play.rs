//! Local play in a headless app over a real install: the player spawns on
//! the map, lands on its floor and walks when forward is held, the map's
//! props become physics bodies, and the map is stocked for a fight. Gated on
//! `RUSTLIFE_TEST_GAME_DIR` and `#[ignore]`d so content-free CI still passes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bevy::MinimalPlugins;
use bevy::asset::AssetPlugin;
use bevy::image::Image;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::{ButtonState, InputPlugin};
use bevy::mesh::Mesh;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use rl_assets::{SourceAssetPlugin, SourceLoadersPlugin};
use rl_client::player::{LocalPlayer, PlayerPlugin, Sim, ViewMode};
use rl_client::props::{PhysicsBody, PropsPlugin};
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

/// A headless client in `dm_lockdown` at spawn `spawn`, with `dummies`
/// training dummies; `None` when no install is named.
#[expect(
    clippy::expect_used,
    reason = "test helper: content that fails to mount must fail the test"
)]
fn client(spawn: usize, dummies: usize) -> Option<App> {
    let dir = std::env::var("RUSTLIFE_TEST_GAME_DIR").ok()?;
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
        WorldPlugin { spawn },
        PlayerPlugin {
            data: GameData::builtin().expect("data"),
            dummies,
        },
        PropsPlugin,
    ));
    Some(app)
}

/// Updates until the local player has spawned.
fn wait_for_player(app: &mut App) {
    for _ in 0..2000 {
        app.update();
        if player(app).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(player(app).is_some(), "the player spawned");
}

#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn the_player_spawns_lands_and_walks() {
    let Some(mut app) = client(6, 0) else {
        return;
    };

    wait_for_player(&mut app);
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

#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn the_maps_props_become_physics_bodies() {
    let Some(mut app) = client(0, 0) else {
        return;
    };

    // Wait for the models to stream in and their bodies to be made.
    let bodies = |app: &App| {
        app.world()
            .get_resource::<Sim>()
            .map_or(0, |sim| sim.0.physics().props().count())
    };
    for _ in 0..3000 {
        app.update();
        if bodies(&app) > 300 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let count = bodies(&app);
    assert!(count > 300, "dm_lockdown's props have bodies: {count}");
    let moving = app
        .world_mut()
        .query::<&PhysicsBody>()
        .iter(app.world())
        .count();
    assert!(moving > 10, "its physics props can move: {moving}");

    // Nothing falls out of the world while the props settle.
    run_for(&mut app, 2.0);
    let sim = app.world().resource::<Sim>();
    let physics = sim.0.physics();
    let fallen = physics
        .props()
        .filter_map(|body| physics.pose(body))
        .filter(|pose| pose.position.z < -16_384.0)
        .count();
    assert_eq!(fallen, 0, "props fell out of the world");
}

#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn the_map_is_stocked_and_weapons_answer_their_keys() {
    let Some(mut app) = client(0, 2) else {
        return;
    };
    wait_for_player(&mut app);
    let local = player(&app).expect("player").id;
    {
        let sim = &app.world().resource::<Sim>().0;
        assert_eq!(sim.players().count(), 3, "the player and two dummies");
        assert!(sim.pickups().len() > 50, "{}", sim.pickups().len());
    }
    let weapon = |app: &App| {
        let sim = &app.world().resource::<Sim>().0;
        let active = sim.player(local).and_then(|p| p.inventory.active());
        active.and_then(|weapon| sim.arsenal().weapon_name(weapon).map(str::to_owned))
    };
    let clip = |app: &App| {
        let sim = &app.world().resource::<Sim>().0;
        let player = sim.player(local).expect("player");
        player
            .inventory
            .active()
            .and_then(|w| player.inventory.clip(w))
    };
    assert_eq!(weapon(&app).as_deref(), Some("smg"));

    // Holding fire empties rounds from the clip.
    let full = clip(&app);
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);
    run_for(&mut app, 0.3);
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .release(MouseButton::Left);
    assert!(clip(&app) < full, "{:?} -> {:?}", full, clip(&app));

    // `1` takes the Manipulator. A press must arrive as a keyboard message:
    // the input system clears fresh presses made on the resource itself.
    app.world_mut().write_message(KeyboardInput {
        key_code: KeyCode::Digit1,
        logical_key: Key::Character("1".into()),
        state: ButtonState::Pressed,
        text: None,
        repeat: false,
        window: Entity::PLACEHOLDER,
    });
    run_for(&mut app, 0.1);
    assert_eq!(weapon(&app).as_deref(), Some("manipulator"));
}
