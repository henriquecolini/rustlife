//! Models the game moves: pickups, projectiles and other players.
//!
//! Each is drawn with its model through the props pipeline
//! (`props::spawn_model`: lit like any prop, but with no physics
//! body of its own) and placed from the game:
//!
//! - **pickups** where the map put them, hidden while taken;
//! - **projectiles** along their flight, turned to face where they fly,
//!   appearing and vanishing with them;
//! - **other players** at their feet, turned to their view's yaw, hidden
//!   while dead; models come and go as players join and leave. Each plays
//!   the sequence of what they are doing with the weapon in hand
//!   (`animate_players`), and holds that weapon's world model: its
//!   `ValveBiped.Weapon_bone` laid on the hand's bone of that name, as the
//!   engine merges a weapon's bones with its holder's.
//!
//! Projectiles and other players are drawn where [`Drawn`] places them:
//! interpolated between two of the server's snapshots, a little in the
//! past, every frame.

use std::collections::HashMap;

use bevy::prelude::*;
use rl_assets::coords;
use rl_data::PlayerAnimationConfig;
use rl_sim::movement::PlayerState;
use rl_sim::simulation::{PlayerId, Simulation};

use crate::AppState;
use crate::animation::{Animated, SequencePlayer};
use crate::effect_material::{EffectMaterial, effect_material};
use crate::net::{Drawn, Game};
use crate::player::Gameplay;
use crate::props::{LightAt, ModelHandles, Moving, PhysicsBody, spawn_model};
use crate::ragdolls::Ragdoll;

/// Draws the game's pickups, projectiles and other players.
pub struct ActorsPlugin;

