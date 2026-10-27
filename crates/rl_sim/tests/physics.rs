//! Prop physics scenarios in synthetic rooms: props fall and settle, roll
//! down slopes, block and carry players, get pushed by them, hurt them when
//! they hit hard, and behave as configured when held, debris or frozen.

use glam::{Quat, Vec3};
use rl_data::{GameData, MovementConfig, PhysicsConfig};
use rl_sim::collision::{ConvexHull, Hull, WorldBuilder, contents};
use rl_sim::movement::{PlayerState, UserCmd, simulate};
use rl_sim::physics::{BodyId, JointDesc, JointKind, Motion, PhysicsWorld, PropDesc};
use rl_sim::scene::Scene;

/// One 66 Hz tick.
const DT: f32 = 1.0 / 66.0;

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

fn physics_config() -> PhysicsConfig {
    data().physics
}

fn movement_config() -> MovementConfig {
    data().movement
}

/// A huge floor with its top at z = 0.
fn floor() -> WorldBuilder {
    WorldBuilder::new().solid_box(
        Vec3::new(-4096.0, -4096.0, -64.0),
        Vec3::new(4096.0, 4096.0, 0.0),
    )
}

/// A cube of half-size `half` resting its base at `origin`.
fn crate_at(origin: Vec3, half: f32, mass: f32, motion: Motion) -> PropDesc {
    PropDesc {
        pieces: vec![ConvexHull::from_box(
            Vec3::new(-half, -half, 0.0),
            Vec3::new(half, half, 2.0 * half),
        )],
        origin,
        rotation: Quat::IDENTITY,
        motion,
        mass,
        material: *physics_config().materials.get("wood"),
        linear_damping: 0.0,
        angular_damping: 0.0,
        debris: false,
        start_asleep: false,
        grabbable: true,
        free_on_grab: false,
        breakable: None,
        surface: String::new(),
    }
}

fn run(physics: &mut PhysicsWorld, ticks: usize) {
    for _ in 0..ticks {
        physics.step();
    }
}

fn position(physics: &PhysicsWorld, body: BodyId) -> Vec3 {
    physics.pose(body).map_or(Vec3::NAN, |pose| pose.position)
}

#[test]
fn a_dropped_crate_settles_on_the_floor() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let body = physics.add_prop(&crate_at(
        Vec3::new(0.0, 0.0, 100.0),
        16.0,
        20.0,
        Motion::Dynamic,
    ));
    run(&mut physics, 200);
    let rest = position(&physics, body);
    assert!(rest.z.abs() < 1.0, "base rests on the floor, got {rest}");
    assert!(
        rest.truncate().length() < 1.0,
        "fell straight down, got {rest}"
    );
    let (velocity, _) = physics.velocity(body).expect("body");
    assert!(velocity.length() < 1.0, "at rest, got {velocity}");
}

#[test]
fn props_slide_down_a_steep_slope() {
    // A 60° ramp rising along +X.
    let world = floor()
        .ramp(Vec3::new(0.0, -256.0, 0.0), Vec3::new(128.0, 256.0, 221.0))
        .build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let body = physics.add_prop(&crate_at(
        Vec3::new(100.0, 0.0, 200.0),
        8.0,
        10.0,
        Motion::Dynamic,
    ));
    run(&mut physics, 200);
    let end = position(&physics, body);
    assert!(
        end.x < 0.0,
        "the crate ended up off the ramp's foot, got {end}"
    );
}

#[test]
fn players_collide_with_props_and_can_stand_on_them() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    physics.add_prop(&crate_at(
        Vec3::new(64.0, 0.0, 0.0),
        16.0,
        50.0,
        Motion::Fixed,
    ));
    physics.step();
    let config = movement_config();
    let scene = Scene {
        world: &world,
        physics: &physics,
        ignore: None,
    };

    // Walking into the crate stops at its face (x = 48, minus the hull).
    let mut state = PlayerState::at(Vec3::new(0.0, 0.0, 1.0));
    let forward = UserCmd {
        forward: 1.0,
        ..UserCmd::default()
    };
    let mut touched = false;
    for _ in 0..132 {
        let (next, events) = simulate(&state, &forward, &config, DT, &scene);
        touched |= events.touches().next().is_some();
        state = next;
    }
    let face = 48.0 - config.hulls.standing.maxs[0];
    assert!(
        (state.origin.x - face).abs() < 0.5,
        "stopped at the face: {state:?}"
    );
    assert!(touched, "movement reports running into the crate");

    // Dropped onto it, the player stands on its top.
    let mut state = PlayerState::at(Vec3::new(64.0, 0.0, 100.0));
    for _ in 0..132 {
        state = simulate(&state, &UserCmd::default(), &config, DT, &scene).0;
    }
    assert!(state.on_ground, "{state:?}");
    // Like on brushes, a landing counts within the 2-unit ground probe.
    assert!((32.0..34.0).contains(&state.origin.z), "on top: {state:?}");
}

