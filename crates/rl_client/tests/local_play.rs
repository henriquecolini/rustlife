//! Hosted play in a headless app over a real install: the client hosts the
//! map in-process and joins it; the player spawns on the map, lands on its
//! floor and walks when forward is held, the map's props have bodies, and
//! the map is stocked for a fight. Gated on `RUSTLIFE_TEST_GAME_DIR` and
//! `#[ignore]`d so content-free CI still passes.

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
use rl_client::content::Content;
use rl_client::effect_material::EffectMaterial;
use rl_client::menu::Menu;
use rl_client::net::{Connection, NetPlugin, NetSettings, Network};
use rl_client::player::{LocalPlayer, PlayerPlugin, ViewMode};
use rl_client::props::{PhysicsBody, PropsPlugin};
use rl_client::settings::{Bindings, Settings};
use rl_client::world::WorldPlugin;
use rl_client::{AppState, CurrentMap, GameVfs};
use rl_config::UserConfig;
use rl_data::GameData;
use rl_net::conditioner::NetConditions;
use rl_sim::simulation::Simulation;
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

/// The client's replica of the game.
fn sim(app: &App) -> Option<&Simulation> {
    app.world()
        .get_resource::<Connection>()?
        .0
        .session()
        .map(rl_net::Session::sim)
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
    app.init_asset::<EffectMaterial>();
    app.add_plugins(SourceLoadersPlugin);
    app.insert_resource(CurrentMap("dm_lockdown".into()));
    app.insert_resource(Content {
        config: UserConfig::default(),
        path: None,
        games: Vec::new(),
    });
    app.insert_resource(Settings(rl_config::Settings::default()));
    app.init_resource::<Menu>();
    app.init_resource::<Bindings>();
    app.insert_state(AppState::InGame);
    app.add_plugins((
        WorldPlugin { spawn: Some(spawn) },
        PlayerPlugin {
            data: GameData::builtin().expect("data"),
        },
        NetPlugin {
            settings: NetSettings {
                network: Network::Host { port: None },
                name: "tester".to_owned(),
                conditions: NetConditions::NONE,
                dummies,
            },
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

    // Wait for the game to start and the props to find their bodies.
    let bodies = |app: &App| sim(app).map_or(0, |sim| sim.physics().props().count());
    for _ in 0..3000 {
        app.update();
        let moving = app
            .world_mut()
            .query::<&PhysicsBody>()
            .iter(app.world())
            .count();
        if bodies(&app) > 300 && moving > 10 {
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
    let physics = sim(&app).expect("game").physics();
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
    run_for(&mut app, 0.2);
    {
        let sim = sim(&app).expect("game");
        assert_eq!(sim.players().count(), 3, "the player and two dummies");
        assert!(sim.pickups().len() > 50, "{}", sim.pickups().len());
    }
    let weapon = |app: &App| {
        let sim = sim(app).expect("game");
        let active = sim.player(local).and_then(|p| p.inventory.active());
        active.and_then(|weapon| sim.arsenal().weapon_name(weapon).map(str::to_owned))
    };
    let clip = |app: &App| {
        let sim = sim(app).expect("game");
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

/// Every spawn point of `dm_lockdown` puts the player somewhere free, and a
/// player left standing there stays put: no overlap with the map, no
/// jitter.
#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn spawns_are_free_and_steady() {
    let Some(mut probe) = client(0, 0) else {
        return;
    };
    wait_for_player(&mut probe);
    let spawns = sim(&probe).expect("game").spawns().len();
    drop(probe);
    let mut bad = Vec::new();
    for index in 0..spawns {
        let Some(mut app) = client(index, 0) else {
            return;
        };
        wait_for_player(&mut app);
        // The player is made where the server put it, then moved to the
        // chosen spawn and left to land: let that finish first.
        run_for(&mut app, 0.6);
        let first = player(&app).expect("player").current.origin;
        // Then stand still for a second, watching for overlap and wander.
        let mut overlap_ticks = 0;
        let mut travelled = 0.0_f32;
        let mut last = first;
        for _ in 0..200 {
            app.update();
            std::thread::sleep(Duration::from_millis(5));
            let now = player(&app).expect("player").current.origin;
            travelled += (now - last).length();
            last = now;
            let sim = sim(&app).expect("game");
            let hull = rl_sim::collision::Hull::from(&sim.data().movement.hulls.standing);
            let trace = sim.world().trace_hull(
                now,
                now,
                &hull,
                rl_sim::collision::contents::MASK_PLAYER_SOLID,
            );
            overlap_ticks += usize::from(trace.start_solid);
        }
        let overlap = overlap_ticks > 0;
        eprintln!(
            "spawn {index}: at {first:?} overlap ticks {overlap_ticks} travelled {travelled:.2}"
        );
        if overlap || travelled > 2.0 {
            bad.push(index);
        }
    }
    assert!(bad.is_empty(), "bad spawns: {bad:?}");
}

/// A scripted teleport into solid map settles on a free spot and stays
/// there: the capture tools used to be left jittering inside a wall.
#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn a_scripted_teleport_into_a_wall_settles_free() {
    let Some(mut app) = client(0, 0) else {
        return;
    };
    app.insert_resource(
        rl_client::script::InputScript::default()
            .with_teleport(Some(([-2843.0, 1928.0, 60.0], (0.0, 0.0)))),
    );
    wait_for_player(&mut app);
    run_for(&mut app, 0.6);
    let settled = player(&app).expect("player").current.origin;
    let mut travelled = 0.0_f32;
    let mut last = settled;
    for _ in 0..200 {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
        let now = player(&app).expect("player").current.origin;
        travelled += (now - last).length();
        last = now;
        let sim = sim(&app).expect("game");
        let hull = rl_sim::collision::Hull::from(&sim.data().movement.hulls.standing);
        let trace = sim.world().trace_hull(
            now,
            now,
            &hull,
            rl_sim::collision::contents::MASK_PLAYER_SOLID,
        );
        assert!(!trace.start_solid, "inside the map at {now:?}");
    }
    assert!(
        travelled < 1.0,
        "wandered {travelled} units from {settled:?}"
    );
}