impl Plugin for ActorsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PickupsSpawned>()
            .add_systems(OnEnter(AppState::InGame), reset)
            .add_systems(
                Update,
                (
                    spawn_pickups,
                    show_pickups,
                    draw_players,
                    animate_players,
                    hold_weapons,
                    grip_weapons,
                    draw_projectiles,
                    glow_projectiles,
                )
                    .chain()
                    .after(crate::camera::CameraPlaced)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Whether the pickups of the current game have models.
#[derive(Resource, Default)]
struct PickupsSpawned(bool);

/// The model of a pickup, by index into the simulation's pickups.
#[derive(Component)]
pub(crate) struct PickupView(usize);

/// The model of another player.
#[derive(Component)]
pub(crate) struct PlayerView(pub(crate) PlayerId);

/// The world model of a weapon a player holds, and who holds it.
#[derive(Component)]
struct HeldWeapon {
    holder: Entity,
}

/// Which weapon a player model holds, and the entity of its model.
#[derive(Component)]
struct Holding {
    weapon: Option<rl_sim::weapons::WeaponId>,
    model: Option<Entity>,
}

/// A held weapon not yet laid in its holder's hand (its model, or the
/// holder's skeleton, is loading).
#[derive(Component)]
struct Gripping;

/// The model of a projectile, by its id.
#[derive(Component)]
struct ProjectileView(u32);

fn reset(mut spawned: ResMut<PickupsSpawned>) {
    spawned.0 = false;
}

/// Gives every pickup a model once the game's simulation exists.
fn spawn_pickups(
    mut commands: Commands,
    mut spawned: ResMut<PickupsSpawned>,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
    game: Game,
) {
    let Some(sim) = game.sim() else {
        return;
    };
    if spawned.0 {
        return;
    }
    spawned.0 = true;
    for (index, pickup) in sim.pickups().iter().enumerate() {
        let transform = Transform {
            translation: coords::position(pickup.origin.to_array()),
            rotation: coords::rotation(pickup.model_angles()),
            ..default()
        };
        let entity = spawn_model(
            &mut commands,
            &mut models,
            &asset_server,
            &pickup.model,
            transform,
        );
        commands.entity(entity).insert(PickupView(index));
        if let Some(body) = pickup.body {
            commands.entity(entity).insert((PhysicsBody(body), Moving));
        }
    }
}

/// Hides taken pickups. Stations (wall chargers) stay on their wall while
/// they recharge.
fn show_pickups(game: Game, mut pickups: Query<(&PickupView, &mut Visibility)>) {
    let Some(sim) = game.sim() else {
        return;
    };
    for (view, mut visibility) in &mut pickups {
        let shown = sim
            .pickups()
            .get(view.0)
            .is_some_and(|pickup| pickup.available() || pickup.station.is_some());
        visibility.set_if_neq(if shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}

/// The transform of a player model standing at `feet` facing `yaw`.
fn player_transform(feet: Vec3, yaw: f32) -> Transform {
    Transform {
        translation: coords::position(feet.to_array()),
        rotation: coords::rotation([0.0, yaw, 0.0]),
        ..default()
    }
}

/// Places every other player where they are drawn this frame, hiding the
/// dead; gives newcomers a model and takes it from those who left.
fn draw_players(
    mut commands: Commands,
    drawn: Res<Drawn>,
    game: Game,
    gameplay: Res<Gameplay>,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
    time: Res<Time>,
    corpses: Query<&Ragdoll>,
    mut views: Query<(Entity, &PlayerView, &mut Transform, &mut Visibility), Without<Ragdoll>>,
) {
    let Some(world) = &drawn.0 else {
        return;
    };
    let local = game.local();
    // The interpolated world is deliberately a little behind the server.
    // A player the replica already knows is dead must be hidden at once,
    // even if the pose being interpolated still says alive; conversely a
    // respawn waits for the drawn pose to catch up rather than appearing at
    // the corpse's position.
    let authoritative_alive = |id: PlayerId| {
        game.sim()
            .and_then(|sim| sim.player(id))
            .is_some_and(rl_sim::simulation::Player::alive)
    };
    // A kill message can arrive just before the snapshot that marks the
    // player dead. Until that snapshot catches up, the fresh corpse is the
    // authority on them being down.
    let just_died = |id: PlayerId| {
        corpses
            .iter()
            .any(|corpse| corpse.player == id && time.elapsed_secs() - corpse.born < 1.0)
    };
    let mut shown = Vec::new();
    for (entity, view, mut transform, mut visibility) in &mut views {
        match world.players.get(&view.0).filter(|_| Some(view.0) != local) {
            Some(pose) => {
                *transform = player_transform(pose.feet, pose.yaw);
                visibility.set_if_neq(
                    if pose.alive && authoritative_alive(view.0) && !just_died(view.0) {
                        Visibility::Inherited
                    } else {
                        Visibility::Hidden
                    },
                );
                shown.push(view.0);
            }
            None => commands.entity(entity).try_despawn(),
        }
    }
    let player_models = &gameplay.0.player.models;
    for (&id, pose) in &world.players {
        if Some(id) == local || shown.contains(&id) {
            continue;
        }
        let Some(model) = player_models.get(id.index() % player_models.len().max(1)) else {
            continue;
        };
        let entity = spawn_model(
            &mut commands,
            &mut models,
            &asset_server,
            model,
            player_transform(pose.feet, pose.yaw),
        );
        commands.entity(entity).insert((
            PlayerView(id),
            Moving,
            // A model spawned for a player who is already dead must not
            // show its rest pose for a frame before the next pass hides it.
            if pose.alive && authoritative_alive(id) && !just_died(id) {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            },
        ));
    }
}

/// What a player is doing, as their model shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stance {
    Idle,
    Run,
    Crouch,
    CrouchWalk,
    Jump,
}

impl Stance {
    /// The stance of a player in `state`.
    fn of(state: &PlayerState, config: &PlayerAnimationConfig) -> Self {
        let moving = state.speed_2d() > config.still_speed;
        match (
            state.on_ground || state.ladder.is_some(),
            state.ducked,
            moving,
        ) {
            (false, ..) => Self::Jump,
            (true, true, false) => Self::Crouch,
            (true, true, true) => Self::CrouchWalk,
            (true, false, false) => Self::Idle,
            (true, false, true) => Self::Run,
        }
    }

    fn prefix(self, config: &PlayerAnimationConfig) -> &str {
        match self {
            Self::Idle => &config.idle,
            Self::Run => &config.run,
            Self::Crouch => &config.crouch,
            Self::CrouchWalk => &config.crouch_walk,
            Self::Jump => &config.jump,
        }
    }
}

/// The direction a player moves relative to where they face, degrees in
/// `-180..=180` (the `move_yaw` of a run's blends).
fn move_yaw(state: &PlayerState, yaw: f32) -> f32 {
    let heading = state.velocity.y.atan2(state.velocity.x).to_degrees();
    (heading - yaw + 180.0).rem_euclid(360.0) - 180.0
}

/// Plays on each other player's model the sequence of their stance with
/// the weapon in hand (`run_smg1`), a run blended by direction and paced by
/// speed.
fn animate_players(
    game: Game,
    drawn: Res<Drawn>,
    mut views: Query<(&PlayerView, &Animated, &mut SequencePlayer)>,
) {
    let Some(sim) = game.sim() else {
        return;
    };
    let config = &sim.data().player.animation;
    for (view, animated, mut sequences) in &mut views {
        let Some(player) = sim.player(view.0) else {
            continue;
        };
        let yaw = drawn
            .0
            .as_ref()
            .and_then(|world| world.players.get(&view.0))
            .map_or(player.look.1, |pose| pose.yaw);
        let state = &player.state;
        let stance = Stance::of(state, config);
        let hold = hold_of(sim, player.inventory.active(), config);
        let Some(sequence) = player_sequence(animated, stance.prefix(config), hold, config) else {
            continue;
        };
        let rate = match stance {
            Stance::Run => state.speed_2d() / config.run_speed,
            Stance::CrouchWalk => state.speed_2d() / config.crouch_speed,
            _ => 1.0,
        };
        sequences.fade = config.fade;
        sequences.play(sequence, move_yaw(state, yaw), rate.clamp(0.25, 2.5));
    }
}

/// Gives each other player the world model of the weapon in their hand,
/// changing it when they switch; keeps it lit where its holder is.
fn hold_weapons(
    mut commands: Commands,
    game: Game,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
    mut views: Query<(Entity, &PlayerView, &GlobalTransform, Option<&mut Holding>), With<Animated>>,
    mut lights: Query<&mut LightAt, With<HeldWeapon>>,
) {
    let Some(sim) = game.sim() else {
        return;
    };
    for (holder, view, global, holding) in &mut views {
        let light = global.translation() + Vec3::Y * HELD_LIGHT_HEIGHT;
        let weapon = sim
            .player(view.0)
            .and_then(|player| player.inventory.active());
        if let Some(holding) = &holding
            && holding.weapon == weapon
        {
            if let Some(mut at) = holding.model.and_then(|model| lights.get_mut(model).ok()) {
                at.0 = light;
            }
            continue;
        }
        if let Some(old) = holding.as_ref().and_then(|holding| holding.model) {
            commands.entity(old).try_despawn();
        }
        let model = weapon
            .and_then(|weapon| sim.arsenal().weapon(weapon))
            .map(|config| {
                let entity = spawn_model(
                    &mut commands,
                    &mut models,
                    &asset_server,
                    &config.world_model,
                    Transform::IDENTITY,
                );
                commands.entity(entity).insert((
                    HeldWeapon { holder },
                    Gripping,
                    Moving,
                    LightAt(light),
                    ChildOf(holder),
                    Visibility::Hidden,
                ));
                entity
            });
        commands.entity(holder).insert(Holding { weapon, model });
    }
}

/// How high above its holder's feet a held weapon is lit, metres (40
/// units).
const HELD_LIGHT_HEIGHT: f32 = 40.0 * coords::METERS_PER_UNIT;

/// Lays each newly loaded held weapon on its holder's hand, as the engine
/// merges bones: the first of the weapon's bones that the holder also has
/// (`ValveBiped.Bip01_R_Hand`) is laid on the holder's, by placing the
/// weapon's root at the inverse of that bone's rest pose.
fn grip_weapons(
    mut commands: Commands,
    mut weapons: Query<(Entity, &HeldWeapon, &crate::props::Prop, &mut Transform), With<Gripping>>,
    holders: Query<&Children>,
    joints: Query<(Entity, &Name, Option<&Children>)>,
    models: Res<Assets<rl_assets::ModelAsset>>,
    asset_server: Res<AssetServer>,
) {
    for (entity, held, prop, mut transform) in &mut weapons {
        let Some(model) = models.get(&prop.model) else {
            if asset_server.load_state(&prop.model).is_failed() {
                commands.entity(entity).remove::<Gripping>();
            }
            continue;
        };
        let Ok(children) = holders.get(held.holder) else {
            continue;
        };
        let Some(animation) = &model.animation else {
            // A one-bone weapon model: in the hand as it is, if the holder
            // has one.
            if let Some(hand) = find_joint(children, &joints, HAND_BONE) {
                commands
                    .entity(entity)
                    .insert((ChildOf(hand), Visibility::Inherited));
            }
            commands.entity(entity).remove::<Gripping>();
            continue;
        };
        let skeleton = &animation.skeleton;
        let shared = skeleton
            .joints
            .iter()
            .enumerate()
            .find_map(|(index, joint)| Some((index, find_joint(children, &joints, &joint.name)?)));
        // The holder's joints may not be spawned yet; try again then.
        let Some((bone, joint)) = shared else {
            continue;
        };
        if let Some(rest) = skeleton.rest_globals().get(bone) {
            *transform = Transform::from_matrix(rest.inverse());
        }
        commands
            .entity(entity)
            .remove::<Gripping>()
            .insert((ChildOf(joint), Visibility::Inherited));
    }
}

/// The holder's bone a weapon model without a skeleton goes in.
const HAND_BONE: &str = "ValveBiped.Bip01_R_Hand";

/// The joint named `name` among the descendants of an entity with
/// `children`.
fn find_joint(
    children: &Children,
    joints: &Query<(Entity, &Name, Option<&Children>)>,
    name: &str,
) -> Option<Entity> {
    let mut stack: Vec<Entity> = children.iter().collect();
    // Skeletons are a few dozen joints; the bound stops a malformed tree.
    for _ in 0..1024 {
        let entity = stack.pop()?;
        let Ok((entity, joint, below)) = joints.get(entity) else {
            continue;
        };
        if joint.as_str().eq_ignore_ascii_case(name) {
            return Some(entity);
        }
        if let Some(below) = below {
            stack.extend(below.iter());
        }
    }
    None
}

/// How the weapon in hand is held: its data's hold, else the default.
fn hold_of<'a>(
    sim: &'a Simulation,
    weapon: Option<rl_sim::weapons::WeaponId>,
    config: &'a PlayerAnimationConfig,
) -> &'a str {
    weapon
        .and_then(|weapon| sim.arsenal().weapon(weapon))
        .and_then(|weapon| weapon.hold.as_deref())
        .unwrap_or(&config.default_hold)
}

