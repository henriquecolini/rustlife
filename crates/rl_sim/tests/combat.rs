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
use rl_sim::physics::{BodyId, Breakable, Motion, PropDesc};
use rl_sim::rules::MatchEvent;
use rl_sim::simulation::{Life, PlayerId, PropBreak, Simulation, Spawn, TickEvents};
use rl_sim::weapons::WeaponId;
use source_formats::entities::MapEntity;

fn data() -> GameData {
    let mut data = GameData::builtin().expect("builtin data");
    // Matches start the moment the players are there.
    data.rules.warmup = 0.0;
    // Players stand eye to eye here, so a level shot is a headshot; the
    // scenarios that are not about headshots count plain hits.
    data.player.head_scale = 1.0;
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
        // Long enough for the slowest to come up (the grenade's 1.267 s).
        self.idle(100);
    }

    fn tick(&mut self, cmd: &UserCmd) -> TickEvents {
        self.sim.tick(&[(self.shooter, *cmd)])
    }

    /// Pulls the pin of a grenade with `buttons` aiming at `pitch`, holds
    /// it for `held` ticks and lets go: the events of the tick it is
    /// released.
    fn throw(&mut self, buttons: Buttons, pitch: f32, held: usize) -> TickEvents {
        self.hold(buttons, pitch, held);
        self.release(Buttons::NONE, pitch)
    }

    /// Lets go of the button, keeping `buttons` down, until the grenade
    /// leaves the hand (a few ticks): the events of that tick.
    fn release(&mut self, buttons: Buttons, pitch: f32) -> TickEvents {
        let release = UserCmd {
            pitch,
            buttons,
            ..UserCmd::default()
        };
        for _ in 0..8 {
            let events = self.tick(&release);
            if !events.fired.is_empty() {
                return events;
            }
        }
        TickEvents::default()
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
    // 75 damage: armor takes 80% (60), health the rest.
    assert!(
        (target.vitals.health - 85.0).abs() < 1e-3,
        "{:?}",
        target.vitals
    );
    assert!(
        (target.vitals.armor - 40.0).abs() < 1e-3,
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
    assert_eq!(near.health(near.target), 75.0);

    let mut far = Arena::new(&data, 200.0);
    far.arm("bar");
    let events = far.hold(Buttons::ATTACK, 0.0, 1);
    assert_eq!(far.health(far.target), 100.0);
    assert_eq!(events[0].shots.len(), 1, "a swing is reported, hit or miss");
}

#[test]
fn a_swing_that_misses_with_its_line_catches_what_its_box_reaches() {
    let data = data();
    let swing_at = |x: f32, y: f32| {
        let mut arena = Arena::new(&data, 200.0);
        arena.arm("bar");
        arena
            .sim
            .teleport_player(arena.target, Vec3::new(x, y, 1.0));
        arena.hold(Buttons::ATTACK, 0.0, 1);
        arena.health(arena.target) < 100.0
    };
    assert!(swing_at(60.0, 0.0), "the line itself");
    assert!(swing_at(60.0, 25.0), "just off the line, in the box");
    assert!(!swing_at(60.0, 50.0), "beyond the box");
    assert!(!swing_at(20.0, 30.0), "a graze at the edge is a miss");
}

#[test]
fn the_energy_ball_kills_outright_and_dissolves_the_body() {
    let data = data();
    let mut arena = Arena::new(&data, 300.0);
    arena.arm("rifle");
    let arsenal = arena.sim.arsenal().clone();
    let charges = arsenal.find_ammo("rifle_charge").expect("ammo");
    let shooter = arena.shooter;
    arena
        .sim
        .player_mut(shooter)
        .expect("shooter")
        .inventory
        .give_ammo(&arsenal, charges, 3);
    arena.hold(Buttons::ATTACK2, 0.0, 1);
    let events = arena.idle(120);
    let kills: Vec<_> = events.iter().flat_map(|e| &e.kills).collect();
    assert_eq!(kills.len(), 1, "{kills:?}");
    assert!(kills[0].dissolved);
    assert_eq!(kills[0].victim, arena.target);
    // The ball is spent on the kill, and a bullet's kill is no dissolving.
    assert!(arena.sim.projectiles().is_empty());
}

#[test]
fn a_bolt_embeds_in_a_wall_for_two_seconds_sparking() {
    let data = data();
    let wall = WorldBuilder::new().solid_box(
        Vec3::new(300.0, -500.0, 0.0),
        Vec3::new(320.0, 500.0, 300.0),
    );
    let mut arena = Arena::with_world(&data, 2000.0, wall);
    arena.arm("boltcaster");
    let mut events = arena.hold(Buttons::ATTACK, 0.0, 1);
    events.extend(arena.idle(10));
    let impacts: Vec<_> = events
        .iter()
        .flat_map(|e| &e.shots)
        .filter(|s| s.late)
        .collect();
    assert_eq!(impacts.len(), 1, "one impact for the effects");
    let bolt = arena.sim.projectiles()[0].clone();
    assert_eq!(bolt.velocity, Vec3::ZERO);
    assert!((bolt.position.x - 300.0).abs() < 3.0, "{bolt:?}");
    arena.idle(100);
    assert_eq!(arena.sim.projectiles().len(), 1, "still there at 1.5 s");
    arena.idle(60);
    assert!(arena.sim.projectiles().is_empty(), "gone after 2 s");
}

#[test]
fn a_bolt_that_grazes_a_wall_glances_off_and_falls() {
    let data = data();
    let wall = WorldBuilder::new().solid_box(
        Vec3::new(100.0, -3000.0, 0.0),
        Vec3::new(120.0, 3000.0, 300.0),
    );
    let mut arena = Arena::with_world(&data, 2000.0, wall);
    arena.arm("boltcaster");
    let cmd = UserCmd {
        yaw: 80.0,
        buttons: Buttons::ATTACK,
        ..UserCmd::default()
    };
    arena.tick(&cmd);
    for _ in 0..40 {
        arena.tick(&UserCmd {
            yaw: 80.0,
            ..UserCmd::default()
        });
        if arena
            .sim
            .projectiles()
            .first()
            .is_some_and(|b| b.gravity_scale.is_some())
        {
            break;
        }
    }
    let bolt = arena.sim.projectiles()[0].clone();
    assert_eq!(bolt.gravity_scale, Some(1.0));
    // Reflected off a wall facing -X: it now heads away from it.
    assert!(bolt.velocity.x < 0.0, "{bolt:?}");
    assert!(
        (bolt.velocity.length() - 0.75 * 3500.0).abs() < 100.0,
        "{bolt:?}"
    );
}

/// The shooter's SLAMs.
fn slams(arena: &Arena) -> u32 {
    let slam = arena.sim.arsenal().find_ammo("slam").expect("ammo");
    arena
        .sim
        .player(arena.shooter)
        .expect("shooter")
        .inventory
        .ammo(slam)
}

/// A wall across the arena, 100 units ahead of the shooter at the origin.
fn wall() -> WorldBuilder {
    WorldBuilder::new().solid_box(
        Vec3::new(100.0, -500.0, 0.0),
        Vec3::new(120.0, 500.0, 300.0),
    )
}

fn sticking(arena: &Arena) -> bool {
    arena
        .sim
        .player(arena.shooter)
        .expect("shooter")
        .inventory
        .slam_sticking()
}

fn explosions(events: &[TickEvents]) -> usize {
    events.iter().flat_map(|e| &e.explosions).count()
}

#[test]
fn a_slam_picks_its_mode_from_what_is_in_reach() {
    let data = data();
    let mut arena = Arena::with_world(&data, 2000.0, wall());
    arena.arm("slam");
    assert!(sticking(&arena), "a wall 100 units ahead: sticking");
    // Walk back out of reach: the aim holds for a moment, then it throws.
    arena
        .sim
        .teleport_player(arena.shooter, Vec3::new(-2000.0, 0.0, 1.0));
    arena.idle(3);
    assert!(sticking(&arena), "not at once");
    arena.idle(20);
    assert!(!sticking(&arena), "nothing in reach: throwing");
    // And back to the wall.
    arena
        .sim
        .teleport_player(arena.shooter, Vec3::new(0.0, 0.0, 1.0));
    // (The change to throwing was still playing: it waits for it.)
    arena.idle(100);
    assert!(sticking(&arena));
}

#[test]
fn a_change_of_mode_cannot_fire_until_its_transition_is_over() {
    let data = data();
    let mut arena = Arena::with_world(&data, 2000.0, wall());
    arena.arm("slam");
    arena
        .sim
        .teleport_player(arena.shooter, Vec3::new(-2000.0, 0.0, 1.0));
    arena.idle(20);
    assert!(!sticking(&arena));
    // The change to throwing (1 s) is still playing.
    arena.hold(Buttons::ATTACK, 0.0, 20);
    assert!(arena.sim.grenades().is_empty(), "too soon to throw");
    arena.hold(Buttons::ATTACK, 0.0, 80);
    assert_eq!(arena.sim.grenades().len(), 1, "then it throws");
}

#[test]
fn near_a_wall_a_slam_sets_a_tripmine_on_it_and_the_alternate_button_does_nothing() {
    let data = data();
    let mut arena = Arena::with_world(&data, 2000.0, wall());
    arena.arm("slam");
    let before = slams(&arena);
    // The alternate button neither throws, detonates nor switches.
    arena.hold(Buttons::ATTACK2, 0.0, 10);
    assert!(arena.sim.grenades().is_empty());
    assert!(sticking(&arena));
    // The primary button sticks one on the wall.
    arena.hold(Buttons::ATTACK, 0.0, 1);
    assert_eq!(arena.sim.grenades().len(), 1);
    let mine = arena.sim.grenades()[0];
    assert!(
        (mine.position.x - 100.0).abs() < 8.0,
        "on the wall: {mine:?}"
    );
    assert_eq!(slams(&arena), before - 1);
    // It cannot be detonated by hand (before its beam is armed and sets it
    // off on the one who set it).
    arena.idle(30);
    let events = arena.hold(Buttons::ATTACK2, 0.0, 10);
    assert_eq!(explosions(&events), 0);
    assert_eq!(arena.sim.grenades().len(), 1);
}

#[test]
fn a_tripmine_goes_off_when_something_crosses_its_beam_once_armed() {
    let data = data();
    let mut arena = Arena::with_world(&data, 2000.0, wall());
    arena.arm("slam");
    arena.hold(Buttons::ATTACK, 0.0, 1);
    // The mine's beam runs back along the shooter's line: step aside.
    arena
        .sim
        .teleport_player(arena.shooter, Vec3::new(0.0, 300.0, 1.0));
    arena
        .sim
        .teleport_player(arena.target, Vec3::new(40.0, 0.0, 1.0));
    // Unarmed for the first two seconds: even someone in the beam is safe.
    let early = arena.idle(100);
    assert_eq!(explosions(&early), 0);
    assert_eq!(arena.health(arena.target), 100.0);
    let later = arena.idle(60);
    assert_eq!(explosions(&later), 1);
    assert!(arena.health(arena.target) < 60.0, "it hurt");
}

#[test]
fn a_tripmine_set_on_a_player_goes_where_they_go() {
    let data = data();
    let mut arena = Arena::new(&data, 80.0);
    arena.arm("slam");
    // The player ahead is the surface in reach.
    assert!(sticking(&arena));
    arena.hold(Buttons::ATTACK, 0.0, 1);
    arena.idle(5);
    let stuck = arena.sim.grenades()[0].position;
    assert!((stuck.x - 80.0).abs() < 40.0, "on them: {stuck}");
    arena
        .sim
        .teleport_player(arena.target, Vec3::new(80.0, 300.0, 1.0));
    arena.idle(5);
    let moved = arena.sim.grenades()[0].position;
    assert!(moved.y > 250.0, "went with them: {moved}");
}

#[test]
fn in_the_open_a_slam_throws_satchels_that_lie_until_their_thrower_detonates_them() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    arena.arm("slam");
    assert!(!sticking(&arena), "nothing in reach: throwing");
    arena.hold(Buttons::ATTACK, 0.0, 1);
    arena.idle(5);
    assert_eq!(arena.sim.grenades().len(), 1);
    let quiet = arena.idle(300);
    assert_eq!(explosions(&quiet), 0);
    // The alternate button detonates it.
    let mut events = arena.hold(Buttons::ATTACK2, 0.0, 1);
    events.extend(arena.idle(3));
    assert_eq!(explosions(&events), 1);
    assert!(arena.sim.grenades().is_empty());
}