#[test]
fn pushing_moves_light_props_more_than_heavy_ones() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let light = physics.add_prop(&crate_at(
        Vec3::new(0.0, 0.0, 0.0),
        8.0,
        5.0,
        Motion::Dynamic,
    ));
    let heavy = physics.add_prop(&crate_at(
        Vec3::new(0.0, 200.0, 0.0),
        8.0,
        500.0,
        Motion::Dynamic,
    ));
    run(&mut physics, 30);
    for _ in 0..10 {
        // A player running at 300 units/s into each crate's -X face.
        physics.push(light, Vec3::NEG_X, 300.0);
        physics.push(heavy, Vec3::NEG_X, 300.0);
        physics.step();
    }
    let (light_x, heavy_x) = (position(&physics, light).x, position(&physics, heavy).x);
    assert!(light_x > 20.0, "the light crate was shoved, got {light_x}");
    assert!(
        heavy_x < light_x / 5.0,
        "the heavy crate barely moved: {heavy_x} vs {light_x}"
    );
}

#[test]
fn walking_into_a_prop_pushes_it() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let crate_body = physics.add_prop(&crate_at(
        Vec3::new(64.0, 0.0, 0.0),
        12.0,
        10.0,
        Motion::Dynamic,
    ));
    run(&mut physics, 30);
    let config = movement_config();
    let mut state = PlayerState::at(Vec3::new(0.0, 0.0, 1.0));
    let forward = UserCmd {
        forward: 1.0,
        ..UserCmd::default()
    };
    for _ in 0..132 {
        let scene = Scene {
            world: &world,
            physics: &physics,
            ignore: None,
        };
        let (next, events) = simulate(&state, &forward, &config, DT, &scene);
        let touches: Vec<_> = events.touches().copied().collect();
        state = next;
        for touch in touches {
            physics.push(touch.body, touch.normal, touch.speed);
        }
        physics.step();
    }
    let moved = position(&physics, crate_body).x - 64.0;
    assert!(moved > 100.0, "the crate was walked along, moved {moved}");
    assert!(
        state.origin.x > 100.0,
        "the player kept walking, at {}",
        state.origin
    );
}

#[test]
fn a_falling_prop_hurts_the_player_it_lands_on() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let config = movement_config();
    let hull = Hull::from(&config.hulls.standing);
    let player = physics.add_player(Vec3::ZERO, &hull);
    let heavy = physics.add_prop(&crate_at(
        Vec3::new(0.0, 0.0, 600.0),
        12.0,
        80.0,
        Motion::Dynamic,
    ));
    let mut impacts = Vec::new();
    for _ in 0..120 {
        physics.set_player(player, Vec3::ZERO, Vec3::ZERO, &hull);
        impacts.extend(physics.step());
    }
    assert_eq!(impacts.len(), 1, "one impact, counted once: {impacts:?}");
    let impact = impacts[0];
    assert_eq!((impact.player, impact.prop), (player, heavy));
    assert!(impact.speed > 400.0 && impact.damage > 0.0, "{impact:?}");
    let top = hull.maxs.z;
    let rest = position(&physics, heavy);
    assert!(
        rest.z > top - 2.0,
        "rests on the player's head, not through it: {rest}"
    );
}

#[test]
fn walking_into_a_heavy_prop_does_not_hurt() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let hull = Hull::from(&movement_config().hulls.standing);
    // A heavy crate in the player's way.
    physics.add_prop(&crate_at(
        Vec3::new(40.0, 0.0, 0.0),
        12.0,
        200.0,
        Motion::Dynamic,
    ));
    let player = physics.add_player(Vec3::ZERO, &hull);
    let mut impacts = Vec::new();
    for step in 0..60 {
        let at = Vec3::new(step as f32 * 3.0, 0.0, 0.0);
        physics.set_player(player, at, Vec3::new(320.0, 0.0, 0.0), &hull);
        impacts.extend(physics.step());
    }
    assert!(
        impacts.is_empty(),
        "no damage from running into it: {impacts:?}"
    );
}

