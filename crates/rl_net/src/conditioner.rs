//! Simulated network trouble, for testing how the game copes.
//!
//! [`NetConditions`] describes added latency, jitter and loss
//! (`--net-sim 100ms,10ms,2%`); a [`DelayLine`] applies them to whatever
//! passes through it — packets between an in-process server and client, or
//! messages between a client and its UDP transport — holding each item
//! back until its delivery time and dropping some.
//!
//! The latency is the round trip added: each direction gets half. Jitter
//! moves each item's delivery up to that much either way, so unreliable
//! items may arrive out of order, as UDP packets do. Items marked reliable
//! are never dropped and never overtake one another, so a delay line can
//! sit above an ordered, reliable channel without breaking its promise.
//! Randomness is seeded, so a test sees the same losses every run.

use std::collections::VecDeque;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use rl_sim::rng::Rng;
use thiserror::Error;

/// Added latency, jitter and loss.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NetConditions {
    /// Round-trip time added; each direction gets half.
    pub latency: Duration,
    /// Each item's delivery moves by up to this much, earlier or later.
    pub jitter: Duration,
    /// Share of unreliable items dropped, `0..=1`.
    pub loss: f32,
}

impl NetConditions {
    /// No trouble at all.
    pub const NONE: Self = Self {
        latency: Duration::ZERO,
        jitter: Duration::ZERO,
        loss: 0.0,
    };

    /// Whether these conditions change anything.
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.latency.is_zero() && self.jitter.is_zero() && self.loss <= 0.0
    }
}

impl fmt::Display for NetConditions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}ms,{}ms,{}%",
            self.latency.as_millis(),
            self.jitter.as_millis(),
            self.loss * 100.0
        )
    }
}

/// A malformed `--net-sim` value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "bad network conditions \"{0}\": expected LATENCYms[,JITTERms[,LOSS%]], e.g. 100ms,10ms,2%"
)]
pub struct ParseConditionsError(String);

impl FromStr for NetConditions {
    type Err = ParseConditionsError;

    /// Parses `LATENCYms[,JITTERms[,LOSS%]]`, e.g. `100ms,10ms,2%`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let error = || ParseConditionsError(text.to_owned());
        let millis = |part: &str| {
            part.trim()
                .strip_suffix("ms")
                .and_then(|number| number.trim().parse::<u64>().ok())
                .map(Duration::from_millis)
                .ok_or_else(error)
        };
        let mut parts = text.split(',');
        let latency = millis(parts.next().ok_or_else(error)?)?;
        let jitter = parts.next().map_or(Ok(Duration::ZERO), millis)?;
        let loss = match parts.next() {
            Some(part) => part
                .trim()
                .strip_suffix('%')
                .and_then(|number| number.trim().parse::<f32>().ok())
                .filter(|percent| (0.0..=100.0).contains(percent))
                .ok_or_else(error)?,
            None => 0.0,
        };
        if parts.next().is_some() {
            return Err(error());
        }
        Ok(Self {
            latency,
            jitter,
            loss: loss / 100.0,
        })
    }
}

/// How an item passing a [`DelayLine`] may be treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// May be dropped or overtaken.
    Unreliable,
    /// Always arrives, after every reliable item sent before it.
    Reliable,
}

/// One direction of a simulated link: holds items back and drops some,
/// as [`NetConditions`] say.
#[derive(Debug, Clone)]
pub struct DelayLine<T> {
    conditions: NetConditions,
    rng: Rng,
    /// `(delivery time, item)`, in no particular order.
    queue: VecDeque<(Duration, T)>,
    /// The latest delivery time given to a reliable item.
    last_reliable: Duration,
}

impl<T> DelayLine<T> {
    /// A line applying `conditions`, its randomness seeded by `seed`.
    #[must_use]
    pub fn new(conditions: NetConditions, seed: u64) -> Self {
        Self {
            conditions,
            rng: Rng::seeded(&[seed]),
            queue: VecDeque::new(),
            last_reliable: Duration::ZERO,
        }
    }

    /// The conditions applied.
    #[must_use]
    pub fn conditions(&self) -> NetConditions {
        self.conditions
    }