#[test]
fn the_alternate_button_with_no_satchels_out_does_nothing() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    arena.arm("slam");
    arena.hold(Buttons::ATTACK2, 0.0, 10);
    assert!(!sticking(&arena), "it does not switch modes");
    arena.hold(Buttons::ATTACK, 0.0, 1);
    arena.idle(5);
    assert_eq!(arena.sim.grenades().len(), 1, "still throws satchels");
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
fn a_rocket_is_lobbed_from_the_launcher_then_its_motor_lights() {
    let data = data();
    let mut arena = Arena::new(&data, 4000.0);
    arena.arm("launcher");
    arena.hold(Buttons::ATTACK, 0.0, 1);
    let launched = arena.sim.projectiles()[0].clone();
    // Out of the muzzle (12 forward, 6 right, 3 down of the eyes) at 300
    // units/s forwards and 128 up, before the motor is lit.
    assert!(!launched.lit);
    assert!((launched.velocity.x - 300.0).abs() < 5.0, "{launched:?}");
    assert!(launched.velocity.z > 60.0, "{launched:?}");
    assert!((launched.position.x - 12.0).abs() < 8.0, "{launched:?}");
    arena.idle(40);
    let flying = arena.sim.projectiles()[0].clone();
    assert!(flying.lit);
    assert!(
        (flying.velocity.length() - 1500.0).abs() < 1.0,
        "{flying:?}"
    );
}

