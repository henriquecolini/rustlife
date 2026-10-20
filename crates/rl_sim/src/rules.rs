//! The rules of a free-for-all deathmatch (`rules/deathmatch.ron`).
//!
//! [`Match`] keeps the score and the clock: a kill earns the killer a frag,
//! dying by your own hand (or a fall, or the map) costs one, and the match
//! ends at the frag or time limit. After the intermission it starts over.
//! [`choose_spawn`] picks where the dead come back: away from everyone
//! else, with a little chance thrown in so spawns are not predictable.

use glam::Vec3;
use rl_data::RulesConfig;
use serde::{Deserialize, Serialize};

use crate::rng::Rng;
use crate::simulation::PlayerId;

/// One player's score.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Score {
    /// Kills, less suicides.
    pub frags: i32,
    /// Times died.
    pub deaths: u32,
}

/// Where the match is.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MatchState {
    /// Under way.
    Playing,
    /// Over, showing the scores; the next match starts in `left` seconds.
    Intermission {
        /// The player with the most frags, if one stands alone at the top.
        winner: Option<PlayerId>,
        /// Seconds until the next match.
        left: f32,
    },
}

/// A change in the match's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchEvent {
    /// The match ended, won by this player (`None` for a tie).
    Ended(Option<PlayerId>),
    /// The intermission is over: a new match begins.
    Restarted,
}

/// The score and clock of a match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Match {
    state: MatchState,
    /// Seconds played.
    elapsed: f32,
    /// Scores by player index; `None` for ids nobody holds.
    scores: Vec<Option<Score>>,
}

impl Default for Match {
    fn default() -> Self {
        Self::new()
    }
}

impl Match {
    /// A match just started, with no players.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: MatchState::Playing,
            elapsed: 0.0,
            scores: Vec::new(),
        }
    }

    /// Starts a newly joined player's score at nothing.
    pub fn add_player(&mut self, player: PlayerId) {
        if self.scores.len() <= player.index() {
            self.scores.resize(player.index() + 1, None);
        }
        self.scores[player.index()] = Some(Score::default());
    }

    /// Forgets the score of a player who left.
    pub fn remove_player(&mut self, player: PlayerId) {
        if let Some(score) = self.scores.get_mut(player.index()) {
            *score = None;
        }
    }

    /// Where the match is.
    #[must_use]
    pub fn state(&self) -> MatchState {
        self.state
    }

    /// Whether the match is under way.
    #[must_use]
    pub fn playing(&self) -> bool {
        self.state == MatchState::Playing
    }

    /// Seconds played.
    #[must_use]
    pub fn elapsed(&self) -> f32 {
        self.elapsed
    }

    /// Seconds left before the time limit, if there is one.
    #[must_use]
    pub fn time_left(&self, rules: &RulesConfig) -> Option<f32> {
        (rules.time_limit > 0.0).then(|| (rules.time_limit - self.elapsed).max(0.0))
    }

    /// A player's score.
    #[must_use]
    pub fn score(&self, player: PlayerId) -> Score {
        self.scores
            .get(player.index())
            .copied()
            .flatten()
            .unwrap_or_default()
    }

    /// Every player's score.
    pub fn scores(&self) -> impl Iterator<Item = (PlayerId, Score)> + '_ {
        self.scores
            .iter()
            .enumerate()
            .filter_map(|(index, score)| Some((PlayerId::from_index(index), (*score)?)))
    }

    /// Scores a death: a frag for the killer, or the suicide penalty when
    /// there is no other killer. Ends the match if the killer reached the
    /// frag limit.
    pub fn record_death(
        &mut self,
        victim: PlayerId,
        killer: Option<PlayerId>,
        rules: &RulesConfig,
    ) -> Option<MatchEvent> {
        if !self.playing() {
            return None;
        }
        if let Some(score) = self.score_mut(victim) {
            score.deaths += 1;
        }
        let Some(killer) = killer.filter(|&killer| killer != victim) else {
            if let Some(score) = self.score_mut(victim) {
                score.frags -= i32::try_from(rules.suicide_penalty).unwrap_or(i32::MAX);
            }
            return None;
        };
        let score = self.score_mut(killer)?;
        score.frags += 1;
        let limit = i64::from(rules.frag_limit);
        (limit > 0 && i64::from(score.frags) >= limit).then(|| self.end(rules))
    }

    /// Advances the clock by `dt` seconds: ends the match at the time
    /// limit, and starts the next once the intermission is over.
    pub fn tick(&mut self, dt: f32, rules: &RulesConfig) -> Option<MatchEvent> {
        match self.state {
            MatchState::Playing => {
                self.elapsed += dt;
                (rules.time_limit > 0.0 && self.elapsed >= rules.time_limit)
                    .then(|| self.end(rules))
            }
            MatchState::Intermission { winner, left } => {
                if left > dt {
                    self.state = MatchState::Intermission {
                        winner,
                        left: left - dt,
                    };
                    return None;
                }
                self.state = MatchState::Playing;
                self.elapsed = 0.0;
                for score in self.scores.iter_mut().flatten() {
                    *score = Score::default();
                }
                Some(MatchEvent::Restarted)
            }
        }
    }

    fn score_mut(&mut self, player: PlayerId) -> Option<&mut Score> {
        self.scores.get_mut(player.index())?.as_mut()
    }

    fn end(&mut self, rules: &RulesConfig) -> MatchEvent {
        let best = self.scores().map(|(_, score)| score.frags).max();
        let winner = {
            let mut leaders = self.scores().filter(|(_, score)| Some(score.frags) == best);
            match (leaders.next(), leaders.next()) {
                (Some((player, _)), None) => Some(player),
                _ => None,
            }
        };
        self.state = MatchState::Intermission {
            winner,
            left: rules.intermission,
        };
        MatchEvent::Ended(winner)
    }
}

