//! The Gravity Gun's look, from HL2DM's `C_WeaponPhysCannon`: its prongs open
//! (a layer of the view model, driven by the `active` pose parameter), sprites
//! glow on its attachments, and arcs of lightning crackle between the prong
//! tips and the muzzle while it holds something.
//!
//! The prongs open when the gun has something to take (a prop it could pull
//! or grab is under the crosshair), is pulling or is holding one, and close
//! again otherwise. `active` is an interpolated value
//! (`m_ElementParameter`) that moves to its target along a spline: to open
//! takes 0.2 s, to close 0.5 s.
//!
//! The sprites are the game's `CPhysCannonEffect`s, each with a size (units
//! across, drawn facing the camera) and a strength that move to their targets
//! the same way:
//!
//! | sprite | where | ready | holding |
//! |---|---|---|---|
//! | `orangecore1` | muzzle | 14, half | 14, full |
//! | `glow04` tinted orange | the forks' bases, middles and tips | 6.4, a quarter | 8, a quarter |
//! | `orangeflare1` | the forks' tips | - | 6.4, full |
//!
//! The core is the game's size. The glows are an eighth of what `DoEffectReady`
//! and `DoEffectHolding` ask for (51.2 and 64): drawn that big they cover the
//! screen, which the game's frames do not, so they were matched to those.
//! Strengths are blended as the game does, in gamma space, which in linear
//! light is the strength to the power of 2.2.
//!
//! And, while it holds a prop, three `orangelight1` beams of amplitude 16
//! (see [`crate::lightning`]) from each fork's tip to the muzzle.
//!
//! Throwing a prop sets off one more sprite (`DoEffectLaunch`), `orangecore2`
//! at the muzzle: a blast that grows from 8 to 64 units in a tenth of a
//! second while it fades out over a fifth (see [`crate::physcannon_impact`]
//! for the rest of the launch effect).

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use rl_assets::{SOURCE, SpriteAsset, coords};
use rl_data::Shot;
use rl_sim::tools::manipulator::{Aim, liftable_target};

use crate::AppState;
use crate::animation::{Animated, Attachment, SequencePlayer};
use crate::camera::FlyCam;
use crate::effect_material::{EffectMaterial, effect_material};
use crate::lightning::{self, BeamShape, Random};
use crate::net::Game;
use crate::physcannon_impact::PhyscannonImpact;
use crate::player::{LocalPlayer, ViewAngles};
use crate::settings::Bindings;
use crate::viewmodel::{VIEW_LAYER, ViewModel};

/// Seconds the prongs take to open...
pub(crate) const OPEN_SECS: f32 = 0.2;
/// ...and to close.
pub(crate) const CLOSE_SECS: f32 = 0.5;

/// A value on its way to a target along a spline, as `InterpolatedValue`
/// moves HL2's: restarted from where it is whenever the target changes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Element {
    from: f32,
    to: f32,
    start: f32,
    duration: f32,
}

impl Element {
    /// A value that is `value` and stays so until sent elsewhere.
    pub(crate) fn at(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start: 0.0,
            duration: 0.0,
        }
    }

    /// A value that is `from` at `now` and reaches `to` after `duration`
    /// seconds.
    pub(crate) fn between(from: f32, to: f32, duration: f32, now: f32) -> Self {
        Self {
            from,
            to,
            start: now,
            duration,
        }
    }

    /// Where it is at time `now`, seconds.
    pub(crate) fn value(&self, now: f32) -> f32 {
        if self.duration <= 0.0 {
            return self.to;
        }
        let t = ((now - self.start) / self.duration).clamp(0.0, 1.0);
        let eased = t * t * (3.0 - 2.0 * t);
        self.from + (self.to - self.from) * eased
    }

    /// Sends it to `target` over `duration` from where it is at `now`, if
    /// that is not already where it is headed.
    pub(crate) fn go_to(&mut self, target: f32, duration: f32, now: f32) {
        if (self.to - target).abs() < f32::EPSILON {
            return;
        }
        *self = Self {
            from: self.value(now),
            to: target,
            start: now,
            duration,
        };
    }
}

/// The prongs' value, kept with the Gravity Gun's view model.
#[derive(Component, Debug, Default)]
pub(crate) struct Prongs(pub(crate) Element);

/// What the gun is doing, as far as how it looks goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Look {
    /// Nothing to take: prongs shut, no glow.
    #[default]
    Shut,
    /// A prop it could take is under the crosshair: prongs open, a glow.
    Ready,
    /// Pulling a prop in or holding one: a brighter glow, flares and arcs.
    Working,
}

