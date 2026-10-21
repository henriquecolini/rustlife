//! Combat scenarios through the whole simulation, with the built-in data:
//! shooting, armor, swings, rockets and rocket jumps, grenades, shelter,
//! deaths and frags, respawning, pickups, falls, hazards, thrown props and
//! the end of a match.

#![allow(
    clippy::expect_used,
    reason = "test helpers: a missing player or weapon must fail the test"
)]

use std::sync::Arc;

use glam::{Quat, Vec3};
use rl_data::GameData;
use rl_sim::collision::{ConvexHull, WorldBuilder};
use rl_sim::combat::Cause;
use rl_sim::hazards::Hazard;
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::physics::{Breakable, Motion, PropDesc};
use rl_sim::rules::MatchEvent;
use rl_sim::simulation::{Life, PlayerId, PropBreak, Simulation, Spawn, TickEvents};
use rl_sim::weapons::WeaponId;
use source_formats::entities::MapEntity;

fn data() -> GameData {
    let mut data = GameData::builtin().expect("builtin data");
    // Matches start the moment the players are there.
    data.rules.warmup = 0.0;
    data
}

/// A big floor (top at z = 0) with a shooter at the origin facing +X and a
/// target `distance` units ahead.
struct Arena {
    sim: Simulation,
    shooter: PlayerId,
    target: PlayerId,
}

impl Arena {
    fn new(data: &GameData, distance: f32) -> Self {
        Self::with_world(data, distance, WorldBuilder::new())
    }

    fn with_world(data: &GameData, distance: f32, world: WorldBuilder) -> Self {
        let world = world.solid_box(
            Vec3::new(-4096.0, -4096.0, -64.0),
            Vec3::new(4096.0, 4096.0, 0.0),
        );
        let mut sim = Simulation::new(Arc::new(world.build()), data);
        sim.set_spawns(vec![
            Spawn {
                origin: Vec3::new(-3000.0, 0.0, 1.0),
                yaw: 0.0,
            },
            Spawn {
                origin: Vec3::new(2000.0, -2000.0, 1.0),
                yaw: 180.0,
            },
        ]);
        let shooter = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
        let target = sim.spawn_player(Vec3::new(distance, 0.0, 1.0));
        let mut arena = Self {
            sim,
            shooter,
            target,
        };
        // The match starts on the first tick and puts everyone at a spawn
        // point; put them back.
        arena.idle(1);
        arena.sim.teleport_player(shooter, Vec3::new(0.0, 0.0, 1.0));
        arena
            .sim
            .teleport_player(target, Vec3::new(distance, 0.0, 1.0));
        arena.idle(10);
        arena
    }

    fn weapon(&self, name: &str) -> WeaponId {
        self.sim.arsenal().find_weapon(name).expect("weapon")
    }

    /// Gives the shooter `name` with plenty of ammo and takes it in hand,
    /// ready.
    fn arm(&mut self, name: &str) {
        let weapon = self.weapon(name);
        let arsenal = self.sim.arsenal().clone();
        let ammo = arsenal
            .weapon(weapon)
            .and_then(rl_data::WeaponConfig::ammo_type)
            .and_then(|ammo| arsenal.find_ammo(ammo));
        let player = self.sim.player_mut(self.shooter).expect("shooter");
        player.inventory.give_weapon(&arsenal, weapon);
        if let Some(ammo) = ammo {
            player.inventory.give_ammo(&arsenal, ammo, 1000);
        }
        let select = UserCmd {
            select: Some(weapon),
            ..UserCmd::default()
        };
        self.tick(&select);
        self.idle(66);
    }

    fn tick(&mut self, cmd: &UserCmd) -> TickEvents {
        self.sim.tick(&[(self.shooter, *cmd)])
    }

    fn idle(&mut self, ticks: usize) -> Vec<TickEvents> {
        (0..ticks).map(|_| self.tick(&UserCmd::default())).collect()
    }

    /// Holds `buttons` aiming at `pitch` for `ticks` ticks.
    fn hold(&mut self, buttons: Buttons, pitch: f32, ticks: usize) -> Vec<TickEvents> {
        let cmd = UserCmd {
            pitch,
            buttons,
            ..UserCmd::default()
        };
        (0..ticks).map(|_| self.tick(&cmd)).collect()
    }

    fn health(&self, player: PlayerId) -> f32 {
        self.sim.player(player).expect("player").vitals.health
    }

