//! Props that break: crates that splinter, barrels that explode.
//!
//! A prop with a [`Breakable`] has health. Blows queue [`PropHit`]s as they
//! land (a bullet, a swing, a blast, a hard impact), and
//! [`Simulation::resolve_prop_hits`] applies them once the tick's other work
//! is done: each hit is scaled by what the prop is made of (a cardboard box
//! takes more from a club than a bullet), and a prop whose health runs out
//! breaks. Breaking takes it out of the physics world, tells everyone
//! ([`PropBreak`]) and, for an explosive prop, sets off its blast, which
//! hits other props in turn, so barrels chain.
//!
//! Whoever broke a prop is credited with its blast, and with the breaking of
//! the props that blast breaks: a barrel shot by a player is that player's
//! kill.

use std::collections::BTreeMap;

use glam::Vec3;
use rl_data::ExplosionConfig;
use serde::{Deserialize, Serialize};

use super::{BlastSource, PlayerId, Simulation, TickEvents};
use crate::physics::{BodyId, Breakable};

/// A prop does not come back while a player is this close to where it
/// would appear, units.
const RESPAWN_CLEARANCE: f32 = 96.0;

/// Most rounds of chained breaking in one tick, so a malformed map cannot
/// loop forever.
const MAX_ROUNDS: usize = 32;

/// What kind of blow a prop took, which decides how much of its damage the
/// prop feels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Blow {
    /// A bullet.
    Bullet,
    /// A melee swing.
    Club,
    /// A blast.
    Explosion,
    /// Another prop or the map hitting it hard.
    Impact,
}

/// A blow waiting to be applied.
#[derive(Debug, Clone, Copy)]
pub(super) struct PropHit {
    body: BodyId,
    amount: f32,
    blow: Blow,
    attacker: Option<PlayerId>,
}

/// A breakable prop's state.
#[derive(Debug, Clone)]
pub(super) struct PropHealth {
    spec: Breakable,
    health: f32,
    broken: bool,
    /// Seconds until a broken prop that respawns comes back.
    respawn_in: Option<f32>,
}

/// A prop breaking, for sounds and effects. What it was made of and what it
/// breaks into are the prop's ([`Simulation::breakable`],
/// [`PhysicsWorld::surface`](crate::physics::PhysicsWorld::surface)).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PropBreak {
    /// The prop.
    pub body: BodyId,
    /// Where it was (its centre of mass).
    pub position: Vec3,
    /// It exploded as it broke.
    pub exploded: bool,
}

/// A prop striking another prop or the map, for sounds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PropImpactEvent {
    /// The prop that struck.
    pub body: BodyId,
    /// Where.
    pub position: Vec3,
    /// How fast they were closing, units/s.
    pub speed: f32,
    /// The prop's mass, kg.
    pub mass: f32,
}

/// The breakable props of a simulation, by body.
#[derive(Debug, Clone, Default)]
pub(super) struct Breakables {
    props: BTreeMap<BodyId, PropHealth>,
    hits: Vec<PropHit>,
}

impl Breakables {
    pub(super) fn register(&mut self, body: BodyId, spec: &Breakable) {
        self.props.insert(
            body,
            PropHealth {
                health: spec.health,
                spec: spec.clone(),
                broken: false,
                respawn_in: None,
            },
        );
    }

    /// Whether `body` can break and has not yet.
    pub(super) fn contains(&self, body: BodyId) -> bool {
        self.props.get(&body).is_some_and(|state| !state.broken)
    }

    /// Notes that `body` has broken, as the server said.
    pub(super) fn mark_broken(&mut self, body: BodyId) {
        if let Some(state) = self.props.get_mut(&body) {
            state.broken = true;
        }
    }

    /// Notes that `body` is whole again, with all its health.
    pub(super) fn mark_whole(&mut self, body: BodyId) {
        if let Some(state) = self.props.get_mut(&body) {
            state.broken = false;
            state.respawn_in = None;
            state.health = state.spec.health;
        }
    }

    /// How `body` breaks, broken or not.
    pub(super) fn spec(&self, body: BodyId) -> Option<&Breakable> {
        self.props.get(&body).map(|state| &state.spec)
    }
}

impl Simulation {
    /// Queues a blow on a prop (nothing happens to props that cannot
    /// break).
    pub(super) fn hit_prop(
        &mut self,
        body: BodyId,
        amount: f32,
        blow: Blow,
        attacker: Option<PlayerId>,
    ) {
        if amount > 0.0 && self.breakables.contains(body) {
            self.breakables.hits.push(PropHit {
                body,
                amount,
                blow,
                attacker,
            });
        }
    }

