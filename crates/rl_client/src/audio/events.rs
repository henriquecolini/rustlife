//! Sounds of what happens in a tick: weapons firing, reloading and being
//! taken in hand, bullets striking, blasts, pickups, deaths and hard
//! landings, the Manipulator, buttons pressed and things breaking.
//!
//! Weapons and items name their sounds in their own data files; the rest
//! come from `sounds.ron`, and buttons and breakables add what the map
//! says of them (a button's sound number, a breakable's material). The
//! local player's own actions play at the listener; everyone else's where
//! they are drawn.

use bevy::prelude::*;
use rl_data::{Shot, SoundsConfig};
use rl_sim::combat::Cause;
use rl_sim::combat::hitscan::Struck;
use rl_sim::pickups::Gives;
use rl_sim::simulation::{MapEvent, PlayerId, Simulation, TickEvents};
use rl_sim::tools::manipulator::ToolEvent;
use rl_sim::weapons::{Mode, WeaponId};
use source_formats::entities::MapEntity;

use super::SoundPlayer;
use crate::AppState;
use crate::net::{Drawn, Game, LevelEntities};
use crate::player::{LocalPlayer, Ticked};

/// Most impact sounds one tick plays: a shotgun's pellets sound as a few
/// hits, not a dozen.
const MAX_IMPACTS: usize = 3;
/// `func_breakable` materials, by the number the map gives.
const MATERIALS: [&str; 9] = [
    "glass", "wood", "metal", "flesh", "cinder", "tile", "computer", "glass", "rock",
];

/// Plays the sounds of tick events.
pub(super) struct EventSoundsPlugin;

impl Plugin for EventSoundsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (preload, play_events).run_if(in_state(AppState::InGame)),
        );
    }
}

/// Starts loading every sound the gameplay data names, once per game.
fn preload(game: Game, mut done: Local<bool>, mut player: SoundPlayer) {
    let Some(sim) = game.sim() else {
        *done = false;
        return;
    };
    if *done {
        return;
    }
    *done = true;
    let data = sim.data();
    let sounds = &data.sounds;
    let named = data
        .weapons
        .values()
        .flat_map(|weapon| weapon.sounds.values())
        .chain(data.items.values().flat_map(|item| item.sounds.values()))
        .chain([
            &sounds.footsteps.left,
            &sounds.footsteps.right,
            &sounds.fall_damage,
            &sounds.weapon_pickup,
            &sounds.death,
            &sounds.explosion,
            &sounds.impact,
            &sounds.flesh_impact,
        ])
        .chain(sounds.breaking.values())
        .cloned()
        .collect::<Vec<_>>();
    for name in named {
        player.preload(&name);
    }
}

/// Where sounds of players are heard from.
struct Places<'a> {
    sim: &'a Simulation,
    drawn: &'a Drawn,
    local: Option<PlayerId>,
}

/// Where a player's sounds are heard from.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Heard {
    /// At the listener: the local player's own.
    Listener,
    /// At a place, Source space.
    At(Vec3),
}

impl Places<'_> {
    /// Where `player`'s own sounds play: at the listener for the local
    /// player, else at their drawn eyes; `None` for a player not in the
    /// game.
    fn player(&self, player: PlayerId) -> Option<Heard> {
        if Some(player) == self.local {
            return Some(Heard::Listener);
        }
        let eye = self.sim.data().movement.hulls.standing.eye_height;
        let feet = self
            .drawn
            .0
            .as_ref()
            .and_then(|world| world.players.get(&player))
            .map(|pose| pose.feet)
            .or_else(|| self.sim.player(player).map(|p| p.state.origin))?;
        Some(Heard::At(feet + Vec3::Z * eye))
    }

    /// Plays `name` where `who`'s sounds are heard.
    fn play(&self, player: &mut SoundPlayer, who: PlayerId, name: &str) {
        match self.player(who) {
            Some(Heard::Listener) => {
                player.play(name, None, 1.0);
            }
            Some(Heard::At(at)) => {
                player.play(name, Some(at), 1.0);
            }
            None => {}
        }
    }

    /// The centre of the mover of BSP model `model`.
    fn mover(&self, model: u32) -> Option<Vec3> {
        let (mins, maxs) = self.sim.world().mover_bounds(model as usize)?;
        Some((mins + maxs) * 0.5)
    }
}

/// Plays the sounds of every tick event heard this frame.
fn play_events(
    mut ticks: MessageReader<Ticked>,
    game: Game,
    drawn: Res<Drawn>,
    local: Option<Res<LocalPlayer>>,
    entities: Res<LevelEntities>,
    mut player: SoundPlayer,
) {
    let Some(sim) = game.sim() else {
        ticks.clear();
        return;
    };
    let places = Places {
        sim,
        drawn: &drawn,
        local: local.map(|local| local.id),
    };
    for Ticked(events) in ticks.read() {
        weapon_sounds(events, &places, &mut player);
        world_sounds(events, &places, &entities.0, &mut player);
    }
}

/// The sound of `weapon` for `purpose`, if its data names one.
fn weapon_sound<'a>(sim: &'a Simulation, weapon: WeaponId, purpose: &str) -> Option<&'a str> {
    sim.arsenal()
        .weapon(weapon)?
        .sounds
        .get(purpose)
        .map(String::as_str)
}

