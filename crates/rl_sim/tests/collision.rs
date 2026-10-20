//! Trace behaviour against synthetic worlds: brushes, displacements and the
//! invariants every trace must keep.

use glam::Vec3;
use proptest::prelude::*;
use rl_sim::collision::contents::{MASK_PLAYER_SOLID, SOLID, WATER};
use rl_sim::collision::{CollisionWorld, Hull, WorldBuilder};

/// The standing player hull.
fn player() -> Hull {
    Hull::new(Vec3::new(-16.0, -16.0, 0.0), Vec3::new(16.0, 16.0, 72.0))
}

/// A 1024² floor whose top is at z = 0, and a water pool.
fn floor_world() -> CollisionWorld {
    WorldBuilder::new()
        .solid_box(
            Vec3::new(-512.0, -512.0, -64.0),
            Vec3::new(512.0, 512.0, 0.0),
        )
        .box_of(
            WATER,
            Vec3::new(200.0, 200.0, 0.0),
            Vec3::new(300.0, 300.0, 50.0),
        )
        .build()
}

#[test]
fn a_falling_hull_lands_just_above_the_floor() {
    let world = floor_world();
    let trace = world.trace_hull(
        Vec3::new(0.0, 0.0, 100.0),
        Vec3::new(0.0, 0.0, -100.0),
        &player(),
        MASK_PLAYER_SOLID,
    );
    assert!(trace.hit());
    assert_eq!(trace.normal, Vec3::Z);
    assert!(trace.end.z > 0.0 && trace.end.z < 0.1, "{trace:?}");
    assert_eq!(trace.contents, SOLID);
}

#[test]
fn a_hull_resting_on_the_floor_is_not_stuck() {
    let world = floor_world();
    let rest = world.trace_hull(
        Vec3::new(0.0, 0.0, 100.0),
        Vec3::new(0.0, 0.0, -100.0),
        &player(),
        MASK_PLAYER_SOLID,
    );
    let slide = world.trace_hull(
        rest.end,
        rest.end + Vec3::new(64.0, 0.0, 0.0),
        &player(),
        MASK_PLAYER_SOLID,
    );
    assert!(!slide.start_solid && !slide.hit(), "{slide:?}");
}

#[test]
fn water_is_not_solid_but_has_contents() {
    let world = floor_world();
    let through = world.trace_hull(
        Vec3::new(150.0, 250.0, 10.0),
        Vec3::new(350.0, 250.0, 10.0),
        &player(),
        MASK_PLAYER_SOLID,
    );
    assert!(!through.hit());
    assert_eq!(world.point_contents(Vec3::new(250.0, 250.0, 25.0)), WATER);
    assert_eq!(world.point_contents(Vec3::new(250.0, 250.0, 60.0)), 0);
    assert_eq!(world.point_contents(Vec3::new(0.0, 0.0, -10.0)), SOLID);
}

#[test]
fn a_hull_inside_a_brush_is_all_solid() {
    let world = floor_world();
    let trace = world.trace_hull(
        Vec3::new(0.0, 0.0, -40.0),
        Vec3::new(10.0, 0.0, -40.0),
        &player(),
        MASK_PLAYER_SOLID,
    );
    assert!(trace.start_solid && trace.all_solid);
    assert_eq!(trace.fraction, 0.0);
}

#[test]
fn a_box_catches_the_edge_of_a_ramp_where_a_ray_passes() {
    // A ramp rising 64 units over 128 along +X.
    let world = WorldBuilder::new()
        .ramp(Vec3::new(0.0, -64.0, 0.0), Vec3::new(128.0, 64.0, 64.0))
        .build();
    // Just above the top edge: the ray passes, the box (whose bottom corner
    // reaches the slope first) hits the slope.
    let start = Vec3::new(-100.0, 0.0, 66.0);
    let end = Vec3::new(300.0, 0.0, 66.0);
    assert!(!world.trace_ray(start, end, SOLID).hit());
    let hull = Hull::new(Vec3::splat(-8.0), Vec3::splat(8.0));
    let trace = world.trace_hull(start, end, &hull, SOLID);
    assert!(trace.hit(), "{trace:?}");
}

/// A displacement: a 256² grid of triangles forming a hill
/// `z = 32 · sin(x / 64)`.
fn hill() -> Vec<[Vec3; 3]> {
    let point = |x: f32, y: f32| Vec3::new(x, y, 32.0 * (x / 64.0).sin());
    let step = 16.0;
    let mut triangles = Vec::new();
    for i in 0..16 {
        for j in 0..16 {
            let (x, y) = (i as f32 * step, j as f32 * step);
            let near = point(x, y);
            let right = point(x + step, y);
            let up = point(x, y + step);
            let far = point(x + step, y + step);
            triangles.push([near, right, up]);
            triangles.push([right, far, up]);
        }
    }
    triangles
}

#[test]
fn rays_land_on_displacement_surfaces() {
    let world = WorldBuilder::new().displacement(hill()).build();
    for x in [8.0f32, 40.0, 100.0, 200.0] {
        let trace = world.trace_ray(
            Vec3::new(x, 100.0, 200.0),
            Vec3::new(x, 100.0, -200.0),
            SOLID,
        );
        assert!(trace.hit(), "x = {x}");
        // The tessellated surface is within a few units of the curve.
        let expected = 32.0 * (x / 64.0).sin();
        assert!((trace.end.z - expected).abs() < 2.0, "x = {x}: {trace:?}");
        assert!(trace.normal.z > 0.4, "the surface faces up: {trace:?}");
    }
}

