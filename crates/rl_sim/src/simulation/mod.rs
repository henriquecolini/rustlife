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
//! 3. **Map logic** (`logic`): uses, touches, damage and triggers set the
//!    map's wiring off; doors and buttons move, pushing players in their
//!    way or crushing them; teleports, pushes and the map's explosions act.
//! 4. **Physics**: the players' boxes and the movers move to their new
//!    places and the physics world steps; props that hit players hurt
//!    them.
//! 5. **Projectiles** fly, strike and burst.
//! 6. **Pickups** are taken by players touching them, and come back.
//! 7. **Hazards** that are on hurt whoever is inside.
//! 8. **The match** clock runs; it ends at the time limit and restarts after
//!    the intermission, with the map's logic as the map placed it.
//!
//! Every tick reports what happened in [`TickEvents`], for sounds, effects,
//! the kill feed and the HUD. This is the loop the server runs
//! authoritatively.
//!
//! A tick may carry several commands from one player — a server catching up
//! on commands that arrived together — and the player then moves once per
//! command; a player without a command that tick stands still. Hitscan
//! weapons and swings see other players where the shooter saw them when
//! they gave the command ([`UserCmd::view_tick`], `lag`).
//!
//! Clients do not run the tick. They keep a replica that the server's
//! snapshots overwrite (`replica`) and run only their own player's
//! movement and weapon ahead of the server ([`Simulation::predict`]).

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use glam::Vec3;
use rl_data::GameData;
use source_formats::entities::MapEntity;

use crate::collision::{CollisionWorld, Hull, find_free_position};
use crate::combat::hitscan::ShotTrace;
use crate::combat::{Cause, Damage, Target, Vitals};
use crate::hazards::{self, Hazard};
use crate::logic::{LogicState, MapLogic};
use crate::movement::{Buttons, MoveEvents, PlayerState, UserCmd, simulate};
use crate::physics::{BodyId, Impact, JointDesc, PhysicsWorld, PropDesc};
use crate::pickups::{self, Pickup};
use crate::rules::{Match, MatchEvent};
use crate::scene::Scene;
use crate::tools::manipulator::{Manipulator, ToolEvent};
use crate::weapons::projectile::Projectile;
use crate::weapons::{Arsenal, Inventory, Mode, WeaponId};

mod breakables;
mod fire;
mod grenades;
mod lag;
mod life;
mod map;
mod replica;

pub use breakables::{PropBreak, PropIgnition, PropImpactEvent, PropScrapeEvent};
pub use grenades::GrenadeView;
pub use map::MapEvent;

pub use lag::History;
pub use replica::Prediction;

/// A player of a [`Simulation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Spawn {
    /// Where the feet go, Source space.
    pub origin: Vec3,
    /// View yaw, degrees.
    pub yaw: f32,
}

/// Whether a player is in the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Where the player looked on their last command: `(pitch, yaw)`,
    /// degrees.
    pub look: (f32, f32),
}

impl Player {
    /// Whether the player is alive.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.life == Life::Alive
    }
}

/// A bullet or swing, for tracers and impact effects.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExplosionEvent {
    /// Its centre.
    pub position: Vec3,
    /// Its reach, units.
    pub radius: f32,
    /// The weapon whose projectile burst; `None` for the map's own
    /// explosions.
    pub weapon: Option<WeaponId>,
}

/// Damage a player took.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DamageEvent {
    /// Who took it.
    pub victim: PlayerId,
    /// What it was.
    pub damage: Damage,
    /// The health it cost, after armor.
    pub health_lost: f32,
}

/// A death.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Kill {
    /// Who died.
    pub victim: PlayerId,
    /// Who killed them (`None`: themselves or the world).
    pub killer: Option<PlayerId>,
    /// What killed them.
    pub cause: Cause,
}

