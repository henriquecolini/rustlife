//! Choosing the soundscape in charge and crossfading to it.

use super::*;

/// A soundscape entity at `x` along the x axis.
fn spot(entity: usize, x: f32, radius: f32) -> Spot<'static> {
    Spot {
        entity,
        origin: Vec3::new(x, 0.0, 0.0),
        radius,
        enabled: true,
        soundscape: "scape",
        positions: [None; POSITIONS],
    }
}

fn everything_seen(_: Vec3) -> bool {
    true
}

fn looping(wave: &str, volume: f32) -> StartedLoop {
    StartedLoop {
        wave: wave.to_owned(),
        volume,
        pitch: 100.0,
        soundlevel: 75.0,
        position: None,
    }
}

/// Fades `mix` for `seconds` in small steps, returning the sounds stopped.
fn fade_for(mix: &mut Mix, seconds: f32) -> usize {
    let steps = (seconds / 0.01).round() as usize;
    (0..steps).map(|_| mix.fade(0.01).len()).sum()
}

fn volume_of(mix: &Mix, wave: &str) -> Option<(f32, f32)> {
    mix.loops
        .iter()
        .find(|sound| sound.wave == wave)
        .map(|sound| (sound.current, sound.target))
}

#[test]
fn proxies_borrow_the_soundscape_they_name() {
    let entities = [
        MapEntity::new("env_soundscape", [1.0, 0.0, 0.0])
            .with("targetname", "Out")
            .with("soundscape", "strike_outside")
            .with("radius", "300")
            .with("position0", "speaker"),
        MapEntity::new("env_soundscape_proxy", [9.0, 0.0, 0.0])
            .with("MainSoundscapeName", "out")
            .with("StartDisabled", "1"),
        MapEntity::new("env_soundscape_proxy", [9.0, 0.0, 0.0])
            .with("MainSoundscapeName", "nobody"),
        MapEntity::new("info_target", [5.0, 6.0, 7.0]).with("targetname", "Speaker"),
    ];
    let spots = spots(&entities);
    assert_eq!(spots.len(), 2);
    assert_eq!(
        (spots[0].soundscape, spots[0].radius, spots[0].enabled),
        ("strike_outside", 300.0, true)
    );
    assert_eq!(spots[0].positions[0], Some(Vec3::new(5.0, 6.0, 7.0)));
    let proxy = &spots[1];
    assert_eq!(
        (proxy.soundscape, proxy.radius, proxy.origin.x, proxy.entity),
        ("strike_outside", 128.0, 9.0, 1)
    );
    assert!(!proxy.enabled, "its own StartDisabled");
    assert_eq!(
        proxy.positions, spots[0].positions,
        "and the main's positions"
    );
}

#[test]
fn a_position_whose_target_is_missing_is_not_set() {
    let entities = [
        MapEntity::new("env_soundscape", [0.0; 3])
            .with("soundscape", "a")
            .with("position1", "here")
            .with("position3", "gone"),
        MapEntity::new("info_target", [1.0, 2.0, 3.0]).with("targetname", "here"),
    ];
    let spots = spots(&entities);
    assert_eq!(spots[0].position_bits(), 0b10, "only position1");
}

#[test]
fn the_nearest_soundscape_in_reach_and_sight_is_in_charge() {
    let spots = [
        spot(0, 0.0, 500.0),
        spot(1, 300.0, 500.0),
        spot(2, 50.0, 10.0),
    ];
    let listener = Vec3::new(200.0, 0.0, 0.0);
    // The nearer one takes over from the one in charge...
    assert_eq!(audible(&spots, listener, Some(0), everything_seen), Some(1));
    // ...unless it cannot see the listener.
    let unseen = |from: Vec3| from.x < 299.0;
    assert_eq!(audible(&spots, listener, Some(0), unseen), Some(0));
    // A disabled one never plays.
    let mut disabled = spots.clone();
    disabled[1].enabled = false;
    assert_eq!(audible(&disabled, listener, None, everything_seen), Some(0));
    // Out of every radius: none, and the last one keeps playing.
    let far = Vec3::new(5000.0, 0.0, 0.0);
    assert_eq!(audible(&spots, far, Some(1), everything_seen), None);
}

#[test]
fn the_one_in_charge_keeps_a_tie() {
    let spots = [spot(0, -100.0, 500.0), spot(1, 100.0, 500.0)];
    assert_eq!(
        audible(&spots, Vec3::ZERO, Some(1), everything_seen),
        Some(1)
    );
    assert_eq!(
        audible(&spots, Vec3::ZERO, Some(0), everything_seen),
        Some(0)
    );
}

