//! The map's own logic: entity I/O, doors, buttons, triggers and traps.
//!
//! Source maps are programmed by wiring entities together: a button's
//! `OnPressed` output sends `Trigger` to a relay, whose `OnTrigger` opens a
//! door and, four seconds later, enables a `trigger_hurt` (see [`io`]).
//! [`MapLogic`] runs that wiring for the entities rustlife understands:
//!
//! | Entity | Does |
//! |--------|------|
//! | `logic_auto` | fires `OnMapSpawn` (and `OnMultiNewMap` / `OnMultiNewRound`) as the match starts |
//! | `logic_relay` | passes `Trigger` on as `OnTrigger`, while enabled |
//! | `logic_timer` | fires `OnTimer` every `RefireTime` seconds, or at random between two bounds |
//! | `math_counter` | adds, subtracts and sets a value, firing `OutValue`, `OnHitMin`, `OnHitMax` |
//! | `func_door`, `func_door_rotating`, `func_movelinear` | open and close (see [`mover`]) |
//! | `func_button` | pressed by use, touch, damage or `Press` |
//! | `func_brush`, `func_wall_toggle` | shown and hidden, solid or not |
//! | `func_breakable`, `func_breakable_surf` | break when shot enough, or on `Break` |
//! | `trigger_multiple`, `trigger_once` | fire `OnTrigger`, `OnStartTouch`, `OnEndTouch` |
//! | `trigger_teleport`, `trigger_push` | move the players inside (see [`volume`]) |
//! | `trigger_hurt` | the simulation's hazards, switched by `Enable`/`Disable` |
//! | `env_explosion` | an explosion, on `Explode` |
//! | `ambient_generic` | a sound playing at a place, for clients to hear |
//! | `filter_activator_name`/`_class`/`filter_multi` | who sets triggers off |
//!
//! Every entity accepts `Kill` and `FireUser1`–`4`. Inputs to entities
//! rustlife does not run (lights, sprites, NPC spawners) are dropped, and
//! logged once each.
//!
//! The logic runs on the server only. What players see of it — movers'
//! positions and the sounds playing — is replicated ([`LogicState`]); a
//! client's replica only takes it on. The simulation feeds the logic what
//! players did each tick ([`Senses`]) and applies what it asks for in
//! return ([`Effect`]): moving players, explosions, hazards switching. It
//! also moves the movers, since that needs the players' positions.

use std::collections::BTreeSet;

use glam::Vec3;
use rl_data::MapLogicConfig;
use serde::{Deserialize, Serialize};
use source_formats::entities::MapEntity;

use crate::collision::CollisionWorld;
use crate::rng::Rng;
use crate::simulation::PlayerId;

mod inputs;
pub mod io;
pub mod mover;
pub mod volume;

use io::{Connection, Event, Queue};
use mover::{Mover, MoverState, Phase};
use volume::{Filter, Trigger, TriggerKind};

/// `ambient_generic` spawnflags.
mod sound_flags {
    /// Heard everywhere, at full volume.
    pub(super) const EVERYWHERE: u32 = 1;
    /// Silent until told to play.
    pub(super) const START_SILENT: u32 = 16;
    /// Plays once rather than looping.
    pub(super) const NOT_LOOPED: u32 = 32;
}

/// `env_explosion` spawnflags.
mod explosion_flags {
    /// Hurts nobody.
    pub(super) const NO_DAMAGE: u32 = 1;
    /// Can go off more than once.
    pub(super) const REPEATABLE: u32 = 2;
}

/// What a player did to the map this tick, for the logic.
#[derive(Debug, Clone, Copy, Default)]
pub struct Senses<'a> {
    /// Every living player's box: `(player, mins, maxs)`.
    pub players: &'a [(PlayerId, Vec3, Vec3)],
    /// Movers players used: `(player, BSP model)`.
    pub uses: &'a [(PlayerId, usize)],
    /// Movers players touch: `(player, BSP model)`.
    pub touches: &'a [(PlayerId, usize)],
    /// Damage dealt to movers: `(attacker, BSP model, amount)`.
    pub damage: &'a [(Option<PlayerId>, usize, f32)],
}

