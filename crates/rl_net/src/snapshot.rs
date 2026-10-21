//! The world as one client sees it, and the snapshots that carry it.
//!
//! After each tick the server captures a [`WorldState`] for each client:
//! every player's motion and life, the client's own player's health,
//! weapons and Manipulator too ([`Private`]), each moving prop's pose, which
//! pickups are there, the projectiles in flight, the match, and what the
//! map's logic shows: where its doors and buttons are, which brushes are
//! there, which of its sounds play. Other
//! players' private state is not sent: no client needs it, and none should
//! see it.
//!
//! # Deltas
//!
//! A [`Snapshot`] carries a state as the change from a *baseline*: an
//! earlier state the client said it has (its `ack`). Only players, props
//! and pickups that changed are sent, and only the parts of a player that
//! changed; resting props cost nothing. With no baseline (the first
//! snapshot, or after the client fell too far behind), everything is sent,
//! measured against the level as loaded. Both ends keep recent states, so
//! the client rebuilds the full state from its copy of the baseline
//! ([`WorldState::apply`]) and keeps the result as a baseline for later.
//!
//! The props a state lists are those that can move: their body ids are the
//! same on every machine (see `rl_level`), so the client knows which prop
//! each pose belongs to.

use std::collections::{BTreeMap, BTreeSet};

use rl_sim::combat::Vitals;
use rl_sim::logic::mover::MoverState;
use rl_sim::logic::{LogicState, SoundState};
use rl_sim::movement::PlayerState;
use rl_sim::physics::{BodyId, BodyPose};
use rl_sim::rules::Match;
use rl_sim::simulation::{GrenadeView, Life, Player, PlayerId, Simulation};
use rl_sim::tools::manipulator::Manipulator;
use rl_sim::weapons::projectile::Projectile;
use rl_sim::weapons::{Inventory, WeaponId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::events::Event;

/// Where a player is and looks, for everyone.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Motion {
    /// Movement state.
    pub state: PlayerState,
    /// View `(pitch, yaw)`, degrees.
    pub look: (f32, f32),
    /// The weapon in hand.
    pub weapon: Option<WeaponId>,
}

/// What only the player themselves is told: enough to predict their own
/// movement and weapon, and fill their HUD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Private {
    /// Health and armor.
    pub vitals: Vitals,
    /// Weapons and ammo.
    pub inventory: Inventory,
    /// The Manipulator, and what it holds.
    pub manipulator: Manipulator,
}

/// One player, as one client sees them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerView {
    /// Where they are.
    pub motion: Motion,
    /// Alive or dead.
    pub life: Life,
    /// Their private state: only for the client's own player.
    pub private: Option<Private>,
}

impl PlayerView {
    /// How `viewer` sees `player`.
    fn of(id: PlayerId, player: &Player, viewer: Option<PlayerId>) -> Self {
        Self {
            motion: Motion {
                state: player.state,
                look: player.look,
                weapon: player.inventory.active(),
            },
            life: player.life,
            private: (Some(id) == viewer).then(|| Private {
                vitals: player.vitals,
                inventory: player.inventory.clone(),
                manipulator: player.manipulator.clone(),
            }),
        }
    }
}

/// The world after a tick, as one client sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldState {
    /// The tick it follows.
    pub tick: u64,
    /// Every player.
    pub players: BTreeMap<PlayerId, PlayerView>,
    /// The pose of every prop that can move.
    pub props: BTreeMap<BodyId, BodyPose>,
    /// Every prop that has broken.
    pub broken: BTreeSet<BodyId>,
    /// Whether each pickup is there to take.
    pub pickups: Vec<bool>,
    /// Projectiles in flight.
    pub projectiles: Vec<Projectile>,
    /// Grenades in flight.
    pub grenades: Vec<GrenadeView>,
    /// Scores and clock.
    pub game: Match,
    /// What the map's logic shows.
    pub logic: LogicState,
}

