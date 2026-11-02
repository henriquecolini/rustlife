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
use rl_sim::physics::{BodyId, Breakable, Motion, PropDesc};
use rl_sim::simulation::{PlayerId, Simulation, TickEvents};
use rl_sim::tools::manipulator::{Aim, ToolEvent, find_target};

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
        Self::in_world(data, distance, mass, height, WorldBuilder::new())
    }

    /// As [`Room::with_pedestal`], in `world` with the floor added.
    fn in_world(
        data: &GameData,
        distance: f32,
        mass: f32,
        height: f32,
        world: WorldBuilder,
    ) -> Self {
        let mut world = world.solid_box(
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

    /// Where the player aims with `pitch`, along +X.
    fn view(&self, pitch: f32) -> Aim {
        Aim {
            eye: self.sim.eye_position(self.player).expect("player"),
            pitch,
            yaw: 0.0,
        }
    }

    /// The prop the tool would take looking at the crate, within `range`.
    fn target(&self, range: f32) -> Option<BodyId> {
        let manipulator = &self.sim.data().manipulator;
        let (physics, world) = (self.sim.physics(), self.sim.world());
        find_target(
            &self.view(self.aim_pitch()),
            world,
            physics,
            manipulator,
            range,
        )
        .map(|(body, _)| body)
    }

    /// Looking level, `yaw` degrees to the side, pressing `buttons`.
    fn level(yaw: f32, buttons: Buttons) -> UserCmd {
        UserCmd {
            yaw,
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
    let radius = room.sim.physics().radius(room.prop).expect("prop");
    let eye = Vec3::new(0.0, 0.0, 1.0 + 64.0);
    let expected = eye + Vec3::X * (data.manipulator.hold.distance + radius);
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
    for _ in 0..400 {
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
fn far_props_start_to_move_slowly_and_gain_speed() {
    let data = data();
    let grab = &data.manipulator.grab;
    let mut room = Room::new(&data, grab.pull_range - 50.0, 10.0);
    let speed = |room: &Room| {
        room.sim
            .physics()
            .velocity(room.prop)
            .expect("prop")
            .0
            .length()
    };
    let cmd = room.aim(Buttons::ATTACK2);
    room.run(8, &cmd);
    let early = speed(&room);
    assert!(early < grab.pull_speed * 0.5, "gentle at first: {early}");
    for _ in 0..80 {
        let cmd = room.aim(Buttons::ATTACK2);
        room.run(1, &cmd);
    }
    let later = speed(&room);
    assert!(later > early * 2.0, "then faster: {early} -> {later}");
    assert!(later <= grab.pull_speed + 1.0, "never past the top speed");
}

#[test]
fn the_claws_look_no_farther_than_the_open_range_but_a_pull_reaches_on() {
    let data = data();
    let grab = &data.manipulator.grab;
    assert!(grab.range < grab.open_range && grab.open_range < grab.pull_range);

    let near = Room::new(&data, grab.open_range - 40.0, 10.0);
    assert_eq!(near.target(grab.open_range), Some(near.prop));

    let far = Room::new(&data, grab.open_range + 100.0, 10.0);
    assert_eq!(far.target(grab.open_range), None, "too far to open on");
    assert_eq!(far.target(grab.reach()), Some(far.prop), "not to pull");
}

/// A pedestal that puts a crate's centre level with the eyes, so that the view's
/// yaw alone says how far off the crosshair it is.
const EYE_LEVEL: f32 = 56.0;

#[test]
fn a_crate_just_off_the_crosshair_is_grabbed() {
    let data = data();
    // At 100 units, the crate's near face has its edge 8 units off the line at
    // 92: a ray at 6 degrees misses it, by less than the box's 4 units.
    let mut room = Room::with_pedestal(&data, 100.0, 10.0, EYE_LEVEL);
    let events = room.run(1, &Room::level(6.0, Buttons::ATTACK2));
    assert_eq!(events, [ToolEvent::Grabbed(room.prop)], "the box finds it");

    // At 9 degrees the box misses it too, and only the cone can find it.
    let mut room = Room::with_pedestal(&data, 100.0, 10.0, EYE_LEVEL);
    let events = room.run(1, &Room::level(9.0, Buttons::ATTACK2));
    assert_eq!(events, [ToolEvent::Grabbed(room.prop)], "the cone finds it");
}

#[test]
fn a_crate_outside_the_cone_is_not_grabbed() {
    let data = data();
    assert!(data.manipulator.grab.aim_cone > 20_f32.to_radians().cos());
    let mut room = Room::with_pedestal(&data, 100.0, 10.0, EYE_LEVEL);
    assert!(room.run(5, &Room::level(20.0, Buttons::ATTACK2)).is_empty());
    assert_eq!(room.held(), None);
}

#[test]
fn the_cone_stops_short_of_the_pull_range() {
    let data = data();
    // 3 degrees off at 400 units: 21 units off the line, in the cone's angle but
    // past its range; a pull needs the ray or the box to find it.
    let room = Room::with_pedestal(&data, 400.0, 10.0, EYE_LEVEL);
    let aim = room.view(0.0);
    let manipulator = &data.manipulator;
    let found = |range: f32, yaw: f32| {
        find_target(
            &Aim { yaw, ..aim },
            room.sim.world(),
            room.sim.physics(),
            manipulator,
            range,
        )
    };
    assert_eq!(found(1000.0, 3.0), None, "off to the side of a far crate");
    assert!(found(1000.0, 0.0).is_some(), "under the crosshair");
}

#[test]
fn a_crate_behind_a_wall_is_not_grabbed() {
    let data = data();
    // The view passes the wall's end at 9 degrees, the way to the crate does not.
    let wall =
        WorldBuilder::new().solid_box(Vec3::new(50.0, -100.0, 0.0), Vec3::new(60.0, 2.0, 300.0));
    let mut room = Room::in_world(&data, 100.0, 10.0, EYE_LEVEL, wall);
    assert!(room.run(5, &Room::level(9.0, Buttons::ATTACK2)).is_empty());
    assert_eq!(room.held(), None);
}

#[test]
fn a_crate_behind_another_is_not_taken_through_it() {
    let data = data();
    let mut room = Room::with_pedestal(&data, 100.0, 10.0, EYE_LEVEL);
    // A crate fixed in place stands in the way of the line to the light one's
    // centre, but clear of the view, and cannot be taken itself.
    let mut fixed = crate_desc(Vec3::new(60.0, 0.0, EYE_LEVEL), 10.0);
    fixed.motion = Motion::Fixed;
    let fixed = room.sim.add_prop(&fixed);
    room.run(1, &UserCmd::default());
    assert!(!room.sim.physics().is_grabbable(fixed));
    assert!(room.run(5, &Room::level(13.5, Buttons::ATTACK2)).is_empty());
    assert_eq!(room.held(), None);
}

#[test]
fn the_nearest_of_the_crates_in_the_cone_is_taken_and_ties_go_to_the_first() {
    let data = data();
    let mut room = Room::new(&data, 1000.0, 10.0);
    let at = |x: f32, y: f32| crate_desc(Vec3::new(x, y, 0.0), 10.0);
    let (left, right) = (
        room.sim.add_prop(&at(100.0, 20.0)),
        room.sim.add_prop(&at(100.0, -20.0)),
    );
    let far = room.sim.add_prop(&at(150.0, 22.0));
    room.sim.refresh_props();
    assert!(left < right && right < far);
    // Straight between the near two, whose centres are level with each other.
    let eye = room.sim.eye_position(room.player).expect("player");
    let pitch = -(8.0 - eye.z).atan2(100.0).to_degrees();
    let found = find_target(
        &room.view(pitch),
        room.sim.world(),
        room.sim.physics(),
        &data.manipulator,
        data.manipulator.grab.reach(),
    );
    assert_eq!(found.map(|(body, _)| body), Some(left));
}

/// How a test's crate breaks.
fn breakable(health: f32) -> Breakable {
    Breakable {
        health,
        bullets: 1.0,
        club: 1.0,
        explosive: 1.0,
        explosion: None,
        gibs: Vec::new(),
        pieces: Vec::new(),
        gib_count: 0,
        respawn: None,
    }
}

/// A room with a crate `desc` too, at eye level 100 units ahead and 40 to the
/// side.
fn room_with(desc: PropDesc) -> (Room, BodyId) {
    let data = data();
    let mut room = Room::with_pedestal(&data, 100.0, 10.0, EYE_LEVEL);
    let body = room.sim.add_prop(&PropDesc {
        origin: Vec3::new(100.0, 40.0, EYE_LEVEL),
        ..desc
    });
    room.sim.refresh_props();
    (room, body)
}

/// A crate fixed to the world.
fn frozen() -> PropDesc {
    PropDesc {
        motion: Motion::Fixed,
        ..crate_desc(Vec3::ZERO, 10.0)
    }
}

/// Primary fire with the view on the crate [`room_with`] added.
fn punt_at_room_with(room: &mut Room) -> Vec<ToolEvent> {
    let cmd = UserCmd {
        yaw: 21.8,
        pitch: 0.0,
        buttons: Buttons::ATTACK,
        ..UserCmd::default()
    };
    room.run(1, &cmd)
}

#[test]
fn a_punt_breaks_a_frozen_prop_however_healthy() {
    let (mut room, body) = room_with(PropDesc {
        breakable: Some(breakable(1_000.0)),
        ..frozen()
    });
    let events = punt_at_room_with(&mut room);
    assert!(
        matches!(events[..], [ToolEvent::SmashedProp(smashed, _)] if smashed == body),
        "{events:?}"
    );
    assert!(room.sim.physics().is_broken(body));
}

#[test]
fn a_punt_frees_a_frozen_prop_the_map_lets_it_free_and_knocks_it_away() {
    let (mut room, body) = room_with(PropDesc {
        free_on_grab: true,
        ..frozen()
    });
    let before = room.sim.physics().pose(body).expect("prop").position;
    let events = punt_at_room_with(&mut room);
    assert_eq!(events, [ToolEvent::Punted(body)]);
    assert!(room.sim.physics().is_dynamic(body));
    room.run(5, &UserCmd::default());
    let after = room.sim.physics().pose(body).expect("prop").position;
    assert!(after.distance(before) > 10.0, "{before} to {after}");
}

#[test]
fn a_prop_that_can_both_break_and_come_free_breaks() {
    let (mut room, body) = room_with(PropDesc {
        free_on_grab: true,
        breakable: Some(breakable(12.0)),
        ..frozen()
    });
    let events = punt_at_room_with(&mut room);
    assert!(
        matches!(events[..], [ToolEvent::SmashedProp(..)]),
        "{events:?}"
    );
    assert!(room.sim.physics().is_broken(body));
}

#[test]
fn a_punt_is_a_dry_fire_on_a_frozen_prop_that_cannot_break_or_come_free() {
    let (mut room, body) = room_with(frozen());
    let before = room.sim.physics().pose(body).expect("prop").position;
    assert_eq!(punt_at_room_with(&mut room), [ToolEvent::DryFire]);
    room.run(5, &UserCmd::default());
    assert!(!room.sim.physics().is_dynamic(body));
    assert_eq!(
        room.sim.physics().pose(body).expect("prop").position,
        before
    );
}

#[test]
fn a_prop_that_moves_is_knocked_away_not_smashed_even_if_it_can_break() {
    let (mut room, body) = room_with(PropDesc {
        motion: Motion::Dynamic,
        breakable: Some(breakable(12.0)),
        ..crate_desc(Vec3::ZERO, 10.0)
    });
    assert_eq!(punt_at_room_with(&mut room), [ToolEvent::Punted(body)]);
}

#[test]
fn a_punt_finds_a_crate_a_little_further_off_than_a_grab_does() {
    let data = data();
    // The box of a punt is 8 units to the side: at 7 degrees the near face's edge
    // is 3.3 units from reaching the line.
    let mut room = Room::with_pedestal(&data, 100.0, 10.0, EYE_LEVEL);
    let events = room.run(1, &Room::level(7.0, Buttons::ATTACK));
    assert_eq!(events, [ToolEvent::Punted(room.prop)]);
    // There is no cone for a punt.
    let mut room = Room::with_pedestal(&data, 100.0, 10.0, EYE_LEVEL);
    let events = room.run(1, &Room::level(13.0, Buttons::ATTACK));
    assert_eq!(events, [ToolEvent::DryFire]);
}

#[test]
fn a_punt_along_the_floor_is_not_stopped_by_it() {
    let data = data();
    // Aimed at the foot of a crate on the floor: the box hits the floor first.
    let mut room = Room::new(&data, 60.0, 10.0);
    let eye = room.sim.eye_position(room.player).expect("player");
    let pitch = -(1.0 - eye.z).atan2(52.0).to_degrees();
    let punt = UserCmd {
        pitch,
        buttons: Buttons::ATTACK,
        ..UserCmd::default()
    };
    assert_eq!(room.run(1, &punt), [ToolEvent::Punted(room.prop)]);
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
    let expected = data.manipulator.punt.speed_for(mass);
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
    data.manipulator.hold.break_distance = 4.0;
    data.manipulator.hold.break_time = 0.1;
    let mut room = Room::new(&data, 60.0, 10.0);
    let grab = room.aim(Buttons::ATTACK2);
    room.run(1, &grab);
    // Looking straight down puts the hold point under the floor.
    let down = UserCmd {
        pitch: 89.0,
        ..UserCmd::default()
    };
    let events = room.run(66, &down);
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