/// Something the logic asks the simulation to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Effect {
    /// An explosion (`env_explosion`).
    Explosion {
        /// Its centre.
        position: Vec3,
        /// Damage at the centre; zero hurts nobody.
        damage: f32,
        /// Its reach, units.
        radius: f32,
    },
    /// Move a player to a destination, facing `yaw` (`trigger_teleport`).
    Teleport {
        /// Who.
        player: PlayerId,
        /// Where the feet go.
        origin: Vec3,
        /// The way to face, degrees.
        yaw: f32,
    },
    /// Give a player at least this speed along its direction
    /// (`trigger_push`).
    Push {
        /// Who.
        player: PlayerId,
        /// The push velocity, units/s.
        velocity: Vec3,
    },
    /// Switch hazard `index` of the simulation on or off.
    Hazard {
        /// The hazard.
        index: usize,
        /// On or off.
        enabled: bool,
    },
    /// A breakable broke.
    Broke {
        /// Its BSP model.
        model: usize,
    },
    /// A button was pressed.
    Pressed {
        /// Its BSP model.
        model: usize,
    },
}

/// A sound the map plays at a place (`ambient_generic`).
#[derive(Debug, Clone, PartialEq)]
pub struct AmbientSound {
    /// The sound: a soundscript entry or a file under `sound/`.
    pub sound: String,
    /// Where it plays, Source space.
    pub position: Vec3,
    /// Heard everywhere at full volume, rather than fading with distance.
    pub everywhere: bool,
    /// How far it carries, units.
    pub radius: f32,
    /// Whether it loops (else it plays once each time it starts).
    pub looped: bool,
    /// Whether, and how, it plays now.
    pub state: SoundState,
}

/// The replicated part of an ambient sound.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SoundState {
    /// The tick it last started, if it plays.
    pub started: Option<u64>,
    /// Volume, `0..=1`.
    pub volume: f32,
    /// Pitch, 1 for the sound's own.
    pub pitch: f32,
}

/// Everything of the logic that players see, as replicated to clients.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogicState {
    /// Every mover, in the order of [`MapLogic::movers`].
    pub movers: Vec<MoverState>,
    /// Every ambient sound, in the order of [`MapLogic::sounds`].
    pub sounds: Vec<SoundState>,
}

/// A `logic_timer`.
#[derive(Debug, Clone, PartialEq)]
struct Timer {
    enabled: bool,
    /// Seconds between firings.
    refire: f32,
    /// Random intervals between these bounds instead, seconds.
    random: Option<(f32, f32)>,
    /// The tick it fires next, once scheduled.
    next: Option<u64>,
}

/// A `math_counter`.
#[derive(Debug, Clone, PartialEq)]
struct Counter {
    enabled: bool,
    value: f32,
    min: f32,
    max: f32,
    /// At its maximum (or minimum) already: those outputs fire once per
    /// arrival.
    at_max: bool,
    at_min: bool,
}

/// What an entity is to the logic.
#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Auto,
    Relay {
        enabled: bool,
        once: bool,
    },
    Timer(Timer),
    Counter(Counter),
    /// A mover, by index.
    Mover(usize),
    /// A trigger, by index.
    Trigger(usize),
    /// A hazard of the simulation, by index.
    Hurt {
        index: usize,
        enabled: bool,
    },
    Explosion {
        position: Vec3,
        damage: f32,
        radius: f32,
        repeatable: bool,
        spent: bool,
    },
    /// An ambient sound, by index.
    Sound(usize),
    Filter(Filter),
    /// Anything else: inputs to it are dropped.
    Other,
}

/// One entity of the map, as the logic sees it.
#[derive(Debug, Clone, PartialEq)]
struct Node {
    classname: String,
    /// Its name, lower case.
    name: Option<String>,
    outputs: Vec<Connection>,
    kind: Kind,
}

/// The parts of the logic that change, kept whole so a new match can start
/// from the map's own state.
#[derive(Debug, Clone)]
struct Parts {
    nodes: Vec<Node>,
    movers: Vec<Mover>,
    triggers: Vec<Trigger>,
    /// Who is inside each trigger.
    touching: Vec<Vec<PlayerId>>,
    sounds: Vec<AmbientSound>,
}