#[test]
fn a_rocket_follows_the_laser_of_the_one_who_fired_it() {
    let data = data();
    let mut arena = Arena::new(&data, 4000.0);
    arena.arm("launcher");
    // Looking a little down, the laser lands on the floor far ahead.
    arena.hold(Buttons::ATTACK, 3.0, 1);
    arena.idle(26);
    // Turn left: the dot, and the rocket after it, go with the view.
    let turned = UserCmd {
        pitch: 3.0,
        yaw: 30.0,
        ..UserCmd::default()
    };
    for _ in 0..20 {
        arena.tick(&turned);
    }
    let rocket = arena.sim.projectiles()[0].clone();
    assert!(rocket.velocity.y > 100.0, "turned left: {rocket:?}");
    assert!(
        rocket.velocity.x > 1000.0,
        "but still mostly ahead: {rocket:?}"
    );
}

#[test]
fn a_rocket_ignores_a_switched_off_laser() {
    let data = data();
    let mut arena = Arena::new(&data, 4000.0);
    arena.arm("launcher");
    // The alternate button toggles the laser off.
    arena.hold(Buttons::ATTACK2, 3.0, 1);
    arena.idle(5);
    arena.hold(Buttons::ATTACK, 3.0, 1);
    arena.idle(26);
    let turned = UserCmd {
        pitch: 3.0,
        yaw: 30.0,
        ..UserCmd::default()
    };
    for _ in 0..20 {
        arena.tick(&turned);
    }
    let rocket = arena.sim.projectiles()[0].clone();
    assert!(rocket.velocity.y.abs() < 1.0, "straight on: {rocket:?}");
}

#[test]
fn the_launcher_waits_for_its_rocket_then_reloads() {
    let data = data();
    let mut arena = Arena::new(&data, 4000.0);
    arena.arm("launcher");
    let rockets = arena.sim.arsenal().find_ammo("rocket").expect("ammo");
    let events = arena.hold(Buttons::ATTACK, 0.0, 30);
    // Held for half a second: one rocket, not one every half second.
    assert_eq!(events.iter().map(|e| e.fired.len()).sum::<usize>(), 1);
    assert_eq!(arena.sim.projectiles().len(), 1);
    let ammo = |arena: &Arena| {
        arena
            .sim
            .player(arena.shooter)
            .expect("shooter")
            .inventory
            .ammo(rockets)
    };
    let before = ammo(&arena);
    // It flies out of the arena's floor soon enough (the arena has no walls):
    // nothing more is fired meanwhile.
    let mut fired_again = 0;
    for _ in 0..400 {
        let events = arena.hold(Buttons::ATTACK, 0.0, 1);
        fired_again += events[0].fired.len();
        if arena.sim.projectiles().is_empty() {
            break;
        }
    }
    assert_eq!(fired_again, 0);
    assert_eq!(ammo(&arena), before);
    // Once it has expired, the launcher reloads (1.667 s) before the next.
    let mut ticks = 0;
    let mut fired_at = None;
    while fired_at.is_none() && ticks < 400 {
        ticks += 1;
        if !arena.hold(Buttons::ATTACK, 0.0, 1)[0].fired.is_empty() {
            fired_at = Some(ticks);
        }
    }
    let fired_at = fired_at.expect("fires again");
    assert!(
        (105..=118).contains(&fired_at),
        "1.667 s of reloading: {fired_at}"
    );
}

#[test]
fn a_rocket_at_your_feet_is_no_rocket_jump_it_kills_you() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    arena.arm("launcher");
    let events = arena.hold(Buttons::ATTACK | Buttons::JUMP, 89.0, 1);
    assert_eq!(events[0].fired.len(), 1);
    // HL2DM gives no mercy for your own blast: 150 damage at the feet.
    let events = arena.idle(60);
    assert!(!arena.alive(arena.shooter), "dead of their own rocket");
    let kills: Vec<_> = events.iter().flat_map(|events| &events.kills).collect();
    assert_eq!(kills.len(), 1);
    assert_eq!(arena.sim.game().score(arena.shooter).kills, 0);
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
    arena.throw(Buttons::ATTACK, 30.0, 1);
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
    // Drop from 400 units: about 690 units/s on landing, 25 damage.
    arena
        .sim
        .teleport_player(arena.shooter, Vec3::new(0.0, 0.0, 400.0));
    let events = arena.idle(150);
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
            pieces: Vec::new(),
            gib_count: 4,
            respawn: None,
            damage_table: None,
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

/// A prop too heavy for bullets to push off the line of fire.
fn heavy(mut desc: PropDesc) -> PropDesc {
    desc.mass = 5000.0;
    desc
}

/// Fires the magnum once, then waits long enough for the next shot.
fn shoot(arena: &mut Arena) -> Vec<TickEvents> {
    let mut events = arena.hold(Buttons::ATTACK, 0.0, 1);
    events.extend(arena.idle(80));
    events
}

fn ignitions(events: &[TickEvents]) -> usize {
    events.iter().map(|e| e.prop_ignitions.len()).sum()
}

/// `oildrum001_explosive`'s health, and its blast.
const BARREL_HEALTH: f32 = 20.0;
const BARREL_BLAST: (f32, f32) = (120.0, 256.0);

/// A barrel at `x` on the line of fire, too heavy for blasts to move it away.
fn barrel_at(data: &GameData, x: f32) -> PropDesc {
    heavy(breakable_box(
        data,
        Vec3::new(x, 0.0, 0.0),
        (16.0, 90.0),
        BARREL_HEALTH,
        Some(BARREL_BLAST),
    ))
}

/// A barrel dead ahead of the shooter holding `weapon`, and the arena.
fn barrel_arena(data: &GameData, weapon: &str) -> (Arena, BodyId) {
    let mut arena = Arena::new(data, 600.0);
    let barrel = arena.sim.add_prop(&barrel_at(data, 200.0));
    arena.arm(weapon);
    (arena, barrel)
}

/// The tick of each break among `events`, in order, counted from their start.
fn break_ticks(events: &[TickEvents]) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .flat_map(|(tick, e)| e.prop_breaks.iter().map(move |_| tick))
        .collect()
}

const TICKS_PER_SECOND: f32 = 66.0;

#[test]
fn a_swing_of_the_crowbar_sets_a_barrel_off_at_once() {
    let data = data();
    let mut arena = Arena::new(&data, 600.0);
    let barrel = arena.sim.add_prop(&heavy(breakable_box(
        &data,
        Vec3::new(50.0, 0.0, 0.0),
        (16.0, 90.0),
        10.0,
        Some((120.0, 256.0)),
    )));
    arena.arm("bar");
    let mut events = arena.hold(Buttons::ATTACK, 0.0, 1);
    events.extend(arena.idle(60));
    let broke = breaks(&events);
    assert_eq!(broke.len(), 1, "{broke:?}");
    assert_eq!(broke[0].body, barrel);
    assert!(broke[0].exploded);
}

#[test]
fn a_pistol_takes_three_shots_the_second_lights_the_barrel_the_third_sets_it_off() {
    let data = data();
    let (mut arena, barrel) = barrel_arena(&data, "sidearm");
    let first = shoot(&mut arena);
    assert_eq!(ignitions(&first), 0, "8 of 20 is not half its health");
    assert!(breaks(&first).is_empty());
    let second = shoot(&mut arena);
    assert_eq!(ignitions(&second), 1, "the second sets it on fire");
    let ignition = second
        .iter()
        .flat_map(|e| &e.prop_ignitions)
        .next()
        .expect("ignited");
    assert_eq!(ignition.body, barrel);
    // Restored to 20 and taking that bullet's 8: 12 bites, 2.3 s.
    assert!((ignition.fuse - 2.3).abs() < 1e-3, "{}", ignition.fuse);
    assert!(breaks(&second).is_empty(), "it burns for a while first");
    let third = shoot(&mut arena);
    let broke = breaks(&third);
    assert_eq!(broke.len(), 1, "a burning barrel shot goes off at once");
    assert!(broke[0].exploded);
}

