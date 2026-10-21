//! Props striking props and the map: the impacts that make noise and break
//! things.
//!
//! Rapier reports a collision event when two colliders begin to touch.
//! [`Collector`] gathers those during a step, with the contact normal and
//! point; once the step is over, [`PhysicsWorld::contact_impacts`] works out
//! how hard each hit was from the bodies' velocities *before* the step (the
//! solver has already taken the speed out of them by then). Contacts with
//! players are not counted here: [`super::contacts`] handles those.

use std::sync::Mutex;

use glam::Vec3;
use rapier3d::prelude::{
    ColliderHandle, ColliderSet, CollisionEvent, ContactPair, EventHandler, Real, RigidBodySet,
    SoftBodySet, SoftBodyTearEvent,
};

use super::{BodyId, BodyKind, GROUP_PLAYER, GROUP_WORLD, PhysicsWorld};

/// A prop striking another prop or the map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactImpact {
    /// The prop.
    pub body: BodyId,
    /// The other prop, or `None` for the map.
    pub other: Option<BodyId>,
    /// How fast they were closing along the contact, units/s.
    pub speed: f32,
    /// Where they met, Source space.
    pub position: Vec3,
}

/// What a collider belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Party {
    /// The map.
    Map,
    /// A prop.
    Prop(BodyId),
}

/// A contact that began during a step.
#[derive(Debug, Clone, Copy)]
struct Started {
    first: ColliderHandle,
    second: ColliderHandle,
    /// The contact normal, from the first collider to the second.
    normal: Vec3,
    point: Vec3,
}

/// Gathers the contacts that begin during a step.
#[derive(Debug, Default)]
pub(super) struct Collector {
    started: Mutex<Vec<Started>>,
}

impl EventHandler for Collector {
    fn handle_collision_event(
        &self,
        _bodies: &RigidBodySet,
        colliders: &ColliderSet,
        event: CollisionEvent,
        contact_pair: Option<&ContactPair>,
    ) {
        let CollisionEvent::Started(first, second, _) = event else {
            return;
        };
        let Some((manifold, contact)) = contact_pair.and_then(ContactPair::find_deepest_contact)
        else {
            return;
        };
        let Some(collider) = colliders.get(first) else {
            return;
        };
        let point = collider.position().transform_point(contact.local_p1);
        let mut started = self
            .started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        started.push(Started {
            first,
            second,
            normal: manifold.data.normal,
            point,
        });
    }

    fn handle_contact_force_event(
        &self,
        _dt: Real,
        _bodies: &RigidBodySet,
        _colliders: &ColliderSet,
        _contact_pair: &ContactPair,
        _total_force_magnitude: Real,
    ) {
    }

    fn handle_soft_body_tear_event(&self, _soft_bodies: &SoftBodySet, _event: &SoftBodyTearEvent) {}
}

impl Collector {
    fn take(&self) -> Vec<Started> {
        std::mem::take(
            &mut *self
                .started
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

impl PhysicsWorld {
    /// The impacts of props on props and on the map that began during the
    /// step `collector` listened to, given every awake prop's velocity
    /// before it. Slower than `floor` units/s are left out.
    pub(super) fn contact_impacts(
        &self,
        collector: &Collector,
        before: &[(BodyId, Vec3)],
        floor: f32,
    ) -> Vec<ContactImpact> {
        let velocity = |body: Option<BodyId>| {
            body.and_then(|body| before.iter().find(|(id, _)| *id == body))
                .map_or(Vec3::ZERO, |&(_, velocity)| velocity)
        };
        collector
            .take()
            .into_iter()
            .filter_map(|contact| {
                let prop_of = |party| match party {
                    Party::Prop(body) => Some(body),
                    Party::Map => None,
                };
                let first = prop_of(self.contact_party(contact.first)?);
                let second = prop_of(self.contact_party(contact.second)?);
                let closing = (velocity(first) - velocity(second)).dot(contact.normal);
                // Whichever is a prop reports it; props against props are
                // reported once, by the first.
                let body = first.or(second)?;
                let other = if first.is_some() { second } else { None };
                (closing > floor).then_some(ContactImpact {
                    body,
                    other,
                    speed: closing,
                    position: contact.point,
                })
            })
            .collect()
    }

    /// What a collider belongs to; `None` for a player (or nothing known).
    fn contact_party(&self, collider: ColliderHandle) -> Option<Party> {
        let collider = self.rapier.colliders.get(collider)?;
        let groups = collider.collision_groups();
        if groups.memberships.contains(GROUP_PLAYER) {
            return None;
        }
        if groups.memberships.contains(GROUP_WORLD) {
            return Some(Party::Map);
        }
        let id = BodyId(collider.user_data as u32);
        matches!(self.bodies.get(id.index())?.kind, BodyKind::Prop(_)).then_some(Party::Prop(id))
    }
}