/// A map's logic. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct MapLogic {
    parts: Parts,
    initial: Parts,
    /// Named point entities, where teleports send players: name, origin,
    /// yaw.
    destinations: Vec<(String, Vec3, f32)>,
    queue: Queue,
    config: MapLogicConfig,
    dt: f32,
    /// The tick being run.
    tick: u64,
    /// Whether the match's start has been announced (`logic_auto`).
    started: bool,
    /// Whether the match is a rerun (`OnMultiNewRound`).
    rerun: bool,
    /// Inputs and targets already reported as unsupported.
    reported: BTreeSet<String>,
}

impl MapLogic {
    /// The logic of a map's `entities` over its collision `world`, ticking
    /// every `dt` seconds. `hazards` gives, for each `trigger_hurt` the
    /// simulation made a hazard of, the entity's index and the hazard's.
    #[must_use]
    pub fn new(
        entities: &[MapEntity],
        world: &CollisionWorld,
        hazards: &[(usize, usize)],
        config: &MapLogicConfig,
        dt: f32,
    ) -> Self {
        let mut parts = Parts {
            nodes: Vec::new(),
            movers: Vec::new(),
            triggers: Vec::new(),
            touching: Vec::new(),
            sounds: Vec::new(),
        };
        let mut destinations = Vec::new();
        for (index, entity) in entities.iter().enumerate() {
            let name = entity
                .property("targetname")
                .filter(|name| !name.is_empty())
                .map(str::to_ascii_lowercase);
            if let Some(name) = &name {
                destinations.push((name.clone(), Vec3::from(entity.origin), entity.angles[1]));
            }
            let hazard = hazards
                .iter()
                .find(|&&(entity_index, _)| entity_index == index)
                .map(|&(_, hazard)| hazard);
            let kind = kind_of(entity, world, hazard, config, &mut parts);
            let outputs: Vec<Connection> = entity
                .properties
                .iter()
                .filter(|(key, _)| io::is_output(key))
                .filter_map(|(key, value)| Connection::parse(key, value))
                .collect();
            if kind == Kind::Other && name.is_none() && outputs.is_empty() {
                continue;
            }
            parts.nodes.push(Node {
                classname: entity.classname.clone(),
                name,
                outputs,
                kind,
            });
        }
        resolve_parents(&mut parts);
        tracing::info!(
            entities = parts.nodes.len(),
            movers = parts.movers.len(),
            triggers = parts.triggers.len(),
            sounds = parts.sounds.len(),
            "wired the map's logic"
        );
        Self {
            initial: parts.clone(),
            parts,
            destinations,
            queue: Queue::default(),
            config: config.clone(),
            dt,
            tick: 0,
            started: false,
            rerun: false,
            reported: BTreeSet::new(),
        }
    }

    /// Every mover, in a fixed order.
    #[must_use]
    pub fn movers(&self) -> &[Mover] {
        &self.parts.movers
    }

    /// Every ambient sound, in a fixed order.
    #[must_use]
    pub fn sounds(&self) -> &[AmbientSound] {
        &self.parts.sounds
    }

    /// The class of the entity whose BSP model is `model`, if it is a
    /// mover's.
    #[must_use]
    pub fn mover_class_name(&self, model: usize) -> Option<&str> {
        let index = self.mover_index(model)?;
        let node = self.node_of_mover(index)?;
        Some(self.parts.nodes[node].classname.as_str())
    }

    /// The mover of BSP model `model`, by index.
    #[must_use]
    pub fn mover_index(&self, model: usize) -> Option<usize> {
        self.parts
            .movers
            .iter()
            .position(|mover| mover.model == model)
    }