/// The Gravity Gun in hand this frame, if it is.
#[derive(Resource, Debug, Default)]
pub(crate) struct GunLook {
    /// Its view model.
    pub(crate) view: Option<Entity>,
    /// What it is doing.
    pub(crate) look: Look,
}

/// The game's sprites for the effects.
#[derive(Resource, Default)]
struct GunSprites {
    core: Handle<SpriteAsset>,
    glow: Handle<SpriteAsset>,
    flare: Handle<SpriteAsset>,
    arc: Handle<SpriteAsset>,
    blast: Handle<SpriteAsset>,
}

fn load_sprites(mut sprites: ResMut<GunSprites>, assets: Res<AssetServer>) {
    let load = |name: &str| assets.load(format!("{SOURCE}://materials/sprites/{name}.vmt"));
    sprites.core = load("orangecore1");
    sprites.glow = load("glow04_noz");
    sprites.flare = load("orangeflare1");
    sprites.arc = load("orangelight1_noz");
    sprites.blast = load("orangecore2");
}

/// Reads what the gun is doing, from the local player's game state and
/// whether the alternate button is held, and opens or shuts the prongs.
fn read_gun(
    time: Res<Time>,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    angles: Res<ViewAngles>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    bindings: Res<Bindings>,
    mut look: ResMut<GunLook>,
    mut views: Query<(
        Entity,
        &ViewModel,
        &Animated,
        &mut SequencePlayer,
        &mut Prongs,
    )>,
) {
    *look = GunLook::default();
    let (Some(sim), Some(local)) = (game.sim(), local) else {
        return;
    };
    let now = time.elapsed_secs();
    for (entity, view, animated, mut player, mut prongs) in &mut views {
        let manipulator = sim.arsenal().weapon(view.weapon()).is_some_and(|weapon| {
            weapon
                .fire_modes()
                .any(|fire| matches!(fire.shot, Shot::Manipulator))
        });
        let Some(layer) = animated.layer("prong_blend").filter(|_| manipulator) else {
            continue;
        };
        let pulling_wanted = bindings.pressed("attack2", &keys, mouse.as_deref());
        let state = gun_state(sim, local.id, *angles, pulling_wanted);
        *look = GunLook {
            view: Some(entity),
            look: state,
        };
        if state == Look::Shut {
            prongs.0.go_to(0.0, CLOSE_SECS, now);
        } else {
            prongs.0.go_to(1.0, OPEN_SECS, now);
        }
        player.set_layer(layer, prongs.0.value(now), 1.0);
    }
}

/// What the gun of `player` is doing: holding a prop, pulling one in
/// (`pulling_wanted`, the alternate button held, on a prop beyond grab range),
/// ready to take one, or nothing.
fn gun_state(
    sim: &rl_sim::simulation::Simulation,
    player: rl_sim::simulation::PlayerId,
    angles: ViewAngles,
    pulling_wanted: bool,
) -> Look {
    if sim
        .player(player)
        .is_some_and(|p| p.manipulator.held().is_some())
    {
        return Look::Working;
    }
    let Some(eye) = sim.eye_position(player) else {
        return Look::Shut;
    };
    let aim = Aim {
        eye,
        pitch: angles.pitch,
        yaw: angles.yaw,
    };
    let config = &sim.data().manipulator;
    let Some((_, point)) = liftable_target(&aim, sim.world(), sim.physics(), config) else {
        return Look::Shut;
    };
    if pulling_wanted && point.distance(eye) > config.grab.range {
        Look::Working
    } else {
        Look::Ready
    }
}

/// The sprites that glow on the gun's attachments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The disc around the muzzle, `orangecore1`.
    Core,
    /// The soft orange light on the forks, `glow04`.
    Glow,
    /// The flare on each fork's tip, `orangeflare1`, while holding.
    EndCap,
    /// The blast at the muzzle when the gun throws something,
    /// `orangecore2`; set off by a launch rather than by the gun's state.
    Blast,
}

/// Where each sprite goes: its kind and the attachment it sits on. The view
/// model has no third fork's glows, but has its tip.
const PLACEMENTS: [(Kind, &str); 11] = [
    (Kind::Core, "muzzle"),
    (Kind::Blast, "muzzle"),
    (Kind::Glow, "fork1b"),
    (Kind::Glow, "fork1m"),
    (Kind::Glow, "fork1t"),
    (Kind::Glow, "fork2b"),
    (Kind::Glow, "fork2m"),
    (Kind::Glow, "fork2t"),
    (Kind::EndCap, "fork1t"),
    (Kind::EndCap, "fork2t"),
    (Kind::EndCap, "fork3t"),
];