/// How a player changed since the baseline; parts that did not are `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerDelta {
    /// New motion.
    pub motion: Option<Motion>,
    /// New life.
    pub life: Option<Life>,
    /// New private state.
    pub private: Option<Private>,
}

/// A world state sent as the change from a baseline, with the events since
/// the client's last snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The tick it follows.
    pub tick: u64,
    /// The tick of the state it changes, or `None` for a full state.
    pub baseline: Option<u64>,
    /// The number of the client's latest command the server has run.
    pub ack: u32,
    /// How many of the client's commands wait to be run: the client keeps
    /// this small by easing its tick rate.
    pub queued: u16,
    /// Players who are new or changed.
    pub players: Vec<(PlayerId, PlayerDelta)>,
    /// Players who left.
    pub gone: Vec<PlayerId>,
    /// Props that moved.
    pub props: Vec<(BodyId, BodyPose)>,
    /// Every prop that is broken, if that changed since the baseline (props
    /// that respawn leave the set).
    pub broken: Option<Vec<BodyId>>,
    /// Pickups that were taken or came back: `(index, there)`.
    pub pickups: Vec<(u16, bool)>,
    /// The projectiles in flight, if they changed.
    pub projectiles: Option<Vec<Projectile>>,
    /// The grenades in flight, if they changed.
    pub grenades: Option<Vec<GrenadeView>>,
    /// The match, if it changed.
    pub game: Option<Match>,
    /// Movers that changed: `(index, state)`.
    pub movers: Vec<(u16, MoverState)>,
    /// Map sounds that changed: `(index, state)`.
    pub sounds: Vec<(u16, SoundState)>,
    /// What happened since the client's last snapshot.
    pub events: Vec<Event>,
}

/// A snapshot that does not fit the baseline given for it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SnapshotError {
    /// The snapshot changes a state the client does not have.
    #[error("snapshot of tick {tick} needs baseline {baseline}, which is not kept")]
    MissingBaseline {
        /// The snapshot's tick.
        tick: u64,
        /// The baseline it needs.
        baseline: u64,
    },
    /// A player appears without the state a newcomer needs.
    #[error("snapshot of tick {tick} brings {player:?} in without their motion or life")]
    IncompletePlayer {
        /// The snapshot's tick.
        tick: u64,
        /// The player.
        player: PlayerId,
    },
}

impl WorldState {
    /// The world `sim` has after its latest tick, as `viewer` sees it (no
    /// viewer: an onlooker, told nothing private).
    #[must_use]
    pub fn capture(sim: &Simulation, viewer: Option<PlayerId>) -> Self {
        let physics = sim.physics();
        Self {
            tick: sim.tick_count(),
            players: sim
                .players()
                .map(|(id, player)| (id, PlayerView::of(id, player, viewer)))
                .collect(),
            props: physics
                .props()
                .filter(|&body| physics.is_dynamic(body))
                .filter_map(|body| Some((body, physics.pose(body)?)))
                .collect(),
            broken: physics.broken().collect(),
            pickups: sim
                .pickups()
                .iter()
                .map(rl_sim::pickups::Pickup::available)
                .collect(),
            projectiles: sim.projectiles().to_vec(),
            grenades: sim.grenades().to_vec(),
            game: sim.game().clone(),
            logic: sim.logic_state(),
        }
    }

