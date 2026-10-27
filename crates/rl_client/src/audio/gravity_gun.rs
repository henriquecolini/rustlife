//! The Gravity Gun's own sounds, from the client's `C_WeaponPhysCannon`:
//!
//! - the claws' sounds, `OpenClaws` when they open on something to take (or
//!   to hold) and `CloseClaws` when they shut because there is nothing, played
//!   as the prongs move (`OpenElements`, `CloseElements`);
//! - the hold loop, `HoldSound` (`hold_loop.wav`), which hums while it
//!   holds or pulls a prop. It is a `CSoundPatch` whose volume and pitch the
//!   game moves in straight lines: it starts silent at half pitch and comes
//!   up to 0.8 of the script's volume and full pitch over half a second
//!   (`DoEffectHolding`); when the gun lets go it runs back down to silence at
//!   half pitch over a second (`DoEffectReady`, a launch); when its claws
//!   shut on top of that, it is faded out in a tenth of a second
//!   (`DoEffectNone`). Throwing something runs it down too, so it rings out
//!   over a second even as the claws shut.
//!
//! The sounds are the weapon's `sounds` (`open`, `close`, `hold`), so they are
//! data like its others.

use bevy::audio::{AudioSink, AudioSinkPlayback, SpatialAudioSink, Volume};
use bevy::prelude::*;

use super::{Emitter, Repeat, SoundCategory, SoundPlayer};
use crate::AppState;
use crate::gravity_gun::{GunLook, Look};
use crate::net::Game;
use crate::physcannon_impact::PhyscannonImpact;
use crate::player::LocalPlayer;
use crate::settings::Settings;

/// Where the hold loop starts and ends its pitch, percent.
const QUIET_PITCH: f32 = 50.0;
/// Full pitch, percent.
const FULL_PITCH: f32 = 100.0;
/// How loud the loop gets, of the script's volume.
const FULL_VOLUME: f32 = 0.8;
/// Seconds it takes to come up to full volume and pitch.
const RISE_SECS: f32 = 0.5;
/// Seconds it takes to run down when the gun lets go.
const RUN_DOWN_SECS: f32 = 1.0;
/// Seconds it takes to fade out when the claws shut.
const FADE_OUT_SECS: f32 = 0.1;

/// Where the gun is, as far as its sounds go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Phase {
    /// Not in hand.
    #[default]
    Away,
    /// In hand, claws shut.
    Shut,
    /// In hand, claws open on something it could take.
    Ready,
    /// In hand, holding or pulling.
    Working,
}

impl Phase {
    /// What the gun in hand (or not) is doing.
    fn of(look: &GunLook) -> Self {
        if look.view.is_none() {
            return Self::Away;
        }
        match look.look {
            Look::Shut => Self::Shut,
            Look::Ready => Self::Ready,
            Look::Working => Self::Working,
        }
    }
}

/// What a change of phase sounds like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cue {
    /// The claws open.
    Open,
    /// The claws shut.
    Close,
    /// The hold loop starts.
    HoldStarts,
    /// The gun lets go: the loop runs down.
    HoldRunsDown,
    /// The claws shut on a running loop: it fades out.
    HoldFades,
}

/// The cues for going from phase `from` to phase `to`.
pub(super) fn cues(from: Phase, to: Phase) -> Vec<Cue> {
    use Phase::{Away, Ready, Shut, Working};
    let mut cues = Vec::new();
    match (from, to) {
        (Shut, Ready | Working) => cues.push(Cue::Open),
        (Ready | Working, Shut) => cues.push(Cue::Close),
        _ => {}
    }
    match (from, to) {
        (Away | Shut | Ready, Working) => cues.push(Cue::HoldStarts),
        (Working, Ready) => cues.push(Cue::HoldRunsDown),
        (Working, Shut | Away) => cues.push(Cue::HoldFades),
        _ => {}
    }
    cues
}

/// A number moving in a straight line to a target, as a `CSoundPatch`'s
/// volume and pitch do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Ramp {
    from: f32,
    to: f32,
    start: f32,
    secs: f32,
}

impl Ramp {
    fn at(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start: 0.0,
            secs: 0.0,
        }
    }

    fn value(&self, now: f32) -> f32 {
        if self.secs <= 0.0 {
            return self.to;
        }
        let t = ((now - self.start) / self.secs).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * t
    }

    /// Heads for `to` from where it is, taking `secs`.
    fn go(&mut self, to: f32, secs: f32, now: f32) {
        *self = Self {
            from: self.value(now),
            to,
            start: now,
            secs,
        };
    }
}

