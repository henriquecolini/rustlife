//! Trace, movement and simulation throughput. Run with `cargo bench -p
//! rl_sim`; set `RUSTLIFE_TEST_BSP` to a map to also measure traces through a
//! real BSP tree.

#![allow(missing_docs)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use glam::Vec3;
use rl_sim::collision::contents::MASK_PLAYER_SOLID;
use rl_sim::collision::{CollisionWorld, Hull, WorldBuilder};

fn player() -> Hull {
    Hull::new(Vec3::new(-16.0, -16.0, 0.0), Vec3::new(16.0, 16.0, 72.0))
}

/// A room: floor, four walls, a ramp and a few crates.
fn room() -> CollisionWorld {
    let mut builder = WorldBuilder::new()
        .solid_box(
            Vec3::new(-1024.0, -1024.0, -16.0),
            Vec3::new(1024.0, 1024.0, 0.0),
        )
        .solid_box(
            Vec3::new(-1040.0, -1024.0, 0.0),
            Vec3::new(-1024.0, 1024.0, 256.0),
        )
        .solid_box(
            Vec3::new(1024.0, -1024.0, 0.0),
            Vec3::new(1040.0, 1024.0, 256.0),
        )
        .solid_box(
            Vec3::new(-1024.0, -1040.0, 0.0),
            Vec3::new(1024.0, -1024.0, 256.0),
        )
        .solid_box(
            Vec3::new(-1024.0, 1024.0, 0.0),
            Vec3::new(1024.0, 1040.0, 256.0),
        )
        .ramp(Vec3::new(0.0, -512.0, 0.0), Vec3::new(512.0, -256.0, 128.0));
    for i in 0..8 {
        let x = -800.0 + i as f32 * 200.0;
        builder = builder.solid_box(Vec3::new(x, 300.0, 0.0), Vec3::new(x + 64.0, 364.0, 64.0));
    }
    builder.build()
}

/// A spread of short, movement-sized traces across the room.
fn movement_traces() -> Vec<(Vec3, Vec3)> {
    (0..64)
        .map(|i| {
            let angle = i as f32 * 0.7;
            let start = Vec3::new(angle.cos() * 700.0, angle.sin() * 700.0, 1.0);
            (
                start,
                start + Vec3::new(angle.sin() * 5.0, angle.cos() * 5.0, -2.0),
            )
        })
        .collect()
}

fn bench_room(c: &mut Criterion) {
    let world = room();
    let traces = movement_traces();
    let hull = player();
    c.bench_function("room: 64 movement hull traces", |b| {
        b.iter(|| {
            for &(start, end) in &traces {
                black_box(world.trace_hull(start, end, &hull, MASK_PLAYER_SOLID));
            }
        });
    });
}

fn bench_real_map(c: &mut Criterion) {
    let Ok(path) = std::env::var("RUSTLIFE_TEST_BSP") else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let Ok(collision) = source_formats::collision::load(&bytes) else {
        return;
    };
    let Ok(geometry) = source_formats::bsp::load_geometry(&bytes) else {
        return;
    };
    let world = CollisionWorld::new(collision);
    let hull = player();
    let spawns: Vec<Vec3> = geometry
        .spawns
        .iter()
        .map(|spawn| Vec3::from(spawn.origin) + Vec3::Z * 2.0)
        .collect();
    c.bench_function(
        "real map: hull trace from each spawn, 8 units forward",
        |b| {
            b.iter(|| {
                for &start in &spawns {
                    black_box(world.trace_hull(
                        start,
                        start + Vec3::X * 8.0,
                        &hull,
                        MASK_PLAYER_SOLID,
                    ));
                }
            });
        },
    );
    c.bench_function("real map: 4096-unit ray down from each spawn", |b| {
        b.iter(|| {
            for &start in &spawns {
                black_box(world.trace_ray(start, start - Vec3::Z * 4096.0, MASK_PLAYER_SOLID));
            }
        });
    });
}

fn bench_movement(c: &mut Criterion) {
    use rl_sim::movement::{PlayerState, UserCmd, simulate};
    let world = room();
    let Ok(data) = rl_data::GameData::builtin() else {
        return;
    };
    let config = data.movement;
    let mut state = PlayerState::at(Vec3::new(-600.0, -600.0, 1.0));
    let cmd = UserCmd {
        forward: 1.0,
        side: 0.3,
        yaw: 45.0,
        ..UserCmd::default()
    };
    c.bench_function("movement: one tick running through the room", |b| {
        b.iter(|| {
            let (next, _) = simulate(black_box(&state), &cmd, &config, 1.0 / 66.0, &world);
            // Wrap around the room so every tick does real work.
            state = if next.origin.x > 600.0 {
                PlayerState::at(Vec3::new(-600.0, -600.0, 1.0))
            } else {
                next
            };
        });
    });
}

/// A whole simulation tick: one player running into a field of 64 tumbling
/// crates, with pushing and physics.
fn bench_simulation(c: &mut Criterion) {
    use std::sync::Arc;

    use glam::Quat;
    use rl_sim::collision::ConvexHull;
    use rl_sim::movement::UserCmd;
    use rl_sim::physics::{Motion, PropDesc};
    use rl_sim::simulation::Simulation;

    let Ok(data) = rl_data::GameData::builtin() else {
        return;
    };
    let fresh = || {
        let mut sim = Simulation::new(Arc::new(room()), &data);
        let player = sim.spawn_player(Vec3::new(-600.0, -600.0, 1.0));
        for i in 0..64 {
            let (x, y) = ((i % 8) as f32 * 40.0, (i / 8) as f32 * 40.0);
            sim.add_prop(&PropDesc {
                pieces: vec![ConvexHull::from_box(Vec3::splat(-8.0), Vec3::splat(8.0))],
                origin: Vec3::new(x - 400.0, y - 400.0, 8.0 + (i % 3) as f32 * 20.0),
                rotation: Quat::IDENTITY,
                motion: Motion::Dynamic,
                mass: 10.0,
                material: *data.physics.materials.get("wood"),
                linear_damping: 0.0,
                angular_damping: 0.0,
                debris: false,
                start_asleep: false,
                grabbable: true,
                free_on_grab: false,
            });
        }
        (sim, player)
    };
    let (mut sim, mut player) = fresh();
    let cmd = UserCmd {
        forward: 1.0,
        yaw: 45.0,
        ..UserCmd::default()
    };
    let mut ticks = 0;
    c.bench_function("simulation: one tick with 64 crates", |b| {
        b.iter(|| {
            black_box(sim.tick(&[(player, cmd)]));
            ticks += 1;
            // Start over before everything has been shoved into a corner
            // and gone to sleep.
            if ticks == 300 {
                (sim, player) = fresh();
                ticks = 0;
            }
        });
    });
}

criterion_group!(
    benches,
    bench_room,
    bench_real_map,
    bench_movement,
    bench_simulation
);
criterion_main!(benches);
