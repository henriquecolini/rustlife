//! One map's whole simulation, a tick at a time.
//!
//! [`Simulation`] owns the map's collision, its physics world, its players
//! with their health and weapons, the pickups and hazards the map places,
//! projectiles in flight and the match. [`Simulation::tick`] advances
//! everything by one fixed tick in a set order:
//!
//! 1. **Players**, each with their command for the tick: movement against
//!    the map and its props (the prop they hold excepted), pushing the props
//!    they ran into, fall damage, then the weapon in hand — switching,
//!    reloading, firing — or the Manipulator (`fire`).
//! 2. **Respawns**: the dead come back once they press fire or jump after
//!    the respawn delay, or anyway after the forced delay (`life`).
//! 3. **Physics**: the players' boxes move to their new positions and the
//!    physics world steps; props that hit players hurt them.
//! 4. **Projectiles** fly, strike and burst.
//! 5. **Pickups** are taken by players touching them, and come back.
//! 6. **Hazards** hurt whoever is inside.
//! 7. **The match** clock runs; it ends at the time limit and restarts after
//!    the intermission.
//!
//! Every tick reports what happened in [`TickEvents`], for sounds, effects,
//! the kill feed and the HUD. This is the loop the server runs
//! authoritatively; clients will run step 1 for their own player to predict
//! it (M6).

use std::sync::Arc;

use glam::Vec3;
use rl_data::GameData;
use source_formats::entities::MapEntity;

use crate::collision::{CollisionWorld, Hull, find_free_position};
use crate::combat::hitscan::ShotTrace;
use crate::combat::{Cause, Damage, Target, Vitals};
use crate::hazards::{self, Hazard};
use crate::movement::{MoveEvents, PlayerState, UserCmd, simulate};
use crate::physics::{BodyId, Impact, PhysicsWorld, PropDesc};
use crate::pickups::{self, Pickup};
use crate::rules::{Match, MatchEvent};
use crate::scene::Scene;
use crate::tools::manipulator::{Manipulator, ToolEvent};
use crate::weapons::projectile::Projectile;
use crate::weapons::{Arsenal, Inventory, Mode, WeaponId};

mod fire;
mod life;

/// A player of a [`Simulation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlayerId(u32);

impl PlayerId {
    /// The player's index, in the order players joined.
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// The id of the player at `index`.
    #[must_use]
    pub fn from_index(index: usize) -> Self {
        Self(u32::try_from(index).unwrap_or(u32::MAX))
    }
}

/// A place players spawn: a point on the floor and the way to face.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spawn {
    /// Where the feet go, Source space.
    pub origin: Vec3,
    /// View yaw, degrees.
    pub yaw: f32,
}

/// Whether a player is in the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Life {
    /// Alive.
    Alive,
    /// Dead since tick `since`, killed by `killer` (`None`: by themselves
    /// or the world).
    Dead {
        /// The tick of death.
        since: u64,
        /// Who killed them.
        killer: Option<PlayerId>,
    },
}

/// One player's simulated state.
#[derive(Debug, Clone)]
pub struct Player {
    /// Movement state.
    pub state: PlayerState,
    /// The player's box in the physics world.
    body: BodyId,
    /// The player's Manipulator.
    pub manipulator: Manipulator,
    /// Health and armor.
    pub vitals: Vitals,
    /// Weapons and ammo.
    pub inventory: Inventory,
    /// Alive or dead.
    pub life: Life,
    /// Where the player last spawned.
    pub spawn: Spawn,
}

impl Player {
    /// Whether the player is alive.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.life == Life::Alive
    }
}

/// A bullet or swing, for tracers and impact effects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShotEvent {
    /// Who fired.
    pub player: PlayerId,
    /// With what.
    pub weapon: WeaponId,
    /// Where the shot started (the eyes).
    pub start: Vec3,
    /// Where it went.
    pub trace: ShotTrace,
}

/// An explosion, for effects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExplosionEvent {
    /// Its centre.
    pub position: Vec3,
    /// Its reach, units.
    pub radius: f32,
    /// The weapon whose projectile burst.
    pub weapon: WeaponId,
}

/// Damage a player took.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DamageEvent {
    /// Who took it.
    pub victim: PlayerId,
    /// What it was.
    pub damage: Damage,
    /// The health it cost, after armor.
    pub health_lost: f32,
}

/// A death.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Kill {
    /// Who died.
    pub victim: PlayerId,
    /// Who killed them (`None`: themselves or the world).
    pub killer: Option<PlayerId>,
    /// What killed them.
    pub cause: Cause,
}

/// A player came (back) into the game.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RespawnEvent {
    /// Who.
    pub player: PlayerId,
    /// Where.
    pub spawn: Spawn,
}

