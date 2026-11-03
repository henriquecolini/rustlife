//! Physical pickups through the whole simulation: a medkit falls to the
//! floor and knocks on it, taking it hides its body until it respawns where
//! it was placed, the Manipulator can grab and throw it without taking it, and
//! pulls in what the player can use for them to collect.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a missing body or player must fail the test"
)]

use std::sync::Arc;

use glam::{Quat, Vec3};
use rl_data::GameData;
use rl_sim::collision::contents::SOLID;
use rl_sim::collision::{ConvexHull, WorldBuilder};
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::physics::{BodyId, Motion, PropDesc};
use rl_sim::simulation::{PlayerId, Simulation};
use rl_sim::tools::manipulator::ToolEvent;
use source_formats::entities::MapEntity;

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

/// A big floor with its top at z = 0.
fn floor() -> rl_sim::collision::CollisionWorld {
    WorldBuilder::new()
        .solid_box(
            Vec3::new(-4096.0, -4096.0, -64.0),
            Vec3::new(4096.0, 4096.0, 0.0),
        )
        .build()
}

/// A small box-shaped pickup body standing on `origin`, lying as debris:
/// the shape of a medkit's fall, without needing the game's models.
fn pickup_desc(origin: Vec3) -> PropDesc {
    PropDesc {
        pieces: vec![ConvexHull::from_box(
            Vec3::new(-8.0, -8.0, 0.0),
            Vec3::new(8.0, 8.0, 16.0),
        )],
        origin,
        rotation: Quat::IDENTITY,
        motion: Motion::Dynamic,
        mass: 5.0,
        material: *data().physics.materials.get("wood"),
        linear_damping: 0.0,
        angular_damping: 0.0,
        debris: true,
        start_asleep: false,
        grabbable: true,
        free_on_grab: false,
        breakable: None,
        surface: "metal".to_owned(),
        contents: SOLID,
    }
}

/// Adds a medkit at `origin` and gives it a body.
fn add_medkit(sim: &mut Simulation, origin: Vec3) -> BodyId {
    sim.add_entities(&[MapEntity::new("item_healthkit", origin.to_array())]);
    sim.add_pickup_bodies([Some(pickup_desc(origin))]);
    sim.pickups()
        .last()
        .expect("the medkit")
        .body
        .expect("the medkit lies about")
}

/// The pitch that aims the player's eyes at `target`.
fn aim_pitch(sim: &Simulation, player: PlayerId, target: Vec3) -> f32 {
    let state = sim.player(player).expect("player").state;
    let eye = state.origin + Vec3::Z * 64.0;
    let to = target - eye;
    -(to.z.atan2(to.truncate().length())).to_degrees()
}

#[test]
fn a_pickup_falls_to_the_floor_and_its_landing_is_heard() {
    let data = data();
    let mut sim = Simulation::new(Arc::new(floor()), &data);
    let body = add_medkit(&mut sim, Vec3::new(100.0, 0.0, 200.0));
    let start = sim.physics().pose(body).expect("body").position;
    assert_eq!(start, Vec3::new(100.0, 0.0, 200.0));

    let mut impacts = 0;
    for _ in 0..240 {
        impacts += sim.tick(&[]).prop_impacts.len();
    }

    let landed = sim.physics().pose(body).expect("body").position;
    assert!(landed.z < 1.0, "the medkit landed at {landed:?}");
    assert!(impacts > 0, "the medkit's landing made no sound");
    // Its origin follows its body, so touches and the client agree on where
    // it is.
    let pickup = &sim.pickups()[0];
    assert_eq!(pickup.origin, landed);
}

#[test]
fn taking_a_pickup_hides_its_body_until_it_respawns_at_home() {
    let mut data = data();
    data.items.get_mut("medkit").expect("medkit").respawn = 0.2;
    let mut sim = Simulation::new(Arc::new(floor()), &data);
    let home = Vec3::new(20.0, 0.0, 0.0);
    let body = add_medkit(&mut sim, home);
    let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
    // The match starts on the first tick.
    sim.tick(&[]);
    sim.player_mut(player).expect("player").vitals.health = 50.0;

    let events = sim.tick(&[]);
    assert!(
        events
            .pickups
            .iter()
            .any(|&(who, index)| who == player && index == 0),
        "the hurt player standing on the medkit took it: {events:?}"
    );
    assert_eq!(sim.player(player).expect("player").vitals.health, 75.0);
    let pickup = &sim.pickups()[0];
    assert!(!pickup.available(), "the taken medkit is still there");
    assert!(
        !sim.physics().prop_enabled(body),
        "the taken medkit's body is still in the world"
    );

    // A full player does not take it again when it comes back.
    sim.player_mut(player).expect("player").vitals.health = 100.0;
    for _ in 0..30 {
        sim.tick(&[]);
    }
    let pickup = &sim.pickups()[0];
    assert!(pickup.available(), "the medkit did not come back");
    assert!(
        sim.physics().prop_enabled(body),
        "the returned medkit's body is still out of the world"
    );
    let pose = sim.physics().pose(body).expect("body");
    assert!(
        pose.position.distance(home) < 0.1,
        "the medkit came back at {:?}, not home",
        pose.position
    );
    assert_eq!(pickup.origin, pose.position);
}