    /// Where mover `index` is with every mover at `position(i)`: its own
    /// placement carried by its parents'. A client interpolating positions
    /// passes its blended ones.
    #[must_use]
    pub fn mover_pose_with(
        &self,
        index: usize,
        position: &dyn Fn(usize) -> f32,
    ) -> Option<(Vec3, glam::Quat)> {
        let movers = &self.parts.movers;
        let mover = movers.get(index)?;
        let (mut origin, mut rotation) = mover.local_pose(position(index));
        let mut parent = mover.parent;
        // Parents form a forest; the depth bound stops a malformed cycle.
        for _ in 0..movers.len() {
            let Some(parent_index) = parent else {
                break;
            };
            let Some(parent_mover) = movers.get(parent_index) else {
                break;
            };
            let (rest_origin, rest_rotation) = parent_mover.rest_pose();
            let (now_origin, now_rotation) = parent_mover.local_pose(position(parent_index));
            let turn = now_rotation * rest_rotation.inverse();
            origin = now_origin + turn * (origin - rest_origin);
            rotation = turn * rotation;
            parent = parent_mover.parent;
        }
        Some((origin, rotation))
    }

    /// Where mover `index` is now.
    #[must_use]
    pub fn mover_pose(&self, index: usize) -> Option<(Vec3, glam::Quat)> {
        self.mover_pose_with(index, &|index| {
            self.parts
                .movers
                .get(index)
                .map_or(0.0, |mover| mover.position)
        })
    }

    /// What players see of the logic now.
    #[must_use]
    pub fn state(&self) -> LogicState {
        LogicState {
            movers: self.parts.movers.iter().map(Mover::state).collect(),
            sounds: self.parts.sounds.iter().map(|sound| sound.state).collect(),
        }
    }

    /// Takes on a replicated state (a client's replica). Entries beyond
    /// the map's are ignored.
    pub fn restore(&mut self, state: &LogicState) {
        for (mover, &replica) in self.parts.movers.iter_mut().zip(&state.movers) {
            mover.restore(replica);
        }
        for (sound, &replica) in self.parts.sounds.iter_mut().zip(&state.sounds) {
            sound.state = replica;
        }
    }

    /// Puts everything back as the map placed it, for a new match. The
    /// next step announces the match again. Returns the hazards' states to
    /// restore.
    pub fn reset(&mut self) -> Vec<Effect> {
        self.parts = self.initial.clone();
        self.queue.clear();
        self.started = false;
        self.rerun = true;
        self.parts
            .nodes
            .iter()
            .filter_map(|node| match node.kind {
                Kind::Hurt { index, enabled } => Some(Effect::Hazard { index, enabled }),
                _ => None,
            })
            .collect()
    }

    /// Runs tick `tick`: what players did, triggers, timers, movers' waits,
    /// and every event due. Pushes what the simulation must do onto
    /// `effects`. Movers are moved afterwards by the simulation
    /// ([`MapLogic::mover_mut`], [`MapLogic::fire_mover`]).
    pub fn step(&mut self, tick: u64, senses: &Senses, effects: &mut Vec<Effect>) {
        self.tick = tick;
        if !self.started {
            self.started = true;
            self.announce_start();
        }
        for &(player, model) in senses.uses {
            self.use_mover(model, player);
        }
        for &(player, model) in senses.touches {
            self.touch_mover(model, player);
        }
        for &(attacker, model, amount) in senses.damage {
            self.damage_mover(model, attacker, amount, effects);
        }
        self.run_triggers(senses.players, effects);
        self.run_timers();
        self.expire_movers();
        self.run_events(effects);
    }

    /// Mover `index`, to move it.
    pub fn mover_mut(&mut self, index: usize) -> Option<&mut Mover> {
        self.parts.movers.get_mut(index)
    }

    /// Fires `output` of mover `index`, as its last activator — what the
    /// simulation calls when a mover it moved arrived or was blocked.
    pub fn fire_mover(&mut self, index: usize, output: &str) {
        let Some(mover) = self.parts.movers.get(index) else {
            return;
        };
        let activator = mover.activator;
        if let Some(node) = self.node_of_mover(index) {
            self.fire(node, output, activator, None);
        }
    }

