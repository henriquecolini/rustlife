//! Screen fades: a sheet of colour over the whole view that fades away, as
//! the engine's `UTIL_ScreenFade` with `FFADE_IN` makes.
//!
//! The sheet starts at its colour and `alpha` and fades out in a straight
//! line over `seconds`. The engine blends it in linear light, which is how
//! Bevy blends a UI node's colour too, so the same sRGB colour and alpha
//! give the same look: a fade of alpha 32/255 over a dark wall reads as
//! about a third as white on screen, not an eighth.

use bevy::prelude::*;

use crate::AppState;

/// Asks for a fade of the whole screen.
#[derive(Message, Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScreenFade {
    /// The colour, sRGB, `0..=1` each.
    pub color: [f32; 3],
    /// How opaque it starts, `0..=1`.
    pub alpha: f32,
    /// Seconds it takes to fade out.
    pub seconds: f32,
}

/// How opaque `fade` is `elapsed` seconds in: a straight line down to
/// nothing.
pub(crate) fn alpha_at(fade: &ScreenFade, elapsed: f32) -> f32 {
    if fade.seconds <= 0.0 {
        return 0.0;
    }
    (fade.alpha * (1.0 - elapsed / fade.seconds)).clamp(0.0, 1.0)
}

/// Draws screen fades.
pub(crate) struct ScreenFadePlugin;

impl Plugin for ScreenFadePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ScreenFade>()
            .init_resource::<Fading>()
            .add_systems(Startup, make_overlay)
            .add_systems(Update, show_fade.run_if(in_state(AppState::InGame)))
            .add_systems(OnExit(AppState::InGame), clear);
    }
}

/// The fade in progress, if there is one, and when it began.
#[derive(Resource, Default)]
struct Fading(Option<(ScreenFade, f32)>);

/// The sheet drawn over the view.
#[derive(Component)]
struct FadeOverlay;

fn make_overlay(mut commands: Commands) {
    commands.spawn((
        FadeOverlay,
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::NONE),
        // Over the world and the HUD's panels, under menus and the flash of
        // a blast.
        GlobalZIndex(3),
        Pickable::IGNORE,
    ));
}

fn clear(mut fading: ResMut<Fading>) {
    fading.0 = None;
}

/// Starts a fade when one is asked for (a new one replaces the old) and
/// sets the sheet's colour for this frame.
fn show_fade(
    time: Res<Time>,
    mut asked: MessageReader<ScreenFade>,
    mut fading: ResMut<Fading>,
    mut overlay: Query<&mut BackgroundColor, With<FadeOverlay>>,
) {
    let now = time.elapsed_secs();
    for fade in asked.read() {
        fading.0 = Some((*fade, now));
    }
    let wanted = match fading.0 {
        Some((fade, began)) if now - began < fade.seconds => {
            let [r, g, b] = fade.color;
            Color::srgba(r, g, b, alpha_at(&fade, now - began))
        }
        _ => {
            fading.0 = None;
            Color::NONE
        }
    };
    if let Ok(mut color) = overlay.single_mut()
        && color.0 != wanted
    {
        color.0 = wanted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white() -> ScreenFade {
        ScreenFade {
            color: [245.0 / 255.0, 245.0 / 255.0, 1.0],
            alpha: 32.0 / 255.0,
            seconds: 0.1,
        }
    }

    #[test]
    fn a_fade_starts_at_its_alpha_and_runs_out_in_a_straight_line() {
        let fade = white();
        assert!((alpha_at(&fade, 0.0) - 32.0 / 255.0).abs() < 1e-6);
        assert!(
            (alpha_at(&fade, 0.05) - 16.0 / 255.0).abs() < 1e-6,
            "halfway"
        );
        assert_eq!(alpha_at(&fade, 0.1), 0.0);
        assert_eq!(alpha_at(&fade, 5.0), 0.0, "never negative");
    }

    #[test]
    fn a_fade_that_takes_no_time_is_never_seen() {
        let fade = ScreenFade {
            seconds: 0.0,
            ..white()
        };
        assert_eq!(alpha_at(&fade, 0.0), 0.0);
    }

    /// The native game's frames: over a dark wall (sRGB 17, 29, 20) the
    /// first frame of the Gravity Gun's launch read 98, 103, 102, and the
    /// third 77 to 81. Blending in linear light reproduces them.
    #[test]
    fn blended_in_linear_light_it_matches_what_the_game_showed() {
        let to_linear = |c: f32| c.powf(2.2);
        let to_srgb = |c: f32| c.powf(1.0 / 2.2);
        let fade = white();
        let shown = |wall: f32, elapsed: f32| {
            let a = alpha_at(&fade, elapsed);
            255.0 * to_srgb((1.0 - a) * to_linear(wall / 255.0) + a * to_linear(fade.color[0]))
        };
        let first = shown(16.9, 0.0);
        assert!((first - 97.8).abs() < 3.0, "{first}");
        let third = shown(16.9, 2.0 / 60.0);
        assert!((76.0..=82.0).contains(&third), "{third}");
    }
}