/// The model's `<prefix>_<hold>` sequence, or `<prefix>_<default hold>`
/// when it has none for this hold.
fn player_sequence(
    animated: &Animated,
    prefix: &str,
    hold: &str,
    config: &PlayerAnimationConfig,
) -> Option<usize> {
    animated
        .sequence(&format!("{prefix}_{hold}"))
        .or_else(|| animated.sequence(&format!("{prefix}_{}", config.default_hold)))
}

/// Places every projectile where it is drawn this frame: new ones get a
/// model, gone ones lose theirs.
fn draw_projectiles(
    mut commands: Commands,
    drawn: Res<Drawn>,
    game: Game,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
    mut views: Query<(Entity, &ProjectileView, &mut Transform)>,
) {
    let (Some(world), Some(sim)) = (&drawn.0, game.sim()) else {
        return;
    };
    let mut shown = Vec::new();
    for (entity, view, mut transform) in &mut views {
        match world
            .projectiles
            .iter()
            .find(|projectile| projectile.id == view.0)
        {
            Some(projectile) => {
                *transform =
                    projectile_transform(projectile.position, flight_direction(projectile));
                shown.push(view.0);
            }
            None => commands.entity(entity).try_despawn(),
        }
    }
    let arsenal = sim.arsenal();
    for projectile in &world.projectiles {
        if shown.contains(&projectile.id) {
            continue;
        }
        let Some(rl_data::Shot::Projectile(config)) = arsenal
            .fire(projectile.weapon, projectile.mode)
            .map(|fire| &fire.shot)
        else {
            continue;
        };
        let entity = spawn_model(
            &mut commands,
            &mut models,
            &asset_server,
            &config.model,
            projectile_transform(projectile.position, flight_direction(projectile)),
        );
        crate::props::set_skin(&mut commands, entity, config.skin);
        commands
            .entity(entity)
            .insert((ProjectileView(projectile.id), Moving));
    }
}

