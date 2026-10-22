//! Physical pickups through the whole simulation: a medkit falls to the
//! floor and knocks on it, taking it hides its body until it respawns where
//! it was placed, and the Manipulator can grab and throw it without taking
//! it.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a missing body or player must fail the test"
)]

use std::sync::Arc;

use glam::{Quat, Vec3};
use rl_data::GameData;
use rl_sim::collision::{ConvexHull, WorldBuilder};
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::physics::{BodyId, Motion, PropDesc};
use rl_sim::simulation::{PlayerId, Simulation};
use rl_sim::tools::manipulator::ToolEvent;
use source_formats::entities::MapEntity;

#[expect(
    clippy::expect_used,
    reason = "broken built-in data must fail the test"
)]
fn data() -> GameData {
    let mut data = GameData::builtin().expect("builtin data");
    // Matches start the moment the players are there.
    data.rules.warmup = 0.0;
    data
}

/// A big floor with its top at z = 0.
fn floor() -> rl_sim::collision::CollisionWorld {
    WorldBuilder::new()
        .solid_box(
            Vec3::new(-4096.0, -4096.0, -64.0),
            Vec3::new(4096.0, 4096.0, 0.0),
        )
        .build()
}

/// A small box-shaped pickup body standing on `origin`, lying as debris:
/// the shape of a medkit's fall, without needing the game's models.
fn pickup_desc(origin: Vec3) -> PropDesc {
    PropDesc {
        pieces: vec![ConvexHull::from_box(
            Vec3::new(-8.0, -8.0, 0.0),
            Vec3::new(8.0, 8.0, 16.0),
        )],
        origin,
        rotation: Quat::IDENTITY,
        motion: Motion::Dynamic,
        mass: 5.0,
        material: *data().physics.materials.get("wood"),
        linear_damping: 0.0,
        angular_damping: 0.0,
        debris: true,
        start_asleep: false,
        grabbable: true,
        free_on_grab: false,
        breakable: None,
        surface: "metal".to_owned(),
    }
}

/// Adds a medkit at `origin` and gives it a body.
fn add_medkit(sim: &mut Simulation, origin: Vec3) -> BodyId {
    sim.add_entities(&[MapEntity::new("item_healthkit", origin.to_array())]);
    sim.add_pickup_bodies([Some(pickup_desc(origin))]);
    sim.pickups()
        .last()
        .expect("the medkit")
        .body
        .expect("the medkit lies about")
}

/// The pitch that aims the player's eyes at `target`.
fn aim_pitch(sim: &Simulation, player: PlayerId, target: Vec3) -> f32 {
    let state = sim.player(player).expect("player").state;
    let eye = state.origin + Vec3::Z * 64.0;
    let to = target - eye;
    -(to.z.atan2(to.truncate().length())).to_degrees()
}

#[test]
fn a_pickup_falls_to_the_floor_and_its_landing_is_heard() {
    let data = data();
    let mut sim = Simulation::new(Arc::new(floor()), &data);
    let body = add_medkit(&mut sim, Vec3::new(100.0, 0.0, 200.0));
    let start = sim.physics().pose(body).expect("body").position;
    assert_eq!(start, Vec3::new(100.0, 0.0, 200.0));

    let mut impacts = 0;
    for _ in 0..240 {
        impacts += sim.tick(&[]).prop_impacts.len();
    }

    let landed = sim.physics().pose(body).expect("body").position;
    assert!(landed.z < 1.0, "the medkit landed at {landed:?}");
    assert!(impacts > 0, "the medkit's landing made no sound");
    // Its origin follows its body, so touches and the client agree on where
    // it is.
    let pickup = &sim.pickups()[0];
    assert_eq!(pickup.origin, landed);
}

