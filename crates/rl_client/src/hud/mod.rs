//! The HUD: health and armor, ammo, the weapon list, points and clock, the
//! kill feed, messages, the scoreboard with the map vote, the crosshair and
//! the flash of being hurt.
//!
//! The look (fonts, colours, layout) is the client's theme
//! ([`crate::ui::theme`]); `hud.ron` holds only the tunables. Elements are
//! spawned once and updated every frame from the replicated simulation. What
//! only tick events tell — kills, pickups, being hurt, landing a hit — is
//! kept in a short log fed by [`Ticked`] messages.
//!
//! Wording and arithmetic live in plain functions (the `text` module) tested on their
//! own; the systems here only put their results on screen.

mod feed;
mod scoreboard;
mod status;
pub(crate) mod text;
mod vitals;
mod weapons;

use bevy::prelude::*;
use rl_assets::SOURCE;
use rl_data::Rgba;

use crate::AppState;
use crate::net::{Game, Roster};
use crate::player::{Gameplay, Ticked};
use crate::ui::{Fonts, theme};
use text::{KillEntry, Names};

/// Seconds a pickup message stays on screen.
pub(crate) const PICKUP_MESSAGE_SECS: f32 = 2.0;
/// Seconds the screen flashes red after being hurt...
const HURT_FLASH_SECS: f32 = 0.6;
/// ...at most this opaque.
const HURT_FLASH_ALPHA: f32 = 0.55;
/// Seconds the crosshair shows a landed hit.
const HIT_MARKER_SECS: f32 = 0.15;

/// Draws the HUD while in a map.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HudLog>()
            .add_plugins(bevy::diagnostic::FrameTimeDiagnosticsPlugin::default())
            .add_systems(OnEnter(AppState::InGame), (reset_log, spawn_hud).chain())
            .add_systems(
                Update,
                (
                    log_events,
                    vitals::update,
                    weapons::update_ammo,
                    weapons::update_list,
                    status::update,
                    feed::update,
                    scoreboard::show,
                    scoreboard::headings,
                    scoreboard::rows,
                    scoreboard::vote,
                    update_effects,
                )
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// What the HUD remembers from tick events.
#[derive(Resource, Debug, Default)]
pub(crate) struct HudLog {
    /// Kill feed entries and when they happened (app seconds).
    pub(crate) kills: Vec<(KillEntry, f32)>,
    /// The last pickup's name and when it was taken.
    pub(crate) pickup: Option<(String, f32)>,
    /// When the local player was last hurt, and how badly.
    pub(crate) hurt: Option<(f32, f32)>,
    /// When the local player last hit someone.
    pub(crate) hit: Option<f32>,
}

fn reset_log(mut log: ResMut<HudLog>) {
    *log = HudLog::default();
}

/// An sRGB colour from the data files' `[r, g, b, a]`.
pub(crate) fn color(rgba: Rgba) -> Color {
    Color::srgba(rgba[0], rgba[1], rgba[2], rgba[3])
}

/// The full-screen red flash of being hurt: four edges that glow red.
#[derive(Component)]
struct HurtEdge;

/// A stroke of the crosshair.
#[derive(Component)]
struct CrosshairStroke;

/// A full-screen, click-through wrapper that places its children by
/// alignment and margin.
pub(crate) fn anchored(justify: JustifyContent, align: AlignItems, margin: UiRect) -> impl Bundle {
    (
        DespawnOnExit(AppState::InGame),
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            justify_content: justify,
            align_items: align,
            padding: margin,
            ..default()
        },
        GlobalZIndex(10),
        Pickable::IGNORE,
    )
}

fn spawn_hud(
    mut commands: Commands,
    gameplay: Res<Gameplay>,
    fonts: Res<Fonts>,
    assets: Res<AssetServer>,
) {
    spawn_hurt_edges(&mut commands);
    spawn_crosshair(&mut commands, &gameplay.0.hud);
    vitals::spawn(&mut commands, &fonts);
    let icons = assets.load(format!("{SOURCE}://{}", gameplay.0.hud.weapon_icon_font));
    weapons::spawn(&mut commands, &fonts, icons);
    status::spawn(&mut commands, &fonts);
    feed::spawn(
        &mut commands,
        &fonts,
        gameplay.0.hud.kill_feed.lines as usize,
    );
    scoreboard::spawn(&mut commands, &fonts);
}