    /// This state as the change from `baseline` — or, with none, as
    /// everything but the props still where the level placed them
    /// (`initial`) — for a client whose latest run command is `ack`, with
    /// `events`.
    #[must_use]
    pub fn diff(
        &self,
        baseline: Option<&Self>,
        initial: &BTreeMap<BodyId, BodyPose>,
        ack: u32,
        queued: u16,
        events: Vec<Event>,
    ) -> Snapshot {
        let players = self
            .players
            .iter()
            .filter_map(|(&id, view)| {
                let old = baseline.and_then(|base| base.players.get(&id));
                let delta = PlayerDelta {
                    motion: (old.map(|old| old.motion) != Some(view.motion)).then_some(view.motion),
                    life: (old.map(|old| old.life) != Some(view.life)).then_some(view.life),
                    private: view
                        .private
                        .as_ref()
                        .filter(|private| {
                            old.and_then(|old| old.private.as_ref()) != Some(*private)
                        })
                        .cloned(),
                };
                let unchanged =
                    delta.motion.is_none() && delta.life.is_none() && delta.private.is_none();
                (!unchanged).then_some((id, delta))
            })
            .collect();
        let gone = baseline
            .map(|base| {
                base.players
                    .keys()
                    .filter(|id| !self.players.contains_key(id))
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        let base_props = baseline.map_or(initial, |base| &base.props);
        let props = self
            .props
            .iter()
            .filter(|&(body, pose)| base_props.get(body) != Some(pose))
            .map(|(&body, &pose)| (body, pose))
            .collect();
        let broken = baseline
            .is_none_or(|base| base.broken != self.broken)
            .then(|| self.broken.iter().copied().collect());
        let pickups = self
            .pickups
            .iter()
            .enumerate()
            .filter(|&(index, there)| {
                baseline.and_then(|base| base.pickups.get(index)) != Some(there)
            })
            .map(|(index, &there)| (u16::try_from(index).unwrap_or(u16::MAX), there))
            .collect();
        Snapshot {
            tick: self.tick,
            baseline: baseline.map(|base| base.tick),
            ack,
            queued,
            players,
            gone,
            props,
            broken,
            pickups,
            projectiles: baseline
                .is_none_or(|base| base.projectiles != self.projectiles)
                .then(|| self.projectiles.clone()),
            grenades: baseline
                .is_none_or(|base| base.grenades != self.grenades)
                .then(|| self.grenades.clone()),
            game: baseline
                .is_none_or(|base| base.game != self.game)
                .then(|| self.game.clone()),
            movers: changed(
                &self.logic.movers,
                baseline.map(|base| &base.logic.movers[..]),
            ),
            sounds: changed(
                &self.logic.sounds,
                baseline.map(|base| &base.logic.sounds[..]),
            ),
            events,
        }
    }

    /// The state `snapshot` describes, rebuilt from `baseline` — which
    /// must be the state of the snapshot's baseline tick, or `None` for a
    /// full snapshot. A full snapshot's props are measured against
    /// `initial`, the poses the level loaded with.
    ///
    /// # Errors
    ///
    /// Fails if the baseline is not the one the snapshot needs, or a new
    /// player comes without their motion or life.
    pub fn apply(
        baseline: Option<&Self>,
        snapshot: &Snapshot,
        initial: &BTreeMap<BodyId, BodyPose>,
    ) -> Result<Self, SnapshotError> {
        let base = match (snapshot.baseline, baseline) {
            (None, _) => None,
            (Some(tick), Some(base)) if base.tick == tick => Some(base),
            (Some(tick), _) => {
                return Err(SnapshotError::MissingBaseline {
                    tick: snapshot.tick,
                    baseline: tick,
                });
            }
        };
        let mut players = base.map(|base| base.players.clone()).unwrap_or_default();
        for id in &snapshot.gone {
            players.remove(id);
        }
        for (id, delta) in &snapshot.players {
            let view = if let Some(old) = players.get(id) {
                PlayerView {
                    motion: delta.motion.unwrap_or(old.motion),
                    life: delta.life.unwrap_or(old.life),
                    private: delta.private.clone().or_else(|| old.private.clone()),
                }
            } else {
                let (Some(motion), Some(life)) = (delta.motion, delta.life) else {
                    return Err(SnapshotError::IncompletePlayer {
                        tick: snapshot.tick,
                        player: *id,
                    });
                };
                PlayerView {
                    motion,
                    life,
                    private: delta.private.clone(),
                }
            };
            players.insert(*id, view);
        }
        let mut props = base.map_or_else(|| initial.clone(), |base| base.props.clone());
        props.extend(snapshot.props.iter().copied());
        let broken: BTreeSet<BodyId> = match &snapshot.broken {
            Some(all) => all.iter().copied().collect(),
            None => base.map(|base| base.broken.clone()).unwrap_or_default(),
        };
        // A broken prop is gone from the world, so from its poses.
        props.retain(|body, _| !broken.contains(body));
        let mut pickups = base.map(|base| base.pickups.clone()).unwrap_or_default();
        for &(index, there) in &snapshot.pickups {
            let index = usize::from(index);
            if pickups.len() <= index {
                pickups.resize(index + 1, true);
            }
            pickups[index] = there;
        }
        Ok(Self {
            tick: snapshot.tick,
            players,
            props,
            broken,
            pickups,
            projectiles: snapshot
                .projectiles
                .clone()
                .or_else(|| base.map(|base| base.projectiles.clone()))
                .unwrap_or_default(),
            grenades: snapshot
                .grenades
                .clone()
                .or_else(|| base.map(|base| base.grenades.clone()))
                .unwrap_or_default(),
            game: snapshot
                .game
                .clone()
                .or_else(|| base.map(|base| base.game.clone()))
                .unwrap_or_default(),
            logic: LogicState {
                movers: patched(base.map(|base| &base.logic.movers[..]), &snapshot.movers),
                sounds: patched(base.map(|base| &base.logic.sounds[..]), &snapshot.sounds),
            },
        })
    }

    /// Writes this state into a client's replica: the tick count, players
    /// (those gone leave), props, pickups, projectiles and the match.
    /// Players' private state is written only where the state has it.
    pub fn restore(&self, sim: &mut Simulation) {
        sim.set_tick_count(self.tick);
        let gone: Vec<PlayerId> = sim
            .players()
            .map(|(id, _)| id)
            .filter(|id| !self.players.contains_key(id))
            .collect();
        for id in gone {
            sim.leave(id);
        }
        for (&id, view) in &self.players {
            let player = sim.replica_player(id);
            player.state = view.motion.state;
            player.look = view.motion.look;
            player.life = view.life;
            if let Some(private) = &view.private {
                player.vitals = private.vitals;
                player.inventory = private.inventory.clone();
                player.manipulator = private.manipulator.clone();
            }
        }
        for (pickup, &there) in sim.pickups_mut().iter_mut().zip(&self.pickups) {
            pickup.away = if there {
                0.0
            } else {
                pickup.respawn.max(f32::MIN_POSITIVE)
            };
        }
        sim.set_projectiles(self.projectiles.clone());
        sim.set_grenades(self.grenades.clone());
        sim.set_match(self.game.clone());
        if sim.logic_state() != self.logic {
            sim.set_logic_state(&self.logic);
        }
        sim.set_broken_props(&self.broken);
        // Only props that moved are placed: placing one makes collision
        // look at it again.
        let moved: Vec<(BodyId, BodyPose)> = self
            .props
            .iter()
            .filter(|&(&body, pose)| sim.physics().pose(body).as_ref() != Some(pose))
            .map(|(&body, &pose)| (body, pose))
            .collect();
        if !moved.is_empty() {
            sim.set_prop_poses(moved);
        }
    }
}

/// The entries of `now` that differ from `base` (all of them without a
/// base), by index.
fn changed<T: Copy + PartialEq>(now: &[T], base: Option<&[T]>) -> Vec<(u16, T)> {
    now.iter()
        .enumerate()
        .filter(|&(index, entry)| base.and_then(|base| base.get(index)) != Some(entry))
        .map(|(index, &entry)| (u16::try_from(index).unwrap_or(u16::MAX), entry))
        .collect()
}

/// `base` (or nothing) with the `changes` written in. Entries a change
/// skips past are left out: a well-formed snapshot never does that, and
/// the replica ignores what it cannot place.
fn patched<T: Copy>(base: Option<&[T]>, changes: &[(u16, T)]) -> Vec<T> {
    let mut entries = base.map(<[T]>::to_vec).unwrap_or_default();
    for &(index, entry) in changes {
        let index = usize::from(index);
        if index < entries.len() {
            entries[index] = entry;
        } else if index == entries.len() {
            entries.push(entry);
        }
    }
    entries
}

/// The pose of every prop of `sim` that can move: what a full snapshot's
/// props are measured against.
#[must_use]
pub fn initial_props(sim: &Simulation) -> BTreeMap<BodyId, BodyPose> {
    let physics = sim.physics();
    physics
        .props()
        .filter(|&body| physics.is_dynamic(body))
        .filter_map(|body| Some((body, physics.pose(body)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::{Quat, Vec3};
    use rl_sim::collision::{ConvexHull, WorldBuilder};
    use rl_sim::movement::UserCmd;
    use rl_sim::physics::{Motion as BodyMotion, PropDesc};

    use super::*;
    use crate::protocol;

    /// A floor with a resting crate, a falling crate and two players.
    fn sim() -> (Simulation, PlayerId, PlayerId) {
        let data = rl_data::GameData::builtin().expect("builtin");
        let world = WorldBuilder::new()
            .solid_box(
                Vec3::new(-2048.0, -2048.0, -64.0),
                Vec3::new(2048.0, 2048.0, 0.0),
            )
            .build();
        let mut sim = Simulation::new(Arc::new(world), &data);
        for (x, z, asleep) in [(300.0, 0.0, true), (-300.0, 200.0, false)] {
            sim.add_prop(&PropDesc {
                pieces: vec![ConvexHull::from_box(Vec3::splat(-8.0), Vec3::splat(8.0))],
                origin: Vec3::new(x, 0.0, z + 8.0),
                rotation: Quat::IDENTITY,
                motion: BodyMotion::Dynamic,
                mass: 10.0,
                material: *data.physics.materials.get("wood"),
                linear_damping: 0.0,
                angular_damping: 0.0,
                debris: false,
                start_asleep: asleep,
                grabbable: true,
                free_on_grab: false,
                breakable: None,
                surface: String::new(),
            });
        }
        let a = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
        let b = sim.spawn_player(Vec3::new(100.0, 0.0, 1.0));
        (sim, a, b)
    }

    fn walk(sim: &mut Simulation, player: PlayerId, ticks: usize) {
        let cmd = UserCmd {
            forward: 1.0,
            ..UserCmd::default()
        };
        for _ in 0..ticks {
            sim.tick(&[(player, cmd)]);
        }
    }

    #[test]
    fn broken_props_reach_the_replica_and_leave_its_world() {
        use rl_sim::physics::Breakable;

        let data = rl_data::GameData::builtin().expect("builtin");
        let world = WorldBuilder::new()
            .solid_box(
                Vec3::new(-2048.0, -2048.0, -64.0),
                Vec3::new(2048.0, 2048.0, 0.0),
            )
            .build();
        let crate_desc = PropDesc {
            pieces: vec![ConvexHull::from_box(Vec3::splat(-8.0), Vec3::splat(8.0))],
            origin: Vec3::new(300.0, 0.0, 8.0),
            rotation: Quat::IDENTITY,
            motion: BodyMotion::Dynamic,
            mass: 10.0,
            material: *data.physics.materials.get("wood"),
            linear_damping: 0.0,
            angular_damping: 0.0,
            debris: false,
            start_asleep: true,
            grabbable: true,
            free_on_grab: false,
            breakable: Some(Breakable {
                health: 10.0,
                bullets: 1.0,
                club: 1.0,
                explosive: 1.0,
                explosion: None,
                gibs: Vec::new(),
                gib_count: 0,
                respawn: None,
            }),
            surface: String::new(),
        };
        let mut server = Simulation::new(Arc::new(world.clone()), &data);
        let body = server.add_prop(&crate_desc);
        let mut replica = Simulation::new(Arc::new(world), &data);
        replica.add_prop(&crate_desc);
        let initial = initial_props(&server);

        let before = WorldState::capture(&server, None);
        let events = server.damage_prop(body, 50.0, None);
        assert_eq!(events.prop_breaks.len(), 1);
        let after = WorldState::capture(&server, None);
        assert!(after.broken.contains(&body));

        let delta = after.diff(Some(&before), &initial, 0, 0, Vec::new());
        assert_eq!(delta.broken, Some(vec![body]));
        let rebuilt = WorldState::apply(Some(&before), &delta, &initial).expect("delta");
        assert_eq!(rebuilt, after);
        // A client that missed the snapshot still learns of it from a full one.
        let full = after.diff(None, &initial, 0, 0, Vec::new());
        assert_eq!(full.broken, Some(vec![body]));

        rebuilt.restore(&mut replica);
        assert!(replica.physics().is_broken(body));
        assert!(replica.physics().props().next().is_none());
        // And it is not told again.
        let again = rebuilt.diff(Some(&after), &initial, 0, 0, Vec::new());
        assert_eq!(again.broken, None);
    }

    #[test]
    fn only_the_viewer_sees_their_private_state() {
        let (sim, a, b) = sim();
        let state = WorldState::capture(&sim, Some(a));
        assert!(state.players[&a].private.is_some());
        assert!(state.players[&b].private.is_none());
        let onlooker = WorldState::capture(&sim, None);
        assert!(onlooker.players.values().all(|view| view.private.is_none()));
    }

    #[test]
    fn deltas_rebuild_the_state_exactly() {
        let (mut sim, a, b) = sim();
        let initial = initial_props(&sim);
        // The falling crate lands and both crates fall asleep.
        for _ in 0..400 {
            sim.tick(&[]);
        }
        let first = WorldState::capture(&sim, Some(a));
        let full = first.diff(None, &initial, 0, 0, Vec::new());
        let rebuilt = WorldState::apply(None, &full, &initial).expect("full");
        assert_eq!(rebuilt, first);

        walk(&mut sim, b, 10);
        let second = WorldState::capture(&sim, Some(a));
        let delta = second.diff(Some(&first), &initial, 3, 1, Vec::new());
        // Only the walker changed: resting crates are not sent.
        assert_eq!(delta.players.len(), 1, "{:?}", delta.players);
        assert_eq!(delta.players[0].0, b);
        assert!(delta.players[0].1.private.is_none());
        assert!(delta.props.is_empty(), "{:?}", delta.props);
        assert!(
            !full.props.is_empty(),
            "the crates moved since the level loaded"
        );
        let decoded: Snapshot = protocol::decode(&protocol::encode(&delta)).expect("decode");
        let rebuilt = WorldState::apply(Some(&first), &decoded, &initial).expect("delta");
        assert_eq!(rebuilt, second);
        assert!(protocol::encode(&delta).len() < protocol::encode(&full).len());
    }

    #[test]
    fn full_snapshots_skip_props_the_level_placed() {
        let (sim, a, _) = sim();
        let initial = initial_props(&sim);
        let full = WorldState::capture(&sim, Some(a)).diff(None, &initial, 0, 0, Vec::new());
        assert!(full.props.is_empty(), "{:?}", full.props);
    }

    #[test]
    fn players_who_leave_are_gone_from_the_rebuilt_state() {
        let (mut sim, a, b) = sim();
        let initial = initial_props(&sim);
        let before = WorldState::capture(&sim, Some(a));
        sim.leave(b);
        sim.tick(&[]);
        let after = WorldState::capture(&sim, Some(a));
        let delta = after.diff(Some(&before), &initial, 0, 0, Vec::new());
        assert_eq!(delta.gone, [b]);
        let rebuilt = WorldState::apply(Some(&before), &delta, &initial).expect("delta");
        assert!(!rebuilt.players.contains_key(&b));
    }

    #[test]
    fn a_snapshot_needs_its_own_baseline() {
        let (mut sim, a, _) = sim();
        let initial = initial_props(&sim);
        let first = WorldState::capture(&sim, Some(a));
        walk(&mut sim, a, 2);
        let second = WorldState::capture(&sim, Some(a));
        walk(&mut sim, a, 2);
        let third = WorldState::capture(&sim, Some(a));
        let delta = third.diff(Some(&second), &initial, 0, 0, Vec::new());
        assert!(matches!(
            WorldState::apply(Some(&first), &delta, &initial),
            Err(SnapshotError::MissingBaseline { .. })
        ));
        assert!(WorldState::apply(None, &delta, &initial).is_err());
    }

    /// A floor, a slow door opening from the start, a looping sound and a
    /// player.
    fn door_sim() -> (Simulation, PlayerId) {
        let data = rl_data::GameData::builtin().expect("builtin");
        let world = WorldBuilder::new()
            .solid_box(
                Vec3::new(-2048.0, -2048.0, -64.0),
                Vec3::new(2048.0, 2048.0, 0.0),
            )
            .brush_entity(
                "func_door",
                Some("door"),
                Vec3::new(200.0, 0.0, 0.0),
                Vec3::new(-8.0, -64.0, 0.0),
                Vec3::new(8.0, 64.0, 128.0),
            )
            .build();
        let mut sim = Simulation::new(Arc::new(world), &data);
        let entity = |classname, keys: &[(&str, &str)]| {
            keys.iter().fold(
                source_formats::entities::MapEntity::new(classname, [200.0, 0.0, 0.0]),
                |entity, (key, value)| entity.with(key, value),
            )
        };
        sim.add_entities(&[
            entity(
                "func_door",
                &[
                    ("model", "*1"),
                    ("targetname", "door"),
                    ("movedir", "-90 0 0"),
                    ("speed", "20"),
                    ("wait", "-1"),
                ],
            ),
            entity("ambient_generic", &[("message", "Hum.Loop")]),
            entity("logic_auto", &[("OnMapSpawn", "door,Open,,0,-1")]),
        ]);
        let player = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
        (sim, player)
    }

    #[test]
    fn deltas_carry_only_the_movers_that_moved() {
        let (mut sim, a) = door_sim();
        let initial = initial_props(&sim);
        sim.tick(&[]);
        let first = WorldState::capture(&sim, Some(a));
        let full = first.diff(None, &initial, 0, 0, Vec::new());
        assert_eq!((full.movers.len(), full.sounds.len()), (1, 1));
        assert_eq!(
            WorldState::apply(None, &full, &initial).expect("full"),
            first
        );

        sim.tick(&[]);
        let second = WorldState::capture(&sim, Some(a));
        let delta = second.diff(Some(&first), &initial, 0, 0, Vec::new());
        assert_eq!(delta.movers.len(), 1, "the door moved on");
        assert!(delta.sounds.is_empty(), "the hum plays on unchanged");
        let rebuilt = WorldState::apply(Some(&first), &delta, &initial).expect("delta");
        assert_eq!(rebuilt, second);

        let (mut client, _) = door_sim();
        second.restore(&mut client);
        assert_eq!(client.logic_state(), sim.logic_state());
        assert_eq!(client.logic().mover_pose(0), sim.logic().mover_pose(0));
    }

    #[test]
    fn restoring_a_state_makes_the_replica_match() {
        let (mut server, a, b) = sim();
        walk(&mut server, a, 20);
        walk(&mut server, b, 5);
        server.leave(b);
        let state = WorldState::capture(&server, Some(a));
        let (mut client, ..) = sim();
        state.restore(&mut client);
        assert_eq!(client.tick_count(), server.tick_count());
        assert!(client.player(b).is_none());
        let (theirs, ours) = (server.player(a).expect("a"), client.player(a).expect("a"));
        assert_eq!(ours.state, theirs.state);
        assert_eq!(ours.inventory, theirs.inventory);
        assert_eq!(ours.vitals, theirs.vitals);
        assert_eq!(WorldState::capture(&client, Some(a)), state);
    }
}
