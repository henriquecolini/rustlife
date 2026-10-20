//! The map's logic through the whole simulation, with the built-in data:
//! buttons opening doors, lifts carrying players, doors crushing them,
//! traps switched on, teleports, breaking glass and a replica following
//! the server's doors.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a missing player or weapon must fail the test"
)]

use std::sync::Arc;

use glam::Vec3;
use rl_data::GameData;
use rl_sim::collision::WorldBuilder;
use rl_sim::collision::contents::MASK_PLAYER_SOLID;
use rl_sim::combat::Cause;
use rl_sim::combat::hitscan::Struck;
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::simulation::{Life, MapEvent, PlayerId, Simulation, TickEvents};
use source_formats::entities::MapEntity;

fn data() -> GameData {
    GameData::builtin().expect("builtin data")
}

fn entity(classname: &str, origin: Vec3, keys: &[(&str, &str)]) -> MapEntity {
    keys.iter().fold(
        MapEntity::new(classname, origin.to_array()),
        |entity, (key, value)| entity.with(key, value),
    )
}

/// A big floor (top at z = 0) plus `world`'s brush entities and the map's
/// `entities`, with one player standing at the origin.
struct Room {
    sim: Simulation,
    player: PlayerId,
}

impl Room {
    fn new(world: WorldBuilder, entities: &[MapEntity]) -> Self {
        let world = world.solid_box(
            Vec3::new(-4096.0, -4096.0, -64.0),
            Vec3::new(4096.0, 4096.0, 0.0),
        );
        let mut sim = Simulation::new(Arc::new(world.build()), &data());
        sim.add_entities(entities);
        let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
        let mut room = Self { sim, player };
        room.idle(5);
        room
    }

    fn tick(&mut self, cmd: &UserCmd) -> TickEvents {
        self.sim.tick(&[(self.player, *cmd)])
    }

    fn idle(&mut self, ticks: usize) -> Vec<TickEvents> {
        (0..ticks).map(|_| self.tick(&UserCmd::default())).collect()
    }

    /// Walks forward along +X for `ticks` ticks.
    fn walk(&mut self, ticks: usize) {
        let cmd = UserCmd {
            forward: 1.0,
            ..UserCmd::default()
        };
        for _ in 0..ticks {
            self.tick(&cmd);
        }
    }

    /// Presses use for a tick looking at `yaw`, then lets go.
    fn use_at(&mut self, yaw: f32) {
        let press = UserCmd {
            yaw,
            buttons: Buttons::USE,
            ..UserCmd::default()
        };
        self.tick(&press);
        self.tick(&UserCmd {
            yaw,
            ..UserCmd::default()
        });
    }

    fn origin(&self) -> Vec3 {
        self.sim.player(self.player).expect("player").state.origin
    }
}

/// A wall of a door at x = 100 and a button on the wall to the left of a
/// player at the origin; the button opens the door.
fn door_and_button() -> (WorldBuilder, Vec<MapEntity>) {
    let world = WorldBuilder::new()
        .brush_entity(
            "func_door",
            Some("door"),
            Vec3::new(100.0, 0.0, 0.0),
            Vec3::new(-8.0, -256.0, 0.0),
            Vec3::new(8.0, 256.0, 128.0),
        )
        .brush_entity(
            "func_button",
            None,
            Vec3::new(0.0, 44.0, 64.0),
            Vec3::new(-16.0, -4.0, -16.0),
            Vec3::new(16.0, 4.0, 16.0),
        );
    let entities = vec![
        entity(
            "func_door",
            Vec3::new(100.0, 0.0, 0.0),
            &[
                ("model", "*1"),
                ("targetname", "door"),
                ("movedir", "-90 0 0"),
                ("speed", "320"),
                ("wait", "-1"),
            ],
        ),
        entity(
            "func_button",
            Vec3::new(0.0, 44.0, 64.0),
            &[
                ("model", "*2"),
                ("spawnflags", "1025"),
                ("OnPressed", "door,Open,,0,-1"),
            ],
        ),
    ];
    (world, entities)
}

#[test]
fn a_button_opens_the_door_in_the_way() {
    let (world, entities) = door_and_button();
    let mut shut = Room::new(world, &entities);
    shut.walk(66);
    assert!(
        shut.origin().x < 100.0,
        "the door is shut: {}",
        shut.origin()
    );

    let (world, entities) = door_and_button();
    let mut opened = Room::new(world, &entities);
    opened.use_at(90.0);
    opened.idle(40);
    opened.walk(100);
    assert!(
        opened.origin().x > 200.0,
        "through the open door: {}",
        opened.origin()
    );
}

