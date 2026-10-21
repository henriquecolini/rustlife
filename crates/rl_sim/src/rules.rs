//! The rules of a free-for-all deathmatch (`rules/deathmatch.ron`).
//!
//! [`Match`] keeps the score and the clock through a match's life:
//!
//! 1. [`MatchState::Waiting`] until there are enough players. Everyone may
//!    move and shoot, but nobody is hurt and nothing scores.
//! 2. [`MatchState::Warmup`], a short countdown with the same freedom.
//! 3. [`MatchState::Playing`]: a kill earns the killer points (more for some
//!    ways to kill, see [`rl_data::PointsConfig`]), dying by your own hand
//!    (or a fall, or the map) costs some, and the match ends at the score or
//!    time limit.
//! 4. [`MatchState::Intermission`]: the scoreboard, and the vote for the next
//!    map. Afterwards the next match starts.
//!
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
    /// Points earned by kills, less the cost of suicides.
    pub points: i32,
    /// Players killed.
    pub kills: u32,
    /// Times died.
    pub deaths: u32,
}

/// Where the match is.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MatchState {
    /// Waiting for enough players to start.
    Waiting,
    /// Enough players; the match starts when the countdown ends.
    Warmup {
        /// Seconds until the match starts.
        left: f32,
    },
    /// Under way.
    Playing,
    /// Over, showing the scores; the next match starts in `left` seconds.
    Intermission {
        /// The player with the most points, if one stands alone at the top.
        winner: Option<PlayerId>,
        /// Seconds until the next match.
        left: f32,
    },
}