#[test]
fn a_bullet_that_would_kill_it_sets_a_barrel_off_at_once() {
    let data = data();
    let (mut arena, barrel) = barrel_arena(&data, "magnum");
    let first = shoot(&mut arena);
    assert_eq!(ignitions(&first), 0, "it never burned");
    let broke = breaks(&first);
    assert_eq!(broke.len(), 1, "{broke:?}");
    assert_eq!(broke[0].body, barrel);
    assert!(broke[0].exploded);
}

#[test]
fn a_burning_barrel_explodes_when_its_health_burns_away_and_credits_who_lit_it() {
    let data = data();
    let (mut arena, _) = barrel_arena(&data, "sidearm");
    // The target stands next to the barrel, off the line of fire.
    arena
        .sim
        .teleport_player(arena.target, Vec3::new(200.0, 90.0, 1.0));
    arena
        .sim
        .player_mut(arena.target)
        .expect("target")
        .vitals
        .health = 30.0;
    let mut events = arena.hold(Buttons::ATTACK, 0.0, 1);
    events.extend(arena.idle(80));
    let lit = arena.hold(Buttons::ATTACK, 0.0, 1);
    assert_eq!(ignitions(&lit), 1);
    let burn_ticks = (2.3 * TICKS_PER_SECOND) as usize;
    let mut burning = arena.idle(burn_ticks - 4);
    assert!(breaks(&burning).is_empty(), "still burning");
    burning.extend(arena.idle(8));
    let broke = breaks(&burning);
    assert_eq!(broke.len(), 1, "{broke:?}");
    assert!(broke[0].exploded);
    assert_eq!(burning.iter().flat_map(|e| &e.explosions).count(), 1);
    let kills: Vec<_> = burning.iter().flat_map(|e| &e.kills).collect();
    assert_eq!(kills.len(), 1, "{kills:?}");
    assert_eq!(kills[0].victim, arena.target);
    assert_eq!(kills[0].killer, Some(arena.shooter));
    assert_eq!(kills[0].cause, Cause::Explosion);
    let points = arena.sim.game().score(arena.shooter).points;
    assert_eq!(points, i32::try_from(data.rules.points.barrel).unwrap());
}

#[test]
fn barrels_set_each_other_off_one_after_another() {
    let data = data();
    let mut arena = Arena::new(&data, 900.0);
    let barrels: Vec<_> = [200.0, 330.0, 460.0]
        .into_iter()
        .map(|x| arena.sim.add_prop(&barrel_at(&data, x)))
        .collect();
    arena.arm("magnum");
    let mut events = arena.hold(Buttons::ATTACK, 0.0, 1);
    let shot = events
        .iter()
        .position(|e| !e.prop_breaks.is_empty())
        .expect("the magnum sets off the first");
    assert_eq!(events[shot].prop_breaks[0].body, barrels[0]);
    assert_eq!(events[shot].prop_breaks.len(), 1, "only the first, at once");
    let caught: Vec<_> = events[shot]
        .prop_ignitions
        .iter()
        .map(|ignition| ignition.body)
        .collect();
    assert_eq!(caught, barrels[1..], "the others in reach catch");
    let next_tick = arena.idle(1);
    assert!(breaks(&next_tick).is_empty(), "the second is burning");
    assert!(!arena.sim.physics().is_broken(barrels[1]));
    events.extend(next_tick);
    events.extend(arena.idle(66 * 12));
    let broke: Vec<_> = breaks(&events).iter().map(|b| b.body).collect();
    assert_eq!(broke, barrels, "in order");
    assert_eq!(events.iter().flat_map(|e| &e.explosions).count(), 3);
    let at = break_ticks(&events);
    assert!(at[1] - at[0] >= TICKS_PER_SECOND as usize, "{at:?}");
    assert!(at[2] > at[1], "{at:?}");
}

/// A barrel shot off by the magnum, with another `apart` units along the line
/// of fire: the blast, and when the other goes off after it (ticks).
fn blast_beside_a_barrel(data: &GameData, apart: f32) -> (f32, usize) {
    let (mut arena, _) = barrel_arena(data, "magnum");
    let other = arena.sim.add_prop(&barrel_at(data, 200.0 + apart));
    let events = arena.hold(Buttons::ATTACK, 0.0, 1);
    let blast = events.iter().position(|e| !e.prop_breaks.is_empty());
    assert_eq!(blast, Some(0));
    let fuse = events[0]
        .prop_ignitions
        .iter()
        .find(|ignition| ignition.body == other)
        .expect("the other caught")
        .fuse;
    let after = arena.idle(66 * 6);
    let broke = breaks(&after);
    assert_eq!(broke.len(), 1, "{broke:?}");
    assert_eq!(broke[0].body, other);
    (fuse, break_ticks(&after)[0])
}

#[test]
fn a_nearer_blast_gives_a_shorter_fuse() {
    let data = data();
    let (near, near_ticks) = blast_beside_a_barrel(&data, 100.0);
    let (far, far_ticks) = blast_beside_a_barrel(&data, 180.0);
    assert!(near < far, "{near} then {far}");
    assert!(near_ticks < far_ticks, "{near_ticks} then {far_ticks}");
    // The time it is told it has is when it goes.
    let near_seconds = near_ticks as f32 / TICKS_PER_SECOND;
    assert!(
        (near_seconds - near).abs() < 0.1,
        "{near_seconds} and {near}"
    );
}

#[test]
fn a_blast_right_beside_a_barrel_leaves_it_burning_for_about_a_second() {
    let data = data();
    let barrels = data.physics.breakables.barrels;
    let (fuse, ticks) = blast_beside_a_barrel(&data, 34.0);
    assert!(
        fuse <= barrels.burn_time(barrels.min_blast_health + barrels.blast_health_spread + 7.0),
        "{fuse}"
    );
    assert!(
        fuse >= barrels.burn_time(barrels.min_blast_health),
        "{fuse}"
    );
    assert!(ticks as f32 >= 0.85 * TICKS_PER_SECOND, "{ticks}");
}

#[test]
fn a_flame_lights_a_barrel_touching_it_but_not_one_further_off() {
    let data = data();
    let mut arena = Arena::new(&data, 900.0);
    let lit = arena.sim.add_prop(&barrel_at(&data, 200.0));
    let touching = arena.sim.add_prop(&PropDesc {
        origin: Vec3::new(200.0, 36.0, 0.0),
        ..barrel_at(&data, 0.0)
    });
    let further = arena.sim.add_prop(&PropDesc {
        origin: Vec3::new(200.0, 200.0, 0.0),
        ..barrel_at(&data, 0.0)
    });
    arena.arm("sidearm");
    arena.hold(Buttons::ATTACK, 0.0, 1);
    arena.idle(80);
    let mut events = arena.hold(Buttons::ATTACK, 0.0, 1);
    // Up to just before the first goes off, 2.3 s after it caught.
    events.extend(arena.idle((2.2 * TICKS_PER_SECOND) as usize));
    let caught: Vec<_> = events
        .iter()
        .flat_map(|e| &e.prop_ignitions)
        .map(|ignition| ignition.body)
        .collect();
    assert_eq!(caught, [lit, touching], "without any blast");
    assert!(breaks(&events).is_empty());
    assert!(!caught.contains(&further));
}

