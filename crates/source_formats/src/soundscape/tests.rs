//! Soundscapes read and started as HL2DM's `C_SoundscapeSystem` does.

use super::*;

const FILE: &str = r#"
    "Prison.Control_Room"
    {
        "dsp" "1"
        "playlooping" { "volume" "0.4" "pitch" "95" "wave" "^ambient\tone.wav" }
        "playrandom"
        {
            "time" "15, 5"
            "volume" "0.2,0.4"
            "pitch" "100"
            "rndwave" { "wave" "a.wav" "wave" "b.wav" }
        }
        "playsoundscape" { "name" "Distant" "volume" "0.5" }
    }
    "distant"
    {
        "playlooping" { "volume" "0.8" "wave" "far.wav" }
        "playsoundscape" { "name" "prison.control_room" "volume" "1" }
    }
"#;

/// Always the middle of an interval.
fn middle() -> impl FnMut() -> f32 {
    || 0.5
}

fn scapes(text: &str) -> Soundscapes {
    let mut scapes = Soundscapes::default();
    scapes.add_file(text).expect("keyvalues");
    scapes
}

#[test]
fn soundscapes_read_their_sounds() {
    let mut scapes = Soundscapes::default();
    assert_eq!(scapes.add_file(FILE), Some(2));
    let room = scapes.get("prison.control_room").expect("room");
    assert_eq!(
        room.looping,
        [LoopingSound {
            wave: "ambient/tone.wav".to_owned(),
            volume: (0.4, 0.4),
            pitch: (95.0, 95.0),
            level: Level::Decibels((75.0, 75.0)),
            position: None,
        }]
    );
    assert_eq!(room.random[0].waves, ["a.wav", "b.wav"]);
    assert_eq!(
        (room.random[0].time, room.random[0].volume),
        ((15.0, 5.0), (0.2, 0.4))
    );
    assert_eq!(room.children[0].name, "distant");
    assert_eq!(room.children[0].volume, (0.5, 0.5));
}

#[test]
fn keys_default_as_hl2dms_and_the_last_one_wins() {
    let scapes = scapes(
        r#""a" {
            "playlooping" { "wave" "quiet.wav" }
            "playlooping" { "wave" "far.wav" "volume" "1" "attenuation" "0.5" "position" "2" }
            "playlooping" { "wave" "lvl.wav" "volume" "1" "attenuation" "1" "soundlevel" "SNDLVL_90dB" }
            "playrandom" { "wave" "not_in_rndwave.wav" "volume" "1" }
            "playrandom" { "position" "Random" "attenuation" "1,2" "rndwave" { "wave" "x.wav" } }
        }"#,
    );
    let a = scapes.get("a").expect("a");
    assert_eq!(a.looping[0].volume, (0.0, 0.0), "no volume: silent");
    assert_eq!(a.looping[1].level, Level::Attenuation((0.5, 0.5)));
    assert_eq!(a.looping[1].position, Some(2));
    assert_eq!(a.looping[2].level, Level::Decibels((90.0, 90.0)));
    // Only `rndwave` gives a random sound its waves.
    assert_eq!(a.random.len(), 1);
    let random = &a.random[0];
    assert_eq!(random.position, Some(RandomPosition::Random));
    assert_eq!(random.pitch, (0.0, 0.0), "no pitch");
    assert_eq!(random.soundlevel, (70.0, 60.0), "attenuation as levels");
}