/// How fast an animated glow steps through its frames (the engine's
/// `animatedtextureframerate` for the ball's core).
const GLOW_FPS: f32 = 25.0;

/// One of a glowing projectile's two sprites.
#[derive(Component)]
struct GlowQuad {
    projectile: u32,
    /// The material, whose texture steps through `frames` side by side.
    material: Handle<EffectMaterial>,
    frames: u32,
    /// Half-size, metres.
    size: f32,
    /// How fast it pulses, if it does.
    pulse: bool,
}

/// Draws projectiles that glow (the energy ball) as a halo and a core,
/// turned to face the camera and following the projectile.
fn glow_projectiles(
    mut commands: Commands,
    time: Res<Time>,
    drawn: Res<Drawn>,
    game: Game,
    asset_server: Res<AssetServer>,
    sprites: Res<Assets<rl_assets::SpriteAsset>>,
    camera: Query<&GlobalTransform, With<crate::camera::FlyCam>>,
    mut quads: Query<(Entity, &GlowQuad, &mut Transform)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<EffectMaterial>>,
    mut loaded: Local<HashMap<String, Handle<rl_assets::SpriteAsset>>>,
    mut quad_mesh: Local<Option<Handle<Mesh>>>,
) {
    let (Some(world), Some(sim), Ok(camera)) = (&drawn.0, game.sim(), camera.single()) else {
        return;
    };
    let facing = camera.rotation();
    let pulse = 1.0 + 0.08 * (time.elapsed_secs() * 30.0).sin();
    let mut seen = Vec::new();
    for (entity, quad, mut transform) in &mut quads {
        let Some(projectile) = world.projectiles.iter().find(|p| p.id == quad.projectile) else {
            commands.entity(entity).try_despawn();
            continue;
        };
        seen.push((quad.projectile, quad.size.to_bits()));
        transform.translation = coords::position(projectile.position.to_array());
        transform.rotation = facing;
        let size = if quad.pulse {
            quad.size * pulse
        } else {
            quad.size
        };
        transform.scale = Vec3::splat(2.0 * size);
        if quad.frames > 1 {
            // The game steps the ball's core through its frames at 25 a second.
            let frame = (time.elapsed_secs() * GLOW_FPS) as u32 % quad.frames;
            if let Some(mut material) = materials.get_mut(&quad.material) {
                let frames = quad.frames as f32;
                material.base.uv_transform = bevy::math::Affine2::from_scale_angle_translation(
                    Vec2::new(1.0 / frames, 1.0),
                    0.0,
                    Vec2::new(frame as f32 / frames, 0.0),
                );
            }
        }
    }
    let mesh = quad_mesh
        .get_or_insert_with(|| meshes.add(Rectangle::new(1.0, 1.0)))
        .clone();
    for projectile in &world.projectiles {
        let Some(rl_data::Shot::Projectile(config)) = sim
            .arsenal()
            .fire(projectile.weapon, projectile.mode)
            .map(|fire| &fire.shot)
        else {
            continue;
        };
        let Some(glow) = &config.glow else {
            continue;
        };
        for (name, size, pulses) in [
            (&glow.halo, glow.halo_size, false),
            (&glow.core, glow.core_size, true),
        ] {
            let size = size * coords::METERS_PER_UNIT;
            if seen.contains(&(projectile.id, size.to_bits())) {
                continue;
            }
            let handle = loaded
                .entry(name.clone())
                .or_insert_with(|| {
                    asset_server.load(format!("{}://materials/{name}.vmt", rl_assets::SOURCE))
                })
                .clone();
            let Some(sprite) = sprites.get(&handle) else {
                continue;
            };
            let material = materials.add(effect_material(StandardMaterial {
                base_color: Color::WHITE,
                base_color_texture: Some(sprite.image.clone()),
                unlit: true,
                double_sided: true,
                cull_mode: None,
                alpha_mode: AlphaMode::Add,
                ..default()
            }));
            commands.spawn((
                GlowQuad {
                    projectile: projectile.id,
                    material: material.clone(),
                    frames: sprite.frames,
                    size,
                    pulse: pulses,
                },
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material),
                Transform::from_translation(coords::position(projectile.position.to_array()))
                    .with_scale(Vec3::ZERO),
                crate::world::MapScoped,
            ));
        }
    }
}