    /// Fires `logic_auto` outputs for the match starting.
    fn announce_start(&mut self) {
        let autos: Vec<usize> = self
            .parts
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.kind == Kind::Auto)
            .map(|(index, _)| index)
            .collect();
        let occasion = if self.rerun {
            "onmultinewround"
        } else {
            "onmultinewmap"
        };
        for node in autos {
            self.fire(node, "onmapspawn", None, None);
            self.fire(node, occasion, None, None);
        }
    }

    /// A player used the mover of `model`.
    fn use_mover(&mut self, model: usize, player: PlayerId) {
        let Some(index) = self.mover_index(model) else {
            return;
        };
        let tick = self.tick;
        let mover = &mut self.parts.movers[index];
        if !mover.used_by_players() || mover.gone() {
            return;
        }
        tracing::debug!(
            model,
            ?player,
            locked = mover.locked,
            "a player used a mover"
        );
        if mover.locked {
            let output = match mover.class {
                mover::MoverClass::Button => "onuselocked",
                _ => "onlockeduse",
            };
            self.fire_mover_as(index, output, Some(player));
            return;
        }
        let output = match (mover.class, mover.phase) {
            (mover::MoverClass::Door, Phase::Open { .. }) if mover.stays_open() => {
                mover.close(Some(player))
            }
            (_, Phase::Closed | Phase::Closing) => mover.open(Some(player), tick),
            _ => None,
        };
        if let Some(output) = output {
            self.fire_mover_as(index, output, Some(player));
        }
    }

    /// A player touches the mover of `model`.
    fn touch_mover(&mut self, model: usize, player: PlayerId) {
        let Some(index) = self.mover_index(model) else {
            return;
        };
        let tick = self.tick;
        let mover = &mut self.parts.movers[index];
        if !mover.touched_by_players() || mover.locked || mover.gone() {
            return;
        }
        if matches!(mover.phase, Phase::Closed | Phase::Closing)
            && let Some(output) = mover.open(Some(player), tick)
        {
            self.fire_mover_as(index, output, Some(player));
        }
    }

    /// `amount` damage reaches the mover of `model`.
    fn damage_mover(
        &mut self,
        model: usize,
        attacker: Option<PlayerId>,
        amount: f32,
        effects: &mut Vec<Effect>,
    ) {
        let Some(index) = self.mover_index(model) else {
            return;
        };
        let tick = self.tick;
        let mover = &mut self.parts.movers[index];
        if mover.gone() {
            return;
        }
        match mover.class {
            mover::MoverClass::Button => {
                let press =
                    mover.pressed_by_damage() && !mover.locked && mover.phase == Phase::Closed;
                let pressed = if press {
                    mover.open(attacker, tick)
                } else {
                    None
                };
                self.fire_mover_as(index, "ondamaged", attacker);
                if let Some(output) = pressed {
                    effects.push(Effect::Pressed { model });
                    self.fire_mover_as(index, output, attacker);
                }
            }
            mover::MoverClass::Breakable => {
                if mover.damage(amount) {
                    effects.push(Effect::Broke { model });
                    self.fire_mover_as(index, "onbreak", attacker);
                }
            }
            mover::MoverClass::Door | mover::MoverClass::Brush => {}
        }
    }

    /// Who is in each trigger, and what that sets off.
    fn run_triggers(&mut self, players: &[(PlayerId, Vec3, Vec3)], effects: &mut Vec<Effect>) {
        for index in 0..self.parts.triggers.len() {
            let trigger = &self.parts.triggers[index];
            // A spent trigger_once is gone, as Source removes it: nothing
            // it touched leaves it.
            if matches!(trigger.kind, TriggerKind::Multiple { spent: true, .. }) {
                self.parts.touching[index].clear();
                continue;
            }
            let inside: Vec<PlayerId> = if trigger.enabled && trigger.notices_players {
                players
                    .iter()
                    .filter(|(_, mins, maxs)| trigger.touches(*mins, *maxs))
                    .map(|&(player, _, _)| player)
                    .filter(|&player| self.passes(trigger.filter.as_deref(), player))
                    .collect()
            } else {
                Vec::new()
            };
            let before = std::mem::take(&mut self.parts.touching[index]);
            let Some(node) = self.node_of_trigger(index) else {
                continue;
            };
            let entered: Vec<PlayerId> = inside
                .iter()
                .filter(|p| !before.contains(p))
                .copied()
                .collect();
            let left: Vec<PlayerId> = before
                .iter()
                .filter(|p| !inside.contains(p))
                .copied()
                .collect();
            for (count, &player) in entered.iter().enumerate() {
                if before.is_empty() && count == 0 {
                    self.fire(node, "onstarttouchall", Some(player), None);
                }
                self.fire(node, "onstarttouch", Some(player), None);
            }
            for &player in &left {
                self.fire(node, "onendtouch", Some(player), None);
            }
            if inside.is_empty() && !before.is_empty() {
                self.fire(node, "onendtouchall", left.last().copied(), None);
            }
            self.act_on(index, node, &inside, &entered, effects);
            self.parts.touching[index] = inside;
        }
    }

    /// What trigger `index` does to the players `inside` (`entered` this
    /// tick).
    fn act_on(
        &mut self,
        index: usize,
        node: usize,
        inside: &[PlayerId],
        entered: &[PlayerId],
        effects: &mut Vec<Effect>,
    ) {
        let tick = self.tick;
        let dt = self.dt;
        let trigger = &mut self.parts.triggers[index];
        match &mut trigger.kind {
            TriggerKind::Multiple {
                wait,
                ready_at,
                spent,
            } => {
                let Some(&first) = inside.first() else {
                    return;
                };
                if *spent || tick < *ready_at {
                    return;
                }
                if let Some(wait) = *wait {
                    *ready_at = tick + seconds_to_ticks(wait, dt).max(1);
                } else {
                    *spent = true;
                    trigger.enabled = false;
                }
                self.fire(node, "ontrigger", Some(first), None);
            }
            TriggerKind::Teleport { target } => {
                let Some(&(_, origin, yaw)) =
                    self.destinations.iter().find(|(name, _, _)| name == target)
                else {
                    return;
                };
                for &player in inside {
                    effects.push(Effect::Teleport {
                        player,
                        origin,
                        yaw,
                    });
                }
            }
            TriggerKind::Push { velocity, once } => {
                let pushed = if *once { entered } else { inside };
                for &player in pushed {
                    effects.push(Effect::Push {
                        player,
                        velocity: *velocity,
                    });
                }
            }
        }
    }

    /// Whether `player` passes the filter named `filter` (no filter, or a
    /// name no entity has, passes everyone, as in Source).
    fn passes(&self, filter: Option<&str>, player: PlayerId) -> bool {
        self.passes_at_depth(filter, player, 0)
    }

    fn passes_at_depth(&self, filter: Option<&str>, player: PlayerId, depth: usize) -> bool {
        let Some(name) = filter else {
            return true;
        };
        let found = self.parts.nodes.iter().find_map(|node| match &node.kind {
            Kind::Filter(filter) if node.name.as_deref() == Some(name) => Some(filter),
            _ => None,
        });
        let Some(filter) = found else {
            return true;
        };
        // Players have no name and are of class "player".
        let _ = player;
        match filter {
            Filter::Name { name, negated } => name.is_empty() != *negated,
            Filter::Class { class, negated } => (class == "player") != *negated,
            Filter::Multi {
                filters,
                all,
                negated,
            } => {
                // Nested filters are bounded so a cycle cannot recurse for ever.
                if depth > 8 {
                    return false;
                }
                let mut results = filters
                    .iter()
                    .map(|name| self.passes_at_depth(Some(name), player, depth + 1));
                let combined = if *all {
                    results.all(|pass| pass)
                } else {
                    results.any(|pass| pass)
                };
                combined != *negated
            }
            Filter::Unknown => false,
        }
    }

    /// Fires the timers that are due, scheduling the next time.
    fn run_timers(&mut self) {
        let (tick, dt) = (self.tick, self.dt);
        for node in 0..self.parts.nodes.len() {
            let Kind::Timer(timer) = &mut self.parts.nodes[node].kind else {
                continue;
            };
            if !timer.enabled {
                continue;
            }
            let next = if let Some(next) = timer.next {
                next
            } else {
                let next = tick + interval(timer, tick, node, dt);
                timer.next = Some(next);
                next
            };
            if next > tick {
                continue;
            }
            timer.next = Some(tick + interval(timer, tick, node, dt));
            self.fire(node, "ontimer", None, None);
        }
    }

    /// Closes movers that have been open their wait.
    fn expire_movers(&mut self) {
        for index in 0..self.parts.movers.len() {
            if let Some(output) = self.parts.movers[index].expire(self.tick, self.dt) {
                self.fire_mover_as(index, output, None);
            }
        }
    }

    /// Delivers every event due, within the tick's budget.
    fn run_events(&mut self, effects: &mut Vec<Effect>) {
        let mut budget = self.config.max_events_per_tick;
        while let Some(event) = self.queue.pop_due(self.tick) {
            if budget == 0 {
                tracing::warn!(
                    queued = self.queue.len() + 1,
                    "map logic fired too many events in one tick; the rest wait"
                );
                self.queue.push(Event {
                    due: self.tick + 1,
                    ..event
                });
                break;
            }
            budget -= 1;
            self.deliver(&event, effects);
        }
    }

    /// Fires output `output` of node `node`: queues each of its live
    /// connections, passing `value` where a connection has no parameter.
    fn fire(
        &mut self,
        node: usize,
        output: &str,
        activator: Option<PlayerId>,
        value: Option<&str>,
    ) {
        let tick = self.tick;
        let dt = self.dt;
        let Some(entity) = self.parts.nodes.get_mut(node) else {
            return;
        };
        let mut events = Vec::new();
        for connection in entity.outputs.iter_mut().filter(|c| c.output == output) {
            match &mut connection.times {
                Some(0) => continue,
                Some(times) => *times -= 1,
                None => {}
            }
            let parameter = if connection.parameter.is_empty() {
                value.unwrap_or_default().to_owned()
            } else {
                connection.parameter.clone()
            };
            events.push(Event {
                due: tick + seconds_to_ticks(connection.delay, dt),
                target: connection.target.clone(),
                input: connection.input.clone(),
                parameter,
                activator,
                caller: node,
            });
        }
        for event in events {
            self.queue.push(event);
        }
    }

    fn fire_mover_as(&mut self, index: usize, output: &str, activator: Option<PlayerId>) {
        if let Some(node) = self.node_of_mover(index) {
            self.fire(node, output, activator, None);
        }
    }

    fn node_of_mover(&self, index: usize) -> Option<usize> {
        self.parts
            .nodes
            .iter()
            .position(|node| node.kind == Kind::Mover(index))
    }

    fn node_of_trigger(&self, index: usize) -> Option<usize> {
        self.parts
            .nodes
            .iter()
            .position(|node| node.kind == Kind::Trigger(index))
    }

    /// Logs `what` once per map.
    fn report_once(&mut self, what: String) {
        if self.reported.insert(what.clone()) {
            tracing::debug!("map logic: {what}");
        }
    }
}