#[test]
fn taking_a_pickup_hides_its_body_until_it_respawns_at_home() {
    let mut data = data();
    data.items.get_mut("medkit").expect("medkit").respawn = 0.2;
    let mut sim = Simulation::new(Arc::new(floor()), &data);
    let home = Vec3::new(20.0, 0.0, 0.0);
    let body = add_medkit(&mut sim, home);
    let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
    // The match starts on the first tick.
    sim.tick(&[]);
    sim.player_mut(player).expect("player").vitals.health = 50.0;

    let events = sim.tick(&[]);
    assert!(
        events
            .pickups
            .iter()
            .any(|&(who, index)| who == player && index == 0),
        "the hurt player standing on the medkit took it: {events:?}"
    );
    assert_eq!(sim.player(player).expect("player").vitals.health, 75.0);
    let pickup = &sim.pickups()[0];
    assert!(!pickup.available(), "the taken medkit is still there");
    assert!(
        !sim.physics().prop_enabled(body),
        "the taken medkit's body is still in the world"
    );

    // A full player does not take it again when it comes back.
    sim.player_mut(player).expect("player").vitals.health = 100.0;
    for _ in 0..30 {
        sim.tick(&[]);
    }
    let pickup = &sim.pickups()[0];
    assert!(pickup.available(), "the medkit did not come back");
    assert!(
        sim.physics().prop_enabled(body),
        "the returned medkit's body is still out of the world"
    );
    let pose = sim.physics().pose(body).expect("body");
    assert!(
        pose.position.distance(home) < 0.1,
        "the medkit came back at {:?}, not home",
        pose.position
    );
    assert_eq!(pickup.origin, pose.position);
}

#[test]
fn a_pickup_can_be_grabbed_without_being_taken() {
    let data = data();
    let mut sim = Simulation::new(Arc::new(floor()), &data);
    let body = add_medkit(&mut sim, Vec3::new(100.0, 0.0, 0.0));
    let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
    sim.tick(&[]);
    let manipulator = sim.arsenal().find_weapon("manipulator");
    assert!(
        manipulator.is_some(),
        "the built-in data has the Manipulator"
    );
    sim.tick(&[(
        player,
        UserCmd {
            select: manipulator,
            ..UserCmd::default()
        },
    )]);
    for _ in 0..30 {
        sim.tick(&[(player, UserCmd::default())]);
    }

    let center = sim.physics().center_of_mass(body).expect("body");
    let aim = UserCmd {
        pitch: aim_pitch(&sim, player, center),
        buttons: Buttons::ATTACK2,
        ..UserCmd::default()
    };
    let events = sim.tick(&[(player, aim)]);
    assert!(
        events
            .tools
            .iter()
            .any(|(_, event)| matches!(event, ToolEvent::Grabbed(grabbed) if *grabbed == body)),
        "the Manipulator did not grab the medkit: {events:?}"
    );
    let held = sim.player(player).expect("player").manipulator.held();
    assert_eq!(held, Some(body));

    // Touching the medkit while it is held must not take it out of the
    // Manipulator's grip.
    sim.player_mut(player).expect("player").vitals.health = 50.0;
    let events = sim.tick(&[(player, UserCmd::default())]);
    assert!(
        events.pickups.is_empty(),
        "a held medkit was taken: {events:?}"
    );
    assert_eq!(sim.player(player).expect("player").vitals.health, 50.0);
    assert!(sim.pickups()[0].available());
    assert_eq!(
        sim.player(player).expect("player").manipulator.held(),
        Some(body)
    );

    // And it can be thrown.
    sim.player_mut(player).expect("player").vitals.health = 100.0;
    let events = sim.tick(&[(
        player,
        UserCmd {
            pitch: aim.pitch,
            buttons: Buttons::ATTACK,
            ..UserCmd::default()
        },
    )]);
    assert!(
        events
            .tools
            .iter()
            .any(|(_, event)| matches!(event, ToolEvent::Launched(launched) if *launched == body)),
        "the Manipulator did not throw the medkit: {events:?}"
    );
    assert_eq!(sim.player(player).expect("player").manipulator.held(), None);
}
