//! Movement scenarios in synthetic rooms: the behaviours players rely on —
//! friction, acceleration, air-strafing, bunny-hopping, steps, surfing,
//! crouching, swimming and ladders — and determinism.

use glam::Vec3;
use rl_data::{GameData, MovementConfig};
use rl_sim::collision::contents::WATER;
use rl_sim::collision::{CollisionWorld, WorldBuilder};
use rl_sim::movement::{Buttons, MoveEvents, PlayerState, UserCmd, WaterLevel, simulate};

/// One 66 Hz tick.
const DT: f32 = 1.0 / 66.0;

#[expect(
    clippy::expect_used,
    reason = "test helper: broken built-in data must fail the test"
)]
fn config() -> MovementConfig {
    GameData::builtin().expect("builtin data").movement
}

/// A huge flat floor with its top at z = 0.
fn floor() -> WorldBuilder {
    WorldBuilder::new().solid_box(
        Vec3::new(-8192.0, -8192.0, -64.0),
        Vec3::new(8192.0, 8192.0, 0.0),
    )
}

/// A player in a world, ticked by commands.
struct Sim {
    world: CollisionWorld,
    config: MovementConfig,
    state: PlayerState,
}

impl Sim {
    fn new(world: CollisionWorld, origin: Vec3) -> Self {
        Self {
            world,
            config: config(),
            state: PlayerState::at(origin),
        }
    }

    fn tick(&mut self, cmd: &UserCmd) -> MoveEvents {
        let (state, events) = simulate(&self.state, cmd, &self.config, DT, &self.world);
        self.state = state;
        events
    }

    fn run(&mut self, ticks: usize, cmd: &UserCmd) {
        for _ in 0..ticks {
            self.tick(cmd);
        }
    }

    /// Lets the player fall onto whatever is below and come to rest.
    fn settle(&mut self) {
        self.run(66, &UserCmd::default());
        assert!(self.state.on_ground, "settled on a floor: {:?}", self.state);
    }
}

fn forward() -> UserCmd {
    UserCmd {
        forward: 1.0,
        ..UserCmd::default()
    }
}

fn with(buttons: Buttons) -> UserCmd {
    UserCmd {
        buttons,
        ..UserCmd::default()
    }
}

#[test]
fn falling_players_land_on_the_floor() {
    let mut sim = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 200.0));
    let mut landed = None;
    for _ in 0..132 {
        if let Some(speed) = sim.tick(&UserCmd::default()).landed {
            landed = Some(speed);
        }
    }
    assert!(sim.state.on_ground);
    assert!(
        sim.state.origin.z >= 0.0 && sim.state.origin.z < 0.1,
        "{:?}",
        sim.state
    );
    assert_eq!(sim.state.velocity, Vec3::ZERO);
    // From 200 units: v = √(2·g·h) ≈ 566 units/s.
    let speed = landed.expect("a landing event");
    assert!(
        (speed - (2.0 * 800.0 * 200.0f32).sqrt()).abs() < 30.0,
        "{speed}"
    );
}

#[test]
fn running_reaches_and_holds_the_run_speed() {
    let mut sim = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 1.0));
    sim.settle();
    let run_speed = sim.config.ground.run_speed;
    for _ in 0..132 {
        sim.tick(&forward());
        assert!(sim.state.speed_2d() <= run_speed + 1e-3);
    }
    assert!((sim.state.speed_2d() - run_speed).abs() < 1.0);
    assert!(sim.state.velocity.x > 0.0, "yaw 0 runs along +X");

    let walk = UserCmd {
        buttons: Buttons::WALK,
        ..forward()
    };
    sim.run(132, &walk);
    assert!((sim.state.speed_2d() - sim.config.ground.walk_speed).abs() < 1.0);
}

#[test]
fn friction_stops_a_running_player_in_about_half_a_second() {
    let mut sim = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 1.0));
    sim.settle();
    sim.state.velocity = Vec3::new(320.0, 0.0, 0.0);
    let mut previous = 320.0;
    let mut stopped_after = None;
    for tick in 0..66 {
        sim.tick(&UserCmd::default());
        let speed = sim.state.speed_2d();
        assert!(speed <= previous, "friction never speeds up");
        previous = speed;
        if speed == 0.0 && stopped_after.is_none() {
            stopped_after = Some(tick + 1);
        }
    }
    // Friction 4 takes 320 u/s down to the 100 u/s stop speed in
    // ln(3.2) / 4 ≈ 0.29 s, then removes a constant 400 u/s² for the last
    // 0.25 s: about 36 ticks in all.
    let ticks = stopped_after.expect("came to a stop");
    assert!((33..=39).contains(&ticks), "stopped after {ticks} ticks");
}

