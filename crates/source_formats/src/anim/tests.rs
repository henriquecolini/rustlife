//! Tests of the animation reader against models built byte by byte, and
//! (ignored) a real one.

use super::*;

/// Writes a studio model with the given parts at fixed offsets.
struct Builder {
    bytes: Vec<u8>,
}

impl Builder {
    fn new() -> Self {
        let mut bytes = vec![0; 4096];
        bytes[..4].copy_from_slice(b"IDST");
        bytes[4..8].copy_from_slice(&48i32.to_le_bytes());
        Self { bytes }
    }

    fn i32(&mut self, at: usize, value: i32) {
        self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn f32(&mut self, at: usize, value: f32) {
        self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn u16(&mut self, at: usize, value: u16) {
        self.bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, at: usize, text: &str) {
        self.bytes[at..at + text.len()].copy_from_slice(text.as_bytes());
        self.bytes[at + text.len()] = 0;
    }

    /// Two bones at 500: "root" and its child "arm" resting 10 units up,
    /// rotation scale 1 so animated values are radians.
    fn bones(mut self) -> Self {
        self.i32(header::BONES, 2);
        self.i32(header::BONES + 4, 500);
        for (index, (name, parent, z)) in [("root", -1, 0.0), ("arm", 0, 10.0)]
            .into_iter()
            .enumerate()
        {
            let at = 500 + index * BONE_SIZE;
            // Names live at 3000+.
            let name_at = 3000 + index * 16;
            self.i32(at, (name_at - at) as i32);
            self.string(name_at, name);
            self.i32(at + 4, parent);
            self.f32(at + 40, z); // pos.z
            self.f32(at + 56, 1.0); // quat.w
            for axis in 0..3 {
                self.f32(at + 72 + axis * 4, 1.0); // posscale
                self.f32(at + 84 + axis * 4, 1.0 / 1000.0); // rotscale
            }
        }
        self
    }

    /// One looping animation of three frames at 1500: the arm's rotation
    /// about Z animated (RLE: values 0, 1000, 2000 thousandths of a
    /// radian), its position raw.
    fn animation(mut self) -> Self {
        self.i32(header::ANIMATIONS, 1);
        self.i32(header::ANIMATIONS + 4, 1500);
        let at = 1500;
        self.i32(at + 4, (3100 - at) as i32);
        self.string(3100, "swing");
        self.f32(at + 8, 10.0);
        self.i32(at + 12, flags::LOOPING);
        self.i32(at + 16, 3);
        self.i32(at + 56, 200); // data at 1700
        let data = 1700;
        self.bytes[data] = 1; // bone "arm"
        self.bytes[data + 1] = anim_flags::ANIMATED_ROTATION | anim_flags::RAW_POSITION;
        self.u16(data + 2, 0); // last record
        // Value pointer at data + 4: x and y unanimated, z's stream 16
        // bytes on, past the raw position that follows the pointer.
        let pointer = data + 4;
        self.u16(pointer + 4, 16);
        let run = pointer + 16;
        // A run of 3 frames with 3 stored values.
        self.bytes[run] = 3;
        self.bytes[run + 1] = 3;
        for (index, value) in [0i16, 1000, 2000].into_iter().enumerate() {
            self.bytes[run + 2 + index * 2..run + 4 + index * 2]
                .copy_from_slice(&value.to_le_bytes());
        }
        // The raw position, three half floats (1, 2, 3), right after the
        // 6-byte rotation pointer.
        for (axis, half) in [0x3c00u16, 0x4000, 0x4200].into_iter().enumerate() {
            self.u16(pointer + 6 + axis * 2, half);
        }
        self
    }

    /// One sequence at 2000 blending the animation with itself twice
    /// across pose parameter 0.
    fn sequence(mut self) -> Self {
        self.i32(header::SEQUENCES, 1);
        self.i32(header::SEQUENCES + 4, 2000);
        let at = 2000;
        self.i32(at + 4, (3200 - at) as i32);
        self.string(3200, "swing_all");
        self.i32(at + 8, (3220 - at) as i32);
        self.string(3220, "ACT_SWING");
        self.i32(at + 12, flags::LOOPING);
        self.i32(at + 60, 300); // blend indices at 2300
        self.u16(2300, 0);
        self.u16(2302, 0);
        self.i32(at + 68, 2);
        self.i32(at + 72, 1);
        self.i32(at + 76, 0);
        self.i32(at + 80, -1);
        self.f32(at + 84, -1.0);
        self.f32(at + 92, 1.0);
        self
    }

    fn include(mut self, path: &str) -> Self {
        self.i32(header::INCLUDES, 1);
        self.i32(header::INCLUDES + 4, 2600);
        self.i32(2604, 700); // the name at 3300
        self.string(3300, path);
        self
    }

    fn build(self) -> Vec<u8> {
        self.bytes
    }
}

#[test]
fn skeletons_animations_and_sequences_are_read() {
    let model = read(
        &Builder::new()
            .bones()
            .animation()
            .sequence()
            .include("models\\player\\male_anims.mdl")
            .build(),
        None,
    )
    .expect("model");
    assert_eq!(model.bones.len(), 2);
    assert_eq!(
        (model.bones[1].name.as_str(), model.bones[1].parent),
        ("arm", Some(0))
    );
    assert_eq!(model.bones[1].position, [0.0, 0.0, 10.0]);
    assert_eq!(model.includes, ["models/player/male_anims.mdl"]);

    let swing = &model.animations[0];
    assert_eq!(
        (swing.name.as_str(), swing.frames, swing.fps, swing.looping),
        ("swing", 3, 10.0, true)
    );
    let root = &swing.tracks[0];
    assert_eq!(
        root.rotations,
        [[0.0, 0.0, 0.0, 1.0]],
        "unanimated bones rest, held once"
    );
    let arm = &swing.tracks[1];
    assert_eq!(
        arm.positions,
        [[1.0, 2.0, 3.0]],
        "a raw position holds every frame"
    );
    assert_eq!(arm.rotations.len(), 3);
    for (frame, angle) in [0.0f32, 1.0, 2.0].into_iter().enumerate() {
        let [x, y, z, w] = arm.rotations[frame];
        let expected = [0.0, 0.0, (angle * 0.5).sin(), (angle * 0.5).cos()];
        assert!(
            (x - expected[0]).abs() < 1e-5
                && (y - expected[1]).abs() < 1e-5
                && (z - expected[2]).abs() < 1e-5
                && (w - expected[3]).abs() < 1e-5,
            "frame {frame}: {:?}",
            arm.rotations[frame]
        );
    }

    let sequence = &model.sequences[0];
    assert_eq!(
        (sequence.label.as_str(), sequence.activity.as_str()),
        ("swing_all", "ACT_SWING")
    );
    assert_eq!(
        (sequence.blends.as_slice(), sequence.group),
        (&[0, 0][..], [2, 1])
    );
    assert_eq!(sequence.parameters, [Some(0), None]);
    assert!(sequence.looping && !sequence.delta);
}

#[test]
fn run_length_streams_repeat_their_last_value_and_chain_runs() {
    // Two runs: frames 0–3 with values 5, 6 stored (frames 2, 3 repeat 6),
    // then frames 4–5 with 9.
    let mut bytes = vec![0u8; 64];
    // The x component's stream starts 6 bytes past the pointer.
    let pointer = 0;
    bytes[pointer..pointer + 2].copy_from_slice(&6u16.to_le_bytes());
    let run = 6;
    bytes[run] = 2;
    bytes[run + 1] = 4;
    bytes[run + 2..run + 4].copy_from_slice(&5i16.to_le_bytes());
    bytes[run + 4..run + 6].copy_from_slice(&6i16.to_le_bytes());
    let next = run + 6;
    bytes[next] = 1;
    bytes[next + 1] = 2;
    bytes[next + 2..next + 4].copy_from_slice(&9i16.to_le_bytes());
    let file = File { bytes: &bytes };
    let values: Vec<f32> = (0..7)
        .map(|frame| animated_value(&file, pointer, 0, frame).expect("value"))
        .collect();
    assert_eq!(
        values,
        [5.0, 6.0, 6.0, 6.0, 9.0, 9.0, 0.0],
        "past the last run: zero"
    );
}

#[test]
fn packed_quaternions_unpack_to_unit_length() {
    // Identity: x = y = 32768 (zero), z = 16384 (zero), w positive.
    let identity = quaternion48([0x00, 0x80, 0x00, 0x80, 0x00, 0x40]);
    assert_eq!(identity, [0.0, 0.0, 0.0, 1.0]);
    let negative_w = quaternion48([0x00, 0x80, 0x00, 0x80, 0x00, 0xc0]);
    assert_eq!(negative_w[3], -1.0);
    let wide = quaternion64((1_048_576u64 | 1_048_576 << 21 | 1_048_576 << 42).to_le_bytes());
    assert!((wide[3] - 1.0).abs() < 1e-6, "{wide:?}");
    assert_eq!(half_to_f32(0x3c00), 1.0);
    assert_eq!(half_to_f32(0xc000), -2.0);
    let yaw = euler_to_quaternion([0.0, 0.0, std::f32::consts::PI]);
    assert!(
        (yaw[2] - 1.0).abs() < 1e-6 && yaw[3].abs() < 1e-6,
        "{yaw:?}"
    );
}

#[test]
fn garbage_is_refused_without_panicking() {
    assert_eq!(read(b"IDST", None), Err(AnimError::NotModel));
    assert_eq!(read(&[0; 600], None), Err(AnimError::NotModel));
    let mut broken = Builder::new().bones().build();
    broken[header::BONES..header::BONES + 4].copy_from_slice(&100_000i32.to_le_bytes());
    assert!(read(&broken, None).is_err());
    let full = Builder::new().bones().animation().sequence().build();
    for length in [header::SIZE, 600, 1600, 1710, 2100, 3000] {
        let _ = read(&full[..length], None);
    }
}

/// Reads a real model's animations. Set `RUSTLIFE_TEST_MDL` to a `.mdl`
/// with sequences (e.g. `models/player/male_anims.mdl`).
#[test]
#[ignore = "requires a model file; set RUSTLIFE_TEST_MDL"]
fn a_real_models_animations_read() {
    let Ok(path) = std::env::var("RUSTLIFE_TEST_MDL") else {
        return;
    };
    let bytes = std::fs::read(&path).expect("read mdl");
    let model = read(&bytes, None).expect("model");
    eprintln!(
        "{} bones, {} animations, {} sequences",
        model.bones.len(),
        model.animations.len(),
        model.sequences.len()
    );
    assert!(!model.sequences.is_empty());
    for animation in &model.animations {
        for track in &animation.tracks {
            for rotation in &track.rotations {
                let length = rotation.iter().map(|c| c * c).sum::<f32>().sqrt();
                assert!(
                    (length - 1.0).abs() < 1e-2,
                    "{}: {rotation:?}",
                    animation.name
                );
            }
            for position in &track.positions {
                assert!(
                    position.iter().all(|c| c.is_finite() && c.abs() < 10_000.0),
                    "{}",
                    animation.name
                );
            }
        }
    }
}