    fn alive(&self, player: PlayerId) -> bool {
        self.sim.player(player).expect("player").alive()
    }
}

#[test]
fn bullets_hurt_the_player_they_hit_and_kills_score() {
    let data = data();
    let mut arena = Arena::new(&data, 200.0);
    arena.arm("smg");
    let events = arena.hold(Buttons::ATTACK, 0.0, 10);
    let hits: f32 = events
        .iter()
        .flat_map(|events| &events.damage)
        .filter(|damage| damage.victim == arena.target)
        .map(|damage| damage.health_lost)
        .sum();
    assert!(hits > 0.0, "the SMG hits a player 200 units ahead");
    assert!((arena.health(arena.target) - (100.0 - hits)).abs() < 1e-3);

    let events = arena.hold(Buttons::ATTACK, 0.0, 132);
    let kills: Vec<_> = events.iter().flat_map(|events| &events.kills).collect();
    assert_eq!(kills.len(), 1, "{kills:?}");
    assert_eq!(kills[0].victim, arena.target);
    assert_eq!(kills[0].killer, Some(arena.shooter));
    assert_eq!(kills[0].cause, Cause::Weapon(arena.weapon("smg")));
    assert_eq!(arena.sim.game().score(arena.shooter).kills, 1);
    assert_eq!(arena.sim.game().score(arena.target).deaths, 1);
}

#[test]
fn armor_takes_its_share() {
    let data = data();
    let mut arena = Arena::new(&data, 200.0);
    arena.arm("magnum");
    arena
        .sim
        .player_mut(arena.target)
        .expect("target")
        .vitals
        .armor = 100.0;
    arena.hold(Buttons::ATTACK, 0.0, 1);
    let target = arena.sim.player(arena.target).expect("target");
    // 70 damage: armor takes 60% (42), health the rest.
    assert!(
        (target.vitals.health - 72.0).abs() < 1e-3,
        "{:?}",
        target.vitals
    );
    assert!(
        (target.vitals.armor - 58.0).abs() < 1e-3,
        "{:?}",
        target.vitals
    );
}

#[test]
fn swings_only_reach_so_far() {
    let data = data();
    let mut near = Arena::new(&data, 60.0);
    near.arm("bar");
    near.hold(Buttons::ATTACK, 0.0, 1);
    assert_eq!(near.health(near.target), 70.0);

    let mut far = Arena::new(&data, 200.0);
    far.arm("bar");
    let events = far.hold(Buttons::ATTACK, 0.0, 1);
    assert_eq!(far.health(far.target), 100.0);
    assert_eq!(events[0].shots.len(), 1, "a swing is reported, hit or miss");
}

#[test]
fn rockets_burst_on_impact_and_throw_players() {
    let data = data();
    let mut arena = Arena::new(&data, 400.0);
    arena.arm("launcher");
    let events = arena.hold(Buttons::ATTACK, 0.0, 1);
    assert!(events[0].fired.len() == 1 && arena.sim.projectiles().len() == 1);
    let events = arena.idle(40);
    let explosions: Vec<_> = events.iter().flat_map(|e| &e.explosions).collect();
    assert_eq!(explosions.len(), 1);
    assert!(arena.sim.projectiles().is_empty());
    let target = arena.sim.player(arena.target).expect("target");
    assert!(
        target.vitals.health < 100.0 - 50.0,
        "a direct rocket: {:?}",
        target.vitals
    );
}

#[test]
fn a_rocket_at_your_feet_is_a_rocket_jump() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    arena.arm("launcher");
    let events = arena.hold(Buttons::ATTACK | Buttons::JUMP, 89.0, 1);
    assert_eq!(events[0].fired.len(), 1);
    let mut highest: f32 = 0.0;
    let mut events = Vec::new();
    for _ in 0..90 {
        events.push(arena.tick(&UserCmd::default()));
        let origin = arena
            .sim
            .player(arena.shooter)
            .expect("shooter")
            .state
            .origin;
        highest = highest.max(origin.z);
    }
    assert!(
        highest > 150.0,
        "thrown well above a plain jump's 45 units: {highest}"
    );
    let health = arena.health(arena.shooter);
    // Half damage to yourself: about 50 of a rocket's 110.
    assert!((35.0..80.0).contains(&health), "{health}");
    let scores = arena.sim.game().score(arena.shooter);
    assert_eq!(scores.kills, 0);
}