#[test]
fn standing_jumps_reach_the_configured_height() {
    let mut sim = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 1.0));
    sim.settle();
    let ground = sim.state.origin.z;
    assert!(sim.tick(&with(Buttons::JUMP)).jumped);
    let mut peak = ground;
    for _ in 0..66 {
        sim.tick(&UserCmd::default());
        peak = peak.max(sim.state.origin.z);
    }
    let height = peak - ground;
    assert!(
        (height - sim.config.jump.height).abs() < 2.0,
        "jumped {height}"
    );
    assert!(sim.state.on_ground, "came back down");
}

#[test]
fn holding_jump_hops_only_once_without_auto_hop() {
    let mut sim = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 1.0));
    sim.settle();
    assert!(!sim.config.jump.auto_hop);
    let hold = with(Buttons::JUMP);
    let jumps = (0..200).filter(|_| sim.tick(&hold).jumped).count();
    assert_eq!(jumps, 1, "a held button jumps once");

    // Releasing and pressing again on the ground hops every time.
    let mut jumps = 0;
    let mut pressed = true;
    for _ in 0..200 {
        pressed = sim.state.on_ground && !pressed;
        let cmd = if pressed { hold } else { UserCmd::default() };
        jumps += usize::from(sim.tick(&cmd).jumped);
    }
    assert!(jumps >= 5, "{jumps} hops");
}

#[test]
fn auto_hop_jumps_on_every_landing() {
    let mut sim = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 1.0));
    sim.config.jump.auto_hop = true;
    sim.settle();
    let jumps = (0..200)
        .filter(|_| sim.tick(&with(Buttons::JUMP)).jumped)
        .count();
    assert!(jumps >= 5, "{jumps} hops");
}

/// View yaw for air-strafing right: slightly left of the direction of
/// travel, so the wished direction (to the view's right) is just under 90°
/// from the velocity. Speed gained along the velocity peaks when the
/// velocity's component along the wished direction is half the air speed
/// cap, at `acos(cap / 2 / speed)`. Turning the view to follow the velocity,
/// tick after tick, is what a strafing player does with the mouse.
fn strafe_yaw(state: &PlayerState) -> f32 {
    let speed = state.speed_2d().max(1.0);
    let half_cap = config().air.speed_cap * 0.5;
    let angle = (half_cap / speed).min(1.0).acos().to_degrees();
    state.velocity.y.atan2(state.velocity.x).to_degrees() + (90.0 - angle)
}

/// Air-strafes right for `ticks`.
fn strafe(sim: &mut Sim, ticks: usize) {
    for _ in 0..ticks {
        let cmd = UserCmd {
            side: 1.0,
            yaw: strafe_yaw(&sim.state),
            ..UserCmd::default()
        };
        sim.tick(&cmd);
    }
}

#[test]
fn air_strafing_gains_speed() {
    // High in the air, moving at run speed.
    let mut sim = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 4000.0));
    sim.state.velocity = Vec3::new(320.0, 0.0, 0.0);
    strafe(&mut sim, 66);
    assert!(!sim.state.on_ground);
    // About +1 u/s per tick at 320 u/s, falling off as 1/speed.
    let speed = sim.state.speed_2d();
    assert!(speed > 370.0, "air-strafed to {speed}");

    // Without turning, holding a strafe key barely changes speed.
    let mut straight = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 4000.0));
    straight.state.velocity = Vec3::new(320.0, 0.0, 0.0);
    straight.run(
        66,
        &UserCmd {
            side: 1.0,
            ..UserCmd::default()
        },
    );
    assert!(straight.state.speed_2d() < 340.0);
}

