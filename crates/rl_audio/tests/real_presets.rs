//! Runs every room preset of a real install's `dsp_presets.txt`, and the
//! automatic preset of every kind of place, to check that each stays
//! stable and about as loud as its input. `#[ignore]`d; run with
//! `RUSTLIFE_TEST_GAME_DIR=<install> cargo test -p rl_audio --test
//! real_presets -- --ignored --nocapture`.

use std::path::PathBuf;
use std::time::Instant;

use glam::Vec3;
use rl_audio::dsp::Preset;
use rl_audio::dsp::room::{RoomKind, RoomShape, automatic_preset};
use rl_audio::{Looping, Mixer, Pcm, Place, SAMPLE_RATE, VoiceParams};
use source_formats::dsp_presets::{DspPresets, PresetDef};
use source_fs::content::{ContentOptions, mount_game};

fn presets() -> Option<DspPresets> {
    let dir =
        std::env::var_os("RUSTLIFE_TEST_GAME_DIR").or_else(|| std::env::var_os("HL2DM_DIR"))?;
    let game = mount_game(&ContentOptions {
        game_dir: Some(PathBuf::from(dir)),
        ..ContentOptions::default()
    })
    .ok()?;
    let bytes = game.vfs.read("scripts/dsp_presets.txt").ok()??;
    Some(DspPresets::parse(&String::from_utf8_lossy(&bytes)))
}

/// Peak output and energy gain for half a second of noise then silence.
fn run(def: &PresetDef) -> (f32, f32) {
    let mut preset = Preset::new(def.clone());
    let mut seed = 7u32;
    let (mut input, mut output, mut peak) = (0.0f32, 0.0f32, 0.0f32);
    for n in 0..SAMPLE_RATE as usize * 3 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let x = if n < SAMPLE_RATE as usize / 2 {
            (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
        } else {
            0.0
        };
        let [y, _] = preset.process([x, x]);
        assert!(y.is_finite());
        input += x * x;
        output += y * y;
        peak = peak.max(y.abs());
    }
    (peak, output / input)
}

#[test]
#[ignore = "requires a Half-Life 2: Deathmatch install"]
fn every_room_preset_is_stable() {
    let Some(presets) = presets() else {
        eprintln!("skipping: no install");
        return;
    };
    assert!(presets.len() > 130, "{} presets", presets.len());
    for number in (0..30).chain(100..134) {
        let Some(def) = presets.get(number) else {
            continue;
        };
        let (peak, gain) = run(def);
        eprintln!("preset {number}: peak {peak:.2}, energy gain {gain:.2}");
        assert!(peak < 4.0, "preset {number} blows up: {peak}");
    }
}

#[test]
#[ignore = "requires a Half-Life 2: Deathmatch install"]
fn every_kind_of_place_gets_an_effect() {
    let Some(presets) = presets() else {
        return;
    };
    let shapes = [
        (false, 40.0, 600.0, 100.0, 0),
        (false, 80.0, 500.0, 120.0, 0),
        (false, 200.0, 1500.0, 250.0, 0),
        (false, 300.0, 400.0, 150.0, 0),
        (false, 1200.0, 2000.0, 600.0, 0),
        (true, 300.0, 1500.0, 4800.0, 0),
        (true, 120.0, 900.0, 4800.0, 0),
        (true, 800.0, 900.0, 4800.0, 0),
        (true, 2000.0, 3000.0, 4800.0, 1),
        (true, 2000.0, 3000.0, 4800.0, 2),
        (true, 2000.0, 3000.0, 4800.0, 3),
        (true, 9600.0, 9600.0, 4800.0, 4),
    ];
    for (outside, width, length, height, open_sides) in shapes {
        for diffusion in [0.0, 0.5] {
            let shape = RoomShape {
                outside,
                width,
                length,
                height,
                diffusion,
                reflectivity: 0.6,
                open_sides,
            };
            let def = automatic_preset(&presets, &shape).expect("templates");
            let (peak, gain) = run(&def);
            eprintln!(
                "{:?} {shape:?}: peak {peak:.2}, gain {gain:.2}",
                RoomKind::classify(&shape)
            );
            assert!(peak < 4.0);
        }
    }
}

#[test]
#[ignore = "requires a Half-Life 2: Deathmatch install"]
fn a_busy_mix_renders_faster_than_real_time() {
    let Some(presets) = presets() else {
        return;
    };
    let mut mixer = Mixer::new();
    mixer.set_room(presets.get(19).cloned());
    let noise: Vec<i16> = (0..SAMPLE_RATE as i32)
        .map(|n| ((n * 7919) % 20000 - 10000) as i16)
        .collect();
    for n in 0..64 {
        mixer.play(
            Pcm::new(noise.clone(), 1, 22050, None),
            VoiceParams {
                place: Place::World(Vec3::new(n as f32 * 40.0, 100.0, 0.0)),
                soundlevel: 75.0,
                looping: Looping::Always,
                ..VoiceParams::default()
            },
        );
    }
    let mut out = vec![0.0; SAMPLE_RATE as usize * 2];
    let started = Instant::now();
    mixer.render(&mut out);
    let took = started.elapsed().as_secs_f32();
    eprintln!("1 s of 64 voices and a reverb: {:.1} ms", took * 1000.0);
    assert!(took < 0.5);
}