/// The hold loop's volume (a share of the script's) and pitch (percent).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct HoldLoop {
    volume: Ramp,
    pitch: Ramp,
}

impl HoldLoop {
    /// A loop just started: silent at half pitch, coming up.
    pub(super) fn started(now: f32) -> Self {
        let mut hold = Self {
            volume: Ramp::at(0.0),
            pitch: Ramp::at(QUIET_PITCH),
        };
        hold.volume.go(FULL_VOLUME, RISE_SECS, now);
        hold.pitch.go(FULL_PITCH, RISE_SECS, now);
        hold
    }

    /// Runs down to silence at half pitch.
    pub(super) fn run_down(&mut self, now: f32) {
        self.volume.go(0.0, RUN_DOWN_SECS, now);
        self.pitch.go(QUIET_PITCH, RUN_DOWN_SECS, now);
    }

    /// Fades out, unless it is already running down.
    pub(super) fn fade_out(&mut self, now: f32) {
        if self.volume.to > 0.0 {
            self.volume.go(0.0, FADE_OUT_SECS, now);
        }
    }

    pub(super) fn volume(&self, now: f32) -> f32 {
        self.volume.value(now)
    }

    pub(super) fn speed(&self, now: f32) -> f32 {
        self.pitch.value(now) / 100.0
    }

    /// Whether it has gone quiet for good.
    pub(super) fn spent(&self, now: f32) -> bool {
        self.volume.to <= 0.0 && self.volume.value(now) <= 0.0
    }
}

/// Plays the Gravity Gun's sounds.
pub(super) struct GravityGunSoundsPlugin;

impl Plugin for GravityGunSoundsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Sounding>().add_systems(
            Update,
            gun_sounds
                .after(crate::viewmodel::animate_view_model)
                .run_if(in_state(AppState::InGame)),
        );
    }
}

/// What the gun sounded like last frame, and its hold loop if one is playing.
#[derive(Resource, Default)]
struct Sounding {
    phase: Phase,
    hold: Option<Live>,
}

/// A hold loop playing: its emitter, the script's volume, and its envelope.
struct Live {
    emitter: Entity,
    base: f32,
    hold: HoldLoop,
}

/// The sound the local player's weapon names for `purpose`.
fn weapon_sound(game: &Game, local: Option<&LocalPlayer>, purpose: &str) -> Option<String> {
    let sim = game.sim()?;
    let weapon = sim.player(local?.id)?.inventory.active()?;
    sim.arsenal().weapon(weapon)?.sounds.get(purpose).cloned()
}

fn gun_sounds(
    time: Res<Time>,
    look: Res<GunLook>,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    settings: Res<Settings>,
    mut launches: MessageReader<PhyscannonImpact>,
    mut sounding: ResMut<Sounding>,
    mut player: SoundPlayer,
    mut commands: Commands,
    mut emitters: Query<(
        &mut Emitter,
        Option<&mut SpatialAudioSink>,
        Option<&mut AudioSink>,
    )>,
) {
    let now = time.elapsed_secs();
    let phase = Phase::of(&look);
    let local = local.as_deref();
    if launches.read().any(|launch| launch.local)
        && let Some(live) = &mut sounding.hold
    {
        live.hold.run_down(now);
    }
    for cue in cues(sounding.phase, phase) {
        match cue {
            Cue::Open | Cue::Close => {
                let purpose = if cue == Cue::Open { "open" } else { "close" };
                if let Some(name) = weapon_sound(&game, local, purpose) {
                    player.play(&name, None, 1.0);
                }
            }
            Cue::HoldStarts => start_hold(&game, local, &mut player, &mut sounding, now),
            Cue::HoldRunsDown => {
                if let Some(live) = &mut sounding.hold {
                    live.hold.run_down(now);
                }
            }
            Cue::HoldFades => {
                if let Some(live) = &mut sounding.hold {
                    live.hold.fade_out(now);
                }
            }
        }
    }
    sounding.phase = phase;
    let Some(live) = &sounding.hold else {
        return;
    };
    if live.hold.spent(now) {
        commands.entity(live.emitter).try_despawn();
        sounding.hold = None;
        return;
    }
    if let Ok((mut emitter, spatial, plain)) = emitters.get_mut(live.emitter) {
        emitter.volume = (live.base * live.hold.volume(now)).clamp(0.0, 1.0);
        emitter.speed = live.hold.speed(now);
        let level = Volume::Linear(emitter.volume * SoundCategory::Effects.gain(&settings.audio));
        if let Some(mut sink) = spatial {
            sink.set_volume(level);
            sink.set_speed(emitter.speed);
        }
        if let Some(mut sink) = plain {
            sink.set_volume(level);
            sink.set_speed(emitter.speed);
        }
    }
}