/// A player came (back) into the game.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
    /// Reloads begun: who began reloading which weapon.
    pub reloads: Vec<(PlayerId, WeaponId)>,
    /// Weapons brought up: who took which in hand.
    pub deploys: Vec<(PlayerId, WeaponId)>,
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
    /// Stations players drew from this tick: who used which (an index into
    /// [`Simulation::pickups`]).
    pub charging: Vec<(PlayerId, usize)>,
    /// Players spawning.
    pub respawns: Vec<RespawnEvent>,
    /// The match ended or restarted.
    pub match_event: Option<MatchEvent>,
    /// What the map's logic did that players see or hear: doors moving,
    /// buttons pressed, glass breaking, players teleported.
    pub map: Vec<MapEvent>,
    /// Props that broke.
    pub prop_breaks: Vec<PropBreak>,
    /// Explosive props set on fire.
    pub prop_ignitions: Vec<PropIgnition>,
    /// Props that struck props or the map.
    pub prop_impacts: Vec<PropImpactEvent>,
    /// Props sliding along something.
    pub prop_scrapes: Vec<PropScrapeEvent>,
}

/// What set off an explosion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BlastSource {
    /// A weapon's projectile.
    Weapon(WeaponId),
    /// A prop that blew up.
    Prop,
    /// The map's own (`env_explosion`).
    Map,
}

/// A map's simulation: collision, physics, players, weapons, pickups and
/// the match.
#[derive(Debug)]
pub struct Simulation {
    /// The map's collision, with its movers where they are now.
    world: CollisionWorld,
    physics: PhysicsWorld,
    /// Players by id; `None` where a player left and nobody has joined in
    /// their place yet.
    players: Vec<Option<Player>>,
    /// Bodies of players who left, for the next to join.
    spare_bodies: Vec<BodyId>,
    /// Where everyone's box was on recent ticks, for lag compensation.
    history: History,
    data: Arc<GameData>,
    arsenal: Arc<Arsenal>,
    dt: f32,
    /// Ticks run so far.
    tick: u64,
    spawns: Vec<Spawn>,
    pickups: Vec<Pickup>,
    hazards: Vec<Hazard>,
    /// The map's wiring: doors, buttons, triggers, traps.
    logic: MapLogic,
    /// Players who pressed use this tick.
    users: Vec<PlayerId>,
    /// Players who held use this tick, once each.
    holders: Vec<PlayerId>,
    /// Damage dealt to movers this tick: `(attacker, BSP model, amount)`.
    brush_damage: Vec<(Option<PlayerId>, usize, f32)>,
    /// Each mover's pose as last given to collision and physics, and
    /// whether it was moving.
    mover_poses: Vec<map::MoverPose>,
    /// Seconds since hazards last dealt a dose.
    hazard_clock: f32,
    projectiles: Vec<Projectile>,
    next_projectile: u32,
    /// Props players threw, punted or dropped: by whom and on which tick,
    /// so the harm they do is credited.
    thrown: Vec<(BodyId, PlayerId, u64)>,
    /// Grenades in flight (the server's), and where clients see them.
    grenades: Vec<grenades::Grenade>,
    grenade_views: Vec<GrenadeView>,
    /// Blasts to set off once the grenades' fuses have all been looked at.
    pending_blasts: Vec<(PlayerId, WeaponId, Vec3, rl_data::ExplosionConfig)>,
    /// Props that can break, and the blows waiting to hurt them.
    breakables: breakables::Breakables,
    /// Impacts of props on props or the map this tick, for the events.
    prop_impacts_pending: Vec<PropImpactEvent>,
    game: Match,
}

