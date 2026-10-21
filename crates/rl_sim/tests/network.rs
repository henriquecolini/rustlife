//! What the simulation offers networking: players leaving and joining in
//! their place, several commands in one tick, lag-compensated shots,
//! replicas written from outside, and prediction that agrees with the
//! server.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a missing player or weapon must fail the test"
)]

use std::sync::Arc;

use glam::{Quat, Vec3};
use rl_data::GameData;
use rl_sim::collision::contents::MASK_PLAYER_SOLID;
use rl_sim::collision::{CollisionQuery, ConvexHull, Hull, WorldBuilder};
use rl_sim::combat::hitscan::Struck;
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::physics::{BodyId, BodyPose, Motion, PropDesc};
use rl_sim::scene::Scene;
use rl_sim::simulation::{PlayerId, Simulation};

fn data() -> GameData {
    let mut data = GameData::builtin().expect("builtin data");
    // Matches start the moment the players are there.
    data.rules.warmup = 0.0;
    data
}

/// A big floor with its top at z = 0.
fn floor() -> Arc<rl_sim::collision::CollisionWorld> {
    Arc::new(
        WorldBuilder::new()
            .solid_box(
                Vec3::new(-4096.0, -4096.0, -64.0),
                Vec3::new(4096.0, 4096.0, 0.0),
            )
            .build(),
    )
}

fn crate_desc(data: &GameData, origin: Vec3) -> PropDesc {
    PropDesc {
        pieces: vec![ConvexHull::from_box(
            Vec3::new(-16.0, -16.0, 0.0),
            Vec3::new(16.0, 16.0, 32.0),
        )],
        origin,
        rotation: Quat::IDENTITY,
        motion: Motion::Dynamic,
        mass: 20.0,
        material: *data.physics.materials.get("wood"),
        linear_damping: 0.0,
        angular_damping: 0.0,
        debris: false,
        start_asleep: true,
        grabbable: true,
        free_on_grab: false,
        breakable: None,
        surface: String::new(),
    }
}

/// Gives `player` the weapon `name` with plenty of ammo, in hand and
/// ready.
fn arm(sim: &mut Simulation, player: PlayerId, name: &str) {
    let arsenal = sim.arsenal().clone();
    let weapon = arsenal.find_weapon(name).expect("weapon");
    let ammo = arsenal
        .weapon(weapon)
        .and_then(rl_data::WeaponConfig::ammo_type)
        .and_then(|ammo| arsenal.find_ammo(ammo));
    let inventory = &mut sim.player_mut(player).expect("player").inventory;
    inventory.give_weapon(&arsenal, weapon);
    if let Some(ammo) = ammo {
        inventory.give_ammo(&arsenal, ammo, 1000);
    }
    let select = UserCmd {
        select: Some(weapon),
        ..UserCmd::default()
    };
    sim.tick(&[(player, select)]);
    idle(sim, player, 66);
}

/// Runs `ticks` ticks with `player` standing still (their weapon deploys
/// and cools down only as their commands come in).
fn idle(sim: &mut Simulation, player: PlayerId, ticks: usize) {
    for _ in 0..ticks {
        sim.tick(&[(player, UserCmd::default())]);
    }
}

#[test]
fn a_player_who_leaves_gives_their_id_to_the_next() {
    let data = data();
    let mut sim = Simulation::new(floor(), &data);
    let first = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
    let second = sim.spawn_player(Vec3::new(100.0, 0.0, 1.0));
    assert!(sim.leave(first));
    assert!(!sim.leave(first), "already gone");
    assert!(sim.player(first).is_none());
    assert_eq!(
        sim.players().map(|(id, _)| id).collect::<Vec<_>>(),
        [second]
    );
    sim.tick(&[]);

    let third = sim.spawn_player(Vec3::new(-100.0, 0.0, 1.0));
    assert_eq!(third, first, "the free slot is reused");
    assert_eq!(sim.game().score(third).points, 0);
    assert_eq!(sim.players().count(), 2);
    let ids: Vec<PlayerId> = sim.game().scores().map(|(id, _)| id).collect();
    assert_eq!(ids, [first, second]);
}

#[test]
fn several_commands_in_a_tick_move_a_player_several_times() {
    let data = data();
    let run = |batches: &[usize]| {
        let mut sim = Simulation::new(floor(), &data);
        let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
        sim.tick(&[]);
        let walk = UserCmd {
            forward: 1.0,
            ..UserCmd::default()
        };
        for &count in batches {
            let commands = vec![(player, walk); count];
            sim.tick(&commands);
        }
        sim.player(player).expect("player").state
    };
    // Twelve commands arriving one a tick, or in uneven bunches, end in
    // the same place.
    let steady = run(&[1; 12]);
    let bunched = run(&[0, 3, 0, 0, 5, 1, 0, 3]);
    assert_eq!(steady, bunched);
    assert!(steady.origin.x > 10.0, "{:?}", steady.origin);
}