/// Firing, reloading, switching, swings landing, bullets striking and the
/// Manipulator.
fn weapon_sounds(events: &TickEvents, places: &Places, player: &mut SoundPlayer) {
    let sim = places.sim;
    let data = sim.data();
    let play = |player: &mut SoundPlayer, who: PlayerId, weapon: WeaponId, purpose: &str| {
        if let Some(name) = weapon_sound(sim, weapon, purpose) {
            places.play(player, who, name);
        }
    };
    for &(who, weapon, mode) in &events.fired {
        play(
            player,
            who,
            weapon,
            if mode == Mode::Primary {
                "fire"
            } else {
                "fire2"
            },
        );
    }
    for &(who, weapon) in &events.reloads {
        play(player, who, weapon, "reload");
    }
    for &(who, weapon) in &events.deploys {
        play(player, who, weapon, "deploy");
    }
    for &(who, event) in &events.tools {
        let purpose = match event {
            ToolEvent::Grabbed(_) => "pickup",
            ToolEvent::Dropped(_) | ToolEvent::GripBroke(_) => "drop",
            ToolEvent::Launched(_) => "launch",
            ToolEvent::Punted(_) => "punt",
            ToolEvent::DryFire => "dryfire",
        };
        if let Some(weapon) = sim.player(who).and_then(|p| p.inventory.active()) {
            play(player, who, weapon, purpose);
        }
    }
    let mut impacts = 0;
    for shot in &events.shots {
        let melee = sim.arsenal().weapon(shot.weapon).is_some_and(|weapon| {
            weapon
                .fire_modes()
                .any(|fire| matches!(fire.shot, Shot::Melee(_)))
        });
        if melee && shot.trace.struck != Struck::Nothing {
            play(player, shot.player, shot.weapon, "hit");
        }
        if impacts >= MAX_IMPACTS {
            continue;
        }
        let sounds: &SoundsConfig = &data.sounds;
        let name = match shot.trace.struck {
            Struck::Nothing => continue,
            Struck::Player(_) => &sounds.flesh_impact,
            Struck::World | Struck::Prop(_) | Struck::Brush(_) => &sounds.impact,
        };
        impacts += 1;
        player.play(name, Some(shot.trace.end), 1.0);
    }
}

/// Blasts, pickups, deaths, falls, and the map's buttons and breakables.
fn world_sounds(
    events: &TickEvents,
    places: &Places,
    entities: &[MapEntity],
    player: &mut SoundPlayer,
) {
    let sim = places.sim;
    let sounds = &sim.data().sounds;
    for explosion in &events.explosions {
        player.play(&sounds.explosion, Some(explosion.position), 1.0);
    }
    for broke in &events.prop_breaks {
        let material = break_material(sim.physics().surface(broke.body));
        if let Some(name) = sounds.breaking(material) {
            player.play(name, Some(broke.position), 1.0);
        }
    }
    for &(who, index) in &events.pickups {
        let Some(pickup) = sim.pickups().get(index) else {
            continue;
        };
        let name = match pickup.gives {
            Gives::Weapon { .. } => Some(sounds.weapon_pickup.as_str()),
            _ => sim
                .data()
                .items
                .get(&pickup.id)
                .and_then(|item| item.sounds.get("pickup"))
                .map(String::as_str),
        };
        if let Some(name) = name {
            places.play(player, who, name);
        }
    }
    for kill in &events.kills {
        places.play(player, kill.victim, &sounds.death);
    }
    for damage in &events.damage {
        if damage.damage.cause == Cause::Fall {
            places.play(player, damage.victim, &sounds.fall_damage);
        }
    }
    for event in &events.map {
        match *event {
            MapEvent::Pressed { model } => {
                let number = entity_of(entities, model)
                    .and_then(|entity| entity.number("sounds"))
                    .filter(|&number| number > 0.0);
                if let Some(number) = number {
                    player.play(&sounds.button(number as u32), places.mover(model), 1.0);
                }
            }
            MapEvent::Broke { model } => {
                let material = entity_of(entities, model).map_or("glass", |entity| {
                    if entity.classname == "func_breakable_surf" {
                        return "glass";
                    }
                    let index = entity.number("material").unwrap_or(0.0) as usize;
                    MATERIALS.get(index).copied().unwrap_or("default")
                });
                if let Some(name) = sounds.breaking(material) {
                    player.play(name, places.mover(model), 1.0);
                }
            }
            MapEvent::MoverStarted { .. }
            | MapEvent::MoverStopped { .. }
            | MapEvent::Teleported { .. } => {}
        }
    }
}

/// The breaking sound's material of a prop made of surface property
/// `surface`: its name up to the first underscore or digit, with the few
/// that Source spells differently mapped (`metalpanel` is metal,
/// `cardboard` breaks like wood).
fn break_material(surface: &str) -> &str {
    let name = surface
        .split(|c: char| c == '_' || c.is_ascii_digit())
        .next()
        .unwrap_or(surface);
    match name {
        "metalpanel" | "solidmetal" | "metalgrate" | "metalvent" | "canister" => "metal",
        "cardboard" | "paper" | "wood" | "woodcrate" | "wood_box" => "wood",
        "ceramic" | "pottery" | "glassbottle" | "glass" => "glass",
        other => other,
    }
}

/// The map entity whose brush model is `model`.
pub(super) fn entity_of(entities: &[MapEntity], model: u32) -> Option<&MapEntity> {
    entities
        .iter()
        .find(|entity| entity.brush_model() == Some(model as usize))
}