/// The smallest distance between two crates' centres over `ticks` steps
/// of the world.
fn closest_approach(physics: &mut PhysicsWorld, a: BodyId, b: BodyId, ticks: usize) -> f32 {
    let mut closest = f32::INFINITY;
    for _ in 0..ticks {
        physics.step();
        let (ca, cb) = (
            physics.center_of_mass(a).unwrap_or(Vec3::NAN),
            physics.center_of_mass(b).unwrap_or(Vec3::NAN),
        );
        closest = closest.min(ca.distance(cb));
    }
    closest
}

#[test]
fn a_fast_prop_does_not_sink_into_one_pinned_against_a_wall() {
    // A wall behind a resting crate, and another crate thrown hard at it.
    let world = floor()
        .solid_box(
            Vec3::new(-80.0, -200.0, 0.0),
            Vec3::new(-60.0, 200.0, 200.0),
        )
        .build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let resting = physics.add_prop(&crate_at(
        Vec3::new(-40.0, 0.0, 0.0),
        8.0,
        20.0,
        Motion::Dynamic,
    ));
    let thrown = physics.add_prop(&crate_at(
        Vec3::new(300.0, 0.0, 0.0),
        8.0,
        60.0,
        Motion::Dynamic,
    ));
    run(&mut physics, 20);
    physics.set_velocity(thrown, Vec3::new(-2400.0, 0.0, 0.0), Vec3::ZERO);
    // Two half-sizes of 8 keep the centres at least 16 apart (less a
    // little slop) whatever the crates' turn.
    let closest = closest_approach(&mut physics, resting, thrown, 120);
    assert!(closest > 14.0, "the crates stayed apart, got {closest}");
    let wall = physics.center_of_mass(resting).expect("resting").x;
    assert!(wall > -60.0 + 6.0, "not pushed into the wall: {wall}");
}

#[test]
fn a_prop_thrown_at_the_player_still_hurts() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let hull = Hull::from(&movement_config().hulls.standing);
    let player = physics.add_player(Vec3::ZERO, &hull);
    let heavy = physics.add_prop(&crate_at(
        Vec3::new(160.0, 0.0, 20.0),
        12.0,
        60.0,
        Motion::Dynamic,
    ));
    physics.set_velocity(heavy, Vec3::new(-1200.0, 0.0, 0.0), Vec3::ZERO);
    let mut impacts = Vec::new();
    for _ in 0..60 {
        physics.set_player(player, Vec3::ZERO, Vec3::ZERO, &hull);
        impacts.extend(physics.step());
    }
    assert_eq!(impacts.len(), 1, "{impacts:?}");
    assert_eq!(impacts[0].prop, heavy);
    assert!(impacts[0].damage > 0.0);
}

#[test]
fn a_dropped_crate_reports_hitting_the_floor_once() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let body = physics.add_prop(&crate_at(
        Vec3::new(0.0, 0.0, 200.0),
        8.0,
        20.0,
        Motion::Dynamic,
    ));
    let mut hits = Vec::new();
    for _ in 0..200 {
        physics.step();
        hits.extend(physics.take_contacts());
    }
    let landing = hits
        .iter()
        .filter(|hit| hit.body == body && hit.other.is_none())
        .max_by(|a, b| a.speed.total_cmp(&b.speed))
        .expect("the landing was reported");
    // Falling 184 units at 800 units/s² arrives at about 540 units/s.
    assert!((450.0..620.0).contains(&landing.speed), "{landing:?}");
    assert!(landing.position.z.abs() < 3.0, "at the floor: {landing:?}");
    // The settling bounces are quieter.
    assert!(
        hits.iter().filter(|hit| hit.speed > 120.0).count() <= 4,
        "{hits:?}"
    );
}

#[test]
fn a_prop_starting_inside_the_floor_reports_no_hit() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let body = physics.add_prop(&crate_at(
        Vec3::new(0.0, 0.0, -24.0),
        8.0,
        20.0,
        Motion::Dynamic,
    ));
    physics.step();
    let hits = physics.take_contacts();
    assert!(
        hits.iter().all(|hit| hit.body != body),
        "settling out of the floor was reported as a hit: {hits:?}"
    );
}

