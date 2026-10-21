//! Small animations for the menus.
//!
//! * [`Smooth`] eases a node's scale and background colour towards targets
//!   that interaction code sets (hover, press, selection); frame-rate
//!   independent, so it feels the same at any speed.
//! * [`Reveal`] slides a node in from an offset after a delay, ending at
//!   rest; staggering delays down a list makes a screen build itself.
//! * [`Curtain`] is a full-screen overlay that fades out to uncover a new
//!   screen, then removes itself.

use bevy::prelude::*;

/// How quickly [`Smooth`] values approach their targets, per second (the
/// fraction of the remaining distance covered is `1 - e^(-rate * dt)`).
const SMOOTH_RATE: f32 = 16.0;

/// Moves `current` towards `target`, covering `1 - e^(-rate * dt)` of the
/// remaining distance: exponential smoothing that is independent of the
/// frame rate.
#[must_use]
pub fn smooth_toward(current: f32, target: f32, rate: f32, dt: f32) -> f32 {
    let blend = 1.0 - (-rate * dt).exp();
    current + (target - current) * blend
}

/// Decelerating curve: fast at first, easing to rest. `t` is clamped to
/// `0..=1`.
#[must_use]
pub fn ease_out_cubic(t: f32) -> f32 {
    let remaining = 1.0 - t.clamp(0.0, 1.0);
    1.0 - remaining * remaining * remaining
}

/// A node whose scale and background colour follow targets.
#[derive(Component, Debug, Clone, Copy)]
pub struct Smooth {
    /// The scale to approach.
    pub scale: f32,
    /// The background colour to approach (linear RGBA).
    pub color: LinearRgba,
}

impl Smooth {
    /// Starts at rest at `color`, scale 1.
    #[must_use]
    pub fn new(color: Color) -> Self {
        Self {
            scale: 1.0,
            color: color.into(),
        }
    }

    /// Sets the colour to approach.
    pub fn set_color(&mut self, color: Color) {
        self.color = color.into();
    }
}

/// Slides a node in from `offset` pixels away, after `delay` seconds,
/// taking `duration` seconds. Until its delay is over the node sits at the
/// offset.
#[derive(Component, Debug, Clone, Copy)]
pub struct Reveal {
    /// Where the node starts, relative to its place, in logical pixels.
    pub offset: Vec2,
    /// Seconds before the slide begins.
    pub delay: f32,
    /// Seconds the slide takes.
    pub duration: f32,
    /// Seconds since the component was added.
    pub elapsed: f32,
}

impl Reveal {
    /// A slide-in from `offset`, starting after `delay`.
    #[must_use]
    pub fn new(offset: Vec2, delay: f32) -> Self {
        Self {
            offset,
            delay,
            duration: 0.38,
            elapsed: 0.0,
        }
    }

    /// How far the node is from its place, as a fraction of the offset.
    #[must_use]
    pub fn remaining(&self) -> f32 {
        1.0 - ease_out_cubic((self.elapsed - self.delay) / self.duration)
    }

    /// Whether the slide is over.
    #[must_use]
    pub fn done(&self) -> bool {
        self.elapsed >= self.delay + self.duration
    }
}

/// A full-screen overlay that starts opaque and fades away.
#[derive(Component, Debug, Clone, Copy)]
pub struct Curtain {
    /// Seconds the fade takes.
    pub duration: f32,
    /// Seconds since it appeared.
    pub elapsed: f32,
    /// Its colour when opaque.
    pub color: Color,
}

impl Curtain {
    /// A curtain of `color` drawn over a new screen.
    #[must_use]
    pub fn new(color: Color) -> Self {
        Self {
            duration: 0.28,
            elapsed: 0.0,
            color,
        }
    }

    /// The bundle to spawn: covers the screen above everything.
    #[must_use]
    pub fn bundle(self) -> impl Bundle {
        (
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(self.color),
            GlobalZIndex(1000),
            Pickable::IGNORE,
            self,
        )
    }
}

/// Adds the animation systems.
pub struct AnimPlugin;

impl Plugin for AnimPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (smooth, reveal, fade_curtains));
    }
}

