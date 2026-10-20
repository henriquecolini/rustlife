//! The client's idea of the server's clock.
//!
//! A client draws other players, props and projectiles a little in the past
//! so that it always has two snapshots to move between. [`ServerClock`]
//! estimates the newest server tick the client could know of right now —
//! it runs on at the tick rate between snapshots and is pulled gently
//! towards each snapshot's tick as it arrives, so the jitter of arrivals
//! does not shake it — and [`ServerClock::render_tick`] is that estimate
//! less the interpolation delay. The render tick never runs backwards.

use std::time::Duration;

/// Corrections bigger than this many seconds are taken at once (on joining,
/// after a stall); smaller ones are eased in.
const SNAP_SECS: f64 = 0.25;
/// Share of the difference to a snapshot's tick corrected per snapshot.
const EASE: f64 = 0.1;

/// See the [module docs](self).
#[derive(Debug, Clone)]
pub struct ServerClock {
    /// Ticks per second.
    rate: f64,
    /// Ticks the drawing lags behind the estimate.
    delay: f64,
    /// The estimated newest server tick, once a snapshot arrived.
    estimate: Option<f64>,
    /// The last render tick handed out.
    rendered: f64,
}

impl ServerClock {
    /// A clock for a server ticking `tick_rate` times a second, drawn
    /// `interpolation_delay` seconds behind.
    #[must_use]
    pub fn new(tick_rate: u32, interpolation_delay: f32) -> Self {
        let rate = f64::from(tick_rate.max(1));
        Self {
            rate,
            delay: f64::from(interpolation_delay) * rate,
            estimate: None,
            rendered: 0.0,
        }
    }

    /// Runs the clock on by `elapsed`.
    pub fn advance(&mut self, elapsed: Duration) {
        if let Some(estimate) = &mut self.estimate {
            *estimate += elapsed.as_secs_f64() * self.rate;
        }
    }

    /// A snapshot of `tick` arrived.
    pub fn observe(&mut self, tick: u64) {
        let tick = tick as f64;
        self.estimate = Some(match self.estimate {
            Some(estimate) if (tick - estimate).abs() <= SNAP_SECS * self.rate => {
                estimate + (tick - estimate) * EASE
            }
            _ => tick,
        });
    }

    /// The estimated newest server tick, once a snapshot arrived.
    #[must_use]
    pub fn now(&self) -> Option<f64> {
        self.estimate
    }

    /// The tick to draw the world at: the estimate less the interpolation
    /// delay, never earlier than the last one handed out.
    pub fn render_tick(&mut self) -> Option<f64> {
        let tick = (self.estimate? - self.delay).max(self.rendered);
        self.rendered = tick;
        Some(tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_micros(15_152);

    #[test]
    fn nothing_is_known_before_the_first_snapshot() {
        let mut clock = ServerClock::new(66, 0.1);
        clock.advance(Duration::from_secs(1));
        assert_eq!(clock.now(), None);
        assert_eq!(clock.render_tick(), None);
    }

    #[test]
    fn the_first_snapshot_sets_the_clock_and_it_runs_on() {
        let mut clock = ServerClock::new(66, 0.1);
        clock.observe(1000);
        assert_eq!(clock.now(), Some(1000.0));
        clock.advance(Duration::from_secs(1));
        let now = clock.now().expect("set");
        assert!((now - 1066.0).abs() < 1e-6, "{now}");
        let render = clock.render_tick().expect("set");
        assert!((render - (1066.0 - 6.6)).abs() < 1e-6, "{render}");
    }

    #[test]
    fn jittery_arrivals_are_smoothed_and_big_jumps_taken_at_once() {
        let mut clock = ServerClock::new(66, 0.1);
        clock.observe(100);
        // A snapshot three ticks early moves the estimate only a little.
        clock.observe(103);
        let now = clock.now().expect("set");
        assert!((100.0..101.0).contains(&now), "{now}");
        // One a minute ahead resets it.
        clock.observe(100 + 66 * 60);
        assert_eq!(clock.now(), Some(f64::from(100 + 66 * 60)));
    }

    #[test]
    fn the_render_tick_never_runs_backwards() {
        let mut clock = ServerClock::new(66, 0.1);
        clock.observe(500);
        let mut last = clock.render_tick().expect("set");
        for step in 0..500u64 {
            clock.advance(TICK);
            // Arrivals that are late, then early.
            let tick = 500 + step + if step % 7 == 0 { 0 } else { 3 };
            clock.observe(tick);
            let render = clock.render_tick().expect("set");
            assert!(render >= last, "{render} < {last}");
            last = render;
        }
    }
}
