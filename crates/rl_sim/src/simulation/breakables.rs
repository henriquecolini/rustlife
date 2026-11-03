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
//! Explosive props are the exception: they burn. They are HL2DM's barrels. A
//! blast or a flame sets one on fire, and a blast that would kill it leaves it
//! some health to burn through, less the nearer the blast, so neighbours chain
//! with a delay. A bullet that would leave it at half its health or less sets
//! it on fire with its health restored; a bullet that would kill it, or any
//! into a burning one, sets it off at once, as does a swing of a club. Flames
//! eat its health until it explodes and light other barrels touching it. One
//! thrown with the gravity gun goes off on the first thing it touches. A prop
//! that is merely pushed about, or falls, never explodes.
//!
//! Whoever broke a prop is credited with its blast, and with the breaking of
//! the props that blast breaks: a barrel shot by a player is that player's
//! kill.

use std::collections::{BTreeMap, BTreeSet};

use glam::{Quat, Vec3};
use rl_data::{ExplosionConfig, ImpactOther, PropImpact, Scorch};
use serde::{Deserialize, Serialize};

use super::{BlastSource, PlayerId, Simulation, TickEvents};
use crate::combat::blast;
use crate::physics::{BodyId, Breakable, ContactImpact};
use crate::rng::Rng;

/// A prop does not come back while a player is this close to where it
/// would appear, units.
const RESPAWN_CLEARANCE: f32 = 96.0;

/// Most rounds of chained breaking in one tick, so a malformed map cannot
/// loop forever.
const MAX_ROUNDS: usize = 32;

/// Size of a flame on a prop less than twice this across, units (HL2DM's
/// `CEntityFlame::Create`); on larger ones it is half the prop's extent.
const SMALLEST_FLAME: f32 = 16.0;

/// Seed of the random health a blast leaves an explosive prop.
const BLAST_SEED: u64 = 0x0062_6c61_7374;

/// What kind of blow a prop took, which decides how much of its damage the
/// prop feels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Blow {
    /// A bullet.
    Bullet,
    /// A melee swing.
    Club,
    /// A blast, `distance` units from the prop's centre.
    Explosion { distance: f32 },
    /// The heat of a flame on another prop.
    Burn,
    /// Another prop or the map hitting it hard.
    Impact,
    /// Touching anything at all: what a prop thrown with the gravity gun
    /// does to itself.
    Slam,
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
    /// The tick it was added, so the contacts it makes while it settles
    /// into the map do not hurt it.
    born: u64,
    /// While an explosive prop burns, seconds until the flame next bites.
    flame: Option<f32>,
    /// Who set it burning, for the credit.
    igniter: Option<PlayerId>,
    /// Seconds left that it explodes on touching anything: it was thrown
    /// or punted with the gravity gun.
    launched: f32,
}

/// A prop set on fire, for effects: it burns until it explodes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PropIgnition {
    /// The prop.
    pub body: BodyId,
    /// Seconds until it goes off at the health it has now, unless something
    /// sets it off sooner.
    pub fuse: f32,
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
    /// Where its model's origin was, and how it was turned: its pieces
    /// appear here, so they fit together as the model did.
    pub origin: Vec3,
    /// See `origin`.
    pub rotation: Quat,
    /// How it was moving, units/s: its pieces inherit it.
    pub velocity: Vec3,
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

/// A prop sliding along something, for sounds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PropScrapeEvent {
    /// The prop.
    pub body: BodyId,
    /// Where it is.
    pub position: Vec3,
    /// How fast it slides, units/s.
    pub speed: f32,
}

/// Ticks between reports of props that keep sliding.
pub(super) const SCRAPE_INTERVAL: u64 = 5;

/// Ticks a new prop may push itself out of the map or another prop without
/// those contacts counting as impacts. Props placed a little inside a floor
/// or each other would otherwise break before the match starts.
const IMPACT_SETTLE_TICKS: u64 = 45;

/// The breakable props of a simulation, by body.
#[derive(Debug, Clone, Default)]
pub(super) struct Breakables {
    props: BTreeMap<BodyId, PropHealth>,
    hits: Vec<PropHit>,
}

