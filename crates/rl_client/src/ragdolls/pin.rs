//! Bodies pinned to walls by crossbow bolts, as HL2DM's `BoltImpact` pins
//! them.
//!
//! A bolt that strikes someone looks 128 units on along its flight; if the
//! map is there, the server says where (a [`ProjectileEventKind::Hit`] with a
//! pin) and, should the victim die of it, their body is pinned to that point.
//! This is client.so's `StickRagdollNow` (objdump 0x783e30): from the point on
//! the wall it looks back along the bolt's flight for 128 units, and the first
//! of the ragdoll's bones that line meets (`CRagdollBoltEnumerator`, 0x784100:
//! the ray clipped against the model's hitboxes, the hit's physics bone) has
//! its mass doubled and is tied to the world by a ball-and-socket joint: the
//! world's end at the point on the wall, the bone's where the line went into
//! it, so the joint draws the body back to the wall and it hangs there. Then
//! `Weapon_Crossbow.BoltSkewer` plays (the weapon's `skewer`). Here the line is
//! met against the rig's capsules, which are slimmer than the game's hitboxes,
//! so a line that misses them all takes the bone nearest to it, if one is
//! within [`NEAR_MISS`] units.
//!
//! The kill and the hit may be heard in either order (the kill goes reliably,
//! the hit with a snapshot), so a pin waits [`super::HIT_MEMORY`] for the body.

use bevy::prelude::*;
use box3d_rust as b3;
use rl_sim::simulation::{PlayerId, ProjectileEventKind};
use rl_sim::weapons::WeaponId;

use super::{Ragdoll, RagdollPhysics, b3v, b3v_to_glam, segment_distance};
use crate::net::Game;
use crate::player::Ticked;

/// How far back from the wall along the bolt's flight a bone is looked for,
/// units.
const REACH: f32 = 128.0;
/// The step the line is walked in, units.
const STEP: f32 = 0.5;
/// How far from the line a bone may be and still be pinned when the line
/// misses every capsule, units.
const NEAR_MISS: f32 = 12.0;
/// How much heavier a pinned bone is made.
const PINNED_MASS_SCALE: f32 = 2.0;

/// A body was pinned to a wall: by whose weapon, and where the bolt went
/// through it (Source space), for the sound.
#[derive(Message, Debug, Clone, Copy, PartialEq)]
pub(crate) struct Skewered {
    pub weapon: WeaponId,
    pub at: Vec3,
}

/// A pin heard of, waiting for its victim's body.
#[derive(Debug, Clone, Copy)]
struct Pin {
    victim: PlayerId,
    weapon: WeaponId,
    /// The point on the wall, Source units.
    wall: Vec3,
    /// The way the bolt flew (a unit vector).
    direction: Vec3,
    /// When it was heard, seconds of app time.
    heard: f32,
}

/// Pins heard of whose bodies have not been pinned yet.
#[derive(Resource, Default)]
pub(super) struct PendingPins(Vec<Pin>);

/// Where the closest point of the segment from `a` to `b` to `point` is.
fn closest_on_segment(point: Vec3, a: Vec3, b: Vec3) -> Vec3 {
    let along = b - a;
    let length = along.length_squared();
    if length <= f32::EPSILON {
        return a;
    }
    a + along * ((point - a).dot(along) / length).clamp(0.0, 1.0)
}

