//! What the HUD says: names, kills, ammo, messages and scores as plain
//! data, so the wording and arithmetic can be tested without a screen.

use std::collections::BTreeMap;

use rl_data::FireAmmo;
use rl_sim::combat::Cause;
use rl_sim::rules::{MatchState, Score};
use rl_sim::simulation::{Kill, Life, PlayerId, Simulation};
use rl_sim::weapons::{Arsenal, Inventory, Phase, WeaponId};

/// How the HUD names players: the local one "You", others by the name
/// they joined with, or by number (training dummies have no name).
pub(crate) struct Names<'a> {
    pub(crate) local: PlayerId,
    pub(crate) roster: &'a BTreeMap<PlayerId, String>,
}

impl Names<'_> {
    pub(crate) fn of(&self, player: PlayerId) -> String {
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

/// A kill feed entry: `killer [how] victim`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KillEntry {
    /// Who did it; `None` for a fall, a hazard or a suicide.
    pub(crate) killer: Option<String>,
    /// How: a weapon's name, `prop`, `fall`, `hazard`.
    pub(crate) how: String,
    /// Who died.
    pub(crate) victim: String,
    /// The local player did it.
    pub(crate) by_local: bool,
    /// The local player died.
    pub(crate) of_local: bool,
}

/// The kill feed entry for `kill`.
pub(crate) fn kill_entry(kill: &Kill, names: &Names, arsenal: &Arsenal) -> KillEntry {
    let how = match kill.cause {
        Cause::Weapon(weapon) => arsenal
            .weapon(weapon)
            .map_or_else(|| "?".to_owned(), |config| config.name.clone()),
        Cause::Prop => "prop".to_owned(),
        Cause::Fall => "fall".to_owned(),
        Cause::Hazard => "hazard".to_owned(),
        Cause::Explosion => "explosion".to_owned(),
    };
    let killer = kill.killer.filter(|&killer| killer != kill.victim);
    KillEntry {
        killer: killer.map(|killer| names.of(killer)),
        how,
        victim: names.of(kill.victim),
        by_local: killer == Some(names.local),
        of_local: kill.victim == names.local,
    }
}

/// The ammo of the weapon in hand.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Ammo {
    /// Rounds in the clip (for weapons with one) or carried (for those
    /// without); `None` for weapons that need no ammo.
    pub(crate) main: Option<u32>,
    /// Rounds carried, behind a clip.
    pub(crate) reserve: Option<u32>,
    /// The carried rounds of a secondary fire with its own ammo, as
    /// `Name: count`.
    pub(crate) secondary: String,
    /// The clip is empty.
    pub(crate) empty: bool,
    /// The weapon is being reloaded.
    pub(crate) reloading: bool,
    /// The weapon's name.
    pub(crate) weapon: String,
}

/// The ammo readouts of the weapon in hand.
pub(crate) fn ammo(inventory: &Inventory, arsenal: &Arsenal) -> Ammo {
    let Some((weapon, config)) = inventory
        .active()
        .and_then(|weapon| Some((weapon, arsenal.weapon(weapon)?)))
    else {
        return Ammo::default();
    };
    let carried = |id: &str| arsenal.find_ammo(id).map_or(0, |ammo| inventory.ammo(ammo));
    let (main, reserve) = match (&config.clip, config.ammo_type()) {
        (Some(_), Some(ammo)) => (
            Some(inventory.clip(weapon).unwrap_or(0)),
            Some(carried(ammo)),
        ),
        (None, Some(ammo)) => (Some(carried(ammo)), None),
        (_, None) => (None, None),
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
    Ammo {
        main,
        reserve,
        secondary,
        // Only a weapon with a clip can have an empty one.
        empty: config.clip.is_some() && inventory.clip(weapon) == Some(0),
        reloading: matches!(inventory.phase(), Phase::Reloading(_)),
        weapon: config.name.clone(),
    }
}

/// A carried weapon on the weapon strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WeaponSlot {
    /// The weapon.
    pub(crate) weapon: WeaponId,
    /// The weapon's slot number, which selects it.
    pub(crate) slot: u8,
    /// Its name.
    pub(crate) name: String,
    /// Its icon's glyph, if it has one.
    pub(crate) icon: Option<char>,
    /// In hand.
    pub(crate) active: bool,
    /// Has ammo to fire.
    pub(crate) usable: bool,
}

/// The carried weapons in slot order.
pub(crate) fn weapon_slots(inventory: &Inventory, arsenal: &Arsenal) -> Vec<WeaponSlot> {
    let mut weapons: Vec<_> = inventory
        .weapons()
        .filter_map(|weapon| Some((weapon, arsenal.weapon(weapon)?)))
        .collect();
    weapons.sort_by_key(|(_, config)| (config.slot, config.position));
    weapons
        .into_iter()
        .map(|(weapon, config)| WeaponSlot {
            weapon,
            slot: config.slot,
            name: config.name.clone(),
            icon: config.icon,
            active: inventory.active() == Some(weapon),
            usable: inventory.usable(arsenal, weapon),
        })
        .collect()
}