#[test]
fn a_loop_without_a_volume_is_not_started() {
    let scapes = scapes(r#""a" { "playlooping" { "wave" "quiet.wav" } }"#);
    assert!(scapes.start("a", 0, &mut middle()).looping.is_empty());
}

#[test]
fn starting_draws_each_loop_once_within_its_interval() {
    let scapes = scapes(
        r#""a" { "playlooping" { "wave" "w.wav" "volume" "0.4,0.6" "pitch" "90,110" "attenuation" "0.8" } }"#,
    );
    let mut draws = [0.0, 1.0, 0.25, 0.5, 0.5].into_iter();
    let started = scapes.start("a", 0, &mut || draws.next().expect("a draw"));
    let sound = &started.looping[0];
    assert!((sound.volume - 0.4).abs() < 1e-6);
    assert_eq!(sound.pitch, 110.0);
    // 20 / 0.8 + 50.
    assert_eq!(sound.soundlevel, 75.0);
    assert_eq!(sound.position, None, "ambient");
    let mut high = || 0.999;
    let louder = scapes.start("a", 0, &mut high);
    assert!(louder.looping[0].volume > 0.59 && louder.looping[0].volume <= 0.6);
}

#[test]
fn children_mix_in_at_their_drawn_volume_and_cycles_stop_eight_deep() {
    let scapes = scapes(FILE);
    let started = scapes.start("Prison.Control_Room", 0, &mut middle());
    // The room, then the distant scape at half volume holding the room
    // again, and so on: nine soundscapes in all, eight below the top.
    assert_eq!(started.looping.len(), 9);
    assert_eq!(started.looping[1].wave, "far.wav");
    assert!((started.looping[1].volume - 0.4).abs() < 1e-6);
    assert!((started.random[1].master - 0.5).abs() < 1e-6);
    assert!(scapes.start("missing", 0, &mut middle()).looping.is_empty());
}

#[test]
fn positional_sounds_need_their_position_set() {
    let scapes = scapes(
        r#""a" {
            "playlooping" { "wave" "at1.wav" "volume" "1" "position" "1" }
            "playlooping" { "wave" "at3.wav" "volume" "1" "position" "3" }
            "playrandom" { "pitch" "100" "position" "3" "rndwave" { "wave" "r3.wav" } }
            "playrandom" { "pitch" "100" "position" "random" "rndwave" { "wave" "around.wav" } }
            "playrandom" { "pitch" "100" "rndwave" { "wave" "here.wav" } }
        }"#,
    );
    // Only position 1 is set.
    let started = scapes.start("a", 0b10, &mut middle());
    let waves: Vec<&str> = started.looping.iter().map(|l| l.wave.as_str()).collect();
    assert_eq!(waves, ["at1.wav"], "position 3 is skipped");
    assert_eq!(started.looping[0].position, Some(1));
    let places: Vec<RandomPlace> = started.random.iter().map(|r| r.place).collect();
    assert_eq!(
        places,
        [RandomPlace::AroundListener, RandomPlace::Ambient],
        "the random sound at position 3 is skipped"
    );
}

#[test]
fn children_shift_and_override_positions() {
    let scapes = scapes(
        r#"
        "top" {
            "playsoundscape" { "name" "shifted" "position" "2" }
            "playsoundscape" { "name" "pinned" "positionoverride" "1" }
            "playsoundscape" { "name" "ambient_pinned" "ambientpositionoverride" "4" }
        }
        "shifted" { "playlooping" { "wave" "s.wav" "volume" "1" "position" "1" } }
        "pinned" {
            "playlooping" { "wave" "p.wav" "volume" "1" "position" "6" }
            "playlooping" { "wave" "pa.wav" "volume" "1" }
        }
        "ambient_pinned" {
            "playlooping" { "wave" "a.wav" "volume" "1" }
            "playrandom" { "pitch" "100" "position" "random" "rndwave" { "wave" "r.wav" } }
        }"#,
    );
    let started = scapes.start("top", 0xff, &mut middle());
    let at = |wave: &str| {
        started
            .looping
            .iter()
            .find(|sound| sound.wave == wave)
            .map(|sound| sound.position)
    };
    assert_eq!(at("s.wav"), Some(Some(3)), "2 + 1");
    assert_eq!(at("p.wav"), Some(Some(1)), "overridden");
    assert_eq!(at("pa.wav"), Some(Some(1)), "ambient ones too");
    assert_eq!(at("a.wav"), Some(Some(4)));
    assert_eq!(
        started.random[0].place,
        RandomPlace::AroundListener,
        "an ambient override leaves a random position random"
    );
}

#[test]
fn numbers_read_as_hl2dm_reads_them() {
    assert_eq!(interval("0.5"), (0.5, 0.5));
    assert_eq!(interval("3, 1"), (3.0, 1.0));
    assert_eq!(interval("loud"), (0.0, 0.0));
    assert_eq!(atoi(" 12abc"), 12);
    assert_eq!(atoi("-3"), -3);
    assert_eq!(atoi("random"), 0);
    assert_eq!(attenuation_level(0.0), 0.0);
    assert_eq!(attenuation_level(1.0), 70.0);
    let mut unit = || 0.25;
    assert!((draw((15.0, 5.0), &mut unit) - 12.5).abs() < 1e-6);
}

#[test]
fn manifests_list_their_files() {
    let manifest = r#"soundscaples_manifest { "file" "scripts/soundscapes.txt" "file" "Scripts\Soundscapes_Prison.txt" }"#;
    assert_eq!(
        manifest_files(manifest),
        ["scripts/soundscapes.txt", "scripts/soundscapes_prison.txt"]
    );
}