/// The bone a bolt's line meets on its way back from `wall` along
/// `-direction`, and where on it: the first capsule (`bones`, in the world:
/// two ends and a radius) the line goes into, or, if it misses them all, the
/// point of the bone axis nearest to the line, if that is within
/// [`NEAR_MISS`].
pub(super) fn skewered_bone(
    bones: &[(Vec3, Vec3, f32)],
    wall: Vec3,
    direction: Vec3,
) -> Option<(usize, Vec3)> {
    let back = -direction.normalize_or_zero();
    let steps = (REACH / STEP) as usize;
    for step in 0..=steps {
        let point = wall + back * (step as f32 * STEP);
        let inside = bones
            .iter()
            .enumerate()
            .map(|(index, &(a, b, radius))| {
                (
                    index,
                    point.distance(closest_on_segment(point, a, b)) - radius,
                )
            })
            .filter(|&(_, depth)| depth <= 0.0)
            .min_by(|x, y| x.1.total_cmp(&y.1));
        if let Some((index, _)) = inside {
            return Some((index, point));
        }
    }
    let far = wall + back * REACH;
    let (index, distance) = bones
        .iter()
        .enumerate()
        .map(|(index, &(a, b, radius))| (index, segment_distance(wall, far, a, b) - radius))
        .min_by(|x, y| x.1.total_cmp(&y.1))?;
    if distance > NEAR_MISS {
        return None;
    }
    let (a, b, _) = bones[index];
    // The point of the bone's axis nearest the line, found by walking it.
    let nearest = (0..=16)
        .map(|i| a + (b - a) * (i as f32 / 16.0))
        .min_by(|x, y| {
            let off = |p: Vec3| p.distance(closest_on_segment(p, wall, far));
            off(*x).total_cmp(&off(*y))
        })?;
    Some((index, nearest))
}

/// Ties `body` to the world at `wall` by a ball-and-socket joint whose other
/// end is `entry` on the body (both Source space), doubling the body's mass.
/// Returns the fixed body the joint hangs from.
pub(super) fn pin_body(
    world: &mut b3::world::World,
    body: b3::BodyId,
    entry: Vec3,
    wall: Vec3,
) -> b3::BodyId {
    let mut mass = b3::body::body_get_mass_data(world, body);
    mass.mass *= PINNED_MASS_SCALE;
    mass.inertia.cx = b3v(b3v_to_glam(mass.inertia.cx) * PINNED_MASS_SCALE);
    mass.inertia.cy = b3v(b3v_to_glam(mass.inertia.cy) * PINNED_MASS_SCALE);
    mass.inertia.cz = b3v(b3v_to_glam(mass.inertia.cz) * PINNED_MASS_SCALE);
    b3::body::body_set_mass_data(world, body, mass);

    let mut anchor_def = b3::default_body_def();
    anchor_def.type_ = b3::BodyType::Static;
    anchor_def.position = b3v(wall);
    let anchor = b3::body::create_body(world, &anchor_def);

    let local = b3::body::body_get_local_point(world, body, b3v(entry));
    let mut def = b3::default_spherical_joint_def();
    def.base.body_id_a = anchor;
    def.base.body_id_b = body;
    def.base.local_frame_b = b3::Transform {
        p: local,
        q: b3::QUAT_IDENTITY,
    };
    def.base.collide_connected = false;
    b3::joint::create_spherical_joint(world, &def);
    b3::body::body_set_awake(world, body, true);
    anchor
}

/// The colliders of a corpse's bones (`bones`, each in its body of `bodies`)
/// where they are now, Source space.
pub(super) fn bones_in_world(
    world: &b3::world::World,
    bodies: &[b3::BodyId],
    bones: &[(Vec3, Vec3, f32)],
) -> Vec<(Vec3, Vec3, f32)> {
    bodies
        .iter()
        .zip(bones)
        .map(|(body, &(a, b, radius))| {
            if !b3::body::body_is_valid(world, *body) {
                return (Vec3::splat(f32::MAX), Vec3::splat(f32::MAX), 0.0);
            }
            let at =
                |local: Vec3| b3v_to_glam(b3::body::body_get_world_point(world, *body, b3v(local)));
            (at(a), at(b), radius)
        })
        .collect()
}