/// `m:ss` for a number of seconds.
pub(crate) fn clock(secs: f32) -> String {
    let secs = secs.max(0.0).ceil() as u32;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The match status for the top of the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Status {
    /// The local player's points.
    pub(crate) points: i32,
    /// The score that wins, if there is a limit.
    pub(crate) limit: Option<u32>,
    /// The clock: time left, or the countdown to the start; empty when
    /// there is no time limit.
    pub(crate) clock: String,
    /// What the match is doing when it is not under way.
    pub(crate) phase: String,
}

/// The status of the match for `local`.
pub(crate) fn status(sim: &Simulation, local: PlayerId) -> Status {
    let rules = &sim.data().rules;
    let game = sim.game();
    let (clock_text, phase) = match game.state() {
        MatchState::Waiting => (
            String::new(),
            format!("Waiting for players ({} needed)", rules.min_players),
        ),
        MatchState::Warmup { left } => (clock(left), "Match starts in".to_owned()),
        MatchState::Playing => (
            game.time_left(rules).map(clock).unwrap_or_default(),
            String::new(),
        ),
        MatchState::Intermission { .. } => (String::new(), "Match over".to_owned()),
    };
    Status {
        points: game.score(local).points,
        limit: (rules.score_limit > 0).then_some(rules.score_limit),
        clock: clock_text,
        phase,
    }
}

/// A message in the middle of the screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Message {
    /// The big line; empty for no message.
    pub(crate) title: String,
    /// The smaller line below it.
    pub(crate) subtitle: String,
}

impl Message {
    fn new(title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
        }
    }
}

/// The message to show `names.local`: the death and respawn, the winner,
/// the last pickup.
pub(crate) fn message(sim: &Simulation, names: &Names, pickup: Option<&str>) -> Message {
    let local = names.local;
    if let MatchState::Intermission { winner, left } = sim.game().state() {
        let title = winner.map_or_else(
            || "Draw".to_owned(),
            |winner| {
                if winner == local {
                    "You win!".to_owned()
                } else {
                    format!("{} wins", names.of(winner))
                }
            },
        );
        return Message::new(title, format!("Next match in {}", clock(left)));
    }
    let Some(player) = sim.player(local) else {
        return Message::default();
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
        return Message::new(by, hint);
    }
    Message::new(pickup.unwrap_or_default(), "")
}

/// A row of the scoreboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScoreRow {
    /// Place, from 1.
    pub(crate) rank: usize,
    /// The player.
    pub(crate) player: PlayerId,
    /// Their name.
    pub(crate) name: String,
    /// Their score.
    pub(crate) score: Score,
    /// The row is the local player's.
    pub(crate) local: bool,
}

