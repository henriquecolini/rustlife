//! Trace throughput. Run with `cargo bench -p rl_sim`; set `RUSTLIFE_TEST_BSP`
//! to a map to also measure traces through a real BSP tree.

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

criterion_group!(benches, bench_room, bench_real_map);
criterion_main!(benches);
