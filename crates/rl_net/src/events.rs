//! What happened during ticks, as clients hear of it.
//!
//! The server's [`TickEvents`] drive sounds, effects and the HUD. They
//! travel to each client inside its snapshots as a list of [`Event`]s: the
//! ticks since the client's last snapshot, minus what the client predicted
//! for itself (its own trigger pulls and bullets), which it already showed.
//! Like snapshots they may be lost; they are only for show. Kills, which
//! the kill feed must not miss, go reliably instead
//! ([`ServerMessage::Kill`](crate::protocol::ServerMessage::Kill)).
//!
//! A client turns the events back into [`TickEvents`] ([`to_tick_events`]),
//! so the code drawing effects reads the same type whichever side made it.

use rl_sim::rules::MatchEvent;
use rl_sim::simulation::{
    DamageEvent, ExplosionEvent, Kill, MapEvent, PlayerId, RespawnEvent, ShotEvent, TickEvents,
};
use rl_sim::tools::manipulator::ToolEvent;
use rl_sim::weapons::{Mode, WeaponId};
use serde::{Deserialize, Serialize};

/// One thing that happened, for show.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// A player pulled a weapon's trigger.
    Fired {
        /// Who.
        player: PlayerId,
        /// With what.
        weapon: WeaponId,
        /// Which fire mode.
        mode: Mode,
    },
    /// A bullet or swing.
    Shot(ShotEvent),
    /// An explosion.
    Explosion(ExplosionEvent),
    /// Damage taken.
    Damage(DamageEvent),
    /// A pickup taken.
    Pickup {
        /// By whom.
        player: PlayerId,
        /// Which (an index into the simulation's pickups).
        index: u16,
    },
    /// A player drew from a station (a wall charger).
    Charging {
        /// Who.
        player: PlayerId,
        /// Which (an index into the simulation's pickups).
        index: u16,
    },
    /// A player came (back) into the game.
    Respawn(RespawnEvent),
    /// A player's Manipulator acted.
    Tool {
        /// Whose.
        player: PlayerId,
        /// What it did.
        event: ToolEvent,
    },
    /// The match ended or restarted.
    Match(MatchEvent),
    /// The map's logic did something: a door moved, a button was pressed,
    /// glass broke, someone was teleported.
    Map(MapEvent),
}

/// The events of a tick that `viewer` should hear of: everything but the
/// trigger pulls and shots of their own, which they predicted. Kills are
/// left out (they go reliably).
#[must_use]
pub fn from_tick_events(events: &TickEvents, viewer: Option<PlayerId>) -> Vec<Event> {
    let mine = |player: PlayerId| Some(player) == viewer;
    let mut out = Vec::new();
    out.extend(
        events
            .fired
            .iter()
            .filter(|(player, ..)| !mine(*player))
            .map(|&(player, weapon, mode)| Event::Fired {
                player,
                weapon,
                mode,
            }),
    );
    out.extend(
        events
            .shots
            .iter()
            .filter(|shot| !mine(shot.player))
            .map(|&shot| Event::Shot(shot)),
    );
    out.extend(
        events
            .explosions
            .iter()
            .map(|&blast| Event::Explosion(blast)),
    );
    out.extend(events.damage.iter().map(|&damage| Event::Damage(damage)));
    out.extend(events.pickups.iter().map(|&(player, index)| Event::Pickup {
        player,
        index: u16::try_from(index).unwrap_or(u16::MAX),
    }));
    out.extend(
        events
            .charging
            .iter()
            .map(|&(player, index)| Event::Charging {
                player,
                index: u16::try_from(index).unwrap_or(u16::MAX),
            }),
    );
    out.extend(
        events
            .respawns
            .iter()
            .map(|&respawn| Event::Respawn(respawn)),
    );
    out.extend(
        events
            .tools
            .iter()
            .map(|&(player, event)| Event::Tool { player, event }),
    );
    out.extend(events.match_event.map(Event::Match));
    out.extend(events.map.iter().map(|&event| Event::Map(event)));
    out
}

/// Events heard from the server, and kills, as one [`TickEvents`].
#[must_use]
pub fn to_tick_events(events: &[Event], kills: &[Kill]) -> TickEvents {
    let mut tick = TickEvents {
        kills: kills.to_vec(),
        ..TickEvents::default()
    };
    for event in events {
        match *event {
            Event::Fired {
                player,
                weapon,
                mode,
            } => tick.fired.push((player, weapon, mode)),
            Event::Shot(shot) => tick.shots.push(shot),
            Event::Explosion(blast) => tick.explosions.push(blast),
            Event::Damage(damage) => tick.damage.push(damage),
            Event::Pickup { player, index } => tick.pickups.push((player, usize::from(index))),
            Event::Charging { player, index } => {
                tick.charging.push((player, usize::from(index)));
            }
            Event::Respawn(respawn) => tick.respawns.push(respawn),
            Event::Tool { player, event } => tick.tools.push((player, event)),
            Event::Match(event) => tick.match_event = Some(event),
            Event::Map(event) => tick.map.push(event),
        }
    }
    tick
}

#[cfg(test)]
mod tests {
    use glam::Vec3;
    use rl_sim::combat::hitscan::{ShotTrace, Struck};
    use rl_sim::combat::{Cause, Damage};

    use super::*;

    fn player(index: usize) -> PlayerId {
        PlayerId::from_index(index)
    }

    fn shot(by: usize) -> ShotEvent {
        ShotEvent {
            player: player(by),
            weapon: WeaponId::from_index(1),
            start: Vec3::ZERO,
            trace: ShotTrace {
                end: Vec3::X,
                normal: Vec3::ZERO,
                struck: Struck::Nothing,
            },
        }
    }

    fn tick() -> TickEvents {
        TickEvents {
            fired: vec![
                (player(0), WeaponId::from_index(1), Mode::Primary),
                (player(1), WeaponId::from_index(1), Mode::Secondary),
            ],
            shots: vec![shot(0), shot(1)],
            damage: vec![DamageEvent {
                victim: player(0),
                damage: Damage {
                    amount: 10.0,
                    attacker: Some(player(1)),
                    cause: Cause::Fall,
                },
                health_lost: 10.0,
            }],
            pickups: vec![(player(1), 3)],
            match_event: Some(MatchEvent::Restarted),
            ..TickEvents::default()
        }
    }

    #[test]
    fn viewers_do_not_hear_their_own_predicted_shots() {
        let events = from_tick_events(&tick(), Some(player(0)));
        let back = to_tick_events(&events, &[]);
        assert_eq!(back.fired.len(), 1);
        assert_eq!(back.fired[0].0, player(1));
        assert_eq!(back.shots, [shot(1)]);
        assert_eq!(back.damage.len(), 1, "damage to the viewer is heard");
    }

    #[test]
    fn events_round_trip_for_onlookers() {
        let original = tick();
        let back = to_tick_events(&from_tick_events(&original, None), &[]);
        assert_eq!(back, original);
    }
}