/// Notes the pins heard of and pins each to its victim's body once it lies
/// there.
pub(super) fn pin_ragdolls(
    mut ticks: MessageReader<Ticked>,
    time: Res<Time>,
    game: Game,
    mut pending: ResMut<PendingPins>,
    mut physics: ResMut<RagdollPhysics>,
    mut corpses: Query<&mut Ragdoll>,
    mut skewered: MessageWriter<Skewered>,
) {
    let now = time.elapsed_secs();
    for Ticked(events) in ticks.read() {
        for event in &events.projectiles {
            if let ProjectileEventKind::Hit {
                victim,
                pin: Some(wall),
            } = event.kind
            {
                pending.0.push(Pin {
                    victim,
                    weapon: event.weapon,
                    wall,
                    direction: event.normal,
                    heard: now,
                });
            }
        }
    }
    pending.0.retain(|pin| now - pin.heard <= super::HIT_MEMORY);
    if pending.0.is_empty() || game.sim().is_none() {
        return;
    }
    let Some(world) = physics.current() else {
        return;
    };
    pending.0.retain(|pin| {
        let Some(mut corpse) = corpses.iter_mut().find(|corpse| {
            corpse.player == pin.victim
                && !corpse.frozen
                && corpse.pins.is_empty()
                && (corpse.born - pin.heard).abs() <= super::HIT_MEMORY
        }) else {
            return true;
        };
        let bones = bones_in_world(world, &corpse.bodies, &corpse.bones);
        if let Some((index, entry)) = skewered_bone(&bones, pin.wall, pin.direction)
            && let Some(&body) = corpse.bodies.get(index)
            && b3::body::body_is_valid(world, body)
        {
            debug!(victim = ?pin.victim, bone = index, wall = ?pin.wall, "ragdoll: pinned");
            let anchor = pin_body(world, body, entry, pin.wall);
            corpse.pins.push(anchor);
            skewered.write(Skewered {
                weapon: pin.weapon,
                at: entry,
            });
        }
        false
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two bones along X: a torso from x = 40 to 60 and a head beyond it,
    /// both 5 units thick.
    fn bones() -> Vec<(Vec3, Vec3, f32)> {
        vec![
            (Vec3::new(40.0, 0.0, 0.0), Vec3::new(60.0, 0.0, 0.0), 5.0),
            (Vec3::new(70.0, 0.0, 0.0), Vec3::new(75.0, 0.0, 0.0), 5.0),
        ]
    }

    #[test]
    fn the_bone_nearest_the_wall_along_the_line_is_the_one_pinned_where_the_line_enters_it() {
        // A wall at x = 100, the bolt flying +X: the line goes back from the
        // wall towards -X and meets the head first, at its far end.
        let (bone, entry) =
            skewered_bone(&bones(), Vec3::new(100.0, 0.0, 0.0), Vec3::X).expect("a bone");
        assert_eq!(bone, 1);
        assert!(entry.distance(Vec3::new(80.0, 0.0, 0.0)) < STEP, "{entry}");
        // Off to one side the line passes the head and meets the torso.
        let (bone, entry) =
            skewered_bone(&[bones()[0]], Vec3::new(100.0, 3.0, 0.0), Vec3::X).expect("a bone");
        assert_eq!(bone, 0);
        assert!((entry.x - 64.0).abs() < 1.0, "{entry}");
    }

    #[test]
    fn a_line_that_just_misses_takes_the_nearest_bone_and_a_far_one_none() {
        let (bone, entry) =
            skewered_bone(&bones(), Vec3::new(100.0, 10.0, 0.0), Vec3::X).expect("a near miss");
        assert!(bone <= 1);
        assert!((entry.y).abs() < 1e-4, "on the bone's axis: {entry}");
        assert_eq!(
            skewered_bone(&bones(), Vec3::new(100.0, 40.0, 0.0), Vec3::X),
            None
        );
        // A body more than 128 units from the wall is not reached.
        assert_eq!(
            skewered_bone(&bones(), Vec3::new(300.0, 0.0, 0.0), Vec3::X),
            None
        );
    }
}