#[test]
fn bunny_hopping_keeps_air_strafe_speed() {
    let mut sim = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 1.0));
    sim.settle();
    sim.run(66, &forward());
    let start = sim.state.speed_2d();
    // Strafe in the air, jumping again on the tick of each landing.
    for _ in 0..330 {
        let buttons = if sim.state.on_ground {
            Buttons::JUMP
        } else {
            Buttons::NONE
        };
        sim.tick(&UserCmd {
            side: 1.0,
            yaw: strafe_yaw(&sim.state),
            buttons,
            ..UserCmd::default()
        });
    }
    assert!(
        sim.state.speed_2d() > start * 1.2,
        "{} vs {start}",
        sim.state.speed_2d()
    );
}

#[test]
fn walking_climbs_steps_but_not_taller_ledges() {
    let world = floor()
        .solid_box(Vec3::new(100.0, -64.0, 0.0), Vec3::new(300.0, 64.0, 16.0))
        .solid_box(Vec3::new(100.0, 200.0, 0.0), Vec3::new(300.0, 328.0, 24.0))
        .build();
    let mut sim = Sim::new(world.clone(), Vec3::new(0.0, 0.0, 1.0));
    sim.settle();
    sim.run(66, &forward());
    assert!(
        sim.state.origin.x > 150.0,
        "walked onto the step: {:?}",
        sim.state
    );
    assert!((sim.state.origin.z - 16.0).abs() < 0.1);
    assert!(sim.state.on_ground);

    let mut blocked = Sim::new(world, Vec3::new(0.0, 264.0, 1.0));
    blocked.settle();
    blocked.run(66, &forward());
    assert!(
        blocked.state.origin.x < 100.0,
        "a 24-unit ledge blocks: {:?}",
        blocked.state
    );
    assert!(blocked.state.origin.z < 1.0);
}

#[test]
fn walking_down_a_slope_stays_on_the_ground() {
    // A 20° slope rising along +X: walk down it from the top.
    let rise = 512.0 * 20f32.to_radians().tan();
    let world = floor()
        .ramp(Vec3::new(0.0, -256.0, 0.0), Vec3::new(512.0, 256.0, rise))
        .solid_box(
            Vec3::new(512.0, -256.0, 0.0),
            Vec3::new(1024.0, 256.0, rise),
        )
        .build();
    let mut sim = Sim::new(world, Vec3::new(700.0, 0.0, rise + 1.0));
    sim.settle();
    let down = UserCmd {
        forward: 1.0,
        yaw: 180.0,
        ..UserCmd::default()
    };
    for _ in 0..80 {
        sim.tick(&down);
        assert!(
            sim.state.on_ground,
            "lost the ground at {:?}",
            sim.state.origin
        );
    }
    assert!(sim.state.origin.x < 400.0);
}

#[test]
fn surfing_a_steep_ramp_keeps_its_speed() {
    // A 60° ramp rising along +X; the player slides along it (along Y).
    let rise = 512.0 * 60f32.to_radians().tan();
    let world = floor()
        .ramp(Vec3::new(0.0, -4096.0, 0.0), Vec3::new(512.0, 4096.0, rise))
        .build();
    // On the slope, halfway up, moving along it.
    let x = 256.0;
    let z = x * 60f32.to_radians().tan() + 30.0;
    let mut sim = Sim::new(world, Vec3::new(x - 30.0, -3000.0, z));
    sim.state.velocity = Vec3::new(0.0, 800.0, 0.0);
    // Hold strafe into the ramp (to the right, facing +Y), as surfers do.
    let into_ramp = UserCmd {
        side: 1.0,
        yaw: 90.0,
        ..UserCmd::default()
    };
    sim.run(66, &into_ramp);
    assert!(!sim.state.on_ground, "a steep ramp is never a floor");
    assert!(sim.state.velocity.y > 700.0, "{:?}", sim.state);
    assert!(
        sim.state.origin.z > 0.0,
        "still on the ramp: {:?}",
        sim.state
    );
    assert!(sim.state.origin.y > -2400.0, "travelled along it");
}