/// What happened during a tick, for sounds, effects, the kill feed and the
/// HUD.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TickEvents {
    /// Each moved player's movement events.
    pub moves: Vec<(PlayerId, MoveEvents)>,
    /// Props hitting players.
    pub impacts: Vec<(PlayerId, Impact)>,
    /// What players' Manipulators did.
    pub tools: Vec<(PlayerId, ToolEvent)>,
    /// Trigger pulls: who fired which weapon's fire mode.
    pub fired: Vec<(PlayerId, WeaponId, Mode)>,
    /// Bullets and swings.
    pub shots: Vec<ShotEvent>,
    /// Explosions.
    pub explosions: Vec<ExplosionEvent>,
    /// Damage taken.
    pub damage: Vec<DamageEvent>,
    /// Deaths.
    pub kills: Vec<Kill>,
    /// Pickups taken: who took which (an index into
    /// [`Simulation::pickups`]).
    pub pickups: Vec<(PlayerId, usize)>,
    /// Players spawning.
    pub respawns: Vec<RespawnEvent>,
    /// The match ended or restarted.
    pub match_event: Option<MatchEvent>,
}

/// A map's simulation: collision, physics, players, weapons, pickups and
/// the match.
#[derive(Debug)]
pub struct Simulation {
    world: Arc<CollisionWorld>,
    physics: PhysicsWorld,
    players: Vec<Player>,
    data: Arc<GameData>,
    arsenal: Arc<Arsenal>,
    dt: f32,
    /// Ticks run so far.
    tick: u64,
    spawns: Vec<Spawn>,
    pickups: Vec<Pickup>,
    hazards: Vec<Hazard>,
    /// Seconds since hazards last dealt a dose.
    hazard_clock: f32,
    projectiles: Vec<Projectile>,
    next_projectile: u32,
    /// Props players threw, punted or dropped: by whom and on which tick,
    /// so the harm they do is credited.
    thrown: Vec<(BodyId, PlayerId, u64)>,
    game: Match,
}

impl Simulation {
    /// A simulation of `world` with `data`'s tuning, ticking at its tick
    /// rate.
    #[must_use]
    pub fn new(world: Arc<CollisionWorld>, data: &GameData) -> Self {
        let dt = data.simulation.tick_interval();
        let physics = PhysicsWorld::new(&world, &data.physics, dt);
        Self {
            world,
            physics,
            players: Vec::new(),
            arsenal: Arc::new(Arsenal::new(data)),
            data: Arc::new(data.clone()),
            dt,
            tick: 0,
            spawns: Vec::new(),
            pickups: Vec::new(),
            hazards: Vec::new(),
            hazard_clock: 0.0,
            projectiles: Vec::new(),
            next_projectile: 0,
            thrown: Vec::new(),
            game: Match::new(),
        }
    }

    /// The length of a tick, seconds.
    #[must_use]
    pub fn dt(&self) -> f32 {
        self.dt
    }

    /// Ticks run so far.
    #[must_use]
    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    /// The gameplay data.
    #[must_use]
    pub fn data(&self) -> &GameData {
        &self.data
    }

    /// The weapons and ammo types, numbered.
    #[must_use]
    pub fn arsenal(&self) -> &Arsenal {
        &self.arsenal
    }

    /// The map's static collision.
    #[must_use]
    pub fn world(&self) -> &CollisionWorld {
        &self.world
    }

    /// The physics world, for reading prop poses.
    #[must_use]
    pub fn physics(&self) -> &PhysicsWorld {
        &self.physics
    }

    /// The match: scores and clock.
    #[must_use]
    pub fn game(&self) -> &Match {
        &self.game
    }

    /// The map's pickups.
    #[must_use]
    pub fn pickups(&self) -> &[Pickup] {
        &self.pickups
    }

    /// The map's hazards.
    #[must_use]
    pub fn hazards(&self) -> &[Hazard] {
        &self.hazards
    }

    /// Projectiles in flight.
    #[must_use]
    pub fn projectiles(&self) -> &[Projectile] {
        &self.projectiles
    }

    /// Adds a prop.
    pub fn add_prop(&mut self, desc: &PropDesc) -> BodyId {
        self.physics.add_prop(desc)
    }

    /// Sets where players spawn.
    pub fn set_spawns(&mut self, spawns: Vec<Spawn>) {
        self.spawns = spawns;
    }

    /// Adds the pickups and hazards of the map's `entities` (see
    /// `entity_map.ron`).
    pub fn add_entities(&mut self, entities: &[MapEntity]) {
        self.pickups
            .extend(pickups::from_entities(entities, &self.data, &self.arsenal));
        self.hazards
            .extend(hazards::from_entities(entities, &self.world));
        tracing::info!(
            pickups = self.pickups.len(),
            hazards = self.hazards.len(),
            "placed the map's pickups and hazards"
        );
    }

    /// Adds a hazard.
    pub fn add_hazard(&mut self, hazard: Hazard) {
        self.hazards.push(hazard);
    }

    /// Adds a pickup.
    pub fn add_pickup(&mut self, pickup: Pickup) {
        self.pickups.push(pickup);
    }

    /// Adds a player at a spawn point chosen by the rules (or the origin,
    /// in a map without spawn points).
    pub fn join(&mut self) -> PlayerId {
        let spawn = self.choose_spawn(None).unwrap_or(Spawn {
            origin: Vec3::ZERO,
            yaw: 0.0,
        });
        self.add_player(spawn)
    }