#[test]
fn a_prop_striking_a_prop_reports_both_and_not_the_map() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let resting = physics.add_prop(&crate_at(
        Vec3::new(0.0, 0.0, 0.0),
        8.0,
        20.0,
        Motion::Dynamic,
    ));
    let thrown = physics.add_prop(&crate_at(
        Vec3::new(120.0, 0.0, 0.0),
        8.0,
        20.0,
        Motion::Dynamic,
    ));
    run(&mut physics, 10);
    physics.take_contacts();
    physics.set_velocity(thrown, Vec3::new(-600.0, 0.0, 0.0), Vec3::ZERO);
    let mut hits = Vec::new();
    for _ in 0..20 {
        physics.step();
        hits.extend(physics.take_contacts());
    }
    let hit = hits
        .iter()
        .find(|hit| hit.other.is_some())
        .expect("a crate-on-crate hit");
    assert_eq!(
        [hit.body, hit.other.expect("other")]
            .iter()
            .filter(|b| **b == resting || **b == thrown)
            .count(),
        2
    );
    assert!(hit.speed > 400.0, "{hit:?}");
}

#[test]
fn a_prop_sliding_along_the_floor_scrapes_until_it_slows() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let body = physics.add_prop(&crate_at(Vec3::ZERO, 16.0, 20.0, Motion::Dynamic));
    run(&mut physics, 20);
    assert!(physics.scrapes().is_empty(), "at rest it is quiet");
    physics.set_velocity(body, Vec3::new(400.0, 0.0, 0.0), Vec3::ZERO);
    physics.step();
    let scrape = physics.scrapes().into_iter().next().expect("sliding");
    assert_eq!(scrape.body, body);
    assert!(scrape.speed > 300.0, "{scrape:?}");
    run(&mut physics, 120);
    assert!(physics.scrapes().is_empty(), "it has come to rest");
}

#[test]
fn a_broken_prop_leaves_the_world_but_keeps_its_id() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let first = physics.add_prop(&crate_at(
        Vec3::new(0.0, 0.0, 100.0),
        8.0,
        20.0,
        Motion::Dynamic,
    ));
    let second = physics.add_prop(&crate_at(
        Vec3::new(60.0, 0.0, 100.0),
        8.0,
        20.0,
        Motion::Dynamic,
    ));
    assert!(physics.break_prop(first));
    assert!(!physics.break_prop(first), "only once");
    assert!(physics.is_broken(first) && !physics.is_broken(second));
    assert_eq!(physics.props().collect::<Vec<_>>(), [second]);
    assert_eq!(physics.broken().collect::<Vec<_>>(), [first]);
    run(&mut physics, 100);
    assert!(position(&physics, first).z > 90.0, "it no longer falls");
    assert!(position(&physics, second).z < 1.0, "the other one does");
    let hull = Hull::from(&movement_config().hulls.standing);
    let through = physics.trace_hull(
        Vec3::new(0.0, 0.0, 120.0),
        Vec3::new(0.0, 0.0, 60.0),
        &hull,
        contents::SOLID,
        None,
    );
    assert!(through.body.is_none(), "players pass through where it was");
}

#[test]
fn held_props_ignore_their_holder_and_gravity() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let hull = Hull::from(&movement_config().hulls.standing);
    let player = physics.add_player(Vec3::ZERO, &hull);
    // Overlapping the player's box, in mid-air.
    let prop = physics.add_prop(&crate_at(
        Vec3::new(10.0, 0.0, 40.0),
        8.0,
        10.0,
        Motion::Dynamic,
    ));
    physics.set_held(prop, Some(player));
    assert_eq!(physics.holder(prop), Some(player));
    let impacts: Vec<_> = (0..30).flat_map(|_| physics.step()).collect();
    assert!(impacts.is_empty());
    let held = position(&physics, prop);
    assert!(
        held.abs_diff_eq(Vec3::new(10.0, 0.0, 40.0), 0.5),
        "not pushed nor falling: {held}"
    );

    physics.set_held(prop, None);
    run(&mut physics, 30);
    assert!(position(&physics, prop).z < 30.0, "falls once released");
}

