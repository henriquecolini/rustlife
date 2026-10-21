//! The animated background of the main menu: a deep gradient with a few
//! large translucent bars drifting slowly across it.

use bevy::prelude::*;

use crate::ui::theme;

/// A bar that sways around its place.
#[derive(Component, Debug, Clone, Copy)]
pub(super) struct Drift {
    /// How far it sways along each axis, logical pixels.
    amplitude: Vec2,
    /// Radians per second.
    speed: f32,
    /// Where in the sway it starts, radians.
    phase: f32,
    /// How it is turned, degrees.
    angle: f32,
}

/// Where and how one bar sits.
struct Bar {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    alpha: f32,
    sway: Vec2,
    speed: f32,
    phase: f32,
}

/// How the bars are turned, degrees.
const BAR_ANGLE: f32 = -24.0;

const fn bar(
    (left, top): (f32, f32),
    (width, height): (f32, f32),
    alpha: f32,
    (sway_x, sway_y): (f32, f32),
    speed: f32,
    phase: f32,
) -> Bar {
    Bar {
        left,
        top,
        width,
        height,
        alpha,
        sway: Vec2::new(sway_x, sway_y),
        speed,
        phase,
    }
}

const BARS: [Bar; 5] = [
    bar(
        (52.0, -20.0),
        (360.0, 1500.0),
        0.055,
        (40.0, 24.0),
        0.11,
        0.0,
    ),
    bar(
        (66.0, -30.0),
        (120.0, 1500.0),
        0.10,
        (60.0, 30.0),
        0.17,
        1.7,
    ),
    bar(
        (78.0, -25.0),
        (260.0, 1500.0),
        0.045,
        (30.0, 40.0),
        0.09,
        3.1,
    ),
    bar((30.0, 40.0), (90.0, 1300.0), 0.05, (50.0, 20.0), 0.14, 4.4),
    bar((90.0, -10.0), (60.0, 1400.0), 0.12, (24.0, 36.0), 0.21, 5.9),
];

/// Spawns the backdrop's nodes as the first children of the menu layer.
pub(super) fn spawn(layer: &mut ChildSpawnerCommands) {
    layer.spawn((
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundGradient::from(LinearGradient::new(
            LinearGradient::TO_BOTTOM_RIGHT,
            vec![
                ColorStop::new(Color::srgb(0.035, 0.043, 0.063), Val::Percent(0.0)),
                ColorStop::new(Color::srgb(0.063, 0.082, 0.125), Val::Percent(70.0)),
                ColorStop::new(Color::srgb(0.090, 0.110, 0.165), Val::Percent(100.0)),
            ],
        )),
        Pickable::IGNORE,
    ));
    for bar in BARS {
        layer.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(bar.left),
                top: Val::Percent(bar.top),
                width: Val::Px(bar.width),
                height: Val::Px(bar.height),
                ..default()
            },
            BackgroundGradient::from(LinearGradient::new(
                LinearGradient::TO_BOTTOM,
                vec![
                    ColorStop::new(theme::ACCENT.with_alpha(0.0), Val::Percent(0.0)),
                    ColorStop::new(theme::ACCENT.with_alpha(bar.alpha), Val::Percent(45.0)),
                    ColorStop::new(theme::ACCENT.with_alpha(0.0), Val::Percent(100.0)),
                ],
            )),
            UiTransform {
                rotation: Rot2::degrees(BAR_ANGLE),
                ..UiTransform::IDENTITY
            },
            Drift {
                amplitude: bar.sway,
                speed: bar.speed,
                phase: bar.phase,
                angle: BAR_ANGLE,
            },
            Pickable::IGNORE,
        ));
    }
    // A darker band along the bottom keeps the footer readable.
    layer.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Px(180.0),
            ..default()
        },
        BackgroundGradient::from(LinearGradient::new(
            LinearGradient::TO_TOP,
            vec![
                ColorStop::new(Color::srgba(0.0, 0.0, 0.0, 0.55), Val::Percent(0.0)),
                ColorStop::new(Color::srgba(0.0, 0.0, 0.0, 0.0), Val::Percent(100.0)),
            ],
        )),
        Pickable::IGNORE,
    ));
}

/// Sways the bars.
pub(super) fn drift(time: Res<Time<Real>>, mut bars: Query<(&Drift, &mut UiTransform)>) {
    let t = time.elapsed_secs();
    for (drift, mut transform) in &mut bars {
        let wave = (t * drift.speed + drift.phase).sin();
        let cross = (t * drift.speed * 0.7 + drift.phase).cos();
        transform.translation = Val2::px(drift.amplitude.x * wave, drift.amplitude.y * cross);
        transform.rotation = Rot2::degrees(drift.angle + 1.5 * wave);
    }
}