fn smooth(
    time: Res<Time<Real>>,
    mut nodes: Query<(&Smooth, &mut BackgroundColor, Option<&mut UiTransform>)>,
) {
    let dt = time.delta_secs();
    for (target, mut background, transform) in &mut nodes {
        let current: LinearRgba = background.0.into();
        let blend = |from: f32, to: f32| smooth_toward(from, to, SMOOTH_RATE, dt);
        let next = LinearRgba::new(
            blend(current.red, target.color.red),
            blend(current.green, target.color.green),
            blend(current.blue, target.color.blue),
            blend(current.alpha, target.color.alpha),
        );
        if next != current {
            background.0 = next.into();
        }
        if let Some(mut transform) = transform {
            let scale = blend(transform.scale.x, target.scale);
            if (scale - transform.scale.x).abs() > f32::EPSILON {
                transform.scale = Vec2::splat(scale);
            }
        }
    }
}

fn reveal(time: Res<Time<Real>>, mut nodes: Query<(&mut Reveal, &mut UiTransform)>) {
    for (mut reveal, mut transform) in &mut nodes {
        if reveal.done() {
            continue;
        }
        reveal.elapsed += time.delta_secs();
        let offset = reveal.offset * reveal.remaining().clamp(0.0, 1.0);
        transform.translation = Val2::px(offset.x, offset.y);
    }
}

fn fade_curtains(
    time: Res<Time<Real>>,
    mut commands: Commands,
    mut curtains: Query<(Entity, &mut Curtain, &mut BackgroundColor)>,
) {
    for (entity, mut curtain, mut background) in &mut curtains {
        curtain.elapsed += time.delta_secs();
        let fraction = curtain.elapsed / curtain.duration;
        if fraction >= 1.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let alpha = curtain.color.alpha() * (1.0 - ease_out_cubic(fraction));
        background.0 = curtain.color.with_alpha(alpha);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothing_converges_without_overshooting_at_any_frame_rate() {
        for dt in [1.0 / 240.0, 1.0 / 60.0, 1.0 / 10.0, 1.0] {
            let mut value = 0.0;
            let mut previous = 0.0;
            for _ in 0..((3.0 / dt) as usize) {
                value = smooth_toward(value, 1.0, SMOOTH_RATE, dt);
                assert!(value >= previous && value <= 1.0, "dt {dt}: {value}");
                previous = value;
            }
            assert!((value - 1.0).abs() < 0.001, "dt {dt}: {value}");
        }
    }

    #[test]
    fn smoothing_takes_the_same_time_whatever_the_frame_rate() {
        let after = |dt: f32| {
            let steps = (0.2 / dt).round() as usize;
            (0..steps).fold(0.0, |value, _| smooth_toward(value, 1.0, SMOOTH_RATE, dt))
        };
        assert!((after(1.0 / 120.0) - after(1.0 / 30.0)).abs() < 0.01);
    }

    #[test]
    fn easing_runs_from_zero_to_one_and_slows_down() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert_eq!(ease_out_cubic(-3.0), 0.0);
        assert_eq!(ease_out_cubic(7.0), 1.0);
        assert!(ease_out_cubic(0.5) > 0.5, "front-loaded");
        assert!(
            ease_out_cubic(0.9) - ease_out_cubic(0.8) < ease_out_cubic(0.2) - ease_out_cubic(0.1)
        );
    }

    #[test]
    fn a_reveal_waits_out_its_delay_then_settles() {
        let mut reveal = Reveal::new(Vec2::new(0.0, 40.0), 0.2);
        assert_eq!(reveal.remaining(), 1.0);
        reveal.elapsed = 0.1;
        assert_eq!(reveal.remaining(), 1.0, "still waiting");
        assert!(!reveal.done());
        reveal.elapsed = 0.2 + reveal.duration / 2.0;
        assert!((0.0..1.0).contains(&reveal.remaining()));
        reveal.elapsed = 0.2 + reveal.duration;
        assert!(reveal.done());
        assert_eq!(reveal.remaining(), 0.0);
    }
}