/// Starts the loop, or brings a running-down one back up.
fn start_hold(
    game: &Game,
    local: Option<&LocalPlayer>,
    player: &mut SoundPlayer,
    sounding: &mut Sounding,
    now: f32,
) {
    if let Some(live) = &mut sounding.hold {
        let (volume, pitch) = (live.hold.volume(now), live.hold.pitch.value(now));
        live.hold.volume = Ramp::at(volume);
        live.hold.pitch = Ramp::at(pitch);
        live.hold.volume.go(FULL_VOLUME, RISE_SECS, now);
        live.hold.pitch.go(FULL_PITCH, RISE_SECS, now);
        return;
    }
    let Some(name) = weapon_sound(game, local, "hold") else {
        return;
    };
    let Some(chosen) = player.choose(&name) else {
        return;
    };
    if let Some(emitter) = player.spawn_chosen(&chosen, None, 0.0, None, Repeat::Loop) {
        sounding.hold = Some(Live {
            emitter,
            base: chosen.volume,
            hold: HoldLoop::started(now),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_claws_open_on_something_to_take_and_shut_when_there_is_nothing() {
        use Phase::{Away, Ready, Shut, Working};
        assert_eq!(cues(Shut, Ready), [Cue::Open]);
        assert_eq!(cues(Ready, Shut), [Cue::Close]);
        assert_eq!(cues(Shut, Working), [Cue::Open, Cue::HoldStarts]);
        assert_eq!(cues(Working, Shut), [Cue::Close, Cue::HoldFades]);
        assert!(cues(Ready, Ready).is_empty());
        assert!(cues(Away, Shut).is_empty(), "taking the gun out is quiet");
        assert!(cues(Ready, Away).is_empty(), "so is putting it away");
    }

    #[test]
    fn the_loop_starts_when_it_takes_hold_and_runs_down_when_it_lets_go() {
        use Phase::{Away, Ready, Working};
        assert_eq!(cues(Ready, Working), [Cue::HoldStarts]);
        assert_eq!(cues(Working, Ready), [Cue::HoldRunsDown]);
        assert_eq!(cues(Working, Working), []);
        assert_eq!(cues(Away, Working), [Cue::HoldStarts], "drawn mid-pull");
        assert_eq!(cues(Working, Away), [Cue::HoldFades]);
    }

    #[test]
    fn the_loop_comes_up_to_pitch_and_volume_in_half_a_second() {
        let hold = HoldLoop::started(10.0);
        assert_eq!((hold.volume(10.0), hold.speed(10.0)), (0.0, 0.5));
        assert!((hold.volume(10.25) - 0.4).abs() < 1e-6);
        assert!((hold.speed(10.25) - 0.75).abs() < 1e-6);
        assert!((hold.volume(10.5) - 0.8).abs() < 1e-6);
        assert!((hold.speed(10.5) - 1.0).abs() < 1e-6);
        assert!((hold.volume(60.0) - 0.8).abs() < 1e-6, "and holds there");
        assert!(!hold.spent(60.0));
    }

    #[test]
    fn letting_go_runs_it_down_over_a_second_from_wherever_it_got_to() {
        let mut hold = HoldLoop::started(0.0);
        hold.run_down(0.25);
        assert!((hold.volume(0.25) - 0.4).abs() < 1e-6, "no jump");
        assert!((hold.volume(0.75) - 0.2).abs() < 1e-6);
        assert!((hold.speed(1.25) - 0.5).abs() < 1e-6, "back to half pitch");
        assert_eq!(hold.volume(1.25), 0.0);
        assert!(!hold.spent(0.75));
        assert!(hold.spent(1.25));
    }

    #[test]
    fn shutting_the_claws_fades_a_running_loop_in_a_tenth_of_a_second() {
        let mut hold = HoldLoop::started(0.0);
        hold.fade_out(1.0);
        assert!((hold.volume(1.05) - 0.4).abs() < 1e-6);
        assert!(hold.spent(1.1));
    }

    #[test]
    fn a_loop_already_running_down_is_not_cut_short() {
        let mut hold = HoldLoop::started(0.0);
        hold.run_down(1.0);
        hold.fade_out(1.2);
        assert!(hold.volume(1.6) > 0.1, "{}", hold.volume(1.6));
        assert!(hold.spent(2.0));
    }
}