/// The prong tips the arcs start from; they end at the muzzle.
const ARC_TIPS: [&str; 3] = ["fork1t", "fork2t", "fork3t"];

/// Where a sprite is headed: how big (units across) and how strong (0 to 1),
/// and seconds each takes to get there.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Goal {
    size: f32,
    size_secs: f32,
    strength: f32,
    strength_secs: f32,
}

impl Kind {
    /// What the sprite does in `look`, if it shows at all. The numbers are
    /// those of `DoEffectReady` and `DoEffectHolding`, the strengths out of
    /// 255.
    fn goal(self, look: Look) -> Option<Goal> {
        let goal = |size, size_secs, strength: f32, strength_secs| Goal {
            size,
            size_secs,
            strength: strength / 255.0,
            strength_secs,
        };
        match (self, look) {
            (_, Look::Shut) | (Self::EndCap, Look::Ready) | (Self::Blast, _) => None,
            (Self::Core, Look::Ready) => Some(goal(14.0, 0.2, 128.0, 0.2)),
            (Self::Core, Look::Working) => Some(goal(14.0, 0.2, 255.0, 0.1)),
            (Self::Glow, Look::Ready) => Some(goal(6.4, 0.2, 64.0, 0.2)),
            (Self::Glow, Look::Working) => Some(goal(8.0, 0.2, 64.0, 0.2)),
            (Self::EndCap, Look::Working) => Some(goal(6.4, 0.2, 255.0, 0.2)),
        }
    }

    /// The size it starts at, units (`StartEffects`).
    fn first_size(self) -> f32 {
        match self {
            Self::Core => 0.0,
            Self::Blast => BLAST_SIZE.0,
            Self::Glow | Self::EndCap => 6.4,
        }
    }

    /// The colour it is drawn with: the glows are tinted orange
    /// (255, 128, 0), the rest are as their textures are.
    fn tint(self) -> Vec3 {
        match self {
            Self::Glow => Vec3::new(1.0, 128.0 / 255.0, 0.0),
            Self::Core | Self::EndCap | Self::Blast => Vec3::ONE,
        }
    }
}

/// How big the launch's blast is, in units across, when it starts and when
/// it is done growing, and the seconds it takes to grow and to fade out
/// (`DoEffectLaunch`).
const BLAST_SIZE: (f32, f32) = (8.0, 64.0);
const BLAST_GROW_SECS: f32 = 0.1;
const BLAST_FADE_SECS: f32 = 0.2;

/// Seconds a sprite takes to go out when the gun has nothing to do.
const FADE_OUT_SECS: f32 = 0.1;

/// A sprite on one of the view model's attachments.
#[derive(Component)]
struct Glow {
    view: Entity,
    attachment: Entity,
    kind: Kind,
    /// Units across.
    size: Element,
    /// 0 to 1.
    strength: Element,
    material: Handle<EffectMaterial>,
}

/// What [`shine`] moves: each sprite and where it is.
type GlowItems = (
    &'static mut Glow,
    &'static mut Transform,
    &'static mut GlobalTransform,
);

/// One arc of lightning, drawn in the view model's space.
#[derive(Component)]
struct Arc {
    view: Entity,
    tip: Entity,
    muzzle: Entity,
    mesh: Handle<Mesh>,
}

/// What an arc looks like while the gun holds something: `DoEffectHolding`'s
/// `BeamInfo_t` for the view model (a width of 0 at the tip to 4 at the muzzle,
/// amplitude 16, 8 segments, speed 150).
const ARC: BeamShape = BeamShape {
    widths: (0.0, 4.0),
    amplitude: 16.0,
    segments: 8,
    speed: 150.0,
};

/// The attachment named `name` on `view`'s model.
fn attachment_of(
    view: Entity,
    name: &str,
    attachments: &Query<(Entity, &Attachment)>,
    parents: &Query<&ChildOf>,
) -> Option<Entity> {
    attachments.iter().find_map(|(entity, attachment)| {
        if attachment.0 != name {
            return None;
        }
        let mut at = entity;
        for _ in 0..32 {
            if at == view {
                return Some(entity);
            }
            at = parents.get(at).ok()?.parent();
        }
        None
    })
}