#[test]
fn crouching_lowers_the_view_and_shrinks_the_hull() {
    let mut sim = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 1.0));
    sim.settle();
    let standing_eye = sim.state.eye_height(&sim.config);
    sim.run(10, &with(Buttons::DUCK));
    assert!(!sim.state.ducked, "crouching takes time");
    assert!(sim.state.eye_height(&sim.config) < standing_eye);
    sim.run(30, &with(Buttons::DUCK));
    assert!(sim.state.ducked);
    assert_eq!(
        sim.state.eye_height(&sim.config),
        sim.config.hulls.ducked.eye_height
    );

    // Crouched movement is slow.
    let crouch_walk = UserCmd {
        buttons: Buttons::DUCK,
        ..forward()
    };
    sim.run(66, &crouch_walk);
    let expected = sim.config.ground.run_speed * sim.config.duck.speed_scale;
    assert!((sim.state.speed_2d() - expected).abs() < 1.0);

    sim.run(66, &UserCmd::default());
    assert!(!sim.state.ducked);
    assert_eq!(sim.state.eye_height(&sim.config), standing_eye);
}

#[test]
fn a_low_ceiling_keeps_a_crouched_player_down() {
    // A 50-unit-high crawlspace: the ducked hull (36) fits, standing (72)
    // does not.
    let world = floor()
        .solid_box(
            Vec3::new(-128.0, -128.0, 50.0),
            Vec3::new(128.0, 128.0, 64.0),
        )
        .build();
    let mut sim = Sim::new(world, Vec3::new(0.0, 0.0, 1.0));
    sim.state.ducked = true;
    sim.state.duck_amount = 1.0;
    sim.settle();
    sim.run(66, &UserCmd::default());
    assert!(sim.state.ducked, "cannot stand up under the ceiling");

    // Crawl out from under it, and the player stands.
    sim.run(
        132,
        &UserCmd {
            forward: 1.0,
            ..UserCmd::default()
        },
    );
    sim.run(33, &UserCmd::default());
    assert!(!sim.state.ducked, "stood up in the open: {:?}", sim.state);
}

#[test]
fn crouch_jumping_lifts_the_feet() {
    let peak = |crouch: bool| {
        let mut sim = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 1.0));
        sim.settle();
        sim.tick(&with(Buttons::JUMP));
        let mut peak = 0.0f32;
        for _ in 0..40 {
            sim.tick(&with(if crouch { Buttons::DUCK } else { Buttons::NONE }));
            peak = peak.max(sim.state.origin.z);
        }
        peak
    };
    let lift = peak(true) - peak(false);
    let hulls = config().hulls;
    let expected = hulls.standing.maxs[2] - hulls.ducked.maxs[2];
    assert!((lift - expected).abs() < 1.0, "lifted {lift}");
}

#[test]
fn swimming_sinks_rises_and_stays_in_the_pool() {
    let world = floor()
        .box_of(
            WATER,
            Vec3::new(-512.0, -512.0, 0.0),
            Vec3::new(512.0, 512.0, 200.0),
        )
        .build();
    let mut sim = Sim::new(world, Vec3::new(0.0, 0.0, 100.0));
    sim.tick(&UserCmd::default());
    assert_eq!(sim.state.water_level, WaterLevel::Eyes);
    let start = sim.state.origin.z;
    sim.run(66, &UserCmd::default());
    let sunk = start - sim.state.origin.z;
    assert!(sunk > 10.0 && sunk < 80.0, "sank slowly: {sunk}");

    sim.run(132, &with(Buttons::JUMP));
    assert!(sim.state.origin.z > start, "swam up: {:?}", sim.state);
    assert!(sim.state.water_level >= WaterLevel::Feet);
}

#[test]
fn ladders_are_grabbed_climbed_and_left_at_the_top() {
    // A 200-unit wall with a ladder up its face and a dismount point on top.
    let world = floor()
        .solid_box(Vec3::new(40.0, -256.0, 0.0), Vec3::new(400.0, 256.0, 200.0))
        .ladder(Vec3::new(20.0, 0.0, 0.0), Vec3::new(20.0, 0.0, 200.0))
        .ladder_dismount(Vec3::new(80.0, 0.0, 201.0))
        .build();
    let mut sim = Sim::new(world, Vec3::new(-10.0, 0.0, 1.0));
    sim.settle();
    sim.tick(&with(Buttons::USE));
    assert_eq!(sim.state.ladder, Some(0), "grabbed the ladder");
    assert!(
        (sim.state.origin.x - 20.0).abs() < 1e-3,
        "snapped to its axis"
    );

    // Climbing takes about a second at 200 units/s.
    for _ in 0..90 {
        sim.tick(&forward());
        if sim.state.ladder.is_none() {
            break;
        }
    }
    assert_eq!(sim.state.ladder, None, "left the ladder at the top");
    assert!((sim.state.origin - Vec3::new(80.0, 0.0, 201.0)).length() < 1e-3);
    sim.settle();
    assert!(
        sim.state.origin.z > 199.0,
        "standing on the wall: {:?}",
        sim.state
    );
}