#[test]
fn a_rising_platform_carries_the_player_up() {
    let world = WorldBuilder::new().brush_entity(
        "func_door",
        Some("lift"),
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(-64.0, -64.0, 0.0),
        Vec3::new(64.0, 64.0, 16.0),
    );
    let entities = [
        entity(
            "func_door",
            Vec3::ZERO,
            &[
                ("model", "*1"),
                ("targetname", "lift"),
                ("movedir", "-90 0 0"),
                ("lip", "-112"),
                ("speed", "100"),
                ("wait", "-1"),
            ],
        ),
        entity(
            "logic_auto",
            Vec3::ZERO,
            &[("OnMapSpawn", "lift,Open,,0,-1")],
        ),
    ];
    let mut room = Room::new(world, &entities);
    let events = room.idle(200);
    let feet = room.origin().z;
    assert!(
        (feet - 144.0).abs() < 2.0,
        "on top of the raised lift: {feet}"
    );
    let model = 1;
    assert!(
        events
            .iter()
            .flat_map(|e| &e.map)
            .any(|event| *event == MapEvent::MoverStopped { model })
    );
}

/// A slab above the player that comes down to the floor, crushing for
/// `dmg` a tick, and waits `wait` seconds at the bottom.
fn crusher(wait: &str) -> (WorldBuilder, Vec<MapEntity>) {
    let world = WorldBuilder::new().brush_entity(
        "func_door",
        Some("crusher"),
        Vec3::new(0.0, 0.0, 100.0),
        Vec3::new(-64.0, -64.0, 0.0),
        Vec3::new(64.0, 64.0, 100.0),
    );
    let entities = vec![
        entity(
            "func_door",
            Vec3::new(0.0, 0.0, 100.0),
            &[
                ("model", "*1"),
                ("targetname", "crusher"),
                ("movedir", "90 0 0"),
                ("speed", "200"),
                ("dmg", "10"),
                ("wait", wait),
            ],
        ),
        entity(
            "logic_auto",
            Vec3::ZERO,
            &[("OnMapSpawn", "crusher,Open,,0,-1")],
        ),
    ];
    (world, entities)
}

#[test]
fn a_door_that_cannot_push_crushes() {
    let (world, entities) = crusher("-1");
    let mut room = Room::new(world, &entities);
    let events = room.idle(60);
    let kill = events
        .iter()
        .flat_map(|e| &e.kills)
        .next()
        .expect("crushed");
    assert_eq!(
        (kill.victim, kill.killer, kill.cause),
        (room.player, None, Cause::Hazard)
    );
    assert!(matches!(
        room.sim.player(room.player).expect("player").life,
        Life::Dead { .. }
    ));
}

#[test]
fn a_blocked_door_that_waits_goes_back_up() {
    let (world, entities) = crusher("3");
    let mut room = Room::new(world, &entities);
    room.idle(40);
    let door = &room.sim.logic().movers()[0];
    assert!(
        room.sim.player(room.player).expect("player").alive(),
        "one dose, then it backed off"
    );
    assert!(door.position < 0.5, "it went back: {}", door.position);
}

#[test]
fn map_logic_switches_a_trap_on() {
    let world = WorldBuilder::new().brush_entity(
        "trigger_hurt",
        Some("pit"),
        Vec3::ZERO,
        Vec3::new(-64.0, -64.0, 0.0),
        Vec3::new(64.0, 64.0, 64.0),
    );
    let entities = [
        entity(
            "trigger_hurt",
            Vec3::ZERO,
            &[
                ("model", "*1"),
                ("targetname", "pit"),
                ("damage", "1000"),
                ("StartDisabled", "1"),
            ],
        ),
        entity(
            "logic_auto",
            Vec3::ZERO,
            &[("OnMapSpawn", "pit,Enable,,1.5,-1")],
        ),
    ];
    let mut room = Room::new(world, &entities);
    assert!(!room.sim.hazards()[0].enabled);
    room.idle(60);
    assert!(
        room.sim.player(room.player).expect("player").alive(),
        "off for 1.5 s"
    );
    room.idle(66);
    assert!(
        !room.sim.player(room.player).expect("player").alive(),
        "then lethal"
    );
}