/// What `entity` is to the logic, adding its mover, trigger or sound to
/// `parts`.
fn kind_of(
    entity: &MapEntity,
    world: &CollisionWorld,
    hazard: Option<usize>,
    config: &MapLogicConfig,
    parts: &mut Parts,
) -> Kind {
    let flags = entity.number("spawnflags").map_or(0, |flags| flags as u32);
    let enabled = entity.property("StartDisabled") != Some("1");
    let model = entity.brush_model();
    if let Some(index) = hazard {
        return Kind::Hurt { index, enabled };
    }
    if let Some(model) = model.filter(|&model| world.is_mover(model))
        && let Some((mins, maxs)) = world.model_bounds(model, Vec3::ZERO, [0.0; 3])
        && let Some(mover) = Mover::from_entity(entity, model, maxs - mins)
    {
        parts.movers.push(mover);
        return Kind::Mover(parts.movers.len() - 1);
    }
    if let Some(model) = model
        && let Some((mins, maxs)) =
            world.model_bounds(model, Vec3::from(entity.origin), entity.angles)
        && let Some(trigger) = Trigger::from_entity(entity, mins, maxs)
    {
        parts.triggers.push(trigger);
        parts.touching.push(Vec::new());
        return Kind::Trigger(parts.triggers.len() - 1);
    }
    if let Some(filter) = Filter::from_entity(entity) {
        return Kind::Filter(filter);
    }
    match entity.classname.as_str() {
        "logic_auto" => Kind::Auto,
        "logic_relay" => Kind::Relay {
            enabled,
            once: flags & 1 != 0,
        },
        "logic_timer" => Kind::Timer(Timer {
            enabled,
            refire: entity.number("RefireTime").unwrap_or(0.0).max(0.0),
            random: (entity.property("UseRandomTime") == Some("1")).then(|| {
                let lower = entity.number("LowerRandomBound").unwrap_or(0.0).max(0.0);
                let upper = entity
                    .number("UpperRandomBound")
                    .unwrap_or(lower)
                    .max(lower);
                (lower, upper)
            }),
            next: None,
        }),
        "math_counter" => {
            let value = entity.number("startvalue").unwrap_or(0.0);
            Kind::Counter(Counter {
                enabled,
                value,
                min: entity.number("min").unwrap_or(0.0),
                max: entity.number("max").unwrap_or(0.0),
                at_max: false,
                at_min: false,
            })
        }
        "env_explosion" => {
            let magnitude = entity.number("iMagnitude").unwrap_or(100.0).max(0.0);
            let radius = entity
                .number("iRadiusOverride")
                .filter(|&radius| radius > 0.0)
                .unwrap_or(magnitude * config.explosion.radius_per_damage);
            Kind::Explosion {
                position: Vec3::from(entity.origin),
                damage: if flags & explosion_flags::NO_DAMAGE != 0 {
                    0.0
                } else {
                    magnitude
                },
                radius,
                repeatable: flags & explosion_flags::REPEATABLE != 0,
                spent: false,
            }
        }
        "ambient_generic" => match entity.property("message").filter(|sound| !sound.is_empty()) {
            Some(sound) => {
                parts.sounds.push(AmbientSound {
                    sound: sound.to_owned(),
                    position: Vec3::from(entity.origin),
                    everywhere: flags & sound_flags::EVERYWHERE != 0,
                    radius: entity.number("radius").unwrap_or(1250.0).max(0.0),
                    looped: flags & sound_flags::NOT_LOOPED == 0,
                    state: SoundState {
                        started: (flags & sound_flags::START_SILENT == 0).then_some(0),
                        volume: (entity.number("health").unwrap_or(10.0) / 10.0).clamp(0.0, 1.0),
                        pitch: (entity.number("pitch").unwrap_or(100.0) / 100.0).clamp(0.01, 2.55),
                    },
                });
                Kind::Sound(parts.sounds.len() - 1)
            }
            None => Kind::Other,
        },
        _ => Kind::Other,
    }
}