#[test]
fn a_prop_let_go_inside_its_holder_flies_off_without_hitting_them() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let hull = Hull::from(&movement_config().hulls.standing);
    let player = physics.add_player(Vec3::ZERO, &hull);
    let prop = physics.add_prop(&crate_at(
        Vec3::new(8.0, 0.0, 30.0),
        8.0,
        30.0,
        Motion::Dynamic,
    ));
    physics.set_held(prop, Some(player));
    physics.step();
    physics.set_held(prop, None);
    physics.set_velocity(prop, Vec3::X * 1000.0, Vec3::ZERO);
    let impacts: Vec<_> = (0..30)
        .flat_map(|_| {
            physics.set_player(player, Vec3::ZERO, Vec3::ZERO, &hull);
            physics.step()
        })
        .collect();
    assert!(impacts.is_empty(), "{impacts:?}");
    assert!(!physics.ignores(player, prop), "forgotten once apart");
    assert!(position(&physics, prop).x > 100.0);
}

#[test]
fn debris_does_not_block_players() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let mut debris = crate_at(Vec3::new(64.0, 0.0, 0.0), 16.0, 5.0, Motion::Fixed);
    debris.debris = true;
    physics.add_prop(&debris);
    physics.add_prop(&crate_at(
        Vec3::new(0.0, 64.0, 0.0),
        16.0,
        5.0,
        Motion::Fixed,
    ));
    physics.step();
    let hull = Hull::from(&movement_config().hulls.standing);
    let through = physics.trace_hull(
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(128.0, 0.0, 1.0),
        &hull,
        contents::MASK_PLAYER_SOLID,
        None,
    );
    assert!(!through.hit(), "{through:?}");
    let blocked = physics.trace_hull(
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(0.0, 128.0, 1.0),
        &hull,
        contents::MASK_PLAYER_SOLID,
        None,
    );
    assert!(blocked.hit());
    assert_eq!(blocked.body, Some(BodyId::from_index(1)));
}

#[test]
fn traces_follow_a_rotated_prop() {
    let world = WorldBuilder::new().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let mut turned = crate_at(Vec3::ZERO, 16.0, 5.0, Motion::Fixed);
    turned.rotation = Quat::from_rotation_z(std::f32::consts::FRAC_PI_4);
    physics.add_prop(&turned);
    physics.step();
    // A ray along +X at mid-height meets the diamond's corner at
    // x = -16·√2.
    let trace = physics.trace_hull(
        Vec3::new(-100.0, 0.0, 16.0),
        Vec3::new(100.0, 0.0, 16.0),
        &Hull::POINT,
        contents::SOLID,
        None,
    );
    let corner = -16.0 * std::f32::consts::SQRT_2;
    assert!((trace.end.x - corner).abs() < 0.1, "{trace:?}");
    // Off to the side of the corner, the ray hits a slanted face.
    let side = physics.trace_hull(
        Vec3::new(-100.0, 8.0, 16.0),
        Vec3::new(100.0, 8.0, 16.0),
        &Hull::POINT,
        contents::SOLID,
        None,
    );
    let expected = Vec3::new(-1.0, 1.0, 0.0).normalize();
    assert!(side.normal.abs_diff_eq(expected, 1e-4), "{side:?}");
}

#[test]
fn frozen_props_are_freed_by_grabbing_when_allowed() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let hull = Hull::from(&movement_config().hulls.standing);
    let player = physics.add_player(Vec3::new(-200.0, 0.0, 0.0), &hull);
    let mut frozen = crate_at(Vec3::new(0.0, 0.0, 50.0), 8.0, 5.0, Motion::Fixed);
    frozen.free_on_grab = true;
    let frozen = physics.add_prop(&frozen);
    let stuck = physics.add_prop(&crate_at(
        Vec3::new(50.0, 0.0, 50.0),
        8.0,
        5.0,
        Motion::Fixed,
    ));
    assert!(physics.is_grabbable(frozen) && !physics.is_grabbable(stuck));
    run(&mut physics, 10);
    assert_eq!(position(&physics, frozen).z, 50.0, "fixed until grabbed");
    physics.set_held(frozen, Some(player));
    physics.set_held(frozen, None);
    run(&mut physics, 60);
    assert!(physics.is_dynamic(frozen));
    assert!(position(&physics, frozen).z < 1.0, "falls once freed");
}