#[test]
fn hulls_rest_on_displacements_without_falling_through() {
    let world = WorldBuilder::new().displacement(hill()).build();
    let hull = player();
    let start = Vec3::new(128.0, 128.0, 200.0);
    let trace = world.trace_hull(start, start - Vec3::Z * 400.0, &hull, SOLID);
    assert!(trace.hit() && !trace.start_solid, "{trace:?}");
    // Resting there, a further trace down goes nowhere.
    let again = world.trace_hull(trace.end, trace.end - Vec3::Z * 10.0, &hull, SOLID);
    assert!(again.fraction < 0.01, "{again:?}");
}

/// The solid cube `-64..=64` on each axis, alone in the world.
fn cube_world() -> CollisionWorld {
    WorldBuilder::new()
        .solid_box(Vec3::splat(-64.0), Vec3::splat(64.0))
        .build()
}

/// Whether a hull at `origin` overlaps the cube's interior by more than
/// `tolerance`.
fn penetrates_cube(origin: Vec3, hull: &Hull, tolerance: f32) -> bool {
    let mins = origin + hull.mins;
    let maxs = origin + hull.maxs;
    (0..3).all(|axis| mins[axis] < 64.0 - tolerance && maxs[axis] > -64.0 + tolerance)
}

fn coordinate() -> impl Strategy<Value = f32> {
    -400.0f32..400.0
}

fn point() -> impl Strategy<Value = Vec3> {
    (coordinate(), coordinate(), coordinate()).prop_map(|(x, y, z)| Vec3::new(x, y, z))
}

proptest! {
    #[test]
    fn fractions_stay_within_the_unit_interval(start in point(), end in point(), size in 0.0f32..32.0) {
        let world = cube_world();
        let hull = Hull::new(Vec3::splat(-size), Vec3::splat(size));
        let trace = world.trace_hull(start, end, &hull, SOLID);
        prop_assert!((0.0..=1.0).contains(&trace.fraction));
        let along = start + (end - start) * trace.fraction;
        prop_assert!((trace.end - along).length() < 1e-2);
    }

    #[test]
    fn traces_from_outside_never_end_inside(start in point(), end in point(), size in 0.0f32..32.0) {
        let world = cube_world();
        let hull = Hull::new(Vec3::splat(-size), Vec3::splat(size));
        prop_assume!(!penetrates_cube(start, &hull, 0.0));
        let trace = world.trace_hull(start, end, &hull, SOLID);
        prop_assert!(!trace.start_solid);
        prop_assert!(!penetrates_cube(trace.end, &hull, 0.01), "{trace:?}");
    }

    #[test]
    fn chains_of_short_moves_never_end_inside(
        face in 0usize..6,
        gap in 0.0f32..0.05,
        across in (-60.0f32..60.0, -60.0f32..60.0),
        steps in proptest::collection::vec((point(), 0.0f32..0.2), 1..40),
    ) {
        // Movement makes many tiny traces from wherever the last one ended,
        // usually within the clip epsilon of a surface. Start resting just
        // off one of the cube's faces and nudge about: none may sink in.
        let world = cube_world();
        let hull = player();
        let (axis, sign) = (face / 2, if face % 2 == 0 { 1.0 } else { -1.0 });
        let mut position = Vec3::ZERO;
        let [u, v] = [(axis + 1) % 3, (axis + 2) % 3];
        position[u] = across.0;
        position[v] = across.1;
        // The hull's side facing the cube sits `gap` units off the face.
        position[axis] = if sign > 0.0 {
            64.0 + gap - hull.mins[axis]
        } else {
            -64.0 - gap - hull.maxs[axis]
        };
        for (direction, length) in steps {
            let end = position + direction.normalize_or_zero() * length;
            let trace = world.trace_hull(position, end, &hull, SOLID);
            prop_assert!(!trace.start_solid, "{trace:?}");
            position = trace.end;
            prop_assert!(!penetrates_cube(position, &hull, 0.0), "sank in at {position}");
        }
    }

    #[test]
    fn hits_report_a_unit_normal_facing_the_mover(start in point(), end in point()) {
        let world = cube_world();
        let trace = world.trace_hull(start, end, &player(), SOLID);
        if trace.hit() && !trace.start_solid {
            prop_assert!((trace.normal.length() - 1.0).abs() < 1e-4);
            prop_assert!(trace.normal.dot(end - start) <= 0.0);
        }
    }

    #[test]
    fn displacements_are_never_tunnelled(x in 20.0f32..236.0, y in 20.0f32..236.0, drop in 10.0f32..2000.0) {
        let world = WorldBuilder::new().displacement(hill()).build();
        let hull = Hull::new(Vec3::new(-16.0, -16.0, 0.0), Vec3::new(16.0, 16.0, 72.0));
        let start = Vec3::new(x, y, 80.0);
        let trace = world.trace_hull(start, start - Vec3::Z * drop, &hull, SOLID);
        if drop > 120.0 {
            prop_assert!(trace.hit(), "fell through: {trace:?}");
        }
        prop_assert!(trace.end.z > -40.0, "{trace:?}");
    }
}

#[test]
fn stuck_hulls_are_nudged_to_the_nearest_room() {
    let world = floor_world();
    let hull = player();
    // 3 units into the floor: the nearest free spot is straight up.
    let free = rl_sim::collision::find_free_position(&world, Vec3::new(0.0, 0.0, -3.0), &hull)
        .expect("room above the floor");
    assert!(free.z > 0.0 && free.z <= 1.0, "{free}");
    let already = Vec3::new(0.0, 0.0, 10.0);
    assert_eq!(
        rl_sim::collision::find_free_position(&world, already, &hull),
        Some(already)
    );
    assert_eq!(
        rl_sim::collision::find_free_position(&world, Vec3::new(0.0, 0.0, -40.0), &hull),
        None,
        "deep inside the floor there is no room nearby"
    );
}