/// Picks a spawn point: the `candidates` points farthest from the nearest
/// of `others` (other living players), of those `usable` (not blocked),
/// one at random. `None` if no point is usable.
pub fn choose_spawn(
    spawns: &[Vec3],
    others: &[Vec3],
    candidates: u32,
    rng: &mut Rng,
    usable: impl Fn(Vec3) -> bool,
) -> Option<usize> {
    let mut ranked: Vec<(usize, f32)> = spawns
        .iter()
        .enumerate()
        .filter(|&(_, &spawn)| usable(spawn))
        .map(|(index, &spawn)| {
            let nearest = others
                .iter()
                .map(|other| other.distance_squared(spawn))
                .fold(f32::INFINITY, f32::min);
            (index, nearest)
        })
        .collect();
    // Farthest first; ties keep map order, so the choice is deterministic.
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let pool = ranked.len().min(candidates.max(1) as usize);
    ranked.get(rng.below(pool)).map(|&(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_data::GameData;

    fn rules() -> RulesConfig {
        GameData::builtin().expect("builtin").rules
    }

    fn player(index: usize) -> PlayerId {
        PlayerId::from_index(index)
    }

    #[test]
    fn kills_score_frags_and_suicides_cost_one() {
        let rules = rules();
        let mut game = Match::new();
        game.add_player(player(0));
        game.add_player(player(1));
        game.record_death(player(1), Some(player(0)), &rules);
        game.record_death(player(0), Some(player(0)), &rules);
        game.record_death(player(1), None, &rules);
        assert_eq!(
            game.score(player(0)),
            Score {
                frags: 0,
                deaths: 1
            }
        );
        assert_eq!(
            game.score(player(1)),
            Score {
                frags: -1,
                deaths: 2
            }
        );
    }

    #[test]
    fn the_frag_limit_ends_the_match_and_the_next_one_starts_clean() {
        let rules = RulesConfig {
            frag_limit: 2,
            ..rules()
        };
        let mut game = Match::new();
        game.add_player(player(0));
        game.add_player(player(1));
        assert_eq!(game.record_death(player(1), Some(player(0)), &rules), None);
        assert_eq!(
            game.record_death(player(1), Some(player(0)), &rules),
            Some(MatchEvent::Ended(Some(player(0))))
        );
        assert!(!game.playing());
        assert_eq!(
            game.record_death(player(0), Some(player(1)), &rules),
            None,
            "no scoring in the intermission"
        );
        let ticks = (rules.intermission * 66.0).ceil() as usize + 1;
        let events: Vec<_> = (0..ticks)
            .filter_map(|_| game.tick(1.0 / 66.0, &rules))
            .collect();
        assert_eq!(events, [MatchEvent::Restarted]);
        assert_eq!(game.score(player(0)), Score::default());
        assert_eq!(game.elapsed(), 0.0);
    }

    #[test]
    fn the_time_limit_ends_the_match_in_a_tie() {
        let rules = RulesConfig {
            time_limit: 1.0,
            ..rules()
        };
        let mut game = Match::new();
        game.add_player(player(0));
        game.add_player(player(1));
        let events: Vec<_> = (0..70)
            .filter_map(|_| game.tick(1.0 / 66.0, &rules))
            .collect();
        assert_eq!(events, [MatchEvent::Ended(None)]);
        assert_eq!(game.time_left(&rules), Some(0.0));
    }

    #[test]
    fn players_who_left_neither_score_nor_win() {
        let rules = RulesConfig {
            frag_limit: 1,
            ..rules()
        };
        let mut game = Match::new();
        game.add_player(player(0));
        game.add_player(player(2));
        game.remove_player(player(0));
        assert_eq!(
            game.record_death(player(0), Some(player(2)), &rules),
            Some(MatchEvent::Ended(Some(player(2))))
        );
        let scores: Vec<_> = game.scores().collect();
        assert_eq!(
            scores,
            [(
                player(2),
                Score {
                    frags: 1,
                    deaths: 0
                }
            )]
        );
    }

    #[test]
    fn spawns_far_from_everyone_are_preferred() {
        let spawns = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1000.0, 0.0, 0.0),
            Vec3::new(2000.0, 0.0, 0.0),
        ];
        let others = [Vec3::new(100.0, 0.0, 0.0)];
        for seed in 0..20 {
            let mut rng = Rng::seeded(&[seed]);
            assert_eq!(
                choose_spawn(&spawns, &others, 1, &mut rng, |_| true),
                Some(2)
            );
            let pick = choose_spawn(&spawns, &others, 2, &mut rng, |_| true);
            assert!(matches!(pick, Some(1 | 2)), "{pick:?}");
        }
        let mut rng = Rng::seeded(&[0]);
        assert_eq!(
            choose_spawn(&spawns, &others, 1, &mut rng, |spawn| spawn.x < 1500.0),
            Some(1),
            "blocked spawns are skipped"
        );
        assert_eq!(choose_spawn(&spawns, &others, 3, &mut rng, |_| false), None);
    }
}