#[test]
fn speeds_are_capped() {
    let world = WorldBuilder::new().build();
    let config = physics_config();
    let mut physics = PhysicsWorld::new(&world, &config, DT);
    let body = physics.add_prop(&crate_at(Vec3::ZERO, 8.0, 5.0, Motion::Dynamic));
    physics.set_velocity(body, Vec3::X * 1e6, Vec3::Z * 1e6);
    physics.step();
    let (linear, angular) = physics.velocity(body).expect("body");
    assert!(linear.length() <= config.max_speed * 1.001);
    assert!(angular.length() <= config.max_angular_speed.to_radians() * 1.001);
}

/// A ball socket joining `body` to the world at `pivot`.
fn hung_from(body: BodyId, pivot: Vec3) -> JointDesc {
    JointDesc {
        kind: JointKind::Ball,
        body1: Some(body),
        body2: None,
        anchor1: pivot,
        anchor2: pivot,
        collide: true,
    }
}

#[test]
fn a_lamp_on_a_ball_socket_hangs_where_the_map_put_it() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let spawn = Vec3::new(0.0, 0.0, 200.0);
    let lamp = physics.add_prop(&crate_at(spawn, 8.0, 10.0, Motion::Dynamic));
    let loose = physics.add_prop(&crate_at(
        Vec3::new(100.0, 0.0, 200.0),
        8.0,
        10.0,
        Motion::Dynamic,
    ));
    // Hung by a point above its top, as dm_steamlab's lamps are.
    assert!(physics.add_joint(&hung_from(lamp, spawn + Vec3::Z * 40.0)));
    run(&mut physics, 300);
    assert!(
        position(&physics, lamp).distance(spawn) < 1.0,
        "the lamp hangs at {}",
        position(&physics, lamp)
    );
    assert!(position(&physics, loose).z < 1.0, "an unhung one falls");
}

#[test]
fn a_hung_lamp_swings_when_hit_and_comes_back() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let spawn = Vec3::new(0.0, 0.0, 200.0);
    let pivot = spawn + Vec3::Z * 40.0;
    // Air drag, as real props have, brings the swing to rest.
    let lamp = physics.add_prop(&PropDesc {
        linear_damping: 0.4,
        ..crate_at(spawn, 8.0, 10.0, Motion::Dynamic)
    });
    assert!(physics.add_joint(&hung_from(lamp, pivot)));
    physics.push(lamp, Vec3::X, 300.0);
    run(&mut physics, 10);
    let swung = position(&physics, lamp);
    assert!(swung.x.abs() > 2.0, "pushed aside to {swung}");
    // On its tether: the pivot stays as far from it as it was.
    assert!((swung.distance(pivot) - 40.0).abs() < 1.0);
    run(&mut physics, 1200);
    assert!(position(&physics, lamp).distance(spawn) < 4.0);
}

#[test]
fn a_rope_lets_a_prop_fall_only_its_length() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let spawn = Vec3::new(0.0, 0.0, 200.0);
    let weight = physics.add_prop(&crate_at(spawn, 8.0, 10.0, Motion::Dynamic));
    let joint = JointDesc {
        kind: JointKind::Rope { length: 60.0 },
        body1: Some(weight),
        body2: None,
        anchor1: spawn,
        anchor2: spawn + Vec3::Z * 20.0,
        collide: true,
    };
    assert!(physics.add_joint(&joint));
    run(&mut physics, 300);
    let z = position(&physics, weight).z;
    assert!((z - 160.0).abs() < 2.0, "dangles at z {z}");
}

#[test]
fn joints_to_missing_bodies_are_refused() {
    let world = floor().build();
    let mut elsewhere = PhysicsWorld::new(&world, &physics_config(), DT);
    let body = elsewhere.add_prop(&crate_at(Vec3::ZERO, 8.0, 10.0, Motion::Dynamic));
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    assert!(!physics.add_joint(&hung_from(body, Vec3::ZERO)));
}