#[test]
fn spread_follows_the_command_number() {
    let data = data();
    let mut sim = Simulation::new(floor(), &data);
    let shooter = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
    arm(&mut sim, shooter, "scattergun");
    let fire = |sim: &mut Simulation, number: u32| {
        let cmd = UserCmd {
            buttons: Buttons::ATTACK,
            number,
            ..UserCmd::default()
        };
        let events = sim.tick(&[(shooter, cmd)]);
        idle(sim, shooter, 132);
        events
            .shots
            .iter()
            .map(|shot| shot.trace.end)
            .collect::<Vec<_>>()
    };
    let first = fire(&mut sim, 7);
    let again = fire(&mut sim, 7);
    let other = fire(&mut sim, 8);
    assert!(!first.is_empty());
    assert_eq!(first, again, "the same number spreads the same");
    assert_ne!(first, other);
}

/// A shooter with a magnum at the origin facing +X, and a runner who
/// stood 300 units ahead on the returned tick and has since strafed for
/// `ticks` ticks.
fn runner_scene(data: &GameData, ticks: usize) -> (Simulation, PlayerId, PlayerId, u64) {
    let mut sim = Simulation::new(floor(), data);
    let shooter = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
    let runner = sim.spawn_player(Vec3::new(300.0, 0.0, 1.0));
    arm(&mut sim, shooter, "magnum");
    let seen = sim.tick_count();
    let strafe = UserCmd {
        side: 1.0,
        ..UserCmd::default()
    };
    for _ in 0..ticks {
        sim.tick(&[(runner, strafe), (shooter, UserCmd::default())]);
    }
    (sim, shooter, runner, seen)
}

/// Fires the shooter's magnum seeing the world at `view_tick`; whether it
/// hit the runner, and whether they were hurt.
fn fire_at_runner(
    data: &GameData,
    ticks: usize,
    view_tick: impl Fn(u64) -> Option<f64>,
) -> (bool, bool) {
    let (mut sim, shooter, runner, seen) = runner_scene(data, ticks);
    let health = |sim: &Simulation| sim.player(runner).expect("runner").vitals.health;
    let before = health(&sim);
    let cmd = UserCmd {
        buttons: Buttons::ATTACK,
        number: 1,
        view_tick: view_tick(seen),
        ..UserCmd::default()
    };
    let events = sim.tick(&[(shooter, cmd)]);
    let hit = events
        .shots
        .iter()
        .any(|shot| shot.trace.struck == Struck::Player(runner));
    (hit, health(&sim) < before)
}

#[test]
fn shots_hit_players_where_the_shooter_saw_them() {
    let data = data();
    let (sim, _, runner, _) = runner_scene(&data, 33);
    let runner_y = sim.player(runner).expect("runner").state.origin.y;
    assert!(runner_y.abs() > 64.0, "the runner moved aside: {runner_y}");

    assert_eq!(
        fire_at_runner(&data, 33, |_| None),
        (false, false),
        "aiming where the runner no longer is"
    );
    assert_eq!(
        fire_at_runner(&data, 33, |seen| Some(seen as f64)),
        (true, true),
        "aiming where the shooter saw the runner"
    );
}

#[test]
fn rewinding_stops_at_the_window() {
    let mut data = data();
    data.net.max_rewind = 0.1;
    assert_eq!(
        fire_at_runner(&data, 33, |seen| Some(seen as f64)),
        (false, false),
        "half a second ago is beyond a tenth of a second's window"
    );
}

/// The server and a client's replica of one map, the client's player the
/// same in both.
struct Pair {
    server: Simulation,
    client: Simulation,
    player: PlayerId,
}

impl Pair {
    fn new(data: &GameData) -> Self {
        let world = floor();
        let mut server = Simulation::new(world.clone(), data);
        let mut client = Simulation::new(world, data);
        for sim in [&mut server, &mut client] {
            sim.add_prop(&crate_desc(data, Vec3::new(200.0, 0.0, 0.0)));
            sim.refresh_props();
        }
        let player = server.spawn_player(Vec3::new(0.0, 0.0, 1.0));
        // The match starts on the first tick, putting the player back at
        // their spawn.
        server.tick(&[]);
        let mut pair = Self {
            server,
            client,
            player,
        };
        pair.sync();
        pair
    }