#[test]
fn a_barrel_that_falls_or_is_knocked_about_does_not_explode() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let barrel = arena.sim.add_prop(&breakable_box(
        &data,
        Vec3::new(-200.0, 400.0, 500.0),
        (16.0, 90.0),
        10.0,
        Some((120.0, 256.0)),
    ));
    let events = arena.idle(200);
    assert!(breaks(&events).is_empty(), "a fall does not set it off");
    assert!(!arena.sim.physics().is_broken(barrel));
    assert!(
        events
            .iter()
            .flat_map(|e| &e.prop_impacts)
            .any(|impact| impact.body == barrel && impact.speed > 500.0),
        "it did land hard"
    );
}

#[test]
fn a_barrel_thrown_with_the_gravity_gun_explodes_on_what_it_touches() {
    let data = data();
    let wall = WorldBuilder::new().solid_box(
        Vec3::new(700.0, -300.0, 0.0),
        Vec3::new(720.0, 300.0, 300.0),
    );
    let mut arena = Arena::with_world(&data, 1500.0, wall);
    let barrel = arena.sim.add_prop(&breakable_box(
        &data,
        Vec3::new(90.0, 0.0, 0.0),
        (16.0, 90.0),
        10.0,
        Some((120.0, 256.0)),
    ));
    arena.arm("manipulator");
    let mut events = arena.hold(Buttons::ATTACK2, 0.0, 30);
    assert!(breaks(&events).is_empty(), "holding it does nothing");
    events.extend(arena.hold(Buttons::ATTACK, 0.0, 1));
    events.extend(arena.idle(120));
    let broke = breaks(&events);
    assert_eq!(broke.len(), 1, "{broke:?}");
    assert_eq!(broke[0].body, barrel);
    assert!(broke[0].exploded);
    assert!(
        broke[0].position.x > 200.0,
        "it flew off before going off: {}",
        broke[0].position
    );
}

/// A barrel small enough to fly at eye level over whatever lies on the floor,
/// in the Gravity Gun's grip in `arena`, weighing `mass`.
fn barrel_in_hand(arena: &mut Arena, data: &GameData, mass: f32) -> BodyId {
    let barrel = arena.sim.add_prop(&PropDesc {
        mass,
        ..breakable_box(
            data,
            Vec3::new(90.0, 0.0, 0.0),
            (8.0, 16.0),
            10.0,
            Some((120.0, 256.0)),
        )
    });
    arena.arm("manipulator");
    let eye = arena.sim.eye_position(arena.shooter).expect("shooter");
    let center = arena.sim.physics().center_of_mass(barrel).expect("barrel");
    let down = (eye.z - center.z).atan2(center.x - eye.x).to_degrees();
    arena.hold(Buttons::ATTACK2, down, 2);
    arena.idle(30);
    assert_eq!(
        arena
            .sim
            .player(arena.shooter)
            .expect("shooter")
            .manipulator
            .held(),
        Some(barrel)
    );
    barrel
}

/// Throws what the shooter holds and lets it fly: what happened.
fn throw_it(arena: &mut Arena) -> Vec<TickEvents> {
    let mut events = arena.hold(Buttons::ATTACK, 0.0, 1);
    events.extend(arena.idle(60));
    events
}

/// A prop that cannot break hanging at eye level `x` units ahead, asleep until
/// something hits it.
fn hanging(data: &GameData, x: f32, mass: f32, debris: bool) -> PropDesc {
    PropDesc {
        mass,
        debris,
        start_asleep: true,
        breakable: None,
        ..breakable_box(data, Vec3::new(x, 0.0, 50.0), (16.0, 32.0), 1000.0, None)
    }
}

/// `barrel` alone broke and exploded in `events`, near `x`.
fn went_off_near(events: &[TickEvents], barrel: BodyId, x: f32) {
    let broke = breaks(events);
    assert_eq!(broke.len(), 1, "{broke:?}");
    assert_eq!(broke[0].body, barrel);
    assert!(broke[0].exploded);
    let at = broke[0].position.x;
    assert!(
        (x - 60.0..x + 10.0).contains(&at),
        "went off at {at}, not {x}"
    );
}

#[test]
fn a_thrown_barrel_explodes_on_another_prop() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    arena.sim.add_prop(&hanging(&data, 400.0, 30.0, false));
    let barrel = barrel_in_hand(&mut arena, &data, 20.0);
    went_off_near(&throw_it(&mut arena), barrel, 400.0);
}

#[test]
fn a_thrown_barrel_explodes_on_a_prop_added_after_it() {
    // Which of two props the physics reports as the one that struck depends on
    // the order they were added in; it must not matter.
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    let barrel = barrel_in_hand(&mut arena, &data, 20.0);
    arena.sim.add_prop(&hanging(&data, 400.0, 30.0, false));
    went_off_near(&throw_it(&mut arena), barrel, 400.0);
}

#[test]
fn a_barrel_going_off_on_a_prop_is_credited_to_its_thrower() {
    let data = data();
    let mut arena = Arena::new(&data, 500.0);
    arena.sim.add_prop(&hanging(&data, 400.0, 30.0, false));
    let barrel = barrel_in_hand(&mut arena, &data, 20.0);
    let events = throw_it(&mut arena);
    went_off_near(&events, barrel, 400.0);
    let blast = events.iter().flat_map(|e| &e.damage).find(|damage| {
        damage.victim == arena.target && damage.damage.attacker == Some(arena.shooter)
    });
    assert!(blast.is_some(), "the thrower is credited: {events:?}");
}

#[test]
fn a_thrown_barrel_explodes_on_a_prop_of_next_to_no_mass() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    arena.sim.add_prop(&hanging(&data, 400.0, 0.05, false));
    let barrel = barrel_in_hand(&mut arena, &data, 20.0);
    went_off_near(&throw_it(&mut arena), barrel, 400.0);
}

#[test]
fn a_thrown_barrel_of_next_to_no_mass_explodes_on_a_prop() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    arena.sim.add_prop(&hanging(&data, 400.0, 30.0, false));
    let barrel = barrel_in_hand(&mut arena, &data, 0.05);
    went_off_near(&throw_it(&mut arena), barrel, 400.0);
}

#[test]
fn a_thrown_barrel_explodes_on_a_pickup() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    // Pickups lie as debris: players pass through them, props do not.
    arena
        .sim
        .add_entities(&[MapEntity::new("item_healthkit", [400.0, 0.0, 50.0])]);
    arena
        .sim
        .add_pickup_bodies([Some(hanging(&data, 400.0, 2.0, true))]);
    let barrel = barrel_in_hand(&mut arena, &data, 20.0);
    went_off_near(&throw_it(&mut arena), barrel, 400.0);
}

