//! One map's whole simulation, a tick at a time.
//!
//! [`Simulation`] owns the map's collision, its physics world and its
//! players, and [`Simulation::tick`] advances everything by one fixed tick
//! in a set order:
//!
//! 1. **Move** each player with a command for the tick, against the map and
//!    its props (the prop a player holds excepted).
//! 2. **Push** the props each player ran into.
//! 3. **Tools**: run each player's Manipulator from where they now stand.
//! 4. **Physics**: move the players' boxes to their new positions and step
//!    the physics world, collecting the impacts of props on players.
//!
//! This is the loop the server runs authoritatively. Clients run only step 1
//! for their own player, to predict it (M6).

use std::sync::Arc;

use glam::Vec3;
use rl_data::{GameData, ManipulatorConfig, MovementConfig};

use crate::collision::{CollisionWorld, find_free_position};
use crate::movement::{MoveEvents, PlayerState, UserCmd, simulate};
use crate::physics::{BodyId, Impact, PhysicsWorld, PropDesc};
use crate::scene::Scene;
use crate::tools::manipulator::{Aim, Intervals, Manipulator, ToolEvent};

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

/// One player's simulated state.
#[derive(Debug, Clone)]
pub struct Player {
    /// Movement state.
    pub state: PlayerState,
    /// The player's box in the physics world.
    body: BodyId,
    /// The player's Manipulator.
    pub manipulator: Manipulator,
}

/// What happened during a tick, for sounds, effects, damage and the HUD.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TickEvents {
    /// Each moved player's movement events.
    pub moves: Vec<(PlayerId, MoveEvents)>,
    /// Props hitting players.
    pub impacts: Vec<(PlayerId, Impact)>,
    /// What players' tools did.
    pub tools: Vec<(PlayerId, ToolEvent)>,
}

/// A map's simulation: collision, physics, players and their tools.
#[derive(Debug)]
pub struct Simulation {
    world: Arc<CollisionWorld>,
    physics: PhysicsWorld,
    players: Vec<Player>,
    movement: MovementConfig,
    manipulator: ManipulatorConfig,
    manipulator_intervals: Intervals,
    dt: f32,
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
            movement: data.movement.clone(),
            manipulator: data.manipulator.clone(),
            manipulator_intervals: manipulator_intervals(data),
            dt,
        }
    }

    /// The length of a tick, seconds.
    #[must_use]
    pub fn dt(&self) -> f32 {
        self.dt
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

    /// Adds a prop.
    pub fn add_prop(&mut self, desc: &PropDesc) -> BodyId {
        self.physics.add_prop(desc)
    }

    /// Adds a player standing at `origin`, nudged free of anything it
    /// overlaps (mappers leave some spawns touching walls).
    pub fn spawn_player(&mut self, origin: Vec3) -> PlayerId {
        let id = PlayerId(u32::try_from(self.players.len()).unwrap_or(u32::MAX));
        let state = PlayerState::at(self.free_position(origin).unwrap_or(origin));
        let body = self
            .physics
            .add_player(state.origin, &state.hull(&self.movement));
        self.players.push(Player {
            state,
            body,
            manipulator: Manipulator::default(),
        });
        id
    }

    /// A player.
    #[must_use]
    pub fn player(&self, id: PlayerId) -> Option<&Player> {
        self.players.get(id.0 as usize)
    }

    /// Where a player's eyes are.
    #[must_use]
    pub fn eye_position(&self, id: PlayerId) -> Option<Vec3> {
        let state = self.player(id)?.state;
        Some(state.origin + Vec3::Z * state.eye_height(&self.movement))
    }

    /// Moves a player to the free spot nearest `origin` and stops them;
    /// `false` (and nothing changes) if there is no room there.
    pub fn teleport_player(&mut self, id: PlayerId, origin: Vec3) -> bool {
        let Some(origin) = self.free_position(origin) else {
            return false;
        };
        let Some(player) = self.players.get_mut(id.0 as usize) else {
            return false;
        };
        player.state = PlayerState::at(origin);
        let hull = player.state.hull(&self.movement);
        self.physics
            .set_player(player.body, origin, Vec3::ZERO, &hull);
        true
    }

    /// Advances the simulation by one tick. Players without a command in
    /// `commands` stand still.
    pub fn tick(&mut self, commands: &[(PlayerId, UserCmd)]) -> TickEvents {
        let mut events = TickEvents::default();
        for &(id, cmd) in commands {
            let Some(player) = self.players.get_mut(id.0 as usize) else {
                continue;
            };
            let scene = Scene {
                world: &self.world,
                physics: &self.physics,
                ignore: player.manipulator.held(),
            };
            let (state, moved) = simulate(&player.state, &cmd, &self.movement, self.dt, &scene);
            player.state = state;
            for touch in moved.touches() {
                self.physics.push(touch.body, touch.normal, touch.speed);
            }
            events.moves.push((id, moved));

            let aim = Aim {
                eye: state.origin + Vec3::Z * state.eye_height(&self.movement),
                pitch: cmd.pitch,
                yaw: cmd.yaw,
            };
            if let Some(event) = player.manipulator.tick(
                player.body,
                &aim,
                cmd.buttons,
                self.manipulator_intervals,
                self.dt,
                &self.world,
                &mut self.physics,
                &self.manipulator,
            ) {
                events.tools.push((id, event));
            }
        }

        for player in &self.players {
            let hull = player.state.hull(&self.movement);
            self.physics.set_player(
                player.body,
                player.state.origin,
                player.state.velocity,
                &hull,
            );
        }
        for impact in self.physics.step() {
            if let Some(index) = self.players.iter().position(|p| p.body == impact.player) {
                events.impacts.push((PlayerId(index as u32), impact));
            }
        }
        events
    }

    fn free_position(&self, origin: Vec3) -> Option<Vec3> {
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
        };
        let hull = crate::collision::Hull::from(&self.movement.hulls.standing);
        find_free_position(&scene, origin, &hull)
    }
}

/// The Manipulator's rest after each action: the intervals of the fire
/// modes of `weapons/manipulator.ron`, or none if it has no such mode.
fn manipulator_intervals(data: &GameData) -> Intervals {
    let weapon = data.weapons.get("manipulator");
    let interval = |fire: Option<&rl_data::FireConfig>| fire.map_or(0.0, |fire| fire.interval);
    Intervals {
        primary: interval(weapon.and_then(|weapon| weapon.primary.as_ref())),
        secondary: interval(weapon.and_then(|weapon| weapon.secondary.as_ref())),
    }
}
