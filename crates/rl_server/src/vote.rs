//! The vote for the next map, held while the scoreboard is up.

use std::collections::BTreeMap;

use renet::ClientId;
use rl_sim::rng::Rng;

/// Maps offered in a vote, at most.
pub(crate) const OPTIONS: usize = 4;

/// An open vote: the maps on offer and who voted for which.
#[derive(Debug)]
pub(crate) struct Vote {
    options: Vec<String>,
    ballots: BTreeMap<ClientId, u16>,
}

impl Vote {
    /// Offers up to [`OPTIONS`] of `maps` other than `current`, picked at
    /// random. `None` if there is nothing else to play.
    pub(crate) fn open(current: &str, maps: &[String], rng: &mut Rng) -> Option<Self> {
        let mut candidates: Vec<&String> = maps.iter().filter(|map| *map != current).collect();
        candidates.dedup();
        // Fisher-Yates, so every map has the same chance of being offered.
        for index in (1..candidates.len()).rev() {
            candidates.swap(index, rng.below(index + 1));
        }
        candidates.truncate(OPTIONS);
        if candidates.is_empty() {
            return None;
        }
        Some(Self {
            options: candidates.into_iter().cloned().collect(),
            ballots: BTreeMap::new(),
        })
    }

    pub(crate) fn options(&self) -> &[String] {
        &self.options
    }

    /// Records `voter`'s choice, replacing any earlier one. `false` if
    /// there is no such option.
    pub(crate) fn cast(&mut self, voter: ClientId, option: u16) -> bool {
        if usize::from(option) >= self.options.len() {
            return false;
        }
        self.ballots.insert(voter, option);
        true
    }

    /// Withdraws a voter's ballot, because they left.
    pub(crate) fn withdraw(&mut self, voter: ClientId) -> bool {
        self.ballots.remove(&voter).is_some()
    }

    /// Votes for each option.
    pub(crate) fn tally(&self) -> Vec<u16> {
        let mut tally = vec![0u16; self.options.len()];
        for &option in self.ballots.values() {
            if let Some(count) = tally.get_mut(usize::from(option)) {
                *count = count.saturating_add(1);
            }
        }
        tally
    }

    /// The map with the most votes; a tie, or no votes at all, is settled at
    /// random among the leaders.
    pub(crate) fn winner(&self, rng: &mut Rng) -> &str {
        let tally = self.tally();
        let best = tally.iter().copied().max().unwrap_or(0);
        let leaders: Vec<usize> = (0..tally.len()).filter(|&i| tally[i] == best).collect();
        let pick = leaders.get(rng.below(leaders.len())).copied().unwrap_or(0);
        &self.options[pick]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maps(names: &[&str]) -> Vec<String> {
        names.iter().map(|&name| name.to_owned()).collect()
    }

    #[test]
    fn the_current_map_is_not_offered_and_the_offer_is_capped() {
        let pool = maps(&["a", "b", "c", "d", "e", "f", "g"]);
        let mut rng = Rng::seeded(&[1]);
        let vote = Vote::open("a", &pool, &mut rng).unwrap();
        assert_eq!(vote.options().len(), OPTIONS);
        assert!(!vote.options().contains(&"a".to_owned()));
        let mut unique = vote.options().to_vec();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), OPTIONS, "no map twice");
    }

    #[test]
    fn nothing_to_vote_on_with_a_single_map() {
        let mut rng = Rng::seeded(&[1]);
        assert!(Vote::open("a", &maps(&["a"]), &mut rng).is_none());
        assert!(Vote::open("a", &[], &mut rng).is_none());
    }

    #[test]
    fn the_most_voted_map_wins_and_the_latest_ballot_counts() {
        let mut rng = Rng::seeded(&[2]);
        let mut vote = Vote::open("a", &maps(&["a", "b", "c", "d"]), &mut rng).unwrap();
        assert!(vote.cast(1, 0));
        assert!(vote.cast(2, 1));
        assert!(vote.cast(3, 1));
        assert!(!vote.cast(4, 9), "no such option");
        assert_eq!(vote.tally(), [1, 2, 0]);
        assert_eq!(vote.winner(&mut rng), vote.options()[1]);

        vote.cast(2, 0);
        vote.cast(3, 0);
        assert_eq!(vote.tally(), [3, 0, 0]);
        assert!(vote.withdraw(3));
        assert!(!vote.withdraw(3));
        assert_eq!(vote.tally(), [2, 0, 0]);
    }

    #[test]
    fn ties_and_empty_votes_are_settled_at_random_among_the_leaders() {
        let pool = maps(&["a", "b", "c"]);
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..40 {
            let mut rng = Rng::seeded(&[seed]);
            let mut vote = Vote::open("z", &pool, &mut rng).unwrap();
            vote.cast(1, 0);
            vote.cast(2, 1);
            let winner = vote.winner(&mut rng).to_owned();
            assert_ne!(winner, vote.options()[2], "the unvoted map cannot win");
            seen.insert(vote.options().iter().position(|map| *map == winner));
        }
        assert_eq!(seen.len(), 2, "both leaders win sometimes: {seen:?}");

        let mut rng = Rng::seeded(&[9]);
        let vote = Vote::open("z", &pool, &mut rng).unwrap();
        let winner = vote.winner(&mut rng).to_owned();
        assert!(pool.contains(&winner), "no votes still picks a map");
    }
}