/// A material for a sprite: its texture, added to what is behind it.
fn sprite_material(
    materials: &mut Assets<EffectMaterial>,
    sprite: &SpriteAsset,
) -> Handle<EffectMaterial> {
    materials.add(effect_material(StandardMaterial {
        base_color: Color::linear_rgba(1.0, 1.0, 1.0, 0.0),
        base_color_texture: Some(sprite.image.clone()),
        unlit: true,
        double_sided: true,
        cull_mode: None,
        alpha_mode: AlphaMode::Add,
        ..default()
    }))
}

/// Makes the gun's sprites and arcs when it comes into hand, and removes
/// them when it goes.
fn spawn_effects(
    mut commands: Commands,
    look: Res<GunLook>,
    sprites: Res<GunSprites>,
    assets: Res<Assets<SpriteAsset>>,
    attachments: Query<(Entity, &Attachment)>,
    parents: Query<&ChildOf>,
    glows: Query<(Entity, &Glow)>,
    arcs: Query<(Entity, &Arc)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<EffectMaterial>>,
) {
    for (entity, glow) in &glows {
        if look.view != Some(glow.view) {
            commands.entity(entity).try_despawn();
        }
    }
    for (entity, arc) in &arcs {
        if look.view != Some(arc.view) {
            commands.entity(entity).try_despawn();
        }
    }
    let Some(view) = look.view else {
        return;
    };
    if glows.iter().any(|(_, glow)| glow.view == view) {
        return;
    }
    let (Some(core), Some(glow), Some(flare), Some(arc), Some(blast)) = (
        assets.get(&sprites.core),
        assets.get(&sprites.glow),
        assets.get(&sprites.flare),
        assets.get(&sprites.arc),
        assets.get(&sprites.blast),
    ) else {
        return;
    };
    let quad = meshes.add(Rectangle::new(1.0, 1.0));
    for (kind, name) in PLACEMENTS {
        let Some(attachment) = attachment_of(view, name, &attachments, &parents) else {
            continue;
        };
        let sprite = match kind {
            Kind::Core => core,
            Kind::Glow => glow,
            Kind::EndCap => flare,
            Kind::Blast => blast,
        };
        let material = sprite_material(&mut materials, sprite);
        commands.spawn((
            Glow {
                view,
                attachment,
                kind,
                size: Element::at(kind.first_size()),
                strength: Element::at(0.0),
                material: material.clone(),
            },
            Mesh3d(quad.clone()),
            MeshMaterial3d(material),
            Transform::from_scale(Vec3::ZERO),
            NoFrustumCulling,
            RenderLayers::layer(VIEW_LAYER),
        ));
    }
    // The beam's texture is tiled along it.
    if let Some(mut image) = images.get_mut(&arc.image) {
        image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_v: ImageAddressMode::Repeat,
            ..ImageSamplerDescriptor::linear()
        });
    }
    let material = materials.add(effect_material(StandardMaterial {
        base_color_texture: Some(arc.image.clone()),
        unlit: true,
        double_sided: true,
        cull_mode: None,
        alpha_mode: AlphaMode::Add,
        ..default()
    }));
    let Some(muzzle) = attachment_of(view, "muzzle", &attachments, &parents) else {
        return;
    };
    for name in ARC_TIPS {
        let Some(tip) = attachment_of(view, name, &attachments, &parents) else {
            continue;
        };
        let mesh = meshes.add(Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        ));
        commands.spawn((
            Arc {
                view,
                tip,
                muzzle,
                mesh: mesh.clone(),
            },
            Mesh3d(mesh),
            MeshMaterial3d(material.clone()),
            Transform::IDENTITY,
            Visibility::Hidden,
            NoFrustumCulling,
            RenderLayers::layer(VIEW_LAYER),
        ));
    }
}

/// Sends each sprite to where the gun's state wants it, and keeps it on its
/// attachment, facing the camera.
fn shine(
    time: Res<Time>,
    look: Res<GunLook>,
    attachments: Query<&GlobalTransform, With<Attachment>>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut glows: Query<GlowItems, (Without<Attachment>, Without<FlyCam>)>,
    mut materials: ResMut<Assets<EffectMaterial>>,
) {
    let now = time.elapsed_secs();
    for (mut glow, mut transform, mut global) in &mut glows {
        match glow.kind.goal(look.look) {
            Some(goal) => {
                glow.size.go_to(goal.size, goal.size_secs, now);
                glow.strength.go_to(goal.strength, goal.strength_secs, now);
            }
            None => glow.strength.go_to(0.0, FADE_OUT_SECS, now),
        }
        let strength = glow.strength.value(now);
        if let Ok(attachment) = attachments.get(glow.attachment) {
            transform.translation = attachment.translation();
        }
        if let Ok(camera) = camera.single() {
            transform.rotation = camera.to_scale_rotation_translation().1;
        }
        let across = glow.size.value(now) * coords::METERS_PER_UNIT;
        transform.scale = if strength > 0.001 {
            Vec3::new(across, across, 1.0)
        } else {
            Vec3::ZERO
        };
        *global = GlobalTransform::from(*transform);
        if let Some(mut material) = materials.get_mut(&glow.material) {
            let tint = glow.kind.tint();
            material.base.base_color =
                Color::linear_rgba(tint.x, tint.y, tint.z, strength.powf(2.2));
        }
    }
}

