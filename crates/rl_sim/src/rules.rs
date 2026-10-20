//! The rules of a free-for-all deathmatch (`rules/deathmatch.ron`).
//!
//! [`Match`] keeps the score and the clock: a kill earns the killer a frag,
//! dying by your own hand (or a fall, or the map) costs one, and the match
//! ends at the frag or time limit. After the intermission it starts over.
//! [`choose_spawn`] picks where the dead come back: away from everyone
//! else, with a little chance thrown in so spawns are not predictable.

use glam::Vec3;
use rl_data::RulesConfig;

use crate::rng::Rng;
use crate::simulation::PlayerId;

/// One player's score.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Score {
    /// Kills, less suicides.
    pub frags: i32,
    /// Times died.
    pub deaths: u32,
}

/// Where the match is.
#[derive(Debug, Clone, Copy, PartialEq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchEvent {
    /// The match ended, won by this player (`None` for a tie).
    Ended(Option<PlayerId>),
    /// The intermission is over: a new match begins.
    Restarted,
}

/// The score and clock of a match.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    state: MatchState,
    /// Seconds played.
    elapsed: f32,
    /// Scores by player index.
    scores: Vec<Score>,
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

    /// Makes room for a newly joined player's score.
    pub fn add_player(&mut self) {
        self.scores.push(Score::default());
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
        self.scores.get(player.index()).copied().unwrap_or_default()
    }

    /// Every score, by player index.
    #[must_use]
    pub fn scores(&self) -> &[Score] {
        &self.scores
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
        if let Some(score) = self.scores.get_mut(victim.index()) {
            score.deaths += 1;
        }
        let Some(killer) = killer.filter(|&killer| killer != victim) else {
            if let Some(score) = self.scores.get_mut(victim.index()) {
                score.frags -= i32::try_from(rules.suicide_penalty).unwrap_or(i32::MAX);
            }
            return None;
        };
        let score = self.scores.get_mut(killer.index())?;
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
                self.scores.fill(Score::default());
                Some(MatchEvent::Restarted)
            }
        }
    }

    fn end(&mut self, rules: &RulesConfig) -> MatchEvent {
        let best = self.scores.iter().map(|score| score.frags).max();
        let mut leaders = self
            .scores
            .iter()
            .enumerate()
            .filter(|(_, score)| Some(score.frags) == best);
        let winner = match (leaders.next(), leaders.next()) {
            (Some((index, _)), None) => Some(PlayerId::from_index(index)),
            _ => None,
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
        game.add_player();
        game.add_player();
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
        game.add_player();
        game.add_player();
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
        game.add_player();
        game.add_player();
        let events: Vec<_> = (0..70)
            .filter_map(|_| game.tick(1.0 / 66.0, &rules))
            .collect();
        assert_eq!(events, [MatchEvent::Ended(None)]);
        assert_eq!(game.time_left(&rules), Some(0.0));
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
