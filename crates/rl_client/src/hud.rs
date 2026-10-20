//! The HUD: health, armor, ammo, weapons, score, kill feed, messages and
//! the crosshair, laid out and coloured by `hud.ron`.
//!
//! Each read-out is a text node inside a full-screen wrapper that places it
//! by its [`Anchor`] and offset. The text is rebuilt from the simulation
//! every frame; what only the tick events tell — kills, pickups, being hurt,
//! landing a hit — is kept in a short log fed by [`Ticked`]
//! messages. Formatting is done by plain functions, tested on their own.
//!
//! The scoreboard shows while `Tab` is held and between matches.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use bevy::prelude::*;
use rl_data::{Anchor, FireAmmo, HudConfig, Placement, Rgba};
use rl_sim::combat::Cause;
use rl_sim::rules::MatchState;
use rl_sim::simulation::{Kill, Life, PlayerId, Simulation};
use rl_sim::weapons::{Arsenal, Inventory, Phase};

use crate::AppState;
use crate::net::{Game, Roster};
use crate::player::{Gameplay, Ticked};

/// Seconds a pickup message stays on screen.
const PICKUP_MESSAGE_SECS: f32 = 2.0;
/// Seconds the screen flashes red after being hurt...
const HURT_FLASH_SECS: f32 = 0.5;
/// ...at most this opaque.
const HURT_FLASH_ALPHA: f32 = 0.45;
/// Seconds the crosshair shows a landed hit.
const HIT_MARKER_SECS: f32 = 0.15;

/// Draws the HUD while in a map.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HudLog>()
            .add_systems(OnEnter(AppState::InGame), (reset_log, spawn_hud))
            .add_systems(
                Update,
                (
                    log_events,
                    update_vitals,
                    update_weapons,
                    update_status,
                    update_effects,
                )
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// A HUD text read-out.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum Readout {
    Health,
    Armor,
    Ammo,
    SecondaryAmmo,
    Weapons,
    Score,
    Message,
    KillFeed,
    Scoreboard,
}

/// The full-screen red flash of being hurt.
#[derive(Component)]
struct HurtFlash;

/// A stroke of the crosshair.
#[derive(Component)]
struct CrosshairStroke;

/// What the HUD remembers from tick events.
#[derive(Resource, Debug, Default)]
struct HudLog {
    /// Kill feed lines and when they happened (app seconds).
    kills: Vec<(String, f32)>,
    /// The last pickup's name and when it was taken.
    pickup: Option<(String, f32)>,
    /// When the local player was last hurt, and how badly.
    hurt: Option<(f32, f32)>,
    /// When the local player last hit someone.
    hit: Option<f32>,
}

fn reset_log(mut log: ResMut<HudLog>) {
    *log = HudLog::default();
}

fn color(rgba: Rgba) -> Color {
    Color::srgba(rgba[0], rgba[1], rgba[2], rgba[3])
}

/// A full-screen wrapper placing `child` by `placement`.
fn place(commands: &mut Commands, placement: Placement, child: impl Bundle) {
    let (x, y) = placement.offset;
    let (horizontal, vertical) = match placement.anchor {
        Anchor::TopLeft => (JustifyContent::FlexStart, AlignItems::FlexStart),
        Anchor::Top => (JustifyContent::Center, AlignItems::FlexStart),
        Anchor::TopRight => (JustifyContent::FlexEnd, AlignItems::FlexStart),
        Anchor::Left => (JustifyContent::FlexStart, AlignItems::Center),
        Anchor::Center => (JustifyContent::Center, AlignItems::Center),
        Anchor::Right => (JustifyContent::FlexEnd, AlignItems::Center),
        Anchor::BottomLeft => (JustifyContent::FlexStart, AlignItems::FlexEnd),
        Anchor::Bottom => (JustifyContent::Center, AlignItems::FlexEnd),
        Anchor::BottomRight => (JustifyContent::FlexEnd, AlignItems::FlexEnd),
    };
    // Offsets push in from an edge, or shift along a centred axis.
    let padding = UiRect {
        left: Val::Px(if horizontal == JustifyContent::FlexStart {
            x
        } else {
            0.0
        }),
        right: Val::Px(if horizontal == JustifyContent::FlexEnd {
            x
        } else {
            0.0
        }),
        top: Val::Px(if vertical == AlignItems::FlexStart {
            y
        } else {
            0.0
        }),
        bottom: Val::Px(if vertical == AlignItems::FlexEnd {
            y
        } else {
            0.0
        }),
    };
    let shift = Node {
        left: Val::Px(if horizontal == JustifyContent::Center {
            x
        } else {
            0.0
        }),
        top: Val::Px(if vertical == AlignItems::Center {
            y
        } else {
            0.0
        }),
        ..default()
    };
    commands
        .spawn((
            DespawnOnExit(AppState::InGame),
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: horizontal,
                align_items: vertical,
                padding,
                ..default()
            },
        ))
        .with_children(|parent| {
            parent.spawn(shift).with_child(child);
        });
}

