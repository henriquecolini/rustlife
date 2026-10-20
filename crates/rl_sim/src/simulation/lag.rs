//! Lag compensation: where players were on recent ticks.
//!
//! A client draws other players in the past — a snapshot interval or two
//! behind the newest snapshot, which itself left the server half a round
//! trip ago — so the target a player aims at has moved on by the time their
//! command reaches the server. To be fair to the shooter, hitscan shots and
//! swings are traced against every player's box as the shooter saw it.
//! [`History`] keeps each tick's boxes for the last `max_rewind` seconds
//! (`net.ron`), and answers for any moment in between by interpolating the
//! two ticks around it, exactly as the client interpolated what it drew.
//!
//! Only player boxes are rewound. The map does not move, and props are not
//! rewound: a shot passes where the props are now.

use std::collections::VecDeque;

use crate::combat::Target;

/// Every living player's box after each recent tick.
#[derive(Debug, Clone, Default)]
pub struct History {
    /// `(tick, boxes after it)`, oldest first.
    frames: VecDeque<(u64, Vec<Target>)>,
    /// Frames kept.
    capacity: usize,
}

impl History {
    /// A history remembering `max_rewind` seconds of ticks lasting `dt`
    /// seconds.
    #[must_use]
    pub fn new(max_rewind: f32, dt: f32) -> Self {
        let ticks = (max_rewind / dt.max(f32::EPSILON)).ceil().max(0.0) as usize;
        Self {
            frames: VecDeque::with_capacity(ticks + 1),
            capacity: ticks + 1,
        }
    }

    /// Records the boxes after `tick`, forgetting the oldest beyond the
    /// window.
    pub fn record(&mut self, tick: u64, targets: Vec<Target>) {
        while self.frames.len() >= self.capacity {
            self.frames.pop_front();
        }
        self.frames.push_back((tick, targets));
    }

    /// The boxes at `tick`, which may fall between ticks: each player is
    /// placed between where they were on the ticks either side, and those
    /// alive on the nearer of the two are included. Moments older than the
    /// window give its oldest frame; `None` for moments at or after the
    /// newest frame (the present is the simulation's to say).
    #[must_use]
    pub fn at(&self, tick: f64) -> Option<Vec<Target>> {
        let &(newest, _) = self.frames.back()?;
        if tick >= newest as f64 {
            return None;
        }
        let after = self
            .frames
            .iter()
            .position(|&(frame, _)| frame as f64 > tick)?;
        let Some(before) = after.checked_sub(1) else {
            return Some(self.frames[0].1.clone());
        };
        let (from_tick, from) = &self.frames[before];
        let (to_tick, to) = &self.frames[after];
        let span = (*to_tick - *from_tick) as f64;
        let fraction = ((tick - *from_tick as f64) / span).clamp(0.0, 1.0) as f32;
        let (nearer, other, toward) = if fraction < 0.5 {
            (from, to, fraction)
        } else {
            (to, from, 1.0 - fraction)
        };
        let blended = nearer
            .iter()
            .map(|target| {
                let Some(seen) = other.iter().find(|seen| seen.player == target.player) else {
                    return *target;
                };
                Target {
                    player: target.player,
                    mins: target.mins.lerp(seen.mins, toward),
                    maxs: target.maxs.lerp(seen.maxs, toward),
                }
            })
            .collect();
        Some(blended)
    }
}

#[cfg(test)]
mod tests {
    use glam::Vec3;

    use super::*;
    use crate::simulation::PlayerId;

    fn target(index: usize, x: f32) -> Target {
        Target {
            player: PlayerId::from_index(index),
            mins: Vec3::new(x - 16.0, -16.0, 0.0),
            maxs: Vec3::new(x + 16.0, 16.0, 72.0),
        }
    }

    fn history() -> History {
        let mut history = History::new(0.05, 0.01);
        for tick in 1..=10 {
            history.record(tick, vec![target(0, tick as f32 * 10.0)]);
        }
        history
    }

    #[test]
    fn keeps_only_the_window() {
        let history = history();
        assert_eq!(history.frames.len(), 6);
        assert_eq!(history.frames.front().map(|frame| frame.0), Some(5));
    }

    #[test]
    fn places_players_between_ticks() {
        let boxes = history().at(7.25).expect("in the window");
        let center = (boxes[0].mins + boxes[0].maxs) / 2.0;
        assert!((center.x - 72.5).abs() < 1e-4, "{center}");
    }

    #[test]
    fn the_present_and_beyond_are_not_history() {
        assert!(history().at(10.0).is_none());
        assert!(history().at(12.0).is_none());
        assert!(History::default().at(1.0).is_none());
    }

    #[test]
    fn moments_before_the_window_give_its_oldest_frame() {
        let boxes = history().at(1.0).expect("clamped");
        assert!((boxes[0].mins.x - 34.0).abs() < 1e-4, "{:?}", boxes[0]);
    }

    #[test]
    fn who_is_alive_follows_the_nearer_tick() {
        let mut history = History::new(1.0, 0.01);
        history.record(1, vec![target(0, 0.0), target(1, 100.0)]);
        history.record(2, vec![target(0, 10.0)]);
        assert_eq!(history.at(1.2).expect("between").len(), 2);
        assert_eq!(history.at(1.8).expect("between").len(), 1);
    }
}