#[test]
fn a_pickup_can_be_grabbed_without_being_taken() {
    let data = data();
    let mut sim = Simulation::new(Arc::new(floor()), &data);
    let body = add_medkit(&mut sim, Vec3::new(100.0, 0.0, 0.0));
    let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
    sim.tick(&[]);
    let manipulator = sim.arsenal().find_weapon("manipulator");
    assert!(
        manipulator.is_some(),
        "the built-in data has the Manipulator"
    );
    sim.tick(&[(
        player,
        UserCmd {
            select: manipulator,
            ..UserCmd::default()
        },
    )]);
    for _ in 0..30 {
        sim.tick(&[(player, UserCmd::default())]);
    }

    let center = sim.physics().center_of_mass(body).expect("body");
    let aim = UserCmd {
        pitch: aim_pitch(&sim, player, center),
        buttons: Buttons::ATTACK2,
        ..UserCmd::default()
    };
    let events = sim.tick(&[(player, aim)]);
    assert!(
        events
            .tools
            .iter()
            .any(|(_, event)| matches!(event, ToolEvent::Grabbed(grabbed) if *grabbed == body)),
        "the Manipulator did not grab the medkit: {events:?}"
    );
    let held = sim.player(player).expect("player").manipulator.held();
    assert_eq!(held, Some(body));

    // Touching the medkit while it is held must not take it out of the
    // Manipulator's grip.
    sim.player_mut(player).expect("player").vitals.health = 50.0;
    let events = sim.tick(&[(player, UserCmd::default())]);
    assert!(
        events.pickups.is_empty(),
        "a held medkit was taken: {events:?}"
    );
    assert_eq!(sim.player(player).expect("player").vitals.health, 50.0);
    assert!(sim.pickups()[0].available());
    assert_eq!(
        sim.player(player).expect("player").manipulator.held(),
        Some(body)
    );

    // And it can be thrown.
    sim.player_mut(player).expect("player").vitals.health = 100.0;
    let events = sim.tick(&[(
        player,
        UserCmd {
            pitch: aim.pitch,
            buttons: Buttons::ATTACK,
            ..UserCmd::default()
        },
    )]);
    assert!(
        events
            .tools
            .iter()
            .any(|(_, event)| matches!(event, ToolEvent::Launched(launched) if *launched == body)),
        "the Manipulator did not throw the medkit: {events:?}"
    );
    assert_eq!(sim.player(player).expect("player").manipulator.held(), None);
}

/// A player with the Manipulator in hand and a pickup of class `class` lying
/// `distance` units ahead, settled.
struct PullRange {
    sim: Simulation,
    player: PlayerId,
    body: BodyId,
}

impl PullRange {
    fn new(data: &GameData, class: &str, distance: f32) -> Self {
        let mut sim = Simulation::new(Arc::new(floor()), data);
        let origin = Vec3::new(distance, 0.0, 0.0);
        sim.add_entities(&[MapEntity::new(class, origin.to_array())]);
        sim.add_pickup_bodies([Some(pickup_desc(origin))]);
        let body = sim.pickups()[0].body.expect("the pickup lies about");
        let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
        sim.tick(&[]);
        let select = UserCmd {
            select: sim.arsenal().find_weapon("manipulator"),
            ..UserCmd::default()
        };
        sim.tick(&[(player, select)]);
        for _ in 0..100 {
            sim.tick(&[(player, UserCmd::default())]);
        }
        Self { sim, player, body }
    }

    fn center(&self) -> Vec3 {
        self.sim.physics().center_of_mass(self.body).expect("body")
    }

    /// Holds secondary fire at the pickup for up to `ticks` ticks, `follow`ing
    /// it with the view or leaving the view where it starts. Returns whether it
    /// was taken and whether it was grabbed; stops when either happens.
    fn pull(&mut self, ticks: usize, follow: bool) -> (bool, bool) {
        let pitch = aim_pitch(&self.sim, self.player, self.center());
        for _ in 0..ticks {
            let cmd = UserCmd {
                pitch: if follow {
                    aim_pitch(&self.sim, self.player, self.center())
                } else {
                    pitch
                },
                buttons: Buttons::ATTACK2,
                ..UserCmd::default()
            };
            let events = self.sim.tick(&[(self.player, cmd)]);
            let taken = !events.pickups.is_empty();
            let grabbed = events
                .tools
                .iter()
                .any(|(_, event)| matches!(event, ToolEvent::Grabbed(_)));
            if taken || grabbed {
                return (taken, grabbed);
            }
        }
        (false, false)
    }

    fn hurt(&mut self) {
        self.sim
            .player_mut(self.player)
            .expect("player")
            .vitals
            .health = 50.0;
    }

