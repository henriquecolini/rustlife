//! Manipulator scenarios through the whole simulation: grabbing within
//! range and mass, pulling from afar, carrying, turning with the view,
//! launching, punting, breaking the grip, cooldowns, and a thrown prop
//! hurting another player. The Manipulator is taken in hand first, as a
//! weapon.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a missing player or prop must fail the test"
)]

use std::sync::Arc;

use glam::{Quat, Vec3};
use rl_data::GameData;
use rl_sim::collision::{ConvexHull, WorldBuilder};
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::physics::{BodyId, Motion, PropDesc};
use rl_sim::simulation::{PlayerId, Simulation, TickEvents};
use rl_sim::tools::manipulator::ToolEvent;

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

/// A crate of half-size 8 (centre 8 above its origin).
fn crate_desc(origin: Vec3, mass: f32) -> PropDesc {
    PropDesc {
        pieces: vec![ConvexHull::from_box(
            Vec3::new(-8.0, -8.0, 0.0),
            Vec3::new(8.0, 8.0, 16.0),
        )],
        origin,
        rotation: Quat::IDENTITY,
        motion: Motion::Dynamic,
        mass,
        material: *data().physics.materials.get("wood"),
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

/// A big floor (top at z = 0), a player at the origin looking along +X and
/// a crate `distance` units ahead, settled.
struct Room {
    sim: Simulation,
    player: PlayerId,
    prop: BodyId,
}

impl Room {
    fn new(data: &GameData, distance: f32, mass: f32) -> Self {
        Self::with_pedestal(data, distance, mass, 0.0)
    }

    /// The crate stands on a pedestal of `height` (0 for none).
    fn with_pedestal(data: &GameData, distance: f32, mass: f32, height: f32) -> Self {
        let mut world = WorldBuilder::new().solid_box(
            Vec3::new(-4096.0, -4096.0, -64.0),
            Vec3::new(4096.0, 4096.0, 0.0),
        );
        if height > 0.0 {
            world = world.solid_box(
                Vec3::new(distance - 16.0, -16.0, 0.0),
                Vec3::new(distance + 16.0, 16.0, height),
            );
        }
        let mut sim = Simulation::new(Arc::new(world.build()), data);
        let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
        let prop = sim.add_prop(&crate_desc(Vec3::new(distance, 0.0, height), mass));
        let mut room = Self { sim, player, prop };
        // The match starts on the first tick, which puts the player back
        // at their spawn with a fresh loadout.
        room.run(1, &UserCmd::default());
        // Take the Manipulator in hand (players spawn holding the
        // loadout's gun) while the crate settles.
        let manipulator = room.sim.arsenal().find_weapon("manipulator");
        assert!(
            manipulator.is_some(),
            "the built-in data has the Manipulator"
        );
        let select = UserCmd {
            select: manipulator,
            ..UserCmd::default()
        };
        room.run(1, &select);
        room.run(30, &UserCmd::default());
        room
    }

    /// The pitch that aims the eyes at the crate's centre.
    fn aim_pitch(&self) -> f32 {
        let state = self.sim.player(self.player).expect("player").state;
        let eye = state.origin + Vec3::Z * 64.0;
        let center = self.center();
        let to = center - eye;
        -(to.z.atan2(to.truncate().length())).to_degrees()
    }

    fn center(&self) -> Vec3 {
        self.sim.physics().center_of_mass(self.prop).expect("prop")
    }

    fn tick(&mut self, cmd: &UserCmd) -> TickEvents {
        self.sim.tick(&[(self.player, *cmd)])
    }

    /// Ticks `count` times and gathers the tool events.
    fn run(&mut self, count: usize, cmd: &UserCmd) -> Vec<ToolEvent> {
        (0..count)
            .flat_map(|_| self.tick(cmd).tools)
            .map(|(_, event)| event)
            .collect()
    }

    fn aim(&self, buttons: Buttons) -> UserCmd {
        UserCmd {
            pitch: self.aim_pitch(),
            buttons,
            ..UserCmd::default()
        }
    }

    fn held(&self) -> Option<BodyId> {
        self.sim
            .player(self.player)
            .expect("player")
            .manipulator
            .held()
    }
}

#[test]
fn grabs_a_prop_in_range_and_carries_it_in_front() {
    let data = data();
    let mut room = Room::new(&data, 100.0, 20.0);
    let grab = room.aim(Buttons::ATTACK2);
    let events = room.run(1, &grab);
    assert_eq!(events, [ToolEvent::Grabbed(room.prop)]);

    // Look straight ahead and let it settle at the hold point.
    let ahead = UserCmd {
        buttons: Buttons::ATTACK2,
        ..UserCmd::default()
    };
    room.run(60, &ahead);
    // The crate reaches 8 units back towards the player.
    let reach = data.manipulator.hold.player_radius + 8.0;
    let eye = Vec3::new(0.0, 0.0, 1.0 + 64.0);
    let expected = eye + Vec3::X * (data.manipulator.hold.clearance + reach);
    let center = room.center();
    assert!(
        center.distance(expected) < 4.0,
        "held at {center}, expected {expected}"
    );

    // Turning round brings it along.
    let behind = UserCmd {
        yaw: 180.0,
        ..UserCmd::default()
    };
    room.run(60, &behind);
    assert!(
        room.center().x < -10.0,
        "followed the view: {}",
        room.center()
    );
    assert_eq!(room.held(), Some(room.prop));
}

#[test]
fn heavy_props_cannot_be_grabbed() {
    let data = data();
    let heavy = data.manipulator.grab.max_mass * 2.0;
    let mut room = Room::new(&data, 100.0, heavy);
    let grab = room.aim(Buttons::ATTACK2);
    assert!(room.run(30, &grab).is_empty());
    assert_eq!(room.held(), None);
}

#[test]
fn distant_props_are_pulled_in_then_grabbed() {
    let data = data();
    let distance = f32::midpoint(
        data.manipulator.grab.range,
        data.manipulator.grab.pull_range,
    );
    let mut room = Room::new(&data, distance, 10.0);
    let mut events = Vec::new();
    for _ in 0..132 {
        let cmd = room.aim(Buttons::ATTACK2);
        events.extend(room.run(1, &cmd));
        if !events.is_empty() {
            break;
        }
    }
    assert_eq!(events, [ToolEvent::Grabbed(room.prop)]);
    assert!(
        room.center().x < distance - 50.0,
        "pulled in: {}",
        room.center()
    );
}

#[test]
fn primary_fire_launches_the_held_prop() {
    let data = data();
    let mass = 30.0;
    let mut room = Room::new(&data, 100.0, mass);
    let grab = room.aim(Buttons::ATTACK2);
    room.run(1, &grab);
    let ahead = UserCmd::default();
    room.run(30, &ahead);
    let fire = UserCmd {
        buttons: Buttons::ATTACK,
        ..UserCmd::default()
    };
    let events = room.run(1, &fire);
    assert_eq!(events, [ToolEvent::Launched(room.prop)]);
    assert_eq!(room.held(), None);
    let (velocity, _) = room.sim.physics().velocity(room.prop).expect("prop");
    let expected = data.manipulator.launch.speed_for(mass);
    assert!(
        (velocity.x - expected).abs() < expected * 0.1,
        "launched at {velocity}, expected about {expected} along +X"
    );
}

#[test]
fn primary_fire_punts_a_prop_that_is_not_held() {
    let data = data();
    let mass = 30.0;
    // On a pedestal with its centre at eye height, so the punt is level.
    let mut room = Room::with_pedestal(&data, 100.0, mass, 56.0);
    let punt = room.aim(Buttons::ATTACK);
    let events = room.run(1, &punt);
    assert_eq!(events, [ToolEvent::Punted(room.prop)]);
    let (velocity, _) = room.sim.physics().velocity(room.prop).expect("prop");
    let (through_center, at_point) = data.manipulator.punt.impulses(mass);
    let expected = (through_center + at_point) / mass;
    assert!(
        (velocity.x - expected).abs() < expected * 0.15 && velocity.z.abs() < expected * 0.15,
        "punted at {velocity}, expected about {expected} along +X"
    );

    // Too far away: nothing to punt.
    let mut far = Room::new(&data, data.manipulator.punt.range + 100.0, mass);
    let punt = far.aim(Buttons::ATTACK);
    assert_eq!(far.run(1, &punt), [ToolEvent::DryFire]);
}

#[test]
fn a_punt_rests_the_tool_for_half_a_second() {
    let data = data();
    let mut room = Room::with_pedestal(&data, 100.0, 30.0, 56.0);
    let punt = room.aim(Buttons::ATTACK);
    assert_eq!(room.run(1, &punt), [ToolEvent::Punted(room.prop)]);
    // Primary fire does nothing for the next half second (33 ticks)...
    assert!(room.run(30, &punt).is_empty(), "rested for 0.5 s");
    // ...and then fires again (at nothing: the prop is gone).
    assert_eq!(room.run(10, &punt), [ToolEvent::DryFire]);
}

#[test]
fn nothing_under_the_crosshair_takes_the_prop_in_the_cone() {
    let data = data();
    let mut room = Room::new(&data, 100.0, 20.0);
    // Eight degrees off: the ray misses a crate 16 wide at 100 units, but
    // it is well inside the cone.
    let off = UserCmd {
        yaw: 8.0,
        ..room.aim(Buttons::ATTACK2)
    };
    assert_eq!(room.run(1, &off), [ToolEvent::Grabbed(room.prop)]);

    // Forty degrees off is outside it.
    let mut room = Room::new(&data, 100.0, 20.0);
    let wide = UserCmd {
        yaw: 40.0,
        ..room.aim(Buttons::ATTACK2)
    };
    assert!(room.run(5, &wide).is_empty());
    assert_eq!(room.held(), None);
}

#[test]
fn a_fruitless_look_rests_secondary_fire() {
    let data = data();
    let mut room = Room::new(&data, 100.0, 20.0);
    // Look away (a miss), then straight at the prop.
    let away = UserCmd {
        yaw: 90.0,
        buttons: Buttons::ATTACK2,
        ..UserCmd::default()
    };
    assert!(room.run(1, &away).is_empty());
    let grab = room.aim(Buttons::ATTACK2);
    assert!(room.run(30, &grab).is_empty(), "still resting");
    assert_eq!(room.run(10, &grab), [ToolEvent::Grabbed(room.prop)]);
}

#[test]
fn a_pull_gives_light_props_the_same_speed_up_per_tick() {
    let data = data();
    let distance = 600.0;
    let mut speeds = Vec::new();
    for mass in [5.0, 20.0, 45.0] {
        let mut room = Room::new(&data, distance, mass);
        let cmd = room.aim(Buttons::ATTACK2);
        room.run(1, &cmd);
        let (velocity, _) = room.sim.physics().velocity(room.prop).expect("prop");
        speeds.push(velocity.length());
    }
    // (mass + 0.5) * 0.02 / mass is within a few percent of 0.02 * 1000/…
    // — about 85 units/s per 15 ms tick for them all.
    for speed in &speeds {
        assert!(
            (60.0..120.0).contains(speed),
            "one tick of pulling gave {speed}: {speeds:?}"
        );
    }
}

#[test]
fn secondary_fire_drops_and_waits_for_a_fresh_press() {
    let data = data();
    let mut room = Room::new(&data, 100.0, 10.0);
    let grab = room.aim(Buttons::ATTACK2);
    room.run(1, &grab);
    let carry = UserCmd::default();
    room.run(40, &carry);
    // Press to drop, keep holding: the prop is not grabbed straight back.
    let press = UserCmd {
        buttons: Buttons::ATTACK2,
        ..UserCmd::default()
    };
    let events = room.run(66, &press);
    assert_eq!(events, [ToolEvent::Dropped(room.prop)]);
    assert_eq!(room.held(), None);
}

#[test]
fn the_grip_breaks_when_the_prop_is_held_back() {
    let mut data = data();
    data.manipulator.hold.break_distance = 1.0;
    let mut room = Room::new(&data, 60.0, 10.0);
    let grab = room.aim(Buttons::ATTACK2);
    room.run(1, &grab);
    // Looking down puts the hold point nearly on the floor, where the
    // crate's centre cannot go: it stays a few units off, and the running
    // average of that error passes the (tiny) limit.
    let down = UserCmd {
        pitch: 89.0,
        ..UserCmd::default()
    };
    let events = room.run(132, &down);
    assert_eq!(events, [ToolEvent::GripBroke(room.prop)]);
}

#[test]
fn a_launched_prop_hurts_the_player_it_hits() {
    let data = data();
    let mut room = Room::new(&data, 60.0, 30.0);
    let victim = room.sim.spawn_player(Vec3::new(400.0, 0.0, 1.0));
    let grab = room.aim(Buttons::ATTACK2);
    room.run(1, &grab);
    room.run(30, &UserCmd::default());
    let fire = UserCmd {
        buttons: Buttons::ATTACK,
        ..UserCmd::default()
    };
    let mut impacts = Vec::new();
    for tick in 0..66 {
        let cmd = if tick == 0 { fire } else { UserCmd::default() };
        impacts.extend(room.tick(&cmd).impacts);
    }
    let hits: Vec<_> = impacts.iter().filter(|(id, _)| *id == victim).collect();
    assert_eq!(hits.len(), 1, "{impacts:?}");
    assert!(hits[0].1.damage > 50.0, "{:?}", hits[0]);
}

#[test]
fn a_prop_reaches_as_far_as_its_corners_along_a_direction() {
    let data = data();
    let room = Room::new(&data, 100.0, 20.0);
    let physics = room.sim.physics();
    let along = |direction: Vec3| {
        physics
            .extent(room.prop, direction.normalize())
            .expect("prop")
    };
    // A crate of half-size 8: a face is 8 away, a corner 8 * sqrt(3).
    assert!((along(-Vec3::X) - 8.0).abs() < 0.1);
    assert!((along(Vec3::Z) - 8.0).abs() < 0.1);
    assert!((along(Vec3::ONE) - 8.0 * 3f32.sqrt()).abs() < 0.1);
}