/// Sets off the blast at the muzzle when the local player's gun throws
/// something: it grows and fades as `DoEffectLaunch` says.
fn kick_blast(
    time: Res<Time>,
    look: Res<GunLook>,
    mut impacts: MessageReader<PhyscannonImpact>,
    mut glows: Query<&mut Glow>,
) {
    let now = time.elapsed_secs();
    for impact in impacts.read() {
        if !impact.local {
            continue;
        }
        for mut glow in &mut glows {
            if glow.kind == Kind::Blast && Some(glow.view) == look.view {
                glow.size = Element::between(BLAST_SIZE.0, BLAST_SIZE.1, BLAST_GROW_SECS, now);
                glow.strength = Element::between(1.0, 0.0, BLAST_FADE_SECS, now);
            }
        }
    }
}

/// Redraws the arcs, new every frame, while the gun is working: from each
/// prong's tip to the muzzle.
fn arcs(
    time: Res<Time>,
    look: Res<GunLook>,
    attachments: Query<&GlobalTransform, With<Attachment>>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut arcs: Query<(&Arc, &mut Visibility)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut random: Local<Option<Random>>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    let random = random.get_or_insert_with(|| Random::new(0x51CA_77E5));
    let now = time.elapsed_secs();
    for (arc, mut visibility) in &mut arcs {
        let ends = (look.look == Look::Working)
            .then(|| {
                attachments
                    .get(arc.tip)
                    .ok()
                    .zip(attachments.get(arc.muzzle).ok())
            })
            .flatten();
        let (Some((tip, muzzle)), Some(mut mesh)) = (ends, meshes.get_mut(&arc.mesh)) else {
            visibility.set_if_neq(Visibility::Hidden);
            continue;
        };
        visibility.set_if_neq(Visibility::Inherited);
        let unit = coords::METERS_PER_UNIT;
        let eye = camera.translation();
        let noise = lightning::fractal_noise(random);
        let toward_camera = |at: Vec3| (eye - at * unit).normalize_or_zero();
        let start = tip.translation() / unit;
        let end = muzzle.translation() / unit;
        let middle = start.midpoint(end);
        let points = lightning::points(start, end, toward_camera(middle), &ARC, &noise, now);
        let (positions, uvs, indices) = lightning::ribbon(&points, toward_camera);
        let positions: Vec<[f32; 3]> = positions
            .into_iter()
            .map(|p| (Vec3::from(p) * unit).to_array())
            .collect();
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_NORMAL,
            vec![[0.0, 0.0, 1.0]; positions.len()],
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
        mesh.insert_indices(Indices::U32(indices));
    }
}

/// Draws the Gravity Gun's prongs, sprites and arcs.
pub(crate) struct GravityGunPlugin;