/// Four edge gradients that glow red when the player is hurt.
fn spawn_hurt_edges(commands: &mut Commands) {
    const THICKNESS: f32 = 190.0;
    // (angle of the gradient towards the inside, node placement)
    let edges: [(f32, Node); 4] = [
        (
            LinearGradient::TO_BOTTOM,
            Node {
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Px(THICKNESS),
                ..default()
            },
        ),
        (
            LinearGradient::TO_TOP,
            Node {
                bottom: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Px(THICKNESS),
                ..default()
            },
        ),
        (
            LinearGradient::TO_RIGHT,
            Node {
                left: Val::Px(0.0),
                width: Val::Px(THICKNESS),
                height: Val::Percent(100.0),
                ..default()
            },
        ),
        (
            LinearGradient::TO_LEFT,
            Node {
                right: Val::Px(0.0),
                width: Val::Px(THICKNESS),
                height: Val::Percent(100.0),
                ..default()
            },
        ),
    ];
    for (angle, mut node) in edges {
        node.position_type = PositionType::Absolute;
        commands.spawn((
            DespawnOnExit(AppState::InGame),
            HurtEdge,
            node,
            BackgroundGradient::from(LinearGradient::new(
                angle,
                vec![
                    ColorStop::new(Color::srgba(0.9, 0.05, 0.05, 0.0), Val::Percent(0.0)),
                    ColorStop::new(Color::srgba(0.9, 0.05, 0.05, 0.0), Val::Percent(100.0)),
                ],
            )),
            GlobalZIndex(9),
            Pickable::IGNORE,
        ));
    }
}

/// Four strokes around the centre of the screen.
fn spawn_crosshair(commands: &mut Commands, hud: &rl_data::HudConfig) {
    let crosshair = hud.crosshair;
    let (length, thickness, gap) = (crosshair.length, crosshair.thickness, crosshair.gap);
    // (left, top, width, height) of each stroke relative to the centre.
    let strokes = [
        (gap, -thickness / 2.0, length, thickness),
        (-gap - length, -thickness / 2.0, length, thickness),
        (-thickness / 2.0, gap, thickness, length),
        (-thickness / 2.0, -gap - length, thickness, length),
    ];
    for (left, top, width, height) in strokes {
        commands.spawn((
            DespawnOnExit(AppState::InGame),
            CrosshairStroke,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                width: Val::Px(width),
                height: Val::Px(height),
                margin: UiRect {
                    left: Val::Px(left),
                    top: Val::Px(top),
                    ..default()
                },
                ..default()
            },
            BackgroundColor(color(crosshair.color)),
            GlobalZIndex(10),
            Pickable::IGNORE,
        ));
    }
}

/// Records what the HUD shows from each tick's events.
fn log_events(
    time: Res<Time>,
    mut ticks: MessageReader<Ticked>,
    game: Game,
    roster: Res<Roster>,
    gameplay: Res<Gameplay>,
    mut log: ResMut<HudLog>,
) {
    let (Some(sim), Some(local)) = (game.sim(), game.local()) else {
        ticks.clear();
        return;
    };
    let names = Names {
        local,
        roster: &roster.0,
    };
    let now = time.elapsed_secs();
    for Ticked(events) in ticks.read() {
        for kill in &events.kills {
            log.kills
                .push((text::kill_entry(kill, &names, sim.arsenal()), now));
        }
        for &(player, index) in &events.pickups {
            if player == local
                && let Some(pickup) = sim.pickups().get(index)
            {
                log.pickup = Some((pickup.name.clone(), now));
            }
        }
        for damage in &events.damage {
            if damage.victim == local {
                log.hurt = Some((now, damage.health_lost));
            } else if damage.damage.attacker == Some(local) {
                log.hit = Some(now);
            }
        }
    }
    let feed = gameplay.0.hud.kill_feed;
    log.kills.retain(|(_, at)| now - at < feed.seconds);
    let excess = log.kills.len().saturating_sub(feed.lines as usize);
    log.kills.drain(..excess);
}

/// The hurt glow and the hit marker.
fn update_effects(
    time: Res<Time>,
    gameplay: Res<Gameplay>,
    log: Res<HudLog>,
    mut edges: Query<&mut BackgroundGradient, With<HurtEdge>>,
    mut strokes: Query<&mut BackgroundColor, With<CrosshairStroke>>,
) {
    let now = time.elapsed_secs();
    let hud = &gameplay.0.hud;
    let alpha = log.hurt.map_or(0.0, |(at, amount)| {
        let fade = 1.0 - (now - at) / HURT_FLASH_SECS;
        (fade * (amount / 40.0).clamp(0.3, 1.0) * HURT_FLASH_ALPHA).max(0.0)
    });
    for mut gradient in &mut edges {
        for gradient in &mut gradient.0 {
            if let Gradient::Linear(linear) = gradient
                && let Some(first) = linear.stops.first_mut()
            {
                let wanted = first.color.with_alpha(alpha);
                if first.color != wanted {
                    first.color = wanted;
                }
            }
        }
    }
    let hit = log.hit.is_some_and(|at| now - at < HIT_MARKER_SECS);
    let stroke = if hit {
        theme::DANGER
    } else {
        color(hud.crosshair.color)
    };
    for mut background in &mut strokes {
        if background.0 != stroke {
            background.0 = stroke;
        }
    }
}