/// A change in the match's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchEvent {
    /// The warmup is over: the match begins, with everyone back at a spawn
    /// point and the scores at nothing.
    Started,
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
    /// A match waiting for its first players.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: MatchState::Waiting,
            elapsed: 0.0,
            scores: Vec::new(),
        }
    }

    /// Starts a newly joined player's score at nothing, and starts the
    /// countdown if there are now enough players.
    pub fn add_player(&mut self, player: PlayerId, rules: &RulesConfig) {
        if self.scores.len() <= player.index() {
            self.scores.resize(player.index() + 1, None);
        }
        self.scores[player.index()] = Some(Score::default());
        self.population_changed(rules);
    }

    /// Forgets the score of a player who left. The match goes back to
    /// waiting if too few players are left to start one, or none to play it.
    pub fn remove_player(&mut self, player: PlayerId, rules: &RulesConfig) {
        if let Some(score) = self.scores.get_mut(player.index()) {
            *score = None;
        }
        self.population_changed(rules);
    }

    fn population_changed(&mut self, rules: &RulesConfig) {
        let players = self.scores().count();
        let enough = players >= rules.min_players.max(1) as usize;
        match self.state {
            MatchState::Waiting if enough => {
                self.state = MatchState::Warmup { left: rules.warmup };
            }
            MatchState::Warmup { .. } if !enough => self.state = MatchState::Waiting,
            MatchState::Playing | MatchState::Intermission { .. } if players == 0 => {
                // An empty server forgets the match that was.
                self.state = MatchState::Waiting;
                self.elapsed = 0.0;
            }
            _ => {}
        }
    }

    /// Where the match is.
    #[must_use]
    pub fn state(&self) -> MatchState {
        self.state
    }

    /// Whether the match is under way: kills hurt and score.
    #[must_use]
    pub fn playing(&self) -> bool {
        self.state == MatchState::Playing
    }

    /// Whether the match is over and the scoreboard is on show. Nobody
    /// moves or fires then.
    #[must_use]
    pub fn intermission(&self) -> bool {
        matches!(self.state, MatchState::Intermission { .. })
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

    /// Scores a death: `points` for the killer, or the suicide penalty when
    /// there is no other killer. Ends the match if the killer reached the
    /// score limit.
    pub fn record_death(
        &mut self,
        victim: PlayerId,
        killer: Option<PlayerId>,
        points: u32,
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
                score.points -= i32::try_from(rules.suicide_penalty).unwrap_or(i32::MAX);
            }
            return None;
        };
        let score = self.score_mut(killer)?;
        score.kills += 1;
        score.points = score
            .points
            .saturating_add(i32::try_from(points).unwrap_or(i32::MAX));
        let limit = i64::from(rules.score_limit);
        (limit > 0 && i64::from(score.points) >= limit).then(|| self.end(rules))
    }

    /// Advances the clock by `dt` seconds: counts down the warmup, ends the
    /// match at the time limit, and starts the next once the intermission is
    /// over.
    pub fn tick(&mut self, dt: f32, rules: &RulesConfig) -> Option<MatchEvent> {
        match self.state {
            MatchState::Waiting => None,
            MatchState::Warmup { left } => {
                if left > dt {
                    self.state = MatchState::Warmup { left: left - dt };
                    return None;
                }
                self.begin();
                Some(MatchEvent::Started)
            }
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
                self.begin();
                Some(MatchEvent::Restarted)
            }
        }
    }

    /// Starts a match from scratch.
    fn begin(&mut self) {
        self.state = MatchState::Playing;
        self.elapsed = 0.0;
        for score in self.scores.iter_mut().flatten() {
            *score = Score::default();
        }
    }

    fn score_mut(&mut self, player: PlayerId) -> Option<&mut Score> {
        self.scores.get_mut(player.index())?.as_mut()
    }

    fn end(&mut self, rules: &RulesConfig) -> MatchEvent {
        let best = self.scores().map(|(_, score)| score.points).max();
        let winner = {
            let mut leaders = self
                .scores()
                .filter(|(_, score)| Some(score.points) == best);
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

    /// Rules that start the match as soon as someone joins.
    fn instant() -> RulesConfig {
        RulesConfig {
            warmup: 0.0,
            ..rules()
        }
    }

    fn player(index: usize) -> PlayerId {
        PlayerId::from_index(index)
    }

    /// A match with players `0..count` already playing.
    fn playing(count: usize, rules: &RulesConfig) -> Match {
        let mut game = Match::new();
        for index in 0..count {
            game.add_player(player(index), rules);
        }
        assert_eq!(game.tick(1.0 / 66.0, rules), Some(MatchEvent::Started));
        game
    }

    fn run(game: &mut Match, seconds: f32, rules: &RulesConfig) -> Vec<MatchEvent> {
        let ticks = (seconds * 66.0).ceil() as usize + 1;
        (0..ticks)
            .filter_map(|_| game.tick(1.0 / 66.0, rules))
            .collect()
    }

    #[test]
    fn kills_score_their_points_and_suicides_cost_some() {
        let rules = instant();
        let mut game = playing(2, &rules);
        game.record_death(player(1), Some(player(0)), 20, &rules);
        game.record_death(player(0), Some(player(0)), 20, &rules);
        game.record_death(player(1), None, 20, &rules);
        let penalty = i32::try_from(rules.suicide_penalty).unwrap();
        assert_eq!(
            game.score(player(0)),
            Score {
                points: 20 - penalty,
                kills: 1,
                deaths: 1
            }
        );
        assert_eq!(
            game.score(player(1)),
            Score {
                points: -penalty,
                kills: 0,
                deaths: 2
            }
        );
    }

    #[test]
    fn the_match_waits_for_enough_players_then_counts_down() {
        let rules = RulesConfig {
            min_players: 2,
            warmup: 3.0,
            ..rules()
        };
        let mut game = Match::new();
        assert_eq!(game.state(), MatchState::Waiting);
        game.add_player(player(0), &rules);
        assert_eq!(run(&mut game, 5.0, &rules), []);
        assert_eq!(
            game.state(),
            MatchState::Waiting,
            "one player is not enough"
        );

        game.add_player(player(1), &rules);
        assert!(matches!(game.state(), MatchState::Warmup { .. }));
        assert_eq!(run(&mut game, 2.0, &rules), []);
        assert!(!game.playing());
        assert_eq!(run(&mut game, 1.5, &rules), [MatchEvent::Started]);
        assert!(game.playing());
    }

    #[test]
    fn nothing_scores_before_the_match_starts() {
        let rules = rules();
        let mut game = Match::new();
        game.add_player(player(0), &rules);
        game.add_player(player(1), &rules);
        assert!(!game.playing());
        assert_eq!(
            game.record_death(player(1), Some(player(0)), 10, &rules),
            None
        );
        assert_eq!(game.score(player(0)), Score::default());
        assert_eq!(game.score(player(1)), Score::default());
    }

    #[test]
    fn losing_players_cancels_the_countdown_and_an_empty_game_resets() {
        let rules = RulesConfig {
            min_players: 2,
            ..rules()
        };
        let mut game = Match::new();
        game.add_player(player(0), &rules);
        game.add_player(player(1), &rules);
        game.remove_player(player(1), &rules);
        assert_eq!(game.state(), MatchState::Waiting);

        let rules = instant();
        let mut game = playing(1, &rules);
        game.record_death(player(0), None, 0, &rules);
        game.remove_player(player(0), &rules);
        assert_eq!(game.state(), MatchState::Waiting);
        assert_eq!(game.elapsed(), 0.0);
        game.add_player(player(0), &rules);
        assert_eq!(game.score(player(0)), Score::default(), "a fresh match");
    }

    #[test]
    fn the_score_limit_ends_the_match_and_the_next_one_starts_clean() {
        let rules = RulesConfig {
            score_limit: 30,
            ..instant()
        };
        let mut game = playing(2, &rules);
        assert_eq!(
            game.record_death(player(1), Some(player(0)), 20, &rules),
            None
        );
        assert_eq!(
            game.record_death(player(1), Some(player(0)), 20, &rules),
            Some(MatchEvent::Ended(Some(player(0))))
        );
        assert!(game.intermission());
        assert_eq!(
            game.record_death(player(0), Some(player(1)), 20, &rules),
            None,
            "no scoring in the intermission"
        );
        assert_eq!(
            run(&mut game, rules.intermission, &rules),
            [MatchEvent::Restarted]
        );
        assert!(game.playing());
        assert_eq!(game.score(player(0)), Score::default());
        assert!(game.elapsed() < 0.1, "the clock restarted");
    }

    #[test]
    fn the_time_limit_ends_the_match_in_a_tie() {
        let rules = RulesConfig {
            time_limit: 1.0,
            ..instant()
        };
        let mut game = playing(2, &rules);
        assert_eq!(run(&mut game, 1.1, &rules), [MatchEvent::Ended(None)]);
        assert_eq!(game.time_left(&rules), Some(0.0));
    }

    #[test]
    fn players_who_left_neither_score_nor_win() {
        let rules = RulesConfig {
            score_limit: 10,
            ..instant()
        };
        let mut game = Match::new();
        game.add_player(player(0), &rules);
        game.add_player(player(2), &rules);
        game.tick(1.0, &rules);
        game.remove_player(player(0), &rules);
        assert_eq!(
            game.record_death(player(0), Some(player(2)), 10, &rules),
            Some(MatchEvent::Ended(Some(player(2))))
        );
        let scores: Vec<_> = game.scores().collect();
        assert_eq!(
            scores,
            [(
                player(2),
                Score {
                    points: 10,
                    kills: 1,
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