/// Every player's score, best first (ties in the order they joined).
pub(crate) fn score_rows(sim: &Simulation, names: &Names) -> Vec<ScoreRow> {
    let mut rows: Vec<_> = sim
        .players()
        .map(|(id, _)| (id, sim.game().score(id)))
        .collect();
    rows.sort_by(|a, b| b.1.points.cmp(&a.1.points).then(a.0.cmp(&b.0)));
    rows.into_iter()
        .enumerate()
        .map(|(index, (player, score))| ScoreRow {
            rank: index + 1,
            player,
            name: names.of(player),
            score,
            local: player == names.local,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use bevy::math::Vec3;
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
    fn kill_entries_name_killer_weapon_and_victim() {
        let arsenal = arsenal();
        let smg = arsenal.find_weapon("smg").expect("smg");
        let (you, other) = (PlayerId::from_index(0), PlayerId::from_index(2));
        let kill = |killer, cause| Kill {
            victim: other,
            killer,
            cause,
        };
        let by_you = kill_entry(&kill(Some(you), Cause::Weapon(smg)), &names(you), &arsenal);
        assert_eq!(by_you.killer.as_deref(), Some("You"));
        assert_eq!(
            (by_you.how.as_str(), by_you.victim.as_str()),
            ("SMG", "Player 3")
        );
        assert!(by_you.by_local && !by_you.of_local);

        let fall = kill_entry(&kill(None, Cause::Fall), &names(you), &arsenal);
        assert_eq!(fall.killer, None);
        assert_eq!(fall.how, "fall");

        let suicide = kill_entry(
            &kill(Some(other), Cause::Weapon(smg)),
            &names(you),
            &arsenal,
        );
        assert_eq!(suicide.killer, None, "a suicide names only the victim");

        let died = kill_entry(&kill(Some(other), Cause::Prop), &names(other), &arsenal);
        assert!(died.of_local && !died.by_local);
    }

    #[test]
    fn ammo_shows_clip_carried_and_secondary_rounds() {
        let arsenal = arsenal();
        let data = GameData::builtin().expect("builtin");
        let mut inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
        inventory.select(&arsenal, arsenal.find_weapon("smg").expect("smg"));
        let smg = ammo(&inventory, &arsenal);
        assert_eq!((smg.main, smg.reserve), (Some(45), Some(45)));
        assert_eq!(smg.secondary, "SMG grenades: 0");
        assert_eq!(smg.weapon, "SMG");
        assert!(!smg.empty && !smg.reloading);

        inventory.select(&arsenal, arsenal.find_weapon("frag").expect("frag"));
        let frag = ammo(&inventory, &arsenal);
        assert_eq!((frag.main, frag.reserve), (Some(1), None));

        inventory.select(&arsenal, arsenal.find_weapon("manipulator").expect("tool"));
        let tool = ammo(&inventory, &arsenal);
        assert_eq!(tool.main, None, "a tool needs no ammo");
        assert!(tool.secondary.is_empty());
    }

    #[test]
    fn the_weapon_strip_lists_by_slot_and_marks_the_one_in_hand() {
        let arsenal = arsenal();
        let data = GameData::builtin().expect("builtin");
        let inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
        let slots = weapon_slots(&inventory, &arsenal);
        assert_eq!(slots.first().map(|s| s.name.as_str()), Some("Crowbar"));
        let active: Vec<_> = slots.iter().filter(|s| s.active).collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "Gravity Gun");
        assert!(slots.windows(2).all(|pair| pair[0].slot <= pair[1].slot));
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
    /// shot from the first's .357 Magnum.
    fn duel(health: f32, data: &GameData) -> Simulation {
        use rl_sim::collision::WorldBuilder;
        use rl_sim::movement::{Buttons, UserCmd};

        let world = WorldBuilder::new()
            .solid_box(
                Vec3::splat(-2000.0).with_z(-64.0),
                Vec3::splat(2000.0).with_z(0.0),
            )
            .build();
        let mut data = data.clone();
        data.rules.warmup = 0.0;
        let mut sim = Simulation::new(std::sync::Arc::new(world), &data);
        let (you, them) = (
            sim.spawn_player(Vec3::new(0.0, 0.0, 1.0)),
            sim.spawn_player(Vec3::new(200.0, 0.0, 1.0)),
        );
        // The match starts on the first tick, resetting both players.
        sim.tick(&[]);
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
        for _ in 0..80 {
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
        let seen_by_victim = message(&sim, &names(them), None);
        assert_eq!(seen_by_victim.title, "Killed by Player 1");
        assert!(
            seen_by_victim.subtitle.starts_with("Respawn in"),
            "{seen_by_victim:?}"
        );
        assert_eq!(message(&sim, &names(you), None), Message::default());
        assert_eq!(
            message(&sim, &names(you), Some("Health Kit")).title,
            "Health Kit"
        );

        let rows = score_rows(&sim, &names(you));
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].rank, rows[0].name.as_str(), rows[0].local),
            (1, "You", true)
        );
        assert_eq!(rows[0].score.points, 12);
        assert_eq!(rows[0].score.kills, 1);
        assert_eq!((rows[1].rank, rows[1].name.as_str()), (2, "Player 2"));
        assert_eq!(rows[1].score.deaths, 1);

        let status = status(&sim, you);
        assert_eq!(status.points, 12);
        assert_eq!(status.limit, Some(250));
        // The magnum took a second and a half to come up.
        assert_eq!(status.clock, "9:59");
        assert!(status.phase.is_empty());
    }

    #[test]
    fn the_end_of_a_match_names_the_winner() {
        let mut data = GameData::builtin().expect("builtin");
        data.rules.score_limit = 1;
        let sim = duel(10.0, &data);
        let winner_sees = message(&sim, &names(PlayerId::from_index(0)), None);
        assert_eq!(winner_sees.title, "You win!");
        assert!(winner_sees.subtitle.starts_with("Next match in"));
        let loser_sees = message(&sim, &names(PlayerId::from_index(1)), None);
        assert_eq!(loser_sees.title, "Player 1 wins");
    }

    #[test]
    fn the_wait_for_players_and_the_countdown_are_announced() {
        let mut data = GameData::builtin().expect("builtin");
        data.rules.min_players = 2;
        data.rules.warmup = 5.0;
        let mut sim = Simulation::new(
            std::sync::Arc::new(rl_sim::collision::WorldBuilder::new().build()),
            &data,
        );
        let you = sim.spawn_player(Vec3::new(0.0, 0.0, 1.0));
        let waiting = status(&sim, you);
        assert_eq!(waiting.phase, "Waiting for players (2 needed)");
        assert_eq!(waiting.clock, "");

        sim.spawn_player(Vec3::new(50.0, 0.0, 1.0));
        let counting = status(&sim, you);
        assert_eq!(counting.phase, "Match starts in");
        assert_eq!(counting.clock, clock(5.0));
    }
}