/// A text read-out of `size` in `hud`'s text colour.
fn readout(kind: Readout, size: f32, hud: &HudConfig) -> impl Bundle {
    (
        kind,
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(size),
            ..default()
        },
        TextColor(color(hud.colors.text)),
    )
}

/// A read-out on a panel.
fn panel(kind: Readout, size: f32, hud: &HudConfig) -> impl Bundle {
    (
        Node {
            padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)),
            flex_direction: FlexDirection::Column,
            ..default()
        },
        BackgroundColor(color(hud.colors.panel)),
        children![readout(kind, size, hud)],
    )
}

fn spawn_hud(mut commands: Commands, gameplay: Res<Gameplay>) {
    let hud = &gameplay.0.hud;
    let (big, small) = (hud.font_size, hud.small_font_size);
    commands.spawn((
        DespawnOnExit(AppState::InGame),
        HurtFlash,
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::NONE),
    ));
    spawn_crosshair(&mut commands, hud);
    place(&mut commands, hud.health, panel(Readout::Health, big, hud));
    place(&mut commands, hud.armor, panel(Readout::Armor, big, hud));
    place(
        &mut commands,
        hud.ammo,
        (
            Node {
                padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                ..default()
            },
            BackgroundColor(color(hud.colors.panel)),
            children![
                readout(Readout::Ammo, big, hud),
                readout(Readout::SecondaryAmmo, small, hud)
            ],
        ),
    );
    place(
        &mut commands,
        hud.weapons,
        readout(Readout::Weapons, small, hud),
    );
    place(
        &mut commands,
        hud.score,
        readout(Readout::Score, small * 1.25, hud),
    );
    place(
        &mut commands,
        hud.messages,
        readout(Readout::Message, small * 1.25, hud),
    );
    place(
        &mut commands,
        hud.kill_feed.place,
        readout(Readout::KillFeed, small, hud),
    );
    place(
        &mut commands,
        Placement {
            anchor: Anchor::Center,
            offset: (0.0, 0.0),
        },
        (
            Node {
                padding: UiRect::all(Val::Px(16.0)),
                ..default()
            },
            BackgroundColor(color(hud.colors.panel)),
            Visibility::Hidden,
            children![readout(Readout::Scoreboard, small, hud)],
        ),
    );
}