    /// Turns the props' impacts on props and the map into sounds and
    /// blows: the prop that struck takes the blow of what it struck (the
    /// map is as heavy as the prop itself), and a prop it struck takes the
    /// blow of it. Whoever threw the striking prop lately is credited.
    pub(super) fn hit_props_by_impacts(&mut self) {
        let config = self.data.physics.breakables.impact;
        for impact in self.physics.take_contacts() {
            let Some(mass) = self.physics.mass(impact.body) else {
                continue;
            };
            self.prop_impacts_pending.push(PropImpactEvent {
                body: impact.body,
                position: impact.position,
                speed: impact.speed,
                mass,
            });
            let attacker = self.thrower_of(impact.body);
            let other_mass = impact.other.and_then(|other| self.physics.mass(other));
            let own = config.damage(other_mass.unwrap_or(mass), impact.speed);
            self.hit_prop(impact.body, own, Blow::Impact, attacker);
            if let (Some(other), Some(_)) = (impact.other, other_mass) {
                let theirs = config.damage(mass, impact.speed);
                self.hit_prop(other, theirs, Blow::Impact, attacker);
            }
        }
    }

    /// How a prop breaks, if it can (also after it has broken): what it
    /// breaks into, whether it explodes.
    #[must_use]
    pub fn breakable(&self, body: BodyId) -> Option<&Breakable> {
        self.breakables.spec(body)
    }

    /// Who threw, punted or dropped `body` lately, if anyone: the credit for
    /// the harm it does.
    pub(super) fn thrower_of(&self, body: BodyId) -> Option<PlayerId> {
        let credit = (self.data.rules.prop_credit / self.dt) as u64;
        self.thrown
            .iter()
            .find(|&&(thrown, _, tick)| thrown == body && self.tick - tick <= credit)
            .map(|&(_, thrower, _)| thrower)
    }

    /// Hurts a prop at once, as if struck by `amount` of impact damage by
    /// `attacker`: for tools and tests. Returns what happened.
    pub fn damage_prop(
        &mut self,
        body: BodyId,
        amount: f32,
        attacker: Option<PlayerId>,
    ) -> TickEvents {
        let mut events = TickEvents::default();
        self.hit_prop(body, amount, Blow::Impact, attacker);
        self.resolve_prop_hits(&mut events);
        events
    }

    /// Brings back the props that respawn once their time is up, unless a
    /// player stands where they would appear.
    pub(super) fn respawn_props(&mut self) {
        let waiting: Vec<BodyId> = self
            .breakables
            .props
            .iter()
            .filter(|(_, state)| state.broken && state.respawn_in.is_some())
            .map(|(&body, _)| body)
            .collect();
        for body in waiting {
            let Some(state) = self.breakables.props.get_mut(&body) else {
                continue;
            };
            let left = state.respawn_in.unwrap_or(0.0) - self.dt;
            state.respawn_in = Some(left.max(0.0));
            if left > 0.0 {
                continue;
            }
            let Some(home) = self.physics.home_position(body) else {
                continue;
            };
            let blocked = self.players().any(|(_, player)| {
                player.alive() && player.state.origin.distance(home) < RESPAWN_CLEARANCE
            });
            if !blocked && self.physics.restore_prop(body) {
                self.breakables.mark_whole(body);
            }
        }
    }

    /// Applies the queued blows; props that run out of health break, and
    /// their blasts queue more blows, until nothing more breaks.
    pub(super) fn resolve_prop_hits(&mut self, events: &mut TickEvents) {
        events.prop_impacts.append(&mut self.prop_impacts_pending);
        for _ in 0..MAX_ROUNDS {
            let hits = std::mem::take(&mut self.breakables.hits);
            if hits.is_empty() {
                return;
            }
            for hit in hits {
                self.apply_prop_hit(hit, events);
            }
        }
        self.breakables.hits.clear();
    }

    fn apply_prop_hit(&mut self, hit: PropHit, events: &mut TickEvents) {
        let Some(state) = self
            .breakables
            .props
            .get_mut(&hit.body)
            .filter(|state| !state.broken)
        else {
            return;
        };
        let scale = match hit.blow {
            Blow::Bullet => state.spec.bullets,
            Blow::Club => state.spec.club,
            Blow::Explosion => state.spec.explosive,
            Blow::Impact => 1.0,
        };
        state.health -= hit.amount * scale;
        if state.health <= 0.0 {
            self.break_prop(hit.body, hit.attacker, events);
        }
    }

    /// Breaks a prop now: out of the physics world, announced, and
    /// exploding if it is explosive.
    pub(super) fn break_prop(
        &mut self,
        body: BodyId,
        attacker: Option<PlayerId>,
        events: &mut TickEvents,
    ) {
        let Some(state) = self
            .breakables
            .props
            .get_mut(&body)
            .filter(|state| !state.broken)
        else {
            return;
        };
        state.broken = true;
        state.respawn_in = state.spec.respawn;
        let spec = state.spec.clone();
        let position = self.physics.center_of_mass(body).unwrap_or(Vec3::ZERO);
        self.physics.break_prop(body);
        for player in self.players.iter_mut().flatten() {
            if player.manipulator.held() == Some(body) {
                player.manipulator.drop_held(&mut self.physics);
            }
        }
        events.prop_breaks.push(PropBreak {
            body,
            position,
            exploded: spec.explosion.is_some(),
        });
        if let Some((damage, radius)) = spec.explosion {
            let tuning = self.data.physics.breakables;
            let explosion = ExplosionConfig {
                radius,
                damage,
                knockback: tuning.explosion_knockback,
                force: tuning.explosion_force,
            };
            self.explode(attacker, BlastSource::Prop, position, &explosion, events);
        }
    }
}