    /// Writes the server's props and player into the client's replica, as
    /// a snapshot would.
    fn sync(&mut self) {
        let poses: Vec<_> = self
            .server
            .physics()
            .props()
            .filter_map(|body| Some((body, self.server.physics().pose(body)?)))
            .collect();
        self.client.set_prop_poses(poses);
        let authority = self.server.player(self.player).expect("player").clone();
        let replica = self.client.replica_player(self.player);
        replica.state = authority.state;
        replica.inventory = authority.inventory;
        replica.vitals = authority.vitals;
        replica.life = authority.life;
        replica.look = authority.look;
        self.client.set_tick_count(self.server.tick_count());
    }
}

#[test]
fn prediction_agrees_with_the_server() {
    let data = data();
    let mut pair = Pair::new(&data);
    let smg = pair.server.arsenal().find_weapon("smg").expect("smg");
    let commands: Vec<UserCmd> = (1..=200u32)
        .map(|number| UserCmd {
            forward: if number % 70 < 50 { 1.0 } else { -1.0 },
            side: (number as f32 * 0.07).sin(),
            yaw: number as f32 * 0.9,
            pitch: 5.0,
            buttons: match number % 40 {
                0 => Buttons::JUMP,
                10..=20 => Buttons::ATTACK,
                25..=28 => Buttons::DUCK,
                _ => Buttons::NONE,
            },
            select: (number == 1).then_some(smg),
            number,
            view_tick: None,
        })
        .collect();
    let mut predicted_shots = Vec::new();
    let mut server_shots = Vec::new();
    for cmd in &commands {
        let prediction = pair.client.predict(pair.player, cmd);
        predicted_shots.extend(prediction.shots.iter().map(|shot| shot.trace.end));
        let events = pair.server.tick(&[(pair.player, *cmd)]);
        server_shots.extend(events.shots.iter().map(|shot| shot.trace.end));
        let authority = pair.server.player(pair.player).expect("server");
        let replica = pair.client.player(pair.player).expect("client");
        assert_eq!(replica.state, authority.state, "command {}", cmd.number);
        assert_eq!(
            replica.inventory, authority.inventory,
            "command {}",
            cmd.number
        );
        // The props the player pushes move on the server; the client
        // hears where to.
        pair.sync();
    }
    assert!(!server_shots.is_empty());
    assert_eq!(predicted_shots, server_shots);
}

#[test]
fn prediction_hurts_nobody() {
    let data = data();
    let mut pair = Pair::new(&data);
    let victim = PlayerId::from_index(1);
    pair.client.replica_player(victim).state.origin = Vec3::new(150.0, 0.0, 1.0);
    let smg = pair.client.arsenal().find_weapon("smg").expect("smg");
    let select = UserCmd {
        select: Some(smg),
        number: 1,
        ..UserCmd::default()
    };
    pair.client.predict(pair.player, &select);
    let before = pair.client.player(victim).expect("victim").vitals.health;
    for number in 2..60 {
        let fire = UserCmd {
            buttons: Buttons::ATTACK,
            number,
            ..UserCmd::default()
        };
        pair.client.predict(pair.player, &fire);
    }
    assert_eq!(
        pair.client.player(victim).expect("victim").vitals.health,
        before
    );
}

#[test]
fn replica_props_collide_where_the_server_put_them() {
    let data = data();
    let mut sim = Simulation::new(floor(), &data);
    let body = sim.add_prop(&crate_desc(&data, Vec3::new(200.0, 0.0, 0.0)));
    sim.refresh_props();
    let hull = Hull::new(Vec3::splat(-1.0), Vec3::splat(1.0));
    let blocked = |sim: &Simulation, y: f32| {
        let scene = Scene {
            world: sim.world(),
            physics: sim.physics(),
            ignore: None,
        };
        scene
            .trace_hull(
                Vec3::new(0.0, y, 16.0),
                Vec3::new(400.0, y, 16.0),
                &hull,
                MASK_PLAYER_SOLID,
            )
            .hit()
    };
    assert!(blocked(&sim, 0.0));
    assert!(!blocked(&sim, 500.0));
    sim.set_prop_poses([(
        body,
        BodyPose {
            position: Vec3::new(200.0, 500.0, 0.0),
            rotation: Quat::IDENTITY,
        },
    )]);
    assert!(!blocked(&sim, 0.0));
    assert!(blocked(&sim, 500.0));
    assert_eq!(
        sim.physics().pose(body).map(|pose| pose.position),
        Some(Vec3::new(200.0, 500.0, 0.0))
    );
    // Players' bodies are not props: placing one does nothing.
    sim.set_prop_poses([(
        BodyId::from_index(99),
        BodyPose {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
        },
    )]);
}