    /// Sends `item` at time `now`.
    pub fn send(&mut self, now: Duration, item: T, delivery: Delivery) {
        if delivery == Delivery::Unreliable && self.rng.unit() < self.conditions.loss {
            return;
        }
        let one_way = self.conditions.latency / 2;
        let jitter = self.conditions.jitter.as_secs_f64() * f64::from(self.rng.unit() * 2.0 - 1.0);
        let mut at = (now + one_way).as_secs_f64() + jitter;
        at = at.max(now.as_secs_f64());
        let mut at = Duration::from_secs_f64(at);
        if delivery == Delivery::Reliable {
            at = at.max(self.last_reliable);
            self.last_reliable = at;
        }
        self.queue.push_back((at, item));
    }

    /// Takes every item due by `now`, earliest first.
    pub fn receive(&mut self, now: Duration) -> Vec<T> {
        let mut due: Vec<(Duration, usize, T)> = Vec::new();
        let mut kept = VecDeque::with_capacity(self.queue.len());
        for (order, (at, item)) in self.queue.drain(..).enumerate() {
            if at <= now {
                due.push((at, order, item));
            } else {
                kept.push_back((at, item));
            }
        }
        self.queue = kept;
        // Ties keep the order of sending.
        due.sort_by_key(|&(at, order, _)| (at, order));
        due.into_iter().map(|(_, _, item)| item).collect()
    }

    /// Items still on their way.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.queue.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn parses_latency_jitter_and_loss() {
        let conditions: NetConditions = "100ms,10ms,2%".parse().expect("valid");
        assert_eq!(conditions.latency, ms(100));
        assert_eq!(conditions.jitter, ms(10));
        assert!((conditions.loss - 0.02).abs() < 1e-6);
        assert_eq!(conditions.to_string(), "100ms,10ms,2%");
        let latency_only: NetConditions = "50ms".parse().expect("valid");
        assert_eq!(latency_only.jitter, Duration::ZERO);
        assert_eq!(latency_only.loss, 0.0);
        assert!(NetConditions::NONE.is_none());
        assert!(!latency_only.is_none());
    }

    #[test]
    fn rejects_malformed_conditions() {
        for text in ["", "100", "100ms,5", "1ms,1ms,101%", "1ms,1ms,1%,1", "fast"] {
            assert!(text.parse::<NetConditions>().is_err(), "{text}");
        }
    }

    #[test]
    fn items_arrive_after_half_the_round_trip() {
        let mut line = DelayLine::new("100ms".parse().expect("valid"), 1);
        line.send(ms(0), 'a', Delivery::Unreliable);
        assert!(line.receive(ms(49)).is_empty());
        assert_eq!(line.receive(ms(50)), ['a']);
        assert_eq!(line.in_flight(), 0);
    }

    #[test]
    fn a_clear_line_delivers_at_once_and_in_order() {
        let mut line = DelayLine::new(NetConditions::NONE, 1);
        for item in 0..5 {
            line.send(ms(10), item, Delivery::Unreliable);
        }
        assert_eq!(line.receive(ms(10)), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn loss_drops_about_its_share_of_unreliable_items_only() {
        let conditions: NetConditions = "0ms,0ms,25%".parse().expect("valid");
        let mut line = DelayLine::new(conditions, 7);
        for item in 0..4000 {
            line.send(ms(0), item, Delivery::Unreliable);
        }
        let arrived = line.receive(ms(0)).len();
        assert!((2800..3200).contains(&arrived), "{arrived}");
        for item in 0..100 {
            line.send(ms(0), item, Delivery::Reliable);
        }
        assert_eq!(line.receive(ms(0)).len(), 100);
    }

    #[test]
    fn jitter_reorders_unreliable_items_but_never_reliable_ones() {
        let conditions: NetConditions = "100ms,40ms".parse().expect("valid");
        let mut line = DelayLine::new(conditions, 3);
        for item in 0..200u64 {
            line.send(ms(item), item, Delivery::Reliable);
        }
        let reliable = line.receive(ms(1000));
        assert!(reliable.windows(2).all(|pair| pair[0] < pair[1]));
        for item in 0..200u64 {
            line.send(ms(item), item, Delivery::Unreliable);
        }
        let unreliable = line.receive(ms(1000));
        assert_eq!(unreliable.len(), 200);
        assert!(unreliable.windows(2).any(|pair| pair[0] > pair[1]));
    }
}