impl Simulation {
    /// A simulation of `world` with `data`'s tuning, ticking at its tick
    /// rate.
    #[must_use]
    pub fn new(world: Arc<CollisionWorld>, data: &GameData) -> Self {
        let dt = data.simulation.tick_interval();
        let physics = PhysicsWorld::new(&world, &data.physics, dt);
        let logic = MapLogic::new(&[], &world, &[], &data.map_logic, dt);
        Self {
            world: (*world).clone(),
            physics,
            players: Vec::new(),
            spare_bodies: Vec::new(),
            history: History::new(data.net.max_rewind, dt),
            arsenal: Arc::new(Arsenal::new(data)),
            data: Arc::new(data.clone()),
            dt,
            tick: 0,
            spawns: Vec::new(),
            pickups: Vec::new(),
            hazards: Vec::new(),
            logic,
            users: Vec::new(),
            holders: Vec::new(),
            brush_damage: Vec::new(),
            mover_poses: Vec::new(),
            hazard_clock: 0.0,
            projectiles: Vec::new(),
            next_projectile: 0,
            thrown: Vec::new(),
            grenades: Vec::new(),
            grenade_views: Vec::new(),
            pending_blasts: Vec::new(),
            breakables: breakables::Breakables::default(),
            prop_impacts_pending: Vec::new(),
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

    /// The map's collision, with its movers where they are now.
    #[must_use]
    pub fn world(&self) -> &CollisionWorld {
        &self.world
    }

    /// The map's logic: its movers, triggers and sounds.
    #[must_use]
    pub fn logic(&self) -> &MapLogic {
        &self.logic
    }

    /// What players see of the map's logic now.
    #[must_use]
    pub fn logic_state(&self) -> LogicState {
        self.logic.state()
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

    /// The grenades in flight, where they are.
    #[must_use]
    pub fn grenades(&self) -> &[GrenadeView] {
        &self.grenade_views
    }

    /// Adds a prop. Players and shots collide with it from the next tick
    /// (or [`Simulation::refresh_props`]) on.
    pub fn add_prop(&mut self, desc: &PropDesc) -> BodyId {
        let body = self.physics.add_prop(desc);
        if let Some(breakable) = &desc.breakable {
            self.breakables.register(body, breakable);
        }
        body
    }

    /// Joins props to each other or the world (see
    /// [`map_joints`](crate::physics::map_joints)). Returns `false` if the
    /// joint names a body that does not exist.
    pub fn add_joint(&mut self, joint: &JointDesc) -> bool {
        self.physics.add_joint(joint)
    }

    /// Lets the props fall and settle for `seconds` before anyone arrives,
    /// as a map compiled with its props at rest would have them: nothing is
    /// heard, nobody is hurt and nothing breaks.
    pub fn settle(&mut self, seconds: f32) {
        let ticks = (seconds / self.dt).ceil() as usize;
        for _ in 0..ticks {
            self.physics.step();
        }
        self.physics.take_contacts();
        self.sync_pickup_poses();
    }

    /// Brings collision up to date with props just added, without
    /// advancing time: for a replica, which never ticks.
    pub fn refresh_props(&mut self) {
        self.physics.refresh();
    }

    /// Where players spawn.
    #[must_use]
    pub fn spawns(&self) -> &[Spawn] {
        &self.spawns
    }

    /// Sets where players spawn.
    pub fn set_spawns(&mut self, spawns: Vec<Spawn>) {
        self.spawns = spawns;
    }

    /// Adds the pickups, hazards and logic of the map's `entities` (see
    /// `entity_map.ron` and `logic`). The logic replaces any added before.
    pub fn add_entities(&mut self, entities: &[MapEntity]) {
        self.pickups
            .extend(pickups::from_entities(entities, &self.data, &self.arsenal));
        let first_hazard = self.hazards.len();
        let mut hazard_entities = Vec::new();
        for (offset, (entity, hazard)) in hazards::from_entities(entities, &self.world)
            .into_iter()
            .enumerate()
        {
            hazard_entities.push((entity, first_hazard + offset));
            self.hazards.push(hazard);
        }
        self.logic = MapLogic::new(
            entities,
            &self.world,
            &hazard_entities,
            &self.data.map_logic,
            self.dt,
        );
        self.sync_movers();
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

    /// Gives the pickups bodies, in pickup order: `descs` has one entry per
    /// pickup, `None` for stations (which stay on their wall) and for
    /// models a body cannot be made for. Call once, after
    /// [`add_entities`](Simulation::add_entities).
    pub fn add_pickup_bodies(&mut self, descs: impl IntoIterator<Item = Option<PropDesc>>) {
        let mut added = 0;
        for (pickup, desc) in self.pickups.iter_mut().zip(descs) {
            if pickup.body.is_some() {
                continue;
            }
            let Some(desc) = desc else {
                continue;
            };
            pickup.body = Some(self.physics.add_prop(&desc));
            added += 1;
        }
        tracing::debug!(added, "gave pickups bodies");
    }

    /// Reads every pickup body's pose into its origin, so touches, stations
    /// and effects use where it lies now.
    pub fn sync_pickup_poses(&mut self) {
        for pickup in &mut self.pickups {
            let Some(body) = pickup.body else {
                continue;
            };
            let Some(pose) = self.physics.pose(body) else {
                continue;
            };
            pickup.origin = pose.position;
        }
    }

    /// Takes pickup bodies out of the world while their pickups are taken,
    /// and puts them back—at home and at rest—when they return: how a
    /// replica follows what the server says is there.
    pub fn set_pickup_presence(&mut self, presence: impl IntoIterator<Item = (BodyId, bool)>) {
        let mut changed = false;
        for (body, there) in presence {
            let was = self.physics.prop_enabled(body);
            if there && !was {
                // A returning pickup may have been taken away from its
                // home, and its home pose may not be in the snapshot
                // because it matches the level's: put it back here.
                self.physics.reset_prop(body);
                changed = true;
            } else if was != there {
                self.physics.set_prop_enabled(body, there);
                changed = true;
            }
        }
        if changed {
            self.physics.refresh();
        }
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
        let id = PlayerId::from_index(
            self.players
                .iter()
                .position(Option::is_none)
                .unwrap_or(self.players.len()),
        );
        let origin = self.free_position(spawn.origin).unwrap_or(spawn.origin);
        let player = self.new_player(Spawn { origin, ..spawn });
        self.put_player(id, player);
        id
    }

    /// A player standing at `spawn` with a fresh body, health and the
    /// loadout.
    fn new_player(&mut self, spawn: Spawn) -> Player {
        let state = PlayerState::at(spawn.origin);
        let hull = state.hull(&self.data.movement);
        let body = match self.spare_bodies.pop() {
            Some(body) => {
                self.physics.set_player_enabled(body, true);
                self.physics
                    .set_player(body, state.origin, Vec3::ZERO, &hull);
                body
            }
            None => self.physics.add_player(state.origin, &hull),
        };
        Player {
            state,
            body,
            manipulator: Manipulator::default(),
            vitals: Vitals::spawn(&self.data.player),
            inventory: Inventory::loadout(&self.arsenal, &self.data.rules.loadout),
            life: Life::Alive,
            spawn,
            look: (0.0, spawn.yaw),
        }
    }

    /// Puts `player` in slot `id`, growing the list as needed, with a fresh
    /// score.
    fn put_player(&mut self, id: PlayerId, player: Player) {
        self.store_player(id, player);
        self.game.add_player(id, &self.data.rules);
    }

    /// Puts `player` in slot `id`, growing the list as needed.
    fn store_player(&mut self, id: PlayerId, player: Player) -> &mut Player {
        if self.players.len() <= id.index() {
            self.players.resize_with(id.index() + 1, || None);
        }
        self.players[id.index()].insert(player)
    }

    /// Takes a player out of the game: what they hold drops, their box
    /// leaves the physics world and their score is forgotten. Their id is
    /// given to the next player to join. Returns whether they were there.
    pub fn leave(&mut self, id: PlayerId) -> bool {
        let Some(player) = self.players.get_mut(id.index()).and_then(Option::take) else {
            return false;
        };
        let mut manipulator = player.manipulator;
        manipulator.drop_held(&mut self.physics);
        self.physics.set_player_enabled(player.body, false);
        self.spare_bodies.push(player.body);
        self.game.remove_player(id, &self.data.rules);
        true
    }

    /// A player.
    #[must_use]
    pub fn player(&self, id: PlayerId) -> Option<&Player> {
        self.players.get(id.index()).and_then(Option::as_ref)
    }

    /// A player, to change: give them weapons or ammo, set their health.
    /// Their box in the physics world follows their state on the next
    /// tick.
    pub fn player_mut(&mut self, id: PlayerId) -> Option<&mut Player> {
        self.players.get_mut(id.index()).and_then(Option::as_mut)
    }

    /// Every player, by id.
    pub fn players(&self) -> impl Iterator<Item = (PlayerId, &Player)> {
        self.players
            .iter()
            .enumerate()
            .filter_map(|(index, player)| Some((PlayerId::from_index(index), player.as_ref()?)))
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
        let Some(player) = self.players.get_mut(id.index()).and_then(Option::as_mut) else {
            return false;
        };
        player.state = PlayerState::at(origin);
        let hull = player.state.hull(&self.data.movement);
        self.physics
            .set_player(player.body, origin, Vec3::ZERO, &hull);
        true
    }

    /// Advances the simulation by one tick. Players without a command in
    /// `commands` stand still; a player with several moves once for each,
    /// in order.
    pub fn tick(&mut self, commands: &[(PlayerId, UserCmd)]) -> TickEvents {
        self.tick += 1;
        let mut events = TickEvents::default();
        if !self.game.intermission() {
            for (id, cmd) in commands {
                self.run_player(*id, cmd, &mut events);
            }
        }
        self.respawn_waiting(commands, &mut events);
        self.run_logic(&mut events);

        for player in self.players.iter().flatten() {
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
        self.sync_pickup_poses();
        self.hit_props_by_impacts();
        self.advance_grenades(&mut events);

        self.advance_projectiles(&mut events);
        self.touch_pickups(&mut events);
        self.apply_hazards(&mut events);
        self.burn_props(&mut events);
        self.resolve_prop_hits(&mut events);
        self.respawn_props();
        self.report_scrapes(&mut events);
        self.forget_old_throws();
        if let Some(event) = self.game.tick(self.dt, &self.data.rules) {
            self.on_match_event(event, &mut events);
        }
        let targets = self.targets();
        self.history.record(self.tick, targets);
        events
    }

    /// Drops the record of props thrown so long ago that nobody is credited
    /// for them any more.
    fn forget_old_throws(&mut self) {
        let credit = (self.data.rules.prop_credit / self.dt) as u64;
        let tick = self.tick;
        self.thrown
            .retain(|&(_, _, thrown)| tick.saturating_sub(thrown) <= credit);
    }

    /// Moves one living player and runs their weapon.
    fn run_player(&mut self, id: PlayerId, cmd: &UserCmd, events: &mut TickEvents) {
        let Some(player) = self.players.get_mut(id.index()).and_then(Option::as_mut) else {
            return;
        };
        if !player.alive() {
            return;
        }
        if cmd.buttons.contains(Buttons::USE) {
            if !player.state.old_buttons.contains(Buttons::USE) {
                self.users.push(id);
            }
            if !self.holders.contains(&id) {
                self.holders.push(id);
            }
        }
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: player.manipulator.held(),
        };
        let (state, moved) = simulate(&player.state, cmd, &self.data.movement, self.dt, &scene);
        player.state = state;
        player.look = (cmd.pitch, cmd.yaw);
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

    /// Every living player's box as a shooter saw it: at `view_tick` if
    /// given and recent enough to be remembered (lag compensation), else
    /// now.
    fn targets_seen(&self, view_tick: Option<f64>) -> Vec<Target> {
        view_tick
            .and_then(|tick| self.history.at(tick))
            .unwrap_or_else(|| self.targets())
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