#[test]
fn walls_shelter_from_grenades() {
    let data = data();
    // A wall between the two players.
    let wall = WorldBuilder::new().solid_box(
        Vec3::new(150.0, -500.0, 0.0),
        Vec3::new(170.0, 500.0, 300.0),
    );
    let mut arena = Arena::with_world(&data, 240.0, wall);
    arena.arm("frag");
    // A frag lobbed into the wall drops at its foot, on the shooter's
    // side, and bursts on its fuse.
    arena.hold(Buttons::ATTACK, 30.0, 1);
    let events = arena.idle(200);
    assert_eq!(events.iter().flat_map(|e| &e.explosions).count(), 1);
    assert_eq!(arena.health(arena.target), 100.0, "sheltered");
}

#[test]
fn the_dead_respawn_at_spawn_points_with_the_loadout() {
    let mut data = data();
    // Always the spawn farthest from everyone else.
    data.rules.respawn.candidates = 1;
    let mut arena = Arena::new(&data, 100.0);
    arena.arm("magnum");
    arena
        .sim
        .player_mut(arena.target)
        .expect("target")
        .vitals
        .health = 10.0;
    arena.hold(Buttons::ATTACK, 0.0, 1);
    assert!(!arena.alive(arena.target));
    assert!(matches!(
        arena.sim.player(arena.target).expect("target").life,
        Life::Dead {
            killer: Some(_),
            ..
        }
    ));
    // The target sends no commands: it comes back after the forced delay.
    let force = (data.rules.respawn.force * 66.0).ceil() as usize;
    let events = arena.idle(force + 1);
    let respawns: Vec<_> = events.iter().flat_map(|e| &e.respawns).collect();
    assert_eq!(respawns.len(), 1);
    let target = arena.sim.player(arena.target).expect("target");
    assert!(target.alive());
    assert_eq!(target.vitals.health, data.player.health.spawn);
    // The spawn farthest from the shooter.
    assert!(
        target.state.origin.distance(Vec3::new(-3000.0, 0.0, 1.0)) < 40.0,
        "{}",
        target.state.origin
    );
    assert_eq!(
        target.inventory.active(),
        arena.sim.arsenal().find_weapon(&data.rules.loadout.active)
    );
}

#[test]
fn pressing_fire_respawns_once_the_delay_is_over() {
    let data = data();
    let mut arena = Arena::new(&data, 100.0);
    arena
        .sim
        .player_mut(arena.shooter)
        .expect("shooter")
        .vitals
        .health = 1.0;
    arena.sim.add_hazard(Hazard {
        mins: Vec3::new(-50.0, -50.0, -10.0),
        maxs: Vec3::new(50.0, 50.0, 10.0),
        damage: 100.0,
        enabled: true,
    });
    let events = arena.idle(40);
    let kills: Vec<_> = events.iter().flat_map(|e| &e.kills).collect();
    assert_eq!(kills.len(), 1);
    assert_eq!((kills[0].killer, kills[0].cause), (None, Cause::Hazard));
    assert_eq!(
        arena.sim.game().score(arena.shooter).points,
        -i32::try_from(arena.sim.data().rules.suicide_penalty).unwrap(),
        "a suicide"
    );
    let Some(Life::Dead { since, .. }) = arena.sim.player(arena.shooter).map(|p| p.life) else {
        panic!("dead");
    };
    // Holding fire brings the player back as soon as the delay is over,
    // well before the forced respawn.
    let back = (0..400)
        .find(|_| !arena.hold(Buttons::ATTACK, 0.0, 1)[0].respawns.is_empty())
        .expect("respawned");
    let waited = (arena.sim.tick_count() - since) as f32 / 66.0;
    assert!(back > 0);
    assert!(
        (data.rules.respawn.delay..data.rules.respawn.delay + 0.05).contains(&waited),
        "{waited}"
    );
}

#[test]
fn players_take_the_pickups_they_need() {
    let data = data();
    let mut arena = Arena::new(&data, 1000.0);
    arena
        .sim
        .add_entities(&[MapEntity::new("item_healthkit", [100.0, 0.0, 0.0])]);
    assert_eq!(arena.sim.pickups().len(), 1);
    arena
        .sim
        .player_mut(arena.shooter)
        .expect("shooter")
        .vitals
        .health = 50.0;
    let walk = UserCmd {
        forward: 1.0,
        ..UserCmd::default()
    };
    let events: Vec<_> = (0..66).map(|_| arena.tick(&walk)).collect();
    let taken: Vec<_> = events.iter().flat_map(|e| &e.pickups).collect();
    assert_eq!(taken.as_slice(), [&(arena.shooter, 0)]);
    assert_eq!(arena.health(arena.shooter), 75.0);
    assert!(!arena.sim.pickups()[0].available());
}

