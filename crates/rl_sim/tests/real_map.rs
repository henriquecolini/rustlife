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