#[test]
fn a_teleport_sends_the_player_to_its_destination() {
    let world = WorldBuilder::new().brush_entity(
        "trigger_teleport",
        None,
        Vec3::new(200.0, 0.0, 0.0),
        Vec3::new(-32.0, -64.0, 0.0),
        Vec3::new(32.0, 64.0, 128.0),
    );
    let mut destination = entity(
        "info_teleport_destination",
        Vec3::new(-1000.0, 500.0, 1.0),
        &[("targetname", "away")],
    );
    destination.angles = [0.0, 90.0, 0.0];
    let entities = [
        entity(
            "trigger_teleport",
            Vec3::new(200.0, 0.0, 0.0),
            &[("model", "*1"), ("spawnflags", "1"), ("target", "away")],
        ),
        destination,
    ];
    let mut room = Room::new(world, &entities);
    let cmd = UserCmd {
        forward: 1.0,
        ..UserCmd::default()
    };
    let teleported = (0..120)
        .flat_map(|_| room.tick(&cmd).map)
        .find_map(|event| match event {
            MapEvent::Teleported { player, spawn } => Some((player, spawn)),
            _ => None,
        })
        .expect("teleported");
    assert_eq!(teleported.0, room.player);
    assert_eq!(teleported.1.yaw, 90.0);
    assert!(
        room.origin().distance(Vec3::new(-1000.0, 500.0, 1.0)) < 64.0,
        "{}",
        room.origin()
    );
}

#[test]
fn glass_stops_bullets_until_it_breaks() {
    let world = WorldBuilder::new().brush_entity(
        "func_breakable_surf",
        None,
        Vec3::new(100.0, 0.0, 0.0),
        Vec3::new(-1.0, -64.0, 0.0),
        Vec3::new(1.0, 64.0, 128.0),
    );
    let entities = [entity(
        "func_breakable_surf",
        Vec3::new(100.0, 0.0, 0.0),
        &[("model", "*1"), ("health", "20")],
    )];
    let mut room = Room::new(world, &entities);
    let target = room.sim.spawn_player(Vec3::new(300.0, 0.0, 1.0));
    let arsenal = room.sim.arsenal().clone();
    let magnum = arsenal.find_weapon("magnum").expect("magnum");
    let ammo = arsenal
        .weapon(magnum)
        .and_then(rl_data::WeaponConfig::ammo_type)
        .and_then(|ammo| arsenal.find_ammo(ammo))
        .expect("ammo");
    let shooter = room.sim.player_mut(room.player).expect("player");
    shooter.inventory.give_weapon(&arsenal, magnum);
    shooter.inventory.give_ammo(&arsenal, ammo, 100);
    room.tick(&UserCmd {
        select: Some(magnum),
        ..UserCmd::default()
    });
    room.idle(66);

    let mut struck = Vec::new();
    let mut broke = false;
    for _ in 0..400 {
        let events = room.tick(&UserCmd {
            buttons: Buttons::ATTACK,
            ..UserCmd::default()
        });
        struck.extend(events.shots.iter().map(|shot| shot.trace.struck));
        broke |= events.map.contains(&MapEvent::Broke { model: 1 });
        room.idle(1);
        if struck.contains(&Struck::Player(target)) {
            break;
        }
    }
    assert_eq!(struck.first(), Some(&Struck::Brush(1)), "{struck:?}");
    assert!(broke, "the glass broke");
    assert!(
        struck.contains(&Struck::Player(target)),
        "then bullets went through: {struck:?}"
    );
}

#[test]
fn a_replica_collides_with_doors_where_the_server_has_them() {
    let world = || {
        WorldBuilder::new().brush_entity(
            "func_door",
            Some("door"),
            Vec3::new(100.0, 0.0, 0.0),
            Vec3::new(-8.0, -256.0, 0.0),
            Vec3::new(8.0, 256.0, 128.0),
        )
    };
    let entities = [
        entity(
            "func_door",
            Vec3::new(100.0, 0.0, 0.0),
            &[
                ("model", "*1"),
                ("targetname", "door"),
                ("movedir", "-90 0 0"),
                ("speed", "640"),
                ("wait", "-1"),
            ],
        ),
        entity(
            "logic_auto",
            Vec3::ZERO,
            &[("OnMapSpawn", "door,Open,,0,-1")],
        ),
    ];
    let mut server = Room::new(world(), &entities);
    server.idle(30);
    let mut replica = Simulation::new(
        Arc::new(
            world()
                .solid_box(
                    Vec3::new(-4096.0, -4096.0, -64.0),
                    Vec3::new(4096.0, 4096.0, 0.0),
                )
                .build(),
        ),
        &data(),
    );
    replica.add_entities(&entities);
    let through = |sim: &Simulation| {
        !sim.world()
            .trace_ray(
                Vec3::new(0.0, 0.0, 64.0),
                Vec3::new(200.0, 0.0, 64.0),
                MASK_PLAYER_SOLID,
            )
            .hit()
    };
    assert!(!through(&replica), "the replica's door starts shut");
    replica.set_logic_state(&server.sim.logic_state());
    assert!(through(&replica), "and opens with the server's state");
    assert!(through(&server.sim));
}
