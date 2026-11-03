//! Carrying props in the hands through the whole simulation: use picks up a
//! light prop and puts the weapon away, the prop is carried ahead and turns
//! with the view, primary fire tosses it gently, secondary fire or use again
//! sets it down, and the weapon comes back out; heavy, big and stood-on props
//! stay where they are, sprinting stops, and a prop left behind falls from the
//! hands.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a missing player or prop must fail the test"
)]

use std::sync::Arc;

use glam::{Quat, Vec3};
use rl_data::GameData;
use rl_sim::collision::contents::SOLID;
use rl_sim::collision::{ConvexHull, WorldBuilder};
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::physics::{BodyId, Motion, PropDesc};
use rl_sim::simulation::{PlayerId, Simulation};
use rl_sim::tools::hands::HandsEvent;
use rl_sim::weapons::Phase;

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

/// A box prop `size` units on a side standing on its origin.
fn box_desc(origin: Vec3, size: f32, mass: f32) -> PropDesc {
    let half = size / 2.0;
    PropDesc {
        pieces: vec![ConvexHull::from_box(
            Vec3::new(-half, -half, 0.0),
            Vec3::new(half, half, size),
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
        contents: SOLID,
    }
}

/// A big floor (top at z = 0), a player at the origin looking along +X with
/// the loadout's gun in hand and a box `distance` units ahead, settled.
struct Room {
    sim: Simulation,
    player: PlayerId,
    prop: BodyId,
}

impl Room {
    fn new(distance: f32, size: f32, mass: f32) -> Self {
        let world = WorldBuilder::new().solid_box(
            Vec3::new(-4096.0, -4096.0, -64.0),
            Vec3::new(4096.0, 4096.0, 0.0),
        );
        let mut sim = Simulation::new(Arc::new(world.build()), &data());
        let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
        let prop = sim.add_prop(&box_desc(Vec3::new(distance, 0.0, 0.0), size, mass));
        let mut room = Self { sim, player, prop };
        // The match starts on the first tick, with a fresh loadout; then the
        // gun in hand finishes deploying and the box settles.
        room.run(120, &UserCmd::default());
        room
    }

    fn center(&self) -> Vec3 {
        self.sim.physics().center_of_mass(self.prop).expect("prop")
    }

    fn eye(&self) -> Vec3 {
        self.sim.eye_position(self.player).expect("player")
    }

    /// Looking at the box's centre, pressing `buttons`.
    fn at_box(&self, buttons: Buttons) -> UserCmd {
        let to = self.center() - self.eye();
        UserCmd {
            pitch: -(to.z.atan2(to.truncate().length())).to_degrees(),
            yaw: to.y.atan2(to.x).to_degrees(),
            buttons,
            ..UserCmd::default()
        }
    }

    /// Ticks `count` times with `cmd` and gathers the hands' events.
    fn run(&mut self, count: usize, cmd: &UserCmd) -> Vec<HandsEvent> {
        (0..count)
            .flat_map(|_| self.sim.tick(&[(self.player, *cmd)]).hands)
            .map(|(_, event)| event)
            .collect()
    }

    /// Presses use looking at the box, then lets go of the key.
    fn use_box(&mut self) -> Vec<HandsEvent> {
        let mut events = self.run(1, &self.at_box(Buttons::USE));
        events.extend(self.run(1, &self.at_box(Buttons::NONE)));
        events
    }

    fn carried(&self) -> Option<BodyId> {
        self.sim.player(self.player).expect("player").hands.held()
    }

    fn holstered(&self) -> bool {
        self.sim
            .player(self.player)
            .expect("player")
            .inventory
            .holstered()
    }

    fn velocity(&self) -> Vec3 {
        self.sim.physics().velocity(self.prop).expect("prop").0
    }
}

/// Level along +X, pressing `buttons`.
fn level(buttons: Buttons) -> UserCmd {
    UserCmd {
        buttons,
        ..UserCmd::default()
    }
}

#[test]
fn use_picks_up_a_light_prop_and_puts_the_weapon_away() {
    let mut room = Room::new(48.0, 16.0, 5.0);
    assert!(!room.holstered());
    assert_eq!(room.use_box(), [HandsEvent::PickedUp(room.prop)]);
    assert_eq!(room.carried(), Some(room.prop));
    assert!(room.holstered(), "the weapon is put away");
    assert!(room.sim.physics().holder(room.prop).is_some());
    assert_eq!(
        room.sim.player(room.player).expect("player").held_prop(),
        Some(room.prop)
    );
    // The weapon does nothing while it is put away: nothing is fired.
    let fired: usize = (0..30)
        .map(|_| {
            room.sim
                .tick(&[(room.player, level(Buttons::ATTACK2))])
                .fired
                .len()
        })
        .sum();
    assert_eq!(fired, 0);
}

#[test]
fn the_prop_is_carried_ahead_and_turns_with_the_view() {
    let mut room = Room::new(48.0, 16.0, 5.0);
    room.use_box();
    room.run(40, &level(Buttons::NONE));
    let ahead = room.center() - room.eye();
    // `gap` 24 + the player's half diagonal (22.6) + the box's 8 back from its
    // origin, straight ahead at eye height.
    let expected = 24.0 + 16.0_f32.hypot(16.0) + 8.0;
    assert!((ahead.x - expected).abs() < 1.0, "{ahead}");
    assert!(ahead.y.abs() < 1.0 && ahead.z.abs() < 1.0, "{ahead}");

    let turned = UserCmd {
        yaw: 90.0,
        ..UserCmd::default()
    };
    room.run(40, &turned);
    let ahead = room.center() - room.eye();
    assert!((ahead.y - expected).abs() < 1.0, "{ahead}");
    assert!(ahead.x.abs() < 1.0, "{ahead}");
    let rotation = room.sim.physics().pose(room.prop).expect("prop").rotation;
    let facing = rotation * Vec3::X;
    assert!(
        facing.abs_diff_eq(Vec3::Y, 0.02),
        "turned with the view: {facing}"
    );
}

#[test]
fn looking_down_holds_the_prop_clear_of_the_feet() {
    let mut room = Room::new(48.0, 16.0, 5.0);
    room.use_box();
    let down = UserCmd {
        pitch: 89.0,
        ..UserCmd::default()
    };
    room.run(40, &down);
    let center = room.center();
    // Never closer than the player's half diagonal plus the box's depth to
    // their upright axis.
    let radius = 16.0_f32.hypot(16.0) + 8.0;
    assert!(center.truncate().length() >= radius - 0.5, "{center}");
}

#[test]
fn primary_fire_tosses_the_prop_gently_and_brings_the_weapon_back() {
    let mut room = Room::new(48.0, 16.0, 5.0);
    room.use_box();
    room.run(40, &level(Buttons::NONE));
    let events = room.sim.tick(&[(room.player, level(Buttons::ATTACK))]);
    assert_eq!(
        events
            .hands
            .iter()
            .map(|&(_, event)| event)
            .collect::<Vec<_>>(),
        [HandsEvent::Tossed(room.prop)]
    );
    assert!(
        events.deploys.iter().any(|&(id, _)| id == room.player),
        "the weapon is drawn again"
    );
    assert!(room.carried().is_none() && !room.holstered());
    let inventory = &room.sim.player(room.player).expect("player").inventory;
    assert!(matches!(inventory.phase(), Phase::Deploying(_)));
    // 1000 × the factor of 5 kg (0.5 + 4.5 / 14.5 × 3.5) over 5 kg, less a
    // tick of gravity.
    let speed = 1000.0 * (0.5 + 4.5 / 14.5 * 3.5) / 5.0;
    let velocity = room.velocity();
    assert!((velocity.x - speed).abs() < 5.0, "{velocity} vs {speed}");
    assert!(velocity.y.abs() < 1.0, "{velocity}");
}

#[test]
fn heavy_props_are_tossed_no_faster_than_a_walk() {
    let mut room = Room::new(56.0, 32.0, 35.0);
    assert_eq!(room.use_box(), [HandsEvent::PickedUp(room.prop)]);
    room.run(60, &level(Buttons::NONE));
    room.run(1, &level(Buttons::ATTACK));
    let speed = room.velocity().x;
    assert!((speed - 4000.0 / 35.0).abs() < 5.0, "{speed}");
}

#[test]
fn secondary_fire_or_use_again_sets_the_prop_down() {
    let mut room = Room::new(48.0, 16.0, 5.0);
    room.use_box();
    room.run(40, &level(Buttons::NONE));
    assert_eq!(
        room.run(1, &level(Buttons::ATTACK2)),
        [HandsEvent::Dropped(room.prop)]
    );
    assert!(room.carried().is_none() && !room.holstered());
    assert!(room.velocity().length() < 1.0, "set down, not thrown");
    room.run(120, &level(Buttons::NONE));

    room.use_box();
    assert_eq!(room.carried(), Some(room.prop));
    room.run(40, &level(Buttons::NONE));
    assert_eq!(
        room.run(1, &level(Buttons::USE)),
        [HandsEvent::Dropped(room.prop)]
    );
    assert!(room.carried().is_none() && !room.holstered());
}

#[test]
fn a_prop_swept_along_is_let_go_no_faster_than_a_walk() {
    let mut room = Room::new(48.0, 16.0, 5.0);
    room.use_box();
    room.run(40, &level(Buttons::NONE));
    // Swing the view round fast, then let go mid-swing.
    for step in 1..=6 {
        let swing = UserCmd {
            yaw: step as f32 * 20.0,
            ..UserCmd::default()
        };
        room.run(1, &swing);
    }
    let fast = room.velocity().length();
    room.run(1, &level(Buttons::ATTACK2));
    let speed = room.velocity().length();
    assert!(fast > 285.0, "the swing was fast: {fast}");
    assert!(speed <= 285.0 + 1.0, "{speed}");
}

#[test]
fn heavy_or_big_props_cannot_be_picked_up() {
    let mut heavy = Room::new(48.0, 16.0, 36.0);
    assert!(heavy.use_box().is_empty());
    assert!(heavy.carried().is_none() && !heavy.holstered());

    let mut big = Room::new(96.0, 130.0, 20.0);
    assert!(big.use_box().is_empty());
    assert!(big.carried().is_none() && !big.holstered());
}

#[test]
fn use_on_nothing_leaves_the_weapon_in_hand() {
    let mut room = Room::new(48.0, 16.0, 5.0);
    let away = UserCmd {
        yaw: 180.0,
        buttons: Buttons::USE,
        ..UserCmd::default()
    };
    assert!(room.run(1, &away).is_empty());
    assert!(!room.holstered());
}

#[test]
fn a_prop_stood_on_cannot_be_picked_up() {
    let mut room = Room::new(200.0, 32.0, 30.0);
    assert!(
        room.sim
            .teleport_player(room.player, Vec3::new(200.0, 0.0, 33.0))
    );
    room.run(30, &level(Buttons::NONE));
    let down = UserCmd {
        pitch: 89.0,
        buttons: Buttons::USE,
        ..UserCmd::default()
    };
    assert!(room.run(1, &down).is_empty());
    assert!(room.carried().is_none());
}

#[test]
fn no_sprinting_while_carrying() {
    let mut room = Room::new(48.0, 16.0, 5.0);
    room.use_box();
    let sprint = UserCmd {
        forward: 1.0,
        buttons: Buttons::SPRINT,
        ..UserCmd::default()
    };
    room.run(20, &sprint);
    let state = room.sim.player(room.player).expect("player").state;
    assert!(!state.sprinting);
}

#[test]
fn a_prop_left_far_behind_falls_from_the_hands_after_the_grace() {
    let mut room = Room::new(48.0, 16.0, 5.0);
    room.use_box();
    room.run(10, &level(Buttons::NONE));
    assert!(
        room.sim
            .teleport_player(room.player, Vec3::new(-3000.0, 0.0, 1.0))
    );
    // Held back from the start, but nothing counts for the first second.
    assert!(room.run(50, &level(Buttons::NONE)).is_empty());
    let events = room.run(40, &level(Buttons::NONE));
    assert_eq!(events, [HandsEvent::Dropped(room.prop)]);
    assert!(!room.holstered());
}

#[test]
fn the_gravity_gun_is_put_away_too() {
    let mut room = Room::new(48.0, 16.0, 5.0);
    let manipulator = room.sim.arsenal().find_weapon("manipulator");
    let select = UserCmd {
        select: manipulator,
        ..UserCmd::default()
    };
    room.run(1, &select);
    room.run(60, &level(Buttons::NONE));
    room.use_box();
    assert_eq!(room.carried(), Some(room.prop));
    // Primary fire tosses what the hands carry rather than punting it.
    let events = room.sim.tick(&[(room.player, level(Buttons::ATTACK))]);
    assert!(events.tools.is_empty(), "{:?}", events.tools);
    assert_eq!(
        events
            .hands
            .iter()
            .map(|&(_, event)| event)
            .collect::<Vec<_>>(),
        [HandsEvent::Tossed(room.prop)]
    );
}