#[test]
fn hard_landings_hurt() {
    let data = data();
    let mut arena = Arena::new(&data, 1000.0);
    // Drop from 300 units: about 690 units/s on landing, 25 damage.
    arena
        .sim
        .teleport_player(arena.shooter, Vec3::new(0.0, 0.0, 300.0));
    let events = arena.idle(120);
    let falls: Vec<_> = events
        .iter()
        .flat_map(|e| &e.damage)
        .filter(|damage| damage.damage.cause == Cause::Fall)
        .collect();
    assert_eq!(falls.len(), 1);
    assert!((10.0..40.0).contains(&falls[0].damage.amount), "{falls:?}");

    // A plain jump never hurts.
    let jumps = arena.hold(Buttons::JUMP, 0.0, 200);
    assert!(jumps.iter().all(|e| e.damage.is_empty()));
}

#[test]
fn a_thrown_prop_kill_is_the_throwers_frag() {
    let data = data();
    let mut arena = Arena::new(&data, 400.0);
    let barrel = arena.sim.add_prop(&PropDesc {
        pieces: vec![ConvexHull::from_box(
            Vec3::new(-10.0, -10.0, 0.0),
            Vec3::new(10.0, 10.0, 40.0),
        )],
        origin: Vec3::new(60.0, 0.0, 0.0),
        rotation: Quat::IDENTITY,
        motion: Motion::Dynamic,
        mass: 30.0,
        material: *data.physics.materials.get("metal"),
        linear_damping: 0.0,
        angular_damping: 0.0,
        debris: false,
        start_asleep: false,
        grabbable: true,
        free_on_grab: false,
        breakable: None,
        surface: String::new(),
    });
    arena.arm("manipulator");
    arena
        .sim
        .player_mut(arena.target)
        .expect("target")
        .vitals
        .health = 5.0;
    let center = arena.sim.physics().center_of_mass(barrel).expect("barrel");
    let to = center - (Vec3::Z * 65.0);
    let pitch = -(to.z.atan2(to.truncate().length())).to_degrees();
    arena.hold(Buttons::ATTACK2, pitch, 1);
    arena.idle(30);
    arena.hold(Buttons::ATTACK, 0.0, 1);
    let events = arena.idle(66);
    let kills: Vec<_> = events.iter().flat_map(|e| &e.kills).collect();
    assert_eq!(kills.len(), 1, "{kills:?}");
    assert_eq!(kills[0].cause, Cause::Prop);
    assert_eq!(kills[0].killer, Some(arena.shooter));
}

#[test]
fn the_score_limit_ends_the_match_and_it_restarts() {
    let mut data = data();
    data.rules.score_limit = 1;
    data.rules.intermission = 1.0;
    let mut arena = Arena::new(&data, 100.0);
    arena.arm("magnum");
    arena
        .sim
        .player_mut(arena.target)
        .expect("target")
        .vitals
        .health = 10.0;
    let events = arena.hold(Buttons::ATTACK, 0.0, 1);
    assert_eq!(
        events[0].match_event,
        Some(MatchEvent::Ended(Some(arena.shooter)))
    );
    assert!(!arena.sim.game().playing());
    let events = arena.idle(70);
    assert!(
        events
            .iter()
            .any(|e| e.match_event == Some(MatchEvent::Restarted))
    );
    assert!(arena.sim.game().playing());
    assert_eq!(arena.sim.game().score(arena.shooter).points, 0);
    assert!(arena.alive(arena.target), "everyone is back");
}

#[test]
fn bullets_shove_props() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let crate_ = arena.sim.add_prop(&PropDesc {
        pieces: vec![ConvexHull::from_box(
            Vec3::new(-8.0, -8.0, 0.0),
            Vec3::new(8.0, 8.0, 16.0),
        )],
        origin: Vec3::new(150.0, 0.0, 56.0),
        rotation: Quat::IDENTITY,
        motion: Motion::Dynamic,
        mass: 5.0,
        material: *data.physics.materials.get("wood"),
        linear_damping: 0.0,
        angular_damping: 0.0,
        debris: false,
        start_asleep: true,
        grabbable: true,
        free_on_grab: false,
        breakable: None,
        surface: String::new(),
    });
    arena.arm("magnum");
    arena.hold(Buttons::ATTACK, 0.0, 1);
    let (velocity, _) = arena.sim.physics().velocity(crate_).expect("crate");
    assert!(velocity.x > 100.0, "{velocity}");
}