    fn holds_it(&self) -> bool {
        self.sim
            .player(self.player)
            .expect("player")
            .manipulator
            .held()
            == Some(self.body)
    }
}

#[test]
fn a_hurt_player_pulling_a_medkit_collects_it() {
    let data = data();
    // Close by, past grab range, and from afar. Like any pull, one from afar has
    // to be followed with the view; a short one gets there on its own.
    for (distance, follow) in [
        (100.0, false),
        (100.0, true),
        (300.0, false),
        (300.0, true),
        (700.0, true),
    ] {
        let mut range = PullRange::new(&data, "item_healthkit", distance);
        range.hurt();
        assert_eq!(
            range.pull(132 + (distance / 5.0) as usize, follow),
            (true, false),
            "from {distance}, following it: {follow}"
        );
        let player = range.sim.player(range.player).expect("player");
        assert_eq!(player.vitals.health, 75.0);
        assert!(!range.sim.pickups()[0].available());
        assert!(!range.sim.physics().prop_enabled(range.body));
        assert_eq!(player.manipulator.held(), None);
    }
}

#[test]
fn a_pull_stops_short_of_nothing_once_the_medkit_is_taken() {
    let data = data();
    let mut range = PullRange::new(&data, "item_healthkit", 100.0);
    range.hurt();
    assert!(range.pull(200, true).0);
    // Secondary fire goes on being held, on nothing: nothing happens.
    let cmd = UserCmd {
        buttons: Buttons::ATTACK2,
        ..UserCmd::default()
    };
    for _ in 0..30 {
        let events = range.sim.tick(&[(range.player, cmd)]);
        assert!(events.tools.is_empty(), "{events:?}");
        assert!(events.pickups.is_empty(), "{events:?}");
    }
}

#[test]
fn a_pulled_medkit_never_gets_past_the_player() {
    let data = data();
    let mut range = PullRange::new(&data, "item_healthkit", 700.0);
    range.hurt();
    let mut last = range.center().x;
    for _ in 0..300 {
        if range.pull(1, true).0 {
            return;
        }
        let x = range.center().x;
        assert!(x <= last + 1.0, "it turned back: {last} -> {x}");
        assert!(x > 0.0, "it passed the player without being taken: {x}");
        last = x;
    }
    panic!("the medkit never arrived, last at {last}");
}

#[test]
fn a_player_at_full_health_holds_the_medkit_they_pull_and_does_not_take_it() {
    let data = data();
    let mut range = PullRange::new(&data, "item_healthkit", 400.0);
    assert_eq!(range.pull(400, true), (false, true));
    assert!(range.holds_it());
    assert!(range.sim.pickups()[0].available());
    assert_eq!(
        range
            .sim
            .player(range.player)
            .expect("player")
            .vitals
            .health,
        100.0
    );
}

#[test]
fn a_player_at_full_armor_holds_the_battery_they_pull() {
    let data = data();
    let mut range = PullRange::new(&data, "item_battery", 400.0);
    range
        .sim
        .player_mut(range.player)
        .expect("player")
        .vitals
        .armor = data.player.armor.max;
    assert_eq!(range.pull(400, true), (false, true));
    assert!(range.holds_it());
    assert!(range.sim.pickups()[0].available());
}

#[test]
fn a_player_short_of_armor_collects_the_battery_they_pull() {
    let data = data();
    let mut range = PullRange::new(&data, "item_battery", 400.0);
    assert_eq!(range.pull(400, true), (true, false));
    assert!(range.sim.player(range.player).expect("player").vitals.armor > 0.0);
}

#[test]
fn a_player_with_the_weapon_and_full_ammo_holds_the_one_they_pull() {
    let data = data();
    let mut range = PullRange::new(&data, "weapon_shotgun", 400.0);
    let arsenal = range.sim.arsenal().clone();
    let shotgun = arsenal.find_weapon("scattergun").expect("weapon");
    let shells = arsenal.find_ammo("buckshot").expect("ammo");
    let inventory = &mut range
        .sim
        .player_mut(range.player)
        .expect("player")
        .inventory;
    inventory.give_weapon(&arsenal, shotgun);
    inventory.give_ammo(&arsenal, shells, 10_000);
    assert_eq!(range.pull(400, true), (false, true));
    assert!(range.holds_it());
    assert!(range.sim.pickups()[0].available());
}

#[test]
fn a_player_without_the_weapon_collects_the_one_they_pull() {
    let data = data();
    let mut range = PullRange::new(&data, "weapon_shotgun", 400.0);
    let shotgun = range
        .sim
        .arsenal()
        .find_weapon("scattergun")
        .expect("weapon");
    let carried = |range: &PullRange| {
        range
            .sim
            .player(range.player)
            .expect("player")
            .inventory
            .has(shotgun)
    };
    assert!(
        !carried(&range),
        "the loadout must not carry it for this test"
    );
    assert_eq!(range.pull(400, true), (true, false));
    assert!(carried(&range));
}