    /// Adds a player standing at `origin`, nudged free of anything it
    /// overlaps (mappers leave some spawns touching walls).
    pub fn spawn_player(&mut self, origin: Vec3) -> PlayerId {
        self.add_player(Spawn { origin, yaw: 0.0 })
    }

    fn add_player(&mut self, spawn: Spawn) -> PlayerId {
        let id = PlayerId::from_index(self.players.len());
        let origin = self.free_position(spawn.origin).unwrap_or(spawn.origin);
        let state = PlayerState::at(origin);
        let body = self
            .physics
            .add_player(state.origin, &state.hull(&self.data.movement));
        self.players.push(Player {
            state,
            body,
            manipulator: Manipulator::default(),
            vitals: Vitals::spawn(&self.data.player),
            inventory: Inventory::loadout(&self.arsenal, &self.data.rules.loadout),
            life: Life::Alive,
            spawn: Spawn { origin, ..spawn },
        });
        self.game.add_player();
        id
    }

    /// A player.
    #[must_use]
    pub fn player(&self, id: PlayerId) -> Option<&Player> {
        self.players.get(id.index())
    }

    /// A player, to change: give them weapons or ammo, set their health.
    /// Their box in the physics world follows their state on the next
    /// tick.
    pub fn player_mut(&mut self, id: PlayerId) -> Option<&mut Player> {
        self.players.get_mut(id.index())
    }

    /// Every player, in joining order.
    pub fn players(&self) -> impl Iterator<Item = (PlayerId, &Player)> {
        self.players
            .iter()
            .enumerate()
            .map(|(index, player)| (PlayerId::from_index(index), player))
    }

    /// Where a player's eyes are.
    #[must_use]
    pub fn eye_position(&self, id: PlayerId) -> Option<Vec3> {
        let state = self.player(id)?.state;
        Some(state.origin + Vec3::Z * state.eye_height(&self.data.movement))
    }

    /// Moves a player to the free spot nearest `origin` and stops them;
    /// `false` (and nothing changes) if there is no room there.
    pub fn teleport_player(&mut self, id: PlayerId, origin: Vec3) -> bool {
        let Some(origin) = self.free_position(origin) else {
            return false;
        };
        let Some(player) = self.players.get_mut(id.index()) else {
            return false;
        };
        player.state = PlayerState::at(origin);
        let hull = player.state.hull(&self.data.movement);
        self.physics
            .set_player(player.body, origin, Vec3::ZERO, &hull);
        true
    }

    /// Advances the simulation by one tick. Players without a command in
    /// `commands` stand still.
    pub fn tick(&mut self, commands: &[(PlayerId, UserCmd)]) -> TickEvents {
        self.tick += 1;
        let mut events = TickEvents::default();
        if self.game.playing() {
            for (id, cmd) in commands {
                self.run_player(*id, cmd, &mut events);
            }
        }
        self.respawn_waiting(commands, &mut events);

        for player in &self.players {
            let hull = player.state.hull(&self.data.movement);
            self.physics.set_player(
                player.body,
                player.state.origin,
                player.state.velocity,
                &hull,
            );
        }
        for impact in self.physics.step() {
            self.prop_impact(impact, &mut events);
        }

        self.advance_projectiles(&mut events);
        self.touch_pickups(&mut events);
        self.apply_hazards(&mut events);
        if let Some(event) = self.game.tick(self.dt, &self.data.rules) {
            self.on_match_event(event, &mut events);
        }
        events
    }

    /// Moves one living player and runs their weapon.
    fn run_player(&mut self, id: PlayerId, cmd: &UserCmd, events: &mut TickEvents) {
        let Some(player) = self.players.get_mut(id.index()) else {
            return;
        };
        if !player.alive() {
            return;
        }
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: player.manipulator.held(),
        };
        let (state, moved) = simulate(&player.state, cmd, &self.data.movement, self.dt, &scene);
        player.state = state;
        for touch in moved.touches() {
            self.physics.push(touch.body, touch.normal, touch.speed);
        }
        events.moves.push((id, moved));

        let fall = moved
            .landed
            .map_or(0.0, |speed| self.data.rules.fall_damage.damage(speed));
        if fall > 0.0 {
            let damage = Damage {
                amount: fall,
                attacker: None,
                cause: Cause::Fall,
            };
            self.hurt(id, damage, events);
        }
        self.run_weapon(id, cmd, events);
    }

    /// Every living player's box, as shots see it.
    fn targets(&self) -> Vec<Target> {
        self.players()
            .filter(|(_, player)| player.alive())
            .map(|(id, player)| {
                let hull = player.state.hull(&self.data.movement);
                Target {
                    player: id,
                    mins: player.state.origin + hull.mins,
                    maxs: player.state.origin + hull.maxs,
                }
            })
            .collect()
    }

    fn free_position(&self, origin: Vec3) -> Option<Vec3> {
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
        };
        let hull = Hull::from(&self.data.movement.hulls.standing);
        find_free_position(&scene, origin, &hull)
    }
}