#[test]
fn combat_is_deterministic() {
    let data = data();
    let run = || {
        let mut arena = Arena::new(&data, 300.0);
        arena.arm("scattergun");
        let mut trail = Vec::new();
        for tick in 0..200 {
            let cmd = UserCmd {
                yaw: (tick as f32 * 0.7).sin() * 4.0,
                buttons: if tick % 40 == 0 {
                    Buttons::ATTACK
                } else {
                    Buttons::NONE
                },
                ..UserCmd::default()
            };
            let events = arena.tick(&cmd);
            trail.push((events.damage, arena.health(arena.target)));
        }
        trail
    };
    assert_eq!(run(), run());
}

/// A box of half-size `half` and `height` standing at `origin` that breaks
/// after `health` damage, exploding for `explosion` (damage, radius) if
/// given.
fn breakable_box(
    data: &GameData,
    origin: Vec3,
    (half, height): (f32, f32),
    health: f32,
    explosion: Option<(f32, f32)>,
) -> PropDesc {
    PropDesc {
        pieces: vec![ConvexHull::from_box(
            Vec3::new(-half, -half, 0.0),
            Vec3::new(half, half, height),
        )],
        origin,
        rotation: Quat::IDENTITY,
        motion: Motion::Dynamic,
        mass: 20.0,
        material: *data.physics.materials.get("metal"),
        linear_damping: 0.0,
        angular_damping: 0.0,
        debris: false,
        start_asleep: false,
        grabbable: true,
        free_on_grab: false,
        breakable: Some(Breakable {
            health,
            bullets: 1.0,
            club: 2.0,
            explosive: 1.0,
            explosion,
            gibs: vec!["models/gibs/metal_gib1.mdl".to_owned()],
            gib_count: 4,
            respawn: None,
        }),
        surface: "metal".to_owned(),
    }
}

/// The breaks among `events`.
fn breaks(events: &[TickEvents]) -> Vec<&PropBreak> {
    events.iter().flat_map(|e| &e.prop_breaks).collect()
}

#[test]
fn shooting_a_prop_breaks_it_when_its_health_runs_out() {
    let data = data();
    let mut arena = Arena::new(&data, 600.0);
    // Standing at eye height, dead ahead.
    let crate_ = arena.sim.add_prop(&breakable_box(
        &data,
        Vec3::new(150.0, 0.0, 0.0),
        (16.0, 90.0),
        25.0,
        None,
    ));
    arena.arm("magnum");
    let mut broken = Vec::new();
    for _ in 0..8 {
        let events = arena.hold(Buttons::ATTACK, 0.0, 1);
        broken.extend(breaks(&events).into_iter().copied());
        arena.idle(40);
        if !broken.is_empty() {
            break;
        }
    }
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert_eq!(broken[0].body, crate_);
    let spec = arena.sim.breakable(crate_).expect("breakable");
    assert_eq!(spec.gib_count, 4);
    assert_eq!(arena.sim.physics().surface(crate_), "metal");
    assert!(!broken[0].exploded);
    assert!(arena.sim.physics().is_broken(crate_));
    assert!(
        !arena.sim.physics().props().any(|body| body == crate_),
        "it is no longer listed among the props"
    );
}

#[test]
fn an_explosive_barrel_kills_whoever_is_near_and_credits_who_shot_it() {
    let data = data();
    let mut arena = Arena::new(&data, 600.0);
    arena.sim.add_prop(&breakable_box(
        &data,
        Vec3::new(200.0, 0.0, 0.0),
        (16.0, 90.0),
        10.0,
        Some((120.0, 256.0)),
    ));
    // The target stands next to the barrel, off the line of fire.
    arena
        .sim
        .teleport_player(arena.target, Vec3::new(200.0, 90.0, 1.0));
    arena.arm("magnum");
    arena
        .sim
        .player_mut(arena.target)
        .expect("target")
        .vitals
        .health = 30.0;
    let events = arena.hold(Buttons::ATTACK, 0.0, 1);
    let mut events = events;
    events.extend(arena.idle(40));
    let broke = breaks(&events);
    assert_eq!(broke.len(), 1, "{broke:?}");
    assert!(broke[0].exploded);
    assert_eq!(
        events.iter().flat_map(|e| &e.explosions).count(),
        1,
        "one blast"
    );
    let kills: Vec<_> = events.iter().flat_map(|e| &e.kills).collect();
    assert_eq!(kills.len(), 1, "{kills:?}");
    assert_eq!(kills[0].victim, arena.target);
    assert_eq!(kills[0].killer, Some(arena.shooter));
    assert_eq!(kills[0].cause, Cause::Explosion);
    let points = arena.sim.game().score(arena.shooter).points;
    assert_eq!(points, i32::try_from(data.rules.points.barrel).unwrap());
}

