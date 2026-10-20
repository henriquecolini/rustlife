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