#[test]
fn a_thrown_barrel_explodes_on_another_player_and_its_thrower_gets_the_credit() {
    let data = data();
    let mut arena = Arena::new(&data, 400.0);
    let barrel = barrel_in_hand(&mut arena, &data, 20.0);
    let events = throw_it(&mut arena);
    went_off_near(&events, barrel, 400.0);
    let blast = events.iter().flat_map(|e| &e.damage).find(|damage| {
        damage.victim == arena.target && damage.damage.attacker == Some(arena.shooter)
    });
    assert!(blast.is_some(), "the thrower is credited: {events:?}");
}

#[test]
fn a_punted_barrel_explodes_on_another_player() {
    let data = data();
    let mut arena = Arena::new(&data, 400.0);
    // Hanging at eye level, to go straight at them.
    let barrel = arena.sim.add_prop(&PropDesc {
        start_asleep: true,
        ..breakable_box(
            &data,
            Vec3::new(90.0, 0.0, 57.0),
            (8.0, 16.0),
            10.0,
            Some((120.0, 256.0)),
        )
    });
    arena.arm("manipulator");
    let mut events = arena.hold(Buttons::ATTACK, 0.0, 1);
    events.extend(arena.idle(60));
    went_off_near(&events, barrel, 400.0);
}

#[test]
fn a_thrown_barrel_explodes_on_a_player_it_is_too_light_to_hurt() {
    let data = data();
    let mut arena = Arena::new(&data, 400.0);
    let barrel = barrel_in_hand(&mut arena, &data, 0.05);
    let events = throw_it(&mut arena);
    let impacts: Vec<_> = events.iter().flat_map(|e| &e.impacts).collect();
    assert!(impacts.is_empty(), "it hurt them: {impacts:?}");
    went_off_near(&events, barrel, 400.0);
}

#[test]
fn a_thrown_barrel_does_not_go_off_on_its_thrower_on_the_way_out() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    let barrel = barrel_in_hand(&mut arena, &data, 20.0);
    let mut events = arena.hold(Buttons::ATTACK, -80.0, 1);
    events.extend(arena.idle(10));
    assert!(breaks(&events).is_empty());
    assert!(!arena.sim.physics().is_broken(barrel));
    let height = arena
        .sim
        .physics()
        .center_of_mass(barrel)
        .expect("barrel")
        .z;
    assert!(height > 150.0, "it got away: {height}");
}

#[test]
fn a_barrel_walked_into_stays_whole() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    let barrel = arena.sim.add_prop(&breakable_box(
        &data,
        Vec3::new(60.0, 0.0, 0.0),
        (16.0, 90.0),
        10.0,
        Some((120.0, 256.0)),
    ));
    let forward = UserCmd {
        forward: 1.0,
        ..UserCmd::default()
    };
    let events: Vec<_> = (0..60).map(|_| arena.tick(&forward)).collect();
    assert!(breaks(&events).is_empty());
    assert!(!arena.sim.physics().is_broken(barrel));
}

#[test]
fn a_barrel_that_is_only_put_down_with_the_gravity_gun_stays_whole() {
    let data = data();
    let mut arena = Arena::new(&data, 1500.0);
    let barrel = arena.sim.add_prop(&breakable_box(
        &data,
        Vec3::new(90.0, 0.0, 0.0),
        (16.0, 90.0),
        10.0,
        Some((120.0, 256.0)),
    ));
    arena.arm("manipulator");
    arena.hold(Buttons::ATTACK2, 0.0, 30);
    // Let go (the grab button again) and let it fall to the floor.
    arena.idle(5);
    arena.hold(Buttons::ATTACK2, 0.0, 1);
    let events = arena.idle(200);
    assert!(breaks(&events).is_empty());
    assert!(!arena.sim.physics().is_broken(barrel));
}

/// A prop made as HL2DM's are, asleep `height` above the floor at the far
/// corner of the arena: `wood_crate001a`'s 30 kg and 30 health by default.
fn hanging_crate(data: &GameData, height: f32) -> PropDesc {
    PropDesc {
        mass: 30.0,
        start_asleep: true,
        material: *data.physics.materials.get("wood_crate"),
        ..breakable_box(
            data,
            Vec3::new(-200.0, 400.0, height),
            (16.0, 32.0),
            30.0,
            None,
        )
    }
}

/// `garbage_glassbottle001a`: 1 kg, 5 health, the glass damage table.
fn hanging_bottle(data: &GameData, height: f32) -> PropDesc {
    fragile(data, height, 1.0, "glassbottle")
}

/// `terracotta01`: 2 kg, 5 health, the glass damage table.
fn hanging_vase(data: &GameData, height: f32) -> PropDesc {
    fragile(data, height, 2.0, "pottery")
}

fn fragile(data: &GameData, height: f32, mass: f32, surface: &str) -> PropDesc {
    let mut desc = PropDesc {
        mass,
        start_asleep: true,
        material: *data.physics.materials.get(surface),
        ..breakable_box(
            data,
            Vec3::new(-200.0, 400.0, height),
            (4.0, 12.0),
            5.0,
            None,
        )
    };
    if let Some(breakable) = &mut desc.breakable {
        breakable.damage_table = Some("glass".to_owned());
    }
    desc
}

/// Adds `desc` asleep in the air, waits until new props may be hurt, and lets
/// it go: what came of it.
fn drop_it(arena: &mut Arena, desc: &PropDesc) -> (BodyId, Vec<TickEvents>) {
    let body = arena.sim.add_prop(desc);
    arena.idle(50);
    arena.sim.set_prop_velocity(body, Vec3::new(0.0, 0.0, -1.0));
    (body, arena.idle(200))
}

#[test]
fn a_hard_fall_breaks_a_crate() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let (crate_, events) = drop_it(&mut arena, &hanging_crate(&data, 500.0));
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
fn a_crate_breaks_falling_about_200_units_and_not_much_less() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let (_, events) = drop_it(&mut arena, &hanging_crate(&data, 180.0));
    assert!(
        breaks(&events).is_empty(),
        "180 units: {:?}",
        breaks(&events)
    );
    let mut arena = Arena::new(&data, 2000.0);
    let (_, events) = drop_it(&mut arena, &hanging_crate(&data, 230.0));
    assert_eq!(breaks(&events).len(), 1, "230 units");
}

#[test]
fn a_gentle_drop_leaves_a_crate_whole() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let (crate_, events) = drop_it(&mut arena, &hanging_crate(&data, 40.0));
    assert!(breaks(&events).is_empty());
    assert!(!arena.sim.physics().is_broken(crate_));
}

/// An arena with a wall across +X, and a crate waiting a little way off it.
fn crate_by_a_wall(data: &GameData) -> (Arena, BodyId) {
    let wall = WorldBuilder::new().solid_box(
        Vec3::new(-300.0, 300.0, 0.0),
        Vec3::new(-280.0, 500.0, 300.0),
    );
    let mut arena = Arena::with_world(data, 2000.0, wall);
    let crate_ = arena.sim.add_prop(&hanging_crate(data, 1.0));
    arena.idle(50);
    (arena, crate_)
}

#[test]
fn a_shove_into_a_wall_leaves_a_crate_whole() {
    let data = data();
    let (mut arena, crate_) = crate_by_a_wall(&data);
    arena
        .sim
        .set_prop_velocity(crate_, Vec3::new(-190.0, 0.0, 0.0));
    let events = arena.idle(60);
    assert!(breaks(&events).is_empty(), "{:?}", breaks(&events));
}