#[test]
fn looking_down_climbs_down_and_jump_lets_go() {
    let world = floor()
        .solid_box(Vec3::new(40.0, -256.0, 0.0), Vec3::new(400.0, 256.0, 200.0))
        .ladder(Vec3::new(20.0, 0.0, 0.0), Vec3::new(20.0, 0.0, 200.0))
        .build();
    let mut sim = Sim::new(world, Vec3::new(20.0, 0.0, 1.0));
    sim.settle();
    sim.tick(&with(Buttons::USE));
    sim.run(33, &forward());
    let height = sim.state.origin.z;
    assert!(height > 80.0);
    let look_down = UserCmd {
        pitch: 60.0,
        ..forward()
    };
    sim.run(10, &look_down);
    assert!(sim.state.origin.z < height, "climbed down");

    let jumped = sim.tick(&with(Buttons::JUMP));
    assert!(jumped.jumped && sim.state.ladder.is_none());
    assert!(sim.state.velocity.x < 0.0, "pushed away from the wall");
}

#[test]
fn identical_inputs_give_bit_identical_results() {
    let world = floor()
        .solid_box(Vec3::new(100.0, -64.0, 0.0), Vec3::new(300.0, 64.0, 16.0))
        .ramp(
            Vec3::new(-600.0, -200.0, 0.0),
            Vec3::new(-300.0, 200.0, 300.0),
        )
        .build();
    let script = |tick: usize| UserCmd {
        forward: if tick % 90 < 60 { 1.0 } else { -0.5 },
        side: ((tick as f32) * 0.1).sin(),
        pitch: 10.0,
        yaw: tick as f32 * 1.7,
        buttons: match tick % 50 {
            0..=2 => Buttons::JUMP,
            20..=30 => Buttons::DUCK,
            _ => Buttons::NONE,
        },
        select: None,
    };
    let run = || {
        let mut sim = Sim::new(world.clone(), Vec3::new(0.0, 0.0, 50.0));
        let mut trail = Vec::new();
        for tick in 0..600 {
            sim.tick(&script(tick));
            trail.push(sim.state);
        }
        trail
    };
    assert_eq!(run(), run());
}

#[test]
fn bad_inputs_never_produce_non_finite_states() {
    let mut sim = Sim::new(floor().build(), Vec3::new(0.0, 0.0, 10.0));
    sim.state.velocity = Vec3::new(f32::NAN, f32::INFINITY, 1e30);
    sim.run(
        10,
        &UserCmd {
            forward: f32::NAN,
            side: 1e9,
            pitch: f32::INFINITY,
            yaw: f32::NAN,
            buttons: Buttons::from_bits(u16::MAX),
            select: None,
        },
    );
    assert!(sim.state.origin.is_finite(), "{:?}", sim.state);
    assert!(sim.state.velocity.is_finite(), "{:?}", sim.state);
}

#[test]
fn grazing_a_slope_never_stops_a_slide_dead() {
    // Regression test for the "ramp bug": drifting away from a steep ramp
    // while still touching it used to clip the velocity to a sliver into
    // the slope, after which every trace hit it at fraction 0 and the slide
    // stopped dead.
    let rise = 512.0 * 60f32.to_radians().tan();
    let world = floor()
        .ramp(Vec3::new(0.0, -4096.0, 0.0), Vec3::new(512.0, 4096.0, rise))
        .build();
    let x = 256.0;
    let z = x * 60f32.to_radians().tan() + 30.0;
    let mut sim = Sim::new(world, Vec3::new(x - 30.0, -3000.0, z));
    sim.state.velocity = Vec3::new(0.0, 800.0, 0.0);
    let away_from_ramp = UserCmd {
        side: -1.0,
        yaw: 90.0,
        ..UserCmd::default()
    };
    for _ in 0..66 {
        sim.tick(&away_from_ramp);
        assert!(sim.state.velocity.y > 790.0, "stopped: {:?}", sim.state);
    }
}