impl Breakables {
    pub(super) fn register(&mut self, body: BodyId, spec: &Breakable, tick: u64) {
        self.props.insert(
            body,
            PropHealth {
                health: spec.health,
                spec: spec.clone(),
                broken: false,
                respawn_in: None,
                born: tick,
                flame: None,
                igniter: None,
                launched: 0.0,
            },
        );
    }

    /// Makes an explosive `body` go off on touching anything for
    /// `seconds` (it was launched with the gravity gun); `0` disarms it.
    pub(super) fn arm(&mut self, body: BodyId, seconds: f32) {
        if let Some(state) = self.props.get_mut(&body)
            && state.spec.explosion.is_some()
        {
            state.launched = seconds;
        }
    }

    /// Whether `body` was added so recently that its settling contacts
    /// should not count as impacts.
    pub(super) fn settling(&self, body: BodyId, tick: u64) -> bool {
        self.props
            .get(&body)
            .is_some_and(|state| tick.saturating_sub(state.born) < IMPACT_SETTLE_TICKS)
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
            state.flame = None;
            state.igniter = None;
            state.launched = 0.0;
        }
    }

    /// Notes that `body` appeared at `tick`: it settles, unhurt, from then.
    pub(super) fn mark_born(&mut self, body: BodyId, tick: u64) {
        if let Some(state) = self.props.get_mut(&body) {
            state.born = tick;
        }
    }

    /// The props burning now.
    fn burning(&self) -> impl Iterator<Item = BodyId> + '_ {
        self.props
            .iter()
            .filter(|(_, state)| state.flame.is_some() && !state.broken)
            .map(|(&body, _)| body)
    }

    /// Notes which props burn, as the server said: a replica's flames only
    /// show that they do, they never bite.
    pub(super) fn mark_burning(&mut self, burning: &BTreeSet<BodyId>) {
        for (body, state) in &mut self.props {
            state.flame = burning.contains(body).then_some(0.0);
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
    /// blows. Fast ones are heard, and knock what they touch (which sets off
    /// a launched barrel); new ones also hurt each prop by its own damage
    /// table. Which of two props is reported as the one that struck is an
    /// accident of their order in the world, so each gets the same treatment,
    /// and whoever threw either lately is credited, its own thrower first.
    pub(super) fn hit_props_by_impacts(&mut self) {
        let report_speed = self.data.physics.breakables.impact.report_speed;
        for impact in self.physics.take_contacts() {
            let Some(mass) = self.physics.mass(impact.body) else {
                continue;
            };
            if self.breakables.settling(impact.body, self.tick)
                || impact
                    .other
                    .is_some_and(|other| self.breakables.settling(other, self.tick))
            {
                continue;
            }
            let thrower = self.thrower_of(impact.body);
            let other_thrower = impact.other.and_then(|other| self.thrower_of(other));
            let attacker = thrower.or(other_thrower);
            let other_attacker = other_thrower.or(thrower);
            if impact.speed > report_speed {
                self.prop_impacts_pending.push(PropImpactEvent {
                    body: impact.body,
                    position: impact.position,
                    speed: impact.speed,
                    mass,
                });
                self.hit_prop(impact.body, 1.0, Blow::Slam, attacker);
                if let Some(other) = impact.other {
                    self.hit_prop(other, 1.0, Blow::Slam, other_attacker);
                }
            }
            if !impact.fresh {
                continue;
            }
            let own = self.impact_damage(&impact, false);
            self.hit_prop(impact.body, own, Blow::Impact, attacker);
            if let Some(other) = impact.other {
                let theirs = self.impact_damage(&impact, true);
                self.hit_prop(other, theirs, Blow::Impact, other_attacker);
            }
        }
    }

    /// The damage `impact` does to the prop that was reported as struck, or
    /// with `to_other` to the other.
    fn impact_damage(&self, impact: &ContactImpact, to_other: bool) -> f32 {
        let (me, them, their_velocity) = if to_other {
            let Some(other) = impact.other else {
                return 0.0;
            };
            (other, Some(impact.body), impact.velocity)
        } else {
            (impact.body, impact.other, impact.other_velocity)
        };
        let Some(spec) = self.breakables.spec(me) else {
            return 0.0;
        };
        let config = &self.data.physics.breakables.impact;
        let Some(table) = config.table(spec.damage_table.as_deref()) else {
            return 0.0;
        };
        let other = them.and_then(|them| Some((them, self.physics.mass(them)?)));
        let other = other.map_or(ImpactOther::MAP, |(them, mass)| ImpactOther {
            mass,
            fixed: !self.physics.is_dynamic(them),
            held: self.physics.holder(them).is_some(),
            speed_sqr: their_velocity.length_squared(),
            falling: (-their_velocity.z / their_velocity.length().max(f32::EPSILON)).max(0.0),
            // HL2DM only spins with damage what it flags as slicing, which
            // no prop here is.
            blade: None,
        });
        table.damage(&PropImpact {
            mass: self.physics.mass(me).unwrap_or(0.0),
            fixed: !self.physics.is_dynamic(me),
            held: self.physics.holder(me).is_some(),
            closing: impact.speed,
            other,
        })
    }

    /// Turns props running into players into knocks, which set off a barrel
    /// just launched as hitting the map does. The player who threw it is not
    /// among them: they ignore each other until they have come apart.
    pub(super) fn hit_props_by_player_touches(&mut self) {
        for touch in self.physics.take_player_touches() {
            let attacker = self.thrower_of(touch.prop);
            self.hit_prop(touch.prop, 1.0, Blow::Slam, attacker);
        }
    }

    /// The props on fire now, on the server or, as it last said, a replica.
    pub fn burning_props(&self) -> impl Iterator<Item = BodyId> + '_ {
        self.breakables.burning()
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

    /// Sets a prop moving at `velocity`, waking it: for tools and tests.
    pub fn set_prop_velocity(&mut self, body: BodyId, velocity: Vec3) {
        self.physics.set_velocity(body, velocity, Vec3::ZERO);
    }

    /// Reports the props that are sliding along something, every few ticks.
    pub(super) fn report_scrapes(&self, events: &mut TickEvents) {
        if !self.tick.is_multiple_of(SCRAPE_INTERVAL) {
            return;
        }
        events
            .prop_scrapes
            .extend(
                self.physics
                    .scrapes()
                    .into_iter()
                    .map(|scrape| PropScrapeEvent {
                        body: scrape.body,
                        position: scrape.position,
                        speed: scrape.speed,
                    }),
            );
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
                self.breakables.mark_born(body, self.tick);
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
        if state.spec.explosion.is_some() {
            self.hit_explosive(hit, events);
            return;
        }
        let scale = match hit.blow {
            Blow::Bullet => state.spec.bullets,
            Blow::Club => state.spec.club,
            Blow::Explosion { .. } => state.spec.explosive,
            Blow::Impact => 1.0,
            Blow::Burn | Blow::Slam => return,
        };
        state.health -= hit.amount * scale;
        if state.health <= 0.0 {
            self.break_prop(hit.body, hit.attacker, events);
        }
    }

    /// What a blow does to an explosive prop: `OnTakeDamage` of HL2DM's
    /// `CBreakableProp`. It may set it off, set it on fire or take health.
    fn hit_explosive(&mut self, hit: PropHit, events: &mut TickEvents) {
        let config = self.data.physics.breakables.barrels;
        let tick = self.tick;
        let Some(state) = self.breakables.props.get_mut(&hit.body) else {
            return;
        };
        let burning = state.flame.is_some();
        let full = state.spec.health;
        let health = match hit.blow {
            Blow::Club => return self.explode_prop(hit, events),
            Blow::Slam if state.launched > 0.0 => return self.explode_prop(hit, events),
            Blow::Slam | Blow::Impact => return,
            Blow::Burn if burning => return,
            Blow::Burn if hit.amount >= state.health => return self.explode_prop(hit, events),
            Blow::Burn => state.health - hit.amount,
            Blow::Bullet => {
                let damage = hit.amount * state.spec.bullets;
                if burning || damage >= state.health {
                    return self.explode_prop(hit, events);
                }
                if state.health - damage > full / 2.0 {
                    state.health -= damage;
                    return;
                }
                full - damage
            }
            Blow::Explosion { distance } => {
                let damage = hit.amount * state.spec.explosive;
                let left = if damage >= state.health {
                    let body = u64::from(hit.body.index() as u32);
                    let unit = Rng::seeded(&[BLAST_SEED, tick, body]).unit();
                    config.blast_health(distance, unit)
                } else {
                    (state.health - damage).max(config.min_blast_health)
                };
                left.min(state.health)
            }
        };
        state.health = health;
        if !burning {
            state.flame = Some(config.flame_first_bite);
            state.igniter = state.igniter.or(hit.attacker);
            events.prop_ignitions.push(PropIgnition {
                body: hit.body,
                fuse: config.burn_time(health),
            });
        }
    }

    fn explode_prop(&mut self, hit: PropHit, events: &mut TickEvents) {
        let attacker = self
            .breakables
            .props
            .get(&hit.body)
            .and_then(|state| state.igniter)
            .or(hit.attacker);
        self.break_prop(hit.body, attacker, events);
    }

    /// Burns the props that are on fire down by a tick, as HL2DM's
    /// `CEntityFlame::FlameThink`: each bite takes health, setting off those
    /// it leaves with none, and heats what is close by. Lets launched ones'
    /// danger run out.
    pub(super) fn burn_props(&mut self, events: &mut TickEvents) {
        let dt = self.dt;
        let config = self.data.physics.breakables.barrels;
        let mut bites = Vec::new();
        for (&body, state) in &mut self.breakables.props {
            if state.broken {
                continue;
            }
            state.launched = (state.launched - dt).max(0.0);
            if let Some(next) = &mut state.flame {
                *next -= dt;
                if *next <= 0.0 {
                    *next += config.flame_interval;
                    state.health -= config.flame_damage;
                    bites.push((body, state.igniter, state.health <= 0.0));
                }
            }
        }
        for (body, igniter, spent) in bites {
            self.heat_around(body, igniter);
            if spent {
                self.break_prop(body, igniter, events);
            }
        }
    }

    /// Heats the other explosive props within half the size of the flame on
    /// `body`: `RadiusDamage` of `DMG_BURN`, which sets them on fire. It
    /// reaches their edge, not their centre as in the game (the game's reach
    /// is less than a barrel's own width, so its flames rarely light another).
    fn heat_around(&mut self, body: BodyId, igniter: Option<PlayerId>) {
        let Some(center) = self.physics.center_of_mass(body) else {
            return;
        };
        let Some((low, high)) = self.physics.bounds(body) else {
            return;
        };
        let extent = (high - low).max_element();
        let size = if extent < 2.0 * SMALLEST_FLAME {
            SMALLEST_FLAME
        } else {
            extent / 2.0
        };
        let heat = ExplosionConfig {
            radius: size / 2.0,
            damage: self.data.physics.breakables.barrels.flame_radius_damage,
            knockback: 0.0,
            knockback_cap: None,
            force: 0.0,
            scorch: Scorch::None,
        };
        for reach in blast::props(center, &heat, &self.physics, &self.world) {
            let catches = self
                .breakables
                .spec(reach.body)
                .is_some_and(|spec| spec.explosion.is_some());
            if reach.body != body && reach.exposed && catches {
                self.hit_prop(reach.body, heat.damage * reach.scale, Blow::Burn, igniter);
            }
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
        let pose = self.physics.pose(body);
        let velocity = self
            .physics
            .velocity(body)
            .map_or(Vec3::ZERO, |(linear, _)| linear);
        self.physics.break_prop(body);
        for player in self.players.iter_mut().flatten() {
            if player.manipulator.held() == Some(body) {
                player.manipulator.drop_held(&mut self.physics);
            }
            if player.hands.held() == Some(body) {
                player
                    .hands
                    .drop_held(&mut self.physics, &self.data.hands.release);
            }
        }
        events.prop_breaks.push(PropBreak {
            body,
            position,
            origin: pose.map_or(position, |pose| pose.position),
            rotation: pose.map_or(Quat::IDENTITY, |pose| pose.rotation),
            velocity,
            exploded: spec.explosion.is_some(),
        });
        if let Some((damage, radius)) = spec.explosion {
            let tuning = &self.data.physics.breakables;
            let explosion = ExplosionConfig {
                radius,
                damage,
                knockback: tuning.explosion_knockback,
                knockback_cap: None,
                force: tuning.explosion_force,
                // A barrel blows up through an env_explosion.
                scorch: Scorch::Below,
            };
            self.explode(
                attacker,
                BlastSource::Prop,
                (position, None),
                &explosion,
                events,
            );
        }
    }
}