/// Four strokes around the centre of the screen.
fn spawn_crosshair(commands: &mut Commands, hud: &HudConfig) {
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
                .push((kill_line(kill, &names, sim.arsenal()), now));
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

/// Health and armor.
fn update_vitals(
    game: Game,
    gameplay: Res<Gameplay>,
    mut texts: Query<(&Readout, &mut Text, &mut TextColor)>,
) {
    let Some(player) = game
        .sim()
        .zip(game.local())
        .and_then(|(sim, local)| sim.player(local))
    else {
        return;
    };
    let hud = &gameplay.0.hud;
    for (readout, mut text, mut text_color) in &mut texts {
        let (line, warn) = match readout {
            Readout::Health => {
                let health = player.vitals.health.max(0.0).ceil();
                (format!("+ {health:.0}"), health <= hud.low_health)
            }
            Readout::Armor => (format!("# {:.0}", player.vitals.armor.ceil()), false),
            _ => continue,
        };
        set_text(&mut text, line);
        text_color.0 = color(if warn {
            hud.colors.warning
        } else {
            hud.colors.text
        });
    }
}

/// Ammo and the carried weapons.
fn update_weapons(
    game: Game,
    gameplay: Res<Gameplay>,
    mut texts: Query<(&Readout, &mut Text, &mut TextColor, &ChildOf)>,
    mut panels: Query<&mut Visibility>,
) {
    let (Some(sim), Some(local)) = (game.sim(), game.local()) else {
        return;
    };
    let Some(player) = sim.player(local) else {
        return;
    };
    let arsenal = sim.arsenal();
    let hud = &gameplay.0.hud;
    let (main, secondary) = ammo_lines(&player.inventory, arsenal);
    // Only a weapon with a clip can have an empty one.
    let empty = player.inventory.active().is_some_and(|weapon| {
        arsenal
            .weapon(weapon)
            .is_some_and(|config| config.clip.is_some())
            && player.inventory.clip(weapon) == Some(0)
    });
    for (readout, mut text, mut text_color, parent) in &mut texts {
        match readout {
            Readout::Ammo => {
                // A tool needs no ammo: hide the panel rather than show it
                // empty.
                if let Ok(mut panel) = panels.get_mut(parent.parent()) {
                    panel.set_if_neq(if main.is_empty() {
                        Visibility::Hidden
                    } else {
                        Visibility::Inherited
                    });
                }
                set_text(&mut text, main.clone());
                text_color.0 = color(if empty {
                    hud.colors.warning
                } else {
                    hud.colors.text
                });
            }
            Readout::SecondaryAmmo => set_text(&mut text, secondary.clone()),
            Readout::Weapons => set_text(&mut text, weapon_list(&player.inventory, arsenal)),
            _ => {}
        }
    }
}

/// Score, clock, messages, kill feed and the scoreboard.
fn update_status(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    game: Game,
    roster: Res<Roster>,
    log: Res<HudLog>,
    mut texts: Query<(&Readout, &mut Text, &ChildOf)>,
    mut panels: Query<&mut Visibility>,
) {
    let (Some(sim), Some(local)) = (game.sim(), game.local()) else {
        return;
    };
    let names = Names {
        local,
        roster: &roster.0,
    };
    let now = time.elapsed_secs();
    let over = !sim.game().playing();
    for (readout, mut text, parent) in &mut texts {
        let line = match readout {
            Readout::Score => score_line(sim, local),
            Readout::Message => message_line(sim, &names, &log, now),
            Readout::KillFeed => log
                .kills
                .iter()
                .map(|(line, _)| line.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            Readout::Scoreboard => {
                let shown = over || keys.pressed(KeyCode::Tab);
                if let Ok(mut visibility) = panels.get_mut(parent.parent()) {
                    *visibility = if shown {
                        Visibility::Inherited
                    } else {
                        Visibility::Hidden
                    };
                }
                scoreboard(sim, &names)
            }
            _ => continue,
        };
        set_text(&mut text, line);
    }
}

/// The hurt flash and the hit marker.
fn update_effects(
    time: Res<Time>,
    gameplay: Res<Gameplay>,
    log: Res<HudLog>,
    mut flash: Query<&mut BackgroundColor, (With<HurtFlash>, Without<CrosshairStroke>)>,
    mut strokes: Query<&mut BackgroundColor, (With<CrosshairStroke>, Without<HurtFlash>)>,
) {
    let now = time.elapsed_secs();
    let hud = &gameplay.0.hud;
    let alpha = log.hurt.map_or(0.0, |(at, amount)| {
        let fade = 1.0 - (now - at) / HURT_FLASH_SECS;
        (fade * (amount / 40.0).clamp(0.3, 1.0) * HURT_FLASH_ALPHA).max(0.0)
    });
    for mut background in &mut flash {
        background.0 = Color::srgba(0.8, 0.0, 0.0, alpha);
    }
    let hit = log.hit.is_some_and(|at| now - at < HIT_MARKER_SECS);
    let stroke = color(if hit {
        hud.colors.warning
    } else {
        hud.crosshair.color
    });
    for mut background in &mut strokes {
        background.0 = stroke;
    }
}

fn set_text(text: &mut Text, line: String) {
    if text.0 != line {
        text.0 = line;
    }
}

/// How the HUD names players: the local one "You", others by the name
/// they joined with, or by number (training dummies have no name).
struct Names<'a> {
    local: PlayerId,
    roster: &'a BTreeMap<PlayerId, String>,
}

impl Names<'_> {
    fn of(&self, player: PlayerId) -> String {
        if player == self.local {
            "You".to_owned()
        } else {
            self.roster
                .get(&player)
                .cloned()
                .unwrap_or_else(|| format!("Player {}", player.index() + 1))
        }
    }
}