#[test]
fn a_radius_of_minus_one_reaches_everywhere() {
    let spots = [spot(0, 0.0, EVERYWHERE), spot(1, 0.0, 0.0)];
    let far = Vec3::new(1.0e5, 0.0, 0.0);
    assert_eq!(audible(&spots, far, None, everything_seen), Some(0));
    assert_eq!(
        audible(&spots[1..], Vec3::ZERO, None, everything_seen),
        None,
        "a radius of 0 reaches nowhere"
    );
}

#[test]
fn a_new_soundscape_crossfades_and_shared_loops_glide() {
    let mut mix = Mix::default();
    mix.begin();
    assert_eq!(mix.add_loop(&looping("old.wav", 0.6), None), Some(0));
    assert_eq!(mix.add_loop(&looping("shared.wav", 0.9), None), Some(1));
    fade_for(&mut mix, 1.5);
    let (current, _) = volume_of(&mix, "old.wav").expect("old");
    assert!((current - 0.5).abs() < 1e-3, "a third of full per second");
    fade_for(&mut mix, 2.0);
    assert_eq!(volume_of(&mix, "old.wav"), Some((0.6, 0.6)));

    // Another soundscape takes over.
    mix.random.push(Pending {
        sound: StartedRandom {
            sound: soundscape::RandomSound {
                waves: vec!["r.wav".to_owned()],
                time: (1.0, 1.0),
                volume: (1.0, 1.0),
                pitch: (100.0, 100.0),
                soundlevel: (0.0, 0.0),
                position: None,
            },
            master: 1.0,
            place: RandomPlace::Ambient,
        },
        place: None,
        due: 0.0,
    });
    mix.begin();
    assert!(mix.random.is_empty(), "random sounds to come are dropped");
    assert_eq!(
        mix.add_loop(&looping("shared.wav", 0.3), None),
        None,
        "the shared loop carries on"
    );
    assert_eq!(mix.add_loop(&looping("new.wav", 0.6), None), Some(2));
    assert_eq!(volume_of(&mix, "old.wav"), Some((0.6, 0.0)));
    assert_eq!(volume_of(&mix, "shared.wav"), Some((0.9, 0.3)));
    assert_eq!(volume_of(&mix, "new.wav"), Some((0.0, 0.6)));
    fade_for(&mut mix, 0.9);
    let (old, _) = volume_of(&mix, "old.wav").expect("old");
    let (new, _) = volume_of(&mix, "new.wav").expect("new");
    assert!(
        (old - 0.3).abs() < 1e-3 && (new - 0.3).abs() < 1e-3,
        "{old} {new}"
    );
    let stopped = fade_for(&mut mix, 1.0);
    assert_eq!(volume_of(&mix, "old.wav"), None, "silent loops are dropped");
    assert_eq!(stopped, 0, "it had no sound to stop in this test");
    assert_eq!(volume_of(&mix, "shared.wav"), Some((0.3, 0.3)));
}

#[test]
fn the_same_soundscape_twice_plays_its_doubled_loops_and_positional_ones_move() {
    let mut mix = Mix::default();
    mix.begin();
    // Two of the same loop in one soundscape are two sounds.
    assert_eq!(mix.add_loop(&looping("hum.wav", 0.5), None), Some(0));
    assert_eq!(mix.add_loop(&looping("hum.wav", 0.5), None), Some(1));
    let here = Some(Vec3::ZERO);
    assert_eq!(mix.add_loop(&looping("buzz.wav", 0.5), here), Some(2));
    assert_eq!(mix.loops[2].current, POSITIONAL_START);
    fade_for(&mut mix, 0.3);
    mix.begin();
    // At the same place it carries on; elsewhere it restarts there at once,
    // at the volume it had.
    let had = mix.loops[2].current;
    assert_eq!(
        mix.add_loop(&looping("buzz.wav", 0.5), Some(Vec3::splat(0.05))),
        None
    );
    mix.begin();
    let moved = mix.add_loop(&looping("buzz.wav", 0.5), Some(Vec3::X * 100.0));
    assert_eq!(moved, Some(2));
    assert_eq!(mix.loops[2].current, had);
    assert_eq!(mix.loops[2].place, Some(Vec3::X * 100.0));
    // An ambient loop is not a positional one.
    assert_eq!(mix.add_loop(&looping("buzz.wav", 0.5), None), Some(3));
}
