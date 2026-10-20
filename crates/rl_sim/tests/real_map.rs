//! Collision against a real map. Set `RUSTLIFE_TEST_BSP` to a `.bsp` path and
//! run with `--ignored`; skipped silently when the variable is unset.

use glam::Vec3;
use rl_sim::collision::contents::{MASK_PLAYER_SOLID, SOLID};
use rl_sim::collision::{CollisionWorld, Hull, find_free_position};

/// The map at `RUSTLIFE_TEST_BSP` and its spawn points, or `None` when the
/// variable is unset.
#[expect(
    clippy::expect_used,
    reason = "test helper: a map that fails to load must fail the test"
)]
fn load() -> Option<(CollisionWorld, Vec<[f32; 3]>)> {
    let path = std::env::var("RUSTLIFE_TEST_BSP").ok()?;
    let bytes = std::fs::read(&path).expect("read bsp");
    let collision = source_formats::collision::load(&bytes).expect("collision");
    let geometry = source_formats::bsp::load_geometry(&bytes).expect("geometry");
    let spawns = geometry.spawns.iter().map(|spawn| spawn.origin).collect();
    Some((CollisionWorld::new(collision), spawns))
}

#[test]
#[ignore = "requires a BSP file; set RUSTLIFE_TEST_BSP"]
fn every_spawn_stands_on_a_floor() {
    let Some((world, spawns)) = load() else {
        return;
    };
    let hull = Hull::new(Vec3::new(-16.0, -16.0, 0.0), Vec3::new(16.0, 16.0, 72.0));
    for spawn in spawns {
        // Mappers place some spawns overlapping nearby brushes; the game
        // nudges those free, as Source does.
        let start = find_free_position(&world, Vec3::from(spawn) + Vec3::Z, &hull)
            .unwrap_or_else(|| panic!("spawn {spawn:?} is stuck"));
        let trace = world.trace_hull(start, start - Vec3::Z * 256.0, &hull, MASK_PLAYER_SOLID);
        assert!(!trace.start_solid);
        assert!(trace.hit(), "spawn {spawn:?} has no floor");
    }
}

#[test]
#[ignore = "requires a BSP file; set RUSTLIFE_TEST_BSP"]
fn displacements_are_never_fallen_through() {
    let Some(path) = std::env::var("RUSTLIFE_TEST_BSP").ok() else {
        return;
    };
    let bytes = std::fs::read(&path).expect("read bsp");
    let collision = source_formats::collision::load(&bytes).expect("collision");
    let displacements = collision.displacements.clone();
    let world = CollisionWorld::new(collision);
    let mut checked = 0;
    for displacement in &displacements {
        for triangle in &displacement.triangles {
            let [a, b, c] = triangle.map(Vec3::from);
            let normal = (b - a).cross(c - a).normalize_or_zero();
            if normal.z.abs() < 0.5 {
                continue;
            }
            // Rays straight down through the surface must stop on it, or
            // above it where another brush covers the displacement.
            let center = (a + b + c) / 3.0;
            let trace = world.trace_ray(center + Vec3::Z * 4.0, center - Vec3::Z * 4.0, SOLID);
            checked += 1;
            assert!(
                trace.start_solid || (trace.hit() && trace.end.z > center.z - 0.5),
                "fell through the displacement at {center}: {trace:?}"
            );
        }
    }
    eprintln!("{checked} displacement samples hold");
}

/// Walks a player out of every spawn for a few seconds — forward, then
/// strafing, jumping and crouching — and checks they never get stuck in or
/// fall out of the world.
#[test]
#[ignore = "requires a BSP file; set RUSTLIFE_TEST_BSP"]
fn players_walk_out_of_every_spawn_without_leaving_the_world() {
    use rl_sim::movement::{Buttons, PlayerState, UserCmd, simulate};
    let Some((world, spawns)) = load() else {
        return;
    };
    let config = rl_data::GameData::builtin().expect("data").movement;
    let hull = Hull::from(&config.hulls.standing);
    let dt = 1.0 / 66.0;
    for (index, spawn) in spawns.iter().enumerate() {
        let origin = find_free_position(&world, Vec3::from(*spawn) + Vec3::Z, &hull)
            .expect("spawn has room");
        let mut state = PlayerState::at(origin);
        let lowest = spawn[2] - 4096.0;
        for tick in 0..330usize {
            let cmd = UserCmd {
                forward: 1.0,
                side: if tick > 132 {
                    (tick as f32 * 0.05).sin()
                } else {
                    0.0
                },
                yaw: index as f32 * 37.0 + tick as f32 * 0.3,
                pitch: 0.0,
                buttons: match tick % 66 {
                    0 => Buttons::JUMP,
                    30..=40 => Buttons::DUCK,
                    _ => Buttons::NONE,
                },
                select: None,
            };
            state = simulate(&state, &cmd, &config, dt, &world).0;
            let stuck = world
                .trace_hull(
                    state.origin,
                    state.origin,
                    &state.hull(&config),
                    MASK_PLAYER_SOLID,
                )
                .all_solid;
            assert!(!stuck, "spawn {index}: stuck at tick {tick}: {state:?}");
            assert!(
                state.origin.z > lowest,
                "spawn {index}: fell out of the world"
            );
        }
    }
}

