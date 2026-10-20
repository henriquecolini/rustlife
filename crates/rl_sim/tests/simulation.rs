//! The whole simulation's tick: players spawn clear of props, are not
//! blocked by what they carry, and a run repeats exactly.

use std::sync::Arc;

use glam::{Quat, Vec3};
use rl_data::GameData;
use rl_sim::collision::{ConvexHull, WorldBuilder};
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::physics::{Motion, PropDesc};
use rl_sim::simulation::Simulation;

#[expect(
    clippy::expect_used,
    reason = "broken built-in data must fail the test"
)]
fn data() -> GameData {
    GameData::builtin().expect("builtin data")
}

fn crate_at(origin: Vec3, motion: Motion) -> PropDesc {
    PropDesc {
        pieces: vec![ConvexHull::from_box(
            Vec3::new(-8.0, -8.0, 0.0),
            Vec3::new(8.0, 8.0, 16.0),
        )],
        origin,
        rotation: Quat::IDENTITY,
        motion,
        mass: 10.0,
        material: *data().physics.materials.get("wood"),
        linear_damping: 0.0,
        angular_damping: 0.0,
        debris: false,
        start_asleep: false,
        grabbable: true,
        free_on_grab: false,
    }
}

fn room() -> Simulation {
    let world = WorldBuilder::new()
        .solid_box(
            Vec3::new(-4096.0, -4096.0, -64.0),
            Vec3::new(4096.0, 4096.0, 0.0),
        )
        .build();
    Simulation::new(Arc::new(world), &data())
}

#[test]
fn players_spawning_inside_a_prop_are_nudged_free() {
    let mut sim = room();
    sim.add_prop(&crate_at(Vec3::ZERO, Motion::Fixed));
    sim.tick(&[]);
    let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
    let origin = sim.player(player).expect("player").state.origin;
    assert!(
        origin != Vec3::new(0.0, 0.0, 1.0),
        "moved off the crate's spot"
    );
    assert!(
        origin.z >= 16.0 || origin.truncate().abs().max_element() >= 24.0,
        "{origin}"
    );
}

#[test]
fn a_carried_prop_does_not_block_its_holder() {
    let mut sim = room();
    let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
    let prop = sim.add_prop(&crate_at(Vec3::new(60.0, 0.0, 0.0), Motion::Dynamic));
    for _ in 0..30 {
        sim.tick(&[(player, UserCmd::default())]);
    }
    // Aim at the crate and grab it.
    let grab = UserCmd {
        pitch: 45.0,
        buttons: Buttons::ATTACK2,
        ..UserCmd::default()
    };
    sim.tick(&[(player, grab)]);
    assert_eq!(
        sim.player(player).expect("player").manipulator.held(),
        Some(prop)
    );
    // Walk forward for two seconds, carrying it.
    let walk = UserCmd {
        forward: 1.0,
        ..UserCmd::default()
    };
    for _ in 0..132 {
        sim.tick(&[(player, walk)]);
    }
    let state = sim.player(player).expect("player").state;
    assert!(state.origin.x > 400.0, "walked on, at {}", state.origin);
    let center = sim.physics().center_of_mass(prop).expect("prop");
    assert!(
        center.x > state.origin.x,
        "the crate stayed in front: {center}"
    );
}

#[test]
fn a_run_repeats_exactly() {
    let run = || {
        let mut sim = room();
        let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
        let props: Vec<_> = (0..4)
            .map(|i| {
                sim.add_prop(&crate_at(
                    Vec3::new(64.0, i as f32 * 20.0, 40.0 * i as f32),
                    Motion::Dynamic,
                ))
            })
            .collect();
        for tick in 0..200 {
            let cmd = UserCmd {
                forward: 1.0,
                yaw: tick as f32,
                buttons: if tick % 50 == 0 {
                    Buttons::ATTACK
                } else {
                    Buttons::NONE
                },
                ..UserCmd::default()
            };
            sim.tick(&[(player, cmd)]);
        }
        let poses: Vec<_> = props.iter().map(|&body| sim.physics().pose(body)).collect();
        (sim.player(player).expect("player").state, poses)
    };
    assert_eq!(run(), run());
}