#[test]
fn barrels_set_each_other_off() {
    let data = data();
    let mut arena = Arena::new(&data, 900.0);
    let barrels: Vec<_> = [200.0, 330.0, 460.0]
        .into_iter()
        .map(|x| {
            arena.sim.add_prop(&breakable_box(
                &data,
                Vec3::new(x, 0.0, 0.0),
                (16.0, 90.0),
                10.0,
                Some((120.0, 200.0)),
            ))
        })
        .collect();
    arena.arm("magnum");
    let mut events = arena.hold(Buttons::ATTACK, 0.0, 1);
    events.extend(arena.idle(60));
    let broke: Vec<_> = breaks(&events).iter().map(|b| b.body).collect();
    for barrel in &barrels {
        assert!(broke.contains(barrel), "{barrel:?} not in {broke:?}");
    }
    assert_eq!(events.iter().flat_map(|e| &e.explosions).count(), 3);
}

#[test]
fn a_hard_fall_breaks_a_crate() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let crate_ = arena.sim.add_prop(&breakable_box(
        &data,
        Vec3::new(-200.0, 400.0, 500.0),
        (8.0, 16.0),
        20.0,
        None,
    ));
    let events = arena.idle(150);
    let broke = breaks(&events);
    assert_eq!(broke.len(), 1, "{broke:?}");
    assert_eq!(broke[0].body, crate_);
    assert!(
        events
            .iter()
            .flat_map(|e| &e.prop_impacts)
            .any(|impact| impact.body == crate_ && impact.speed > 500.0),
        "the landing was heard"
    );
}

#[test]
fn a_gentle_drop_leaves_a_crate_whole() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let crate_ = arena.sim.add_prop(&breakable_box(
        &data,
        Vec3::new(-200.0, 400.0, 6.0),
        (8.0, 16.0),
        20.0,
        None,
    ));
    let events = arena.idle(150);
    assert!(breaks(&events).is_empty());
    assert!(!arena.sim.physics().is_broken(crate_));
}

#[test]
fn a_respawnable_barrel_comes_back_where_it_was_unless_someone_stands_there() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let mut desc = breakable_box(
        &data,
        Vec3::new(-300.0, 500.0, 0.0),
        (16.0, 32.0),
        10.0,
        None,
    );
    if let Some(breakable) = &mut desc.breakable {
        breakable.respawn = Some(2.0);
    }
    let barrel = arena.sim.add_prop(&desc);
    arena.idle(60);
    let home = arena.sim.physics().home_position(barrel).expect("home");

    let broke = arena.sim.damage_prop(barrel, 100.0, None);
    assert_eq!(broke.prop_breaks.len(), 1);
    assert!(arena.sim.physics().is_broken(barrel));

    // Someone stands on the spot as its time comes.
    arena.sim.teleport_player(arena.target, home + Vec3::Z);
    arena.idle(200);
    assert!(
        arena.sim.physics().is_broken(barrel),
        "it waits for the player to move"
    );
    arena
        .sim
        .teleport_player(arena.target, Vec3::new(1500.0, -1500.0, 1.0));
    arena.idle(10);
    assert!(!arena.sim.physics().is_broken(barrel), "it came back");
    let center = arena.sim.physics().center_of_mass(barrel).expect("barrel");
    assert!(
        center.truncate().distance(home.truncate()) < 20.0,
        "{center}"
    );
    // Whole again: it takes the full blow to break it once more.
    assert!(
        arena
            .sim
            .damage_prop(barrel, 5.0, None)
            .prop_breaks
            .is_empty()
    );
    assert_eq!(
        arena.sim.damage_prop(barrel, 100.0, None).prop_breaks.len(),
        1
    );
}