impl Plugin for GravityGunPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GunLook>()
            .init_resource::<GunSprites>()
            .add_systems(Startup, load_sprites)
            .add_systems(
                Update,
                (read_gun, spawn_effects, kick_blast)
                    .chain()
                    .after(crate::viewmodel::animate_view_model)
                    .after(crate::camera::CameraPlaced)
                    .run_if(in_state(AppState::InGame)),
            )
            // On the attachments, which are where they are this frame only
            // once transforms are propagated.
            .add_systems(
                PostUpdate,
                (shine, arcs)
                    .chain()
                    .in_set(crate::camera::AttachedEffects)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_opens_along_a_spline_in_two_tenths_of_a_second() {
        let mut element = Element::default();
        element.go_to(1.0, OPEN_SECS, 10.0);
        assert_eq!(element.value(10.0), 0.0);
        assert!(
            (element.value(10.1) - 0.5).abs() < 1e-5,
            "halfway at the middle"
        );
        assert!(element.value(10.05) < 0.25, "slow to start");
        assert_eq!(element.value(10.2), 1.0);
        assert_eq!(element.value(11.0), 1.0);
    }

    #[test]
    fn closing_starts_from_wherever_opening_got_to() {
        let mut element = Element::default();
        element.go_to(1.0, OPEN_SECS, 0.0);
        let partway = element.value(0.1);
        element.go_to(0.0, CLOSE_SECS, 0.1);
        assert!((element.value(0.1) - partway).abs() < 1e-5, "no jump");
        assert_eq!(element.value(0.6), 0.0);
    }

    #[test]
    fn the_same_target_does_not_restart_it() {
        let mut element = Element::default();
        element.go_to(1.0, OPEN_SECS, 0.0);
        let before = element.value(0.15);
        element.go_to(1.0, OPEN_SECS, 0.15);
        assert!((element.value(0.15) - before).abs() < 1e-6);
    }

    #[test]
    fn a_value_made_at_a_number_is_there() {
        let element = Element::at(6.4);
        assert_eq!(element.value(0.0), 6.4);
        assert_eq!(element.value(100.0), 6.4);
    }

    #[test]
    fn the_sprites_do_what_the_game_says_when_ready_and_holding() {
        let core = |look| Kind::Core.goal(look).unwrap();
        assert_eq!(
            (core(Look::Ready).size, core(Look::Working).size),
            (14.0, 14.0)
        );
        assert!(
            (core(Look::Ready).strength - 0.5).abs() < 0.01,
            "128 of 255"
        );
        assert_eq!(core(Look::Working).strength, 1.0);
        let glow = |look| Kind::Glow.goal(look).unwrap();
        assert_eq!(
            (glow(Look::Ready).size, glow(Look::Working).size),
            (6.4, 8.0)
        );
        assert!(
            (glow(Look::Ready).strength - 0.25).abs() < 0.01,
            "64 of 255"
        );
        assert_eq!(Kind::EndCap.goal(Look::Ready), None, "only while holding");
        assert_eq!(Kind::EndCap.goal(Look::Working).unwrap().size, 6.4);
    }

    #[test]
    fn nothing_shows_while_the_gun_has_nothing_to_do() {
        for kind in [Kind::Core, Kind::Glow, Kind::EndCap, Kind::Blast] {
            assert_eq!(kind.goal(Look::Shut), None);
        }
    }

    #[test]
    fn the_blast_belongs_to_launches_not_to_the_guns_state() {
        for look in [Look::Shut, Look::Ready, Look::Working] {
            assert_eq!(Kind::Blast.goal(look), None);
        }
        assert_eq!(Kind::Blast.first_size(), 8.0);
        assert_eq!(Kind::Blast.tint(), Vec3::ONE);
        assert!(PLACEMENTS.contains(&(Kind::Blast, "muzzle")));
    }

    #[test]
    fn the_blast_grows_in_a_tenth_of_a_second_and_fades_over_a_fifth() {
        let size = Element::between(BLAST_SIZE.0, BLAST_SIZE.1, BLAST_GROW_SECS, 5.0);
        let strength = Element::between(1.0, 0.0, BLAST_FADE_SECS, 5.0);
        assert_eq!((size.value(5.0), strength.value(5.0)), (8.0, 1.0));
        assert!(
            (size.value(5.05) - 36.0).abs() < 1e-3,
            "halfway along its spline"
        );
        assert_eq!(size.value(5.1), 64.0);
        assert_eq!(size.value(6.0), 64.0, "stays big while it fades");
        assert!((strength.value(5.1) - 0.5).abs() < 1e-5);
        assert_eq!(strength.value(5.2), 0.0);
    }

    #[test]
    fn a_blast_on_its_way_out_is_not_restarted_by_the_guns_idle_fade() {
        let mut strength = Element::between(1.0, 0.0, BLAST_FADE_SECS, 0.0);
        let before = strength.value(0.1);
        strength.go_to(0.0, FADE_OUT_SECS, 0.1);
        assert_eq!(strength.value(0.1), before);
        assert_eq!(strength.value(0.2), 0.0);
    }

    #[test]
    fn only_the_glows_are_orange() {
        assert_eq!(Kind::Core.tint(), Vec3::ONE);
        assert_eq!(Kind::EndCap.tint(), Vec3::ONE);
        let glow = Kind::Glow.tint();
        assert_eq!((glow.x, glow.z), (1.0, 0.0));
        assert!((glow.y - 0.5).abs() < 0.01);
    }
}