/// The map's physics world builds in reasonable time, and a crate dropped
/// at each spawn lands near the floor the player would stand on.
#[test]
#[ignore = "requires a BSP file; set RUSTLIFE_TEST_BSP"]
fn crates_dropped_at_spawns_land_on_the_floor() {
    use rl_sim::collision::ConvexHull;
    use rl_sim::physics::{Motion, PhysicsWorld, PropDesc};

    let Some((world, spawns)) = load() else {
        return;
    };
    let config = rl_data::GameData::builtin().expect("builtin").physics;
    let started = std::time::Instant::now();
    let mut physics = PhysicsWorld::new(&world, &config, 1.0 / 66.0);
    eprintln!("built {physics:?} in {:?}", started.elapsed());
    let crates: Vec<_> = spawns
        .iter()
        .map(|&spawn| {
            physics.add_prop(&PropDesc {
                pieces: vec![ConvexHull::from_box(
                    Vec3::new(-8.0, -8.0, 0.0),
                    Vec3::splat(8.0) + Vec3::Z * 8.0,
                )],
                origin: Vec3::from(spawn) + Vec3::Z * 64.0,
                rotation: glam::Quat::IDENTITY,
                motion: Motion::Dynamic,
                mass: 10.0,
                material: *config.materials.get("wood"),
                linear_damping: 0.0,
                angular_damping: 0.0,
                debris: false,
                start_asleep: false,
                grabbable: true,
                free_on_grab: false,
            })
        })
        .collect();
    let started = std::time::Instant::now();
    for _ in 0..200 {
        physics.step();
    }
    eprintln!(
        "200 steps with {} crates in {:?}",
        crates.len(),
        started.elapsed()
    );
    let hull = Hull::new(Vec3::new(-8.0, -8.0, 0.0), Vec3::new(8.0, 8.0, 16.0));
    for (body, spawn) in crates.iter().zip(&spawns) {
        let rest = physics.pose(*body).expect("crate").position;
        let start = Vec3::from(*spawn) + Vec3::Z * 64.0;
        let floor = world.trace_hull(start, start - Vec3::Z * 512.0, &hull, SOLID);
        assert!(
            (rest.z - floor.end.z).abs() < 24.0,
            "crate from {spawn:?} rests at {rest}, floor at {}",
            floor.end
        );
    }
}

/// The map's weapon and item entities become pickups, its kill volumes
/// hazards, and players can join at its spawn points.
#[test]
#[ignore = "requires a BSP file; set RUSTLIFE_TEST_BSP"]
fn the_map_is_stocked_with_pickups() {
    use std::sync::Arc;

    use rl_sim::simulation::{Simulation, Spawn};

    let Some(path) = std::env::var("RUSTLIFE_TEST_BSP").ok() else {
        return;
    };
    let bytes = std::fs::read(&path).expect("read bsp");
    let collision = source_formats::collision::load(&bytes).expect("collision");
    let geometry = source_formats::bsp::load_geometry(&bytes).expect("geometry");
    let data = rl_data::GameData::builtin().expect("builtin");
    let mut sim = Simulation::new(Arc::new(CollisionWorld::new(collision)), &data);
    sim.set_spawns(
        geometry
            .spawns
            .iter()
            .map(|spawn| Spawn {
                origin: Vec3::from(spawn.origin),
                yaw: spawn.yaw,
            })
            .collect(),
    );
    sim.add_entities(&geometry.entities);
    eprintln!(
        "{} pickups, {} hazards",
        sim.pickups().len(),
        sim.hazards().len()
    );
    assert!(!sim.pickups().is_empty(), "a DM map has pickups");
    let players: Vec<_> = (0..4).map(|_| sim.join()).collect();
    for _ in 0..66 {
        sim.tick(&[]);
    }
    for id in players {
        assert!(sim.player(id).expect("player").alive());
    }
}