#[test]
fn a_player_inside_a_prop_is_pushed_out_of_it() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let config = movement_config();
    // A fixed crate whose face is 10 units inside the player's box.
    physics.add_prop(&crate_at(
        Vec3::new(40.0, 0.0, 0.0),
        16.0,
        50.0,
        Motion::Fixed,
    ));
    physics.step();
    let scene = Scene {
        world: &world,
        physics: &physics,
        ignore: None,
    };
    let hull = Hull::from(&config.hulls.standing);
    // Its face is at x = 24; the box reaches 16 either side of the origin.
    let start = Vec3::new(16.0, 0.0, 1.0);
    let mut state = PlayerState::at(start);
    let idle = UserCmd::default();
    // Stuck at the start, as a trace sees it.
    assert!(
        rl_sim::collision::CollisionQuery::trace_hull(
            &scene,
            state.origin,
            state.origin,
            &hull,
            contents::MASK_PLAYER_SOLID
        )
        .start_solid
    );
    let (first, _) = simulate(&state, &idle, &config, DT, &scene);
    let moved = (first.origin - state.origin).length();
    assert!(
        moved <= config.unstick_speed * DT + 1e-3,
        "pushed out gradually, moved {moved}"
    );
    for _ in 0..66 {
        state = simulate(&state, &idle, &config, DT, &scene).0;
    }
    assert!(
        !rl_sim::collision::CollisionQuery::trace_hull(
            &scene,
            state.origin,
            state.origin,
            &hull,
            contents::MASK_PLAYER_SOLID
        )
        .start_solid,
        "free of the crate: {state:?}"
    );
    assert!(state.origin.x < start.x, "pushed away from it: {state:?}");
    // Free, the player can walk again.
    let walk = UserCmd {
        forward: -1.0,
        ..UserCmd::default()
    };
    let (after, _) = simulate(&state, &walk, &config, DT, &scene);
    assert!(after.origin.x < state.origin.x);
}

#[test]
fn a_player_is_not_pushed_through_a_wall_getting_out_of_a_prop() {
    let world = floor()
        .solid_box(
            Vec3::new(-64.0, -256.0, 0.0),
            Vec3::new(-24.0, 256.0, 256.0),
        )
        .build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let config = movement_config();
    physics.add_prop(&crate_at(
        Vec3::new(10.0, 0.0, 0.0),
        16.0,
        50.0,
        Motion::Fixed,
    ));
    physics.step();
    let scene = Scene {
        world: &world,
        physics: &physics,
        ignore: None,
    };
    let mut state = PlayerState::at(Vec3::new(-8.0, 0.0, 1.0));
    for _ in 0..66 {
        state = simulate(&state, &UserCmd::default(), &config, DT, &scene).0;
    }
    assert!(
        state.origin.x - config.hulls.standing.maxs[0] >= -24.0 - 0.1,
        "stayed out of the wall: {state:?}"
    );
}

#[test]
fn a_prop_moving_into_a_player_shoves_them_by_its_mass() {
    let shove_of = |mass: f32| {
        let world = floor().build();
        let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
        let hull = Hull::from(&movement_config().hulls.standing);
        let player = physics.add_player(Vec3::ZERO, &hull);
        let prop = physics.add_prop(&crate_at(
            Vec3::new(120.0, 0.0, 20.0),
            12.0,
            mass,
            Motion::Dynamic,
        ));
        physics.set_velocity(prop, Vec3::new(-600.0, 0.0, 0.0), Vec3::ZERO);
        let mut total = Vec3::ZERO;
        for _ in 0..60 {
            physics.set_player(player, Vec3::ZERO, Vec3::ZERO, &hull);
            physics.step();
            for shove in physics.take_shoves() {
                assert_eq!(shove.player, player);
                total += shove.velocity;
            }
        }
        total
    };
    let light = shove_of(5.0);
    let heavy = shove_of(200.0);
    assert!(
        light.x < 0.0 && heavy.x < 0.0,
        "away from the prop: {light} {heavy}"
    );
    assert!(
        heavy.x < light.x * 2.0,
        "a heavier prop shoves harder: {light} vs {heavy}"
    );
}

#[test]
fn a_resting_prop_shoves_nobody() {
    let world = floor().build();
    let mut physics = PhysicsWorld::new(&world, &physics_config(), DT);
    let hull = Hull::from(&movement_config().hulls.standing);
    let player = physics.add_player(Vec3::ZERO, &hull);
    physics.add_prop(&crate_at(
        Vec3::new(30.0, 0.0, 0.0),
        12.0,
        50.0,
        Motion::Dynamic,
    ));
    for _ in 0..120 {
        physics.set_player(player, Vec3::ZERO, Vec3::ZERO, &hull);
        physics.step();
        for shove in physics.take_shoves() {
            assert!(shove.velocity.length() < 1.0, "{shove:?}");
        }
    }
}