#[test]
fn a_hard_blow_into_a_wall_breaks_a_crate() {
    let data = data();
    let (mut arena, crate_) = crate_by_a_wall(&data);
    let launch = data.manipulator.launch.speed_for(30.0);
    arena
        .sim
        .set_prop_velocity(crate_, Vec3::new(-launch, 0.0, 0.0));
    let events = arena.idle(30);
    assert_eq!(breaks(&events).len(), 1, "{:?}", breaks(&events));
    assert_eq!(breaks(&events)[0].body, crate_);
}

#[test]
fn a_crate_rubbing_or_pressed_against_a_wall_is_only_hurt_the_once() {
    let data = data();
    let (mut arena, crate_) = crate_by_a_wall(&data);
    // Each hit at this speed takes a third of its health.
    let mut events = Vec::new();
    for _ in 0..200 {
        arena
            .sim
            .set_prop_velocity(crate_, Vec3::new(-400.0, 0.0, 0.0));
        events.extend(arena.idle(1));
    }
    assert!(breaks(&events).is_empty(), "{:?}", breaks(&events));
    assert!(
        events
            .iter()
            .flat_map(|e| &e.prop_impacts)
            .filter(|impact| impact.body == crate_)
            .count()
            > 100,
        "it did go on pressing"
    );
}

#[test]
fn repeated_blows_add_up() {
    let data = data();
    let (mut arena, crate_) = crate_by_a_wall(&data);
    let mut events = Vec::new();
    // Off the wall and back into it, far enough apart to be new hits.
    for _ in 0..8 {
        arena
            .sim
            .set_prop_velocity(crate_, Vec3::new(300.0, 0.0, 0.0));
        events.extend(arena.idle(30));
        arena
            .sim
            .set_prop_velocity(crate_, Vec3::new(-400.0, 0.0, 0.0));
        events.extend(arena.idle(30));
    }
    assert_eq!(breaks(&events).len(), 1, "{:?}", breaks(&events));
}

#[test]
fn a_bottle_dropped_a_little_way_breaks() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let (bottle, events) = drop_it(&mut arena, &hanging_bottle(&data, 8.0));
    let broke = breaks(&events);
    assert_eq!(broke.len(), 1, "{broke:?}");
    assert_eq!(broke[0].body, bottle);
}

#[test]
fn a_vase_dropped_a_little_way_breaks() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let (vase, events) = drop_it(&mut arena, &hanging_vase(&data, 8.0));
    let broke = breaks(&events);
    assert_eq!(broke.len(), 1, "{broke:?}");
    assert_eq!(broke[0].body, vase);
}

#[test]
fn a_bottle_set_down_all_but_on_the_floor_is_left_whole() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let (bottle, events) = drop_it(&mut arena, &hanging_bottle(&data, 2.0));
    assert!(breaks(&events).is_empty(), "{:?}", breaks(&events));
    assert!(!arena.sim.physics().is_broken(bottle));
}

#[test]
fn a_bottle_breaks_on_a_landing_too_slow_to_be_heard() {
    let data = data();
    let report = data.physics.breakables.impact.report_speed;
    let fall = (2.0 * data.physics.gravity * 8.0_f32).sqrt();
    assert!(fall < report, "{fall} is under {report}");
    let mut arena = Arena::new(&data, 2000.0);
    let (_, events) = drop_it(&mut arena, &hanging_bottle(&data, 8.0));
    assert_eq!(breaks(&events).len(), 1);
    assert_eq!(events.iter().flat_map(|e| &e.prop_impacts).count(), 0);
}

#[test]
fn a_bottle_dropped_on_a_crate_breaks_and_the_crate_is_left_whole() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let crate_ = arena.sim.add_prop(&hanging_crate(&data, 0.5));
    arena.idle(50);
    let (bottle, events) = drop_it(&mut arena, &hanging_bottle(&data, 60.0));
    let broke = breaks(&events);
    assert_eq!(broke.len(), 1, "{broke:?}");
    assert_eq!(broke[0].body, bottle);
    assert!(!arena.sim.physics().is_broken(crate_));
}

#[test]
fn a_crate_spawned_inside_the_floor_does_not_break_while_settling() {
    let data = data();
    let mut arena = Arena::new(&data, 2000.0);
    let crate_ = arena.sim.add_prop(&breakable_box(
        &data,
        Vec3::new(-200.0, 400.0, -24.0),
        (8.0, 32.0),
        20.0,
        None,
    ));
    let bottle = arena.sim.add_prop(&PropDesc {
        start_asleep: false,
        ..fragile(&data, -4.0, 1.0, "glassbottle")
    });
    let events = arena.idle(100);
    assert!(breaks(&events).is_empty(), "{:?}", breaks(&events));
    assert!(!arena.sim.physics().is_broken(crate_));
    assert!(!arena.sim.physics().is_broken(bottle));
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

#[test]
fn a_frag_is_a_physics_body_that_bounces_and_bursts_when_its_fuse_runs_out() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    arena.arm("frag");
    let events = arena.throw(Buttons::ATTACK, 0.0, 1);
    assert_eq!(events.fired.len(), 1);
    assert_eq!(arena.sim.grenades().len(), 1, "one grenade in flight");
    let first = arena.sim.grenades()[0];

    // Watch it fall, bounce on the floor and roll about.
    let mut heights = Vec::new();
    let mut explosions = Vec::new();
    for _ in 0..200 {
        let events = arena.idle(1);
        if let Some(grenade) = arena.sim.grenades().first() {
            assert_eq!(grenade.id, first.id);
            heights.push(grenade.position.z);
        }
        explosions.extend(events.into_iter().flat_map(|e| e.explosions));
        if arena.sim.grenades().is_empty() {
            break;
        }
    }
    assert_eq!(explosions.len(), 1, "it burst once");
    assert!(arena.sim.grenades().is_empty(), "and is gone");
    let lowest = heights.iter().copied().fold(f32::MAX, f32::min);
    assert!(lowest < 8.0, "it came down to the floor: {lowest}");
    // A bounce: it was rising again after first touching down (a can
    // coming in at a flat angle gives up much of its fall to spin).
    let touched = heights.iter().position(|&z| z < 8.0).expect("touched down");
    assert!(
        heights[touched..].iter().any(|&z| z > lowest + 2.5),
        "it bounced"
    );
    // Its fuse is 2.5 s from the throw.
    let burst_at = heights.len() as f32 / 66.0;
    assert!((2.2..2.7).contains(&burst_at), "{burst_at}");
}

