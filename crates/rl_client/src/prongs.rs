//! The Gravity Gun's prongs: they open when the gun has something to take
//! (a prop it could pull or grab is under the crosshair), is pulling or is
//! holding one, and close again otherwise.
//!
//! The view model's `active` pose parameter, `0` shut to `1` open, drives
//! its `prong_blend` delta layer. As in HL2DM's `C_WeaponPhysCannon` it is an
//! interpolated value (`m_ElementParameter`) that moves to its target along
//! a spline: to open takes 0.2 s, to close 0.5 s.

use bevy::prelude::*;
use rl_data::Shot;
use rl_sim::tools::manipulator::{Aim, liftable_target};

use crate::AppState;
use crate::animation::{Animated, SequencePlayer};
use crate::net::Game;
use crate::player::{LocalPlayer, ViewAngles};
use crate::viewmodel::ViewModel;

/// Seconds the prongs take to open...
pub(crate) const OPEN_SECS: f32 = 0.2;
/// ...and to close.
pub(crate) const CLOSE_SECS: f32 = 0.5;

/// A value on its way to a target along a spline, as `InterpolatedValue`
/// moves HL2's: restarted from where it is whenever the target changes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Element {
    from: f32,
    to: f32,
    start: f32,
    duration: f32,
}

impl Element {
    /// Where it is at time `now`, seconds.
    pub(crate) fn value(&self, now: f32) -> f32 {
        if self.duration <= 0.0 {
            return self.to;
        }
        let t = ((now - self.start) / self.duration).clamp(0.0, 1.0);
        let eased = t * t * (3.0 - 2.0 * t);
        self.from + (self.to - self.from) * eased
    }

    /// Sends it to `target` over `duration` from where it is at `now`, if
    /// that is not already where it is headed.
    pub(crate) fn go_to(&mut self, target: f32, duration: f32, now: f32) {
        if (self.to - target).abs() < f32::EPSILON {
            return;
        }
        *self = Self {
            from: self.value(now),
            to: target,
            start: now,
            duration,
        };
    }
}

/// The prongs' value, kept with the Gravity Gun's view model.
#[derive(Component, Debug, Default)]
pub(crate) struct Prongs(pub(crate) Element);

/// Opens and closes the prongs of the Gravity Gun in hand.
pub(crate) fn drive_prongs(
    time: Res<Time>,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    angles: Res<ViewAngles>,
    mut views: Query<(&ViewModel, &Animated, &mut SequencePlayer, &mut Prongs)>,
) {
    let (Some(sim), Some(local)) = (game.sim(), local) else {
        return;
    };
    let now = time.elapsed_secs();
    for (view, animated, mut player, mut prongs) in &mut views {
        let manipulator = sim.arsenal().weapon(view.weapon()).is_some_and(|weapon| {
            weapon
                .fire_modes()
                .any(|fire| matches!(fire.shot, Shot::Manipulator))
        });
        let Some(layer) = animated.layer("prong_blend").filter(|_| manipulator) else {
            continue;
        };
        let open = sim.player(local.id).is_some_and(|player| {
            player.manipulator.held().is_some()
                || sim.eye_position(local.id).is_some_and(|eye| {
                    let aim = Aim {
                        eye,
                        pitch: angles.pitch,
                        yaw: angles.yaw,
                    };
                    liftable_target(&aim, sim.world(), sim.physics(), &sim.data().manipulator)
                        .is_some()
                })
        });
        if open {
            prongs.0.go_to(1.0, OPEN_SECS, now);
        } else {
            prongs.0.go_to(0.0, CLOSE_SECS, now);
        }
        player.set_layer(layer, prongs.0.value(now), 1.0);
    }
}

/// Runs [`drive_prongs`].
pub(crate) struct ProngsPlugin;

impl Plugin for ProngsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            drive_prongs
                .after(crate::viewmodel::animate_view_model)
                .run_if(in_state(AppState::InGame)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_opens_along_a_spline_in_two_tenths_of_a_second() {
        let mut element = Element::default();
        element.go_to(1.0, OPEN_SECS, 10.0);
        assert_eq!(element.value(10.0), 0.0);
        assert!(
            (element.value(10.1) - 0.5).abs() < 1e-5,
            "halfway at the middle"
        );
        assert!(element.value(10.05) < 0.25, "slow to start");
        assert_eq!(element.value(10.2), 1.0);
        assert_eq!(element.value(11.0), 1.0);
    }

    #[test]
    fn closing_starts_from_wherever_opening_got_to() {
        let mut element = Element::default();
        element.go_to(1.0, OPEN_SECS, 0.0);
        let partway = element.value(0.1);
        element.go_to(0.0, CLOSE_SECS, 0.1);
        assert!((element.value(0.1) - partway).abs() < 1e-5, "no jump");
        assert_eq!(element.value(0.6), 0.0);
    }

    #[test]
    fn the_same_target_does_not_restart_it() {
        let mut element = Element::default();
        element.go_to(1.0, OPEN_SECS, 0.0);
        let before = element.value(0.15);
        element.go_to(1.0, OPEN_SECS, 0.15);
        assert!((element.value(0.15) - before).abs() < 1e-6);
    }
}