/// The way a projectile points: where it flies, or, stuck in a wall, how it
/// was flying.
fn flight_direction(projectile: &rl_sim::weapons::projectile::Projectile) -> Vec3 {
    if projectile.velocity.length_squared() > 1.0 {
        projectile.velocity
    } else {
        projectile.heading
    }
}

/// The transform of a projectile at `position` flying along `velocity`
/// (Source space): models face Source +X.
fn projectile_transform(position: Vec3, velocity: Vec3) -> Transform {
    let direction = velocity.normalize_or(Vec3::X);
    let yaw = direction.y.atan2(direction.x).to_degrees();
    let pitch = -direction.z.asin().to_degrees();
    Transform {
        translation: coords::position(position.to_array()),
        rotation: coords::rotation([pitch, yaw, 0.0]),
        ..default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projectiles_face_where_they_fly() {
        // Flying along Source +Y (Bevy -Z): the model's +X turns to face it.
        let transform = projectile_transform(Vec3::ZERO, Vec3::new(0.0, 500.0, 0.0));
        let facing = transform.rotation * Vec3::X;
        assert!(facing.abs_diff_eq(Vec3::NEG_Z, 1e-5), "{facing}");
        // Climbing: the nose tips up (Bevy +Y).
        let transform = projectile_transform(Vec3::ZERO, Vec3::new(1.0, 0.0, 1.0));
        let facing = transform.rotation * Vec3::X;
        assert!(facing.y > 0.6, "{facing}");
    }

    #[test]
    fn stances_follow_movement() {
        let config = rl_data::GameData::builtin().expect("data").player.animation;
        let mut state = PlayerState::at(Vec3::ZERO);
        state.on_ground = true;
        assert_eq!(Stance::of(&state, &config), Stance::Idle);
        state.velocity = Vec3::new(200.0, 0.0, 0.0);
        assert_eq!(Stance::of(&state, &config), Stance::Run);
        state.ducked = true;
        assert_eq!(Stance::of(&state, &config), Stance::CrouchWalk);
        state.on_ground = false;
        assert_eq!(Stance::of(&state, &config), Stance::Jump);
    }

    #[test]
    fn runs_blend_by_direction_relative_to_the_view() {
        let mut state = PlayerState::at(Vec3::ZERO);
        state.velocity = Vec3::new(0.0, 100.0, 0.0);
        assert!((move_yaw(&state, 90.0)).abs() < 1e-4, "forward");
        assert!((move_yaw(&state, 0.0) - 90.0).abs() < 1e-4, "to the left");
        assert!(
            (move_yaw(&state, 180.0) + 90.0).abs() < 1e-4,
            "to the right"
        );
        assert!(
            (move_yaw(&state, -90.0).abs() - 180.0).abs() < 1e-4,
            "backwards"
        );
    }

    #[test]
    fn players_stand_at_their_feet_facing_their_yaw() {
        let transform = player_transform(Vec3::new(10.0, 0.0, 0.0), 90.0);
        assert!(transform.translation.x > 0.0);
        let facing = transform.rotation * Vec3::X;
        assert!(facing.abs_diff_eq(Vec3::NEG_Z, 1e-5), "{facing}");
    }
}