#[test]
fn a_frag_thrown_at_someone_kills_them_for_the_thrower() {
    let data = data();
    let mut arena = Arena::new(&data, 60.0);
    arena.arm("frag");
    arena
        .sim
        .player_mut(arena.target)
        .expect("target")
        .vitals
        .health = 20.0;
    // The thrower can take their own blast (all of it: HL2DM spares nobody).
    arena
        .sim
        .player_mut(arena.shooter)
        .expect("shooter")
        .vitals
        .health = 1000.0;
    // Dropped at the thrower's feet, it skitters about and goes off within
    // reach of both.
    arena.throw(Buttons::ATTACK, 85.0, 1);
    let events = arena.idle(200);
    let kills: Vec<_> = events.iter().flat_map(|e| &e.kills).collect();
    assert_eq!(kills.len(), 1, "{kills:?}");
    assert_eq!(kills[0].victim, arena.target);
    assert_eq!(kills[0].killer, Some(arena.shooter));
    assert_eq!(
        kills[0].cause,
        Cause::Weapon(arena.weapon("frag")),
        "credited to the frag"
    );
    let frag_points = data.rules.points.for_weapon("frag");
    assert_eq!(
        arena.sim.game().score(arena.shooter).points,
        i32::try_from(frag_points).unwrap()
    );
}

#[test]
fn a_grenade_is_only_thrown_when_the_button_is_let_go() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    arena.arm("frag");
    let frag = arena.weapon("frag");
    // However long it is held, nothing leaves the hand.
    let held = arena.hold(Buttons::ATTACK, 0.0, 120);
    assert!(held.iter().all(|events| events.fired.is_empty()));
    assert!(arena.sim.grenades().is_empty());
    let player = arena.sim.player(arena.shooter).expect("shooter");
    assert_eq!(
        player.inventory.phase(),
        rl_sim::weapons::Phase::Priming(rl_sim::weapons::Mode::Primary)
    );
    let events = arena.release(Buttons::NONE, 0.0);
    assert_eq!(
        events.fired,
        [(arena.shooter, frag, rl_sim::weapons::Mode::Primary)]
    );
    assert_eq!(arena.sim.grenades().len(), 1);
    // The fuse starts at the throw, not at the pin.
    assert!(arena.sim.grenades()[0].age < 0.05);
}

#[test]
fn a_grenade_is_a_cylinder_whose_mass_is_in_the_middle() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    arena.arm("frag");
    arena.throw(Buttons::ATTACK, 0.0, 1);
    let grenade = arena.sim.grenades()[0];
    let body = grenade.body;
    let physics = arena.sim.physics();
    let pose = physics.pose(body).expect("pose");
    let centre = physics.center_of_mass(body).expect("centre");
    // The mass is at the body's origin: the middle of the cylinder.
    assert!(pose.position.distance(centre) < 0.05, "{pose:?} {centre}");
    assert_eq!(grenade.position, centre);
    // Its surface is its own, which names a metal grenade's sounds.
    assert_eq!(arena.sim.surface_of(body), "grenade");
    // It is small: nine units long and three and a half across, so the
    // corners of its ends are (1.75² + 4.5²)^½ from the middle.
    let reach = physics.radius(body).expect("radius");
    assert!((reach - 4.83).abs() < 0.05, "{reach}");
}

#[test]
fn a_grenade_comes_to_rest_lying_on_its_side_not_balanced_on_a_point() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    arena.arm("frag");
    arena.throw(Buttons::ATTACK, -10.0, 1);
    let mut last = Vec3::ZERO;
    for _ in 0..150 {
        arena.idle(1);
        if let Some(grenade) = arena.sim.grenades().first() {
            last = grenade.position;
        }
    }
    // Resting upright its middle would be 4.5 above the floor, on its side
    // 1.75.
    assert!(last.z > 1.0 && last.z < 5.0, "{last}");
}

#[test]
fn the_secondary_button_rolls_a_grenade_along_the_floor_when_crouching() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    arena.arm("frag");
    let crouch = Buttons::DUCK;
    arena.hold(crouch, -20.0, 40);
    arena.hold(Buttons::ATTACK2 | crouch, -20.0, 5);
    let events = arena.release(crouch, -20.0);
    assert_eq!(events.fired.len(), 1);
    assert_eq!(events.fired[0].2, rl_sim::weapons::Mode::Secondary);
    let start = arena.sim.grenades()[0];
    assert!(start.position.z < 6.0, "from the feet: {}", start.position);
    let origin = Vec3::new(0.0, 0.0, 1.0);
    let mut highest = 0.0_f32;
    let mut farthest = 0.0_f32;
    for _ in 0..60 {
        arena.tick(&UserCmd {
            buttons: crouch,
            ..UserCmd::default()
        });
        let grenade = arena.sim.grenades()[0];
        highest = highest.max(grenade.position.z);
        farthest = farthest.max(grenade.position.x - origin.x);
    }
    // Looking down, it still goes along the floor, forward, and rolls far
    // (an overhand throw at 1200 u/s would be metres up and away).
    assert!(highest < 12.0, "never leaves the floor: {highest}");
    assert!((100.0..700.0).contains(&farthest), "{farthest}");
}

#[test]
fn the_secondary_button_lobs_a_grenade_when_standing() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    arena.arm("frag");
    arena.hold(Buttons::ATTACK2, 0.0, 5);
    let events = arena.release(Buttons::NONE, 0.0);
    assert_eq!(events.fired[0].2, rl_sim::weapons::Mode::Secondary);
    let start = arena.sim.grenades()[0];
    assert!(start.position.z > 40.0, "from the hand: {}", start.position);
    let mut highest = 0.0_f32;
    for _ in 0..30 {
        arena.idle(1);
        highest = highest.max(arena.sim.grenades()[0].position.z);
    }
    assert!(highest > start.position.z, "it rises a little first");
}

#[test]
fn headshots_do_triple_damage() {
    let mut data = data();
    data.player.head_scale = 3.0;
    let mut arena = Arena::new(&data, 200.0);
    arena.arm("sidearm");
    // Level with the target's eyes: through the head.
    arena.hold(Buttons::ATTACK, 0.0, 1);
    assert_eq!(arena.health(arena.target), 100.0 - 3.0 * 8.0);
    arena.idle(40);
    // Aimed at the knees (36 units below the eyes over 200 units): the
    // body.
    let pitch = (36.0_f32 / 200.0).atan().to_degrees();
    arena.hold(Buttons::ATTACK, pitch, 1);
    assert_eq!(arena.health(arena.target), 100.0 - 3.0 * 8.0 - 8.0);
}

#[test]
fn the_last_grenade_thrown_leaves_nothing_in_hand() {
    let data = data();
    let mut arena = Arena::new(&data, 3000.0);
    let frag = arena.weapon("frag");
    let smg = arena.weapon("smg");
    // Exactly one grenade: the loadout's.
    let arsenal = arena.sim.arsenal().clone();
    let frags = arsenal.find_ammo("frag").expect("ammo");
    let player = arena.sim.player(arena.shooter).expect("shooter");
    assert_eq!(player.inventory.ammo(frags), 1);
    let select = UserCmd {
        select: Some(frag),
        ..UserCmd::default()
    };
    arena.tick(&select);
    arena.idle(100);
    arena.throw(Buttons::ATTACK, 0.0, 3);
    let after = arena.idle(240);
    let retired = after
        .iter()
        .flat_map(|events| &events.deploys)
        .any(|&(player, weapon)| player == arena.shooter && weapon == smg);
    assert!(retired, "the weapon before it is brought up");
    let player = arena.sim.player(arena.shooter).expect("shooter");
    assert!(!player.inventory.has(frag), "the grenade weapon is gone");
    assert_eq!(player.inventory.active(), Some(smg));
}