/// Points each parented mover at its parent mover; parents that are not
/// movers are let go (their children stay put).
fn resolve_parents(parts: &mut Parts) {
    for index in 0..parts.movers.len() {
        let Some(parent_name) = parts.movers[index].parent_name.clone() else {
            continue;
        };
        parts.movers[index].parent = parts.nodes.iter().find_map(|node| match node.kind {
            Kind::Mover(parent)
                if parent != index && node.name.as_deref() == Some(&parent_name) =>
            {
                Some(parent)
            }
            _ => None,
        });
    }
}

/// A delay in seconds as whole ticks of `dt`, rounded.
fn seconds_to_ticks(seconds: f32, dt: f32) -> u64 {
    if dt <= 0.0 {
        return 0;
    }
    (seconds / dt).round().max(0.0) as u64
}

/// A timer's next interval in ticks (at least one), drawn from its random
/// bounds if it has them — seeded by the tick and the timer, so the server
/// draws the same intervals every run.
fn interval(timer: &Timer, tick: u64, node: usize, dt: f32) -> u64 {
    let seconds = match timer.random {
        Some((lower, upper)) => {
            let mut rng = Rng::seeded(&[tick, node as u64, 0x7469_6d65]);
            lower + (upper - lower) * rng.unit()
        }
        None => timer.refire,
    };
    seconds_to_ticks(seconds, dt).max(1)
}

#[cfg(test)]
mod tests;