/// A kill feed line: `killer [weapon] victim`.
fn kill_line(kill: &Kill, names: &Names, arsenal: &Arsenal) -> String {
    let victim = names.of(kill.victim);
    let how = match kill.cause {
        Cause::Weapon(weapon) => arsenal
            .weapon(weapon)
            .map_or_else(|| "?".to_owned(), |config| config.name.clone()),
        Cause::Prop => "prop".to_owned(),
        Cause::Fall => "fall".to_owned(),
        Cause::Hazard => "hazard".to_owned(),
    };
    match kill.killer.filter(|&killer| killer != kill.victim) {
        Some(killer) => format!("{} [{how}] {victim}", names.of(killer)),
        None => format!("{victim} [{how}]"),
    }
}

/// The ammo read-outs of the weapon in hand: `clip / carried` (or just
/// carried, for weapons without a clip; nothing for weapons that need no
/// ammo), and the carried rounds of a secondary fire with its own ammo.
fn ammo_lines(inventory: &Inventory, arsenal: &Arsenal) -> (String, String) {
    let Some((weapon, config)) = inventory
        .active()
        .and_then(|weapon| Some((weapon, arsenal.weapon(weapon)?)))
    else {
        return (String::new(), String::new());
    };
    let carried = |id: &str| arsenal.find_ammo(id).map_or(0, |ammo| inventory.ammo(ammo));
    let main = match (&config.clip, config.ammo_type()) {
        (Some(_), Some(ammo)) => format!(
            "{} / {}",
            inventory.clip(weapon).unwrap_or(0),
            carried(ammo)
        ),
        (None, Some(ammo)) => carried(ammo).to_string(),
        (_, None) => String::new(),
    };
    let secondary = match config.secondary.as_ref().map(|fire| &fire.ammo) {
        Some(FireAmmo::Reserve { ammo, .. }) if Some(ammo.as_str()) != config.ammo_type() => {
            let name = arsenal
                .find_ammo(ammo)
                .and_then(|id| arsenal.ammo(id))
                .map_or(ammo.as_str(), |ammo| ammo.name.as_str());
            format!("{name}: {}", carried(ammo))
        }
        _ => String::new(),
    };
    (main, secondary)
}

/// The carried weapons by slot, the one in hand marked.
fn weapon_list(inventory: &Inventory, arsenal: &Arsenal) -> String {
    let mut weapons: Vec<_> = inventory
        .weapons()
        .filter_map(|weapon| Some((weapon, arsenal.weapon(weapon)?)))
        .collect();
    weapons.sort_by_key(|(_, config)| (config.slot, config.position));
    weapons
        .iter()
        .map(|&(weapon, config)| {
            let active = inventory.active() == Some(weapon);
            let mark = if active { ">" } else { " " };
            let state = match inventory.phase() {
                Phase::Reloading(_) if active => "  reloading",
                _ => "",
            };
            let dim = if inventory.usable(arsenal, weapon) {
                ""
            } else {
                "  (empty)"
            };
            format!("{mark} {} {}{state}{dim}", config.slot, config.name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `m:ss` for a number of seconds.
fn clock(secs: f32) -> String {
    let secs = secs.max(0.0).ceil() as u32;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The local player's frags, their place and the match clock.
fn score_line(sim: &Simulation, local: PlayerId) -> String {
    let rules = &sim.data().rules;
    let mine = sim.game().score(local).frags;
    let mut line = format!("Frags {mine}");
    if rules.frag_limit > 0 {
        let _ = write!(line, " / {}", rules.frag_limit);
    }
    if let Some(left) = sim.game().time_left(rules) {
        let _ = write!(line, "    {}", clock(left));
    }
    line
}

/// The message in the middle of the screen: the match's end, the player's
/// death and respawn, or the last pickup.
fn message_line(sim: &Simulation, names: &Names, log: &HudLog, now: f32) -> String {
    let local = names.local;
    if let MatchState::Intermission { winner, left } = sim.game().state() {
        let result = winner.map_or_else(
            || "Match over: a draw".to_owned(),
            |winner| format!("Match over: {} won", names.of(winner)),
        );
        return format!("{result}\nNext match in {}", clock(left));
    }
    let Some(player) = sim.player(local) else {
        return String::new();
    };
    if let Life::Dead { since, killer } = player.life {
        let respawn = sim.data().rules.respawn;
        let dead_for = sim.tick_count().saturating_sub(since) as f32 * sim.dt();
        let by = killer.filter(|&killer| killer != local).map_or_else(
            || "You died".to_owned(),
            |killer| format!("Killed by {}", names.of(killer)),
        );
        let hint = if dead_for < respawn.delay {
            format!("Respawn in {:.1}", respawn.delay - dead_for)
        } else {
            "Press fire to respawn".to_owned()
        };
        return format!("{by}\n{hint}");
    }
    match &log.pickup {
        Some((name, at)) if now - at < PICKUP_MESSAGE_SECS => name.clone(),
        _ => String::new(),
    }
}

/// Every player's score, best first.
fn scoreboard(sim: &Simulation, names: &Names) -> String {
    let mut rows: Vec<_> = sim
        .players()
        .map(|(id, _)| (id, sim.game().score(id)))
        .collect();
    rows.sort_by(|a, b| b.1.frags.cmp(&a.1.frags).then(a.0.cmp(&b.0)));
    let mut lines = vec![format!("{:<12}{:>7}{:>8}", "Player", "Frags", "Deaths")];
    for (id, score) in rows {
        lines.push(format!(
            "{:<12}{:>7}{:>8}",
            names.of(id),
            score.frags,
            score.deaths
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use rl_data::GameData;

    use super::*;

    fn arsenal() -> Arsenal {
        Arsenal::new(&GameData::builtin().expect("builtin"))
    }

    const NO_NAMES: &BTreeMap<PlayerId, String> = &BTreeMap::new();

    fn names(local: PlayerId) -> Names<'static> {
        Names {
            local,
            roster: NO_NAMES,
        }
    }

    #[test]
    fn kill_lines_name_killer_weapon_and_victim() {
        let arsenal = arsenal();
        let smg = arsenal.find_weapon("smg").expect("smg");
        let (you, other) = (PlayerId::from_index(0), PlayerId::from_index(2));
        let kill = |killer, cause| Kill {
            victim: other,
            killer,
            cause,
        };
        assert_eq!(
            kill_line(&kill(Some(you), Cause::Weapon(smg)), &names(you), &arsenal),
            "You [SMG] Player 3"
        );
        assert_eq!(
            kill_line(&kill(None, Cause::Fall), &names(you), &arsenal),
            "Player 3 [fall]"
        );
        assert_eq!(
            kill_line(
                &kill(Some(other), Cause::Weapon(smg)),
                &names(you),
                &arsenal
            ),
            "Player 3 [SMG]",
            "a suicide names only the victim"
        );
    }

    #[test]
    fn ammo_shows_clip_carried_and_secondary_rounds() {
        let arsenal = arsenal();
        let data = GameData::builtin().expect("builtin");
        let mut inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
        assert_eq!(
            ammo_lines(&inventory, &arsenal),
            ("45 / 90".to_owned(), "SMG grenades: 0".to_owned())
        );
        inventory.select(&arsenal, arsenal.find_weapon("frag").expect("frag"));
        assert_eq!(ammo_lines(&inventory, &arsenal).0, "1");
        inventory.select(&arsenal, arsenal.find_weapon("manipulator").expect("tool"));
        assert_eq!(
            ammo_lines(&inventory, &arsenal),
            (String::new(), String::new())
        );
    }

    #[test]
    fn the_weapon_list_marks_the_one_in_hand() {
        let arsenal = arsenal();
        let data = GameData::builtin().expect("builtin");
        let inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
        let list = weapon_list(&inventory, &arsenal);
        let lines: Vec<&str> = list.lines().collect();
        assert_eq!(lines.first(), Some(&"  1 Manipulator"));
        assert!(lines.contains(&"> 3 SMG"), "{list}");
    }

    #[test]
    fn players_are_named_by_the_roster() {
        let roster = BTreeMap::from([(PlayerId::from_index(1), "Alyx".to_owned())]);
        let names = Names {
            local: PlayerId::from_index(0),
            roster: &roster,
        };
        assert_eq!(names.of(PlayerId::from_index(0)), "You");
        assert_eq!(names.of(PlayerId::from_index(1)), "Alyx");
        assert_eq!(names.of(PlayerId::from_index(4)), "Player 5");
    }

    #[test]
    fn clocks_round_up_to_whole_seconds() {
        assert_eq!(clock(600.0), "10:00");
        assert_eq!(clock(61.2), "1:02");
        assert_eq!(clock(-3.0), "0:00");
    }

    /// A floor with two players, the second with `health` left, after one
    /// shot from the first's Hand Cannon.
    fn duel(health: f32, data: &GameData) -> Simulation {
        use rl_sim::collision::WorldBuilder;
        use rl_sim::movement::{Buttons, UserCmd};

        let world = WorldBuilder::new()
            .solid_box(
                Vec3::splat(-2000.0).with_z(-64.0),
                Vec3::splat(2000.0).with_z(0.0),
            )
            .build();
        let mut sim = Simulation::new(std::sync::Arc::new(world), data);
        let (you, them) = (
            sim.spawn_player(Vec3::new(0.0, 0.0, 1.0)),
            sim.spawn_player(Vec3::new(200.0, 0.0, 1.0)),
        );
        let arsenal = sim.arsenal().clone();
        let magnum = arsenal.find_weapon("magnum").expect("magnum");
        sim.player_mut(you)
            .expect("you")
            .inventory
            .give_weapon(&arsenal, magnum);
        sim.player_mut(them).expect("them").vitals.health = health;
        let select = UserCmd {
            select: Some(magnum),
            ..UserCmd::default()
        };
        sim.tick(&[(you, select)]);
        for _ in 0..40 {
            sim.tick(&[(you, UserCmd::default())]);
        }
        let fire = UserCmd {
            buttons: Buttons::ATTACK,
            ..UserCmd::default()
        };
        sim.tick(&[(you, fire)]);
        sim
    }

    #[test]
    fn the_dead_are_told_who_killed_them_and_when_they_respawn() {
        let data = GameData::builtin().expect("builtin");
        let sim = duel(10.0, &data);
        let (you, them) = (PlayerId::from_index(0), PlayerId::from_index(1));
        let log = HudLog::default();
        // Seen by the victim (as if they were the local player).
        let message = message_line(&sim, &names(them), &log, 0.0);
        assert!(
            message.starts_with("Killed by Player 1\nRespawn in"),
            "{message}"
        );
        assert_eq!(message_line(&sim, &names(you), &log, 0.0), "");
        let board = scoreboard(&sim, &names(you));
        let rows: Vec<&str> = board.lines().collect();
        assert_eq!(rows.len(), 3);
        assert!(
            rows[1].starts_with("You") && rows[1].contains(" 1 "),
            "{board}"
        );
        assert!(rows[2].starts_with("Player 2"), "{board}");
        assert_eq!(score_line(&sim, you), "Frags 1 / 25    10:00");
    }

    #[test]
    fn the_end_of_a_match_names_the_winner() {
        let mut data = GameData::builtin().expect("builtin");
        data.rules.frag_limit = 1;
        let sim = duel(10.0, &data);
        let message = message_line(
            &sim,
            &names(PlayerId::from_index(1)),
            &HudLog::default(),
            0.0,
        );
        assert!(message.starts_with("Match over: Player 1 won"), "{message}");
    }
}
