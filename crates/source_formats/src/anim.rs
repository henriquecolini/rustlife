//! Skeletons and animations of Source models.
//!
//! A model's `.mdl` holds its skeleton — bones, each with a parent and a
//! rest position and orientation relative to it — and its animations:
//!
//! - **Animations** (`mstudioanimdesc_t`) store, for each animated bone, a
//!   position and a rotation per frame. Each is either one raw value for
//!   every frame (a 48-bit vector, a 48- or 64-bit quaternion) or a
//!   run-length encoded stream of 16-bit values per component, scaled by
//!   the bone's `posscale`/`rotscale` and, unless the animation is a delta,
//!   added to the bone's rest value. Long animations are split into
//!   sections, and models that stream their animations keep the data in an
//!   `.ani` file of blocks.
//! - **Sequences** (`mstudioseqdesc_t`) are what the game plays: a label
//!   (`run_smg1`), an activity (`ACT_HL2MP_RUN_SMG1`), flags (looping,
//!   delta), and a grid of animations to blend between by up to two pose
//!   parameters (`move_yaw` for the eight directions of a run).
//! - **Included models** (`$includemodel`) lend their sequences: HL2DM's
//!   player models take theirs from `models/player/male_anims.mdl`. Their
//!   bones are matched to the including model's by name.
//!
//! [`read`] decodes all of that into plain values in Source space (Z up,
//! inches, quaternions `[x, y, z, w]`), every frame of every animated bone
//! spelled out. The layouts follow the public Source SDK's `studio.h`.

use thiserror::Error;

/// `studiohdr_t` field offsets.
mod header {
    pub(super) const FLAGS: usize = 152;
    pub(super) const BONES: usize = 156;
    pub(super) const ANIMATIONS: usize = 180;
    pub(super) const SEQUENCES: usize = 188;
    pub(super) const ATTACHMENTS: usize = 240;
    pub(super) const POSE_PARAMETERS: usize = 300;
    pub(super) const INCLUDES: usize = 336;
    pub(super) const ANIM_BLOCK_NAME: usize = 348;
    pub(super) const ANIM_BLOCKS: usize = 352;
    /// The header's size: nothing above lies inside it.
    pub(super) const SIZE: usize = 408;
}

/// Record sizes.
const BONE_SIZE: usize = 216;
const ANIM_DESC_SIZE: usize = 100;
const SEQUENCE_SIZE: usize = 212;
const ATTACHMENT_SIZE: usize = 92;
const POSE_PARAMETER_SIZE: usize = 20;
const EVENT_SIZE: usize = 80;

/// Per-bone animation flags (`STUDIO_ANIM_*`).
mod anim_flags {
    pub(super) const RAW_POSITION: u8 = 0x01;
    pub(super) const RAW_ROTATION: u8 = 0x02;
    pub(super) const ANIMATED_POSITION: u8 = 0x04;
    pub(super) const ANIMATED_ROTATION: u8 = 0x08;
    pub(super) const DELTA: u8 = 0x10;
    pub(super) const RAW_ROTATION_64: u8 = 0x20;
}

/// Sequence and animation flags (`STUDIO_*`).
mod flags {
    pub(super) const LOOPING: i32 = 0x0001;
    pub(super) const DELTA: i32 = 0x0004;
}

/// Most frames an animation may claim: far beyond any real one, and a
/// guard against corrupt counts allocating gigabytes.
const MAX_FRAMES: usize = 100_000;
/// Most bones a model may have (Source's limit).
const MAX_BONES: usize = 256;

/// Errors from [`read`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AnimError {
    /// Not a model, or a version this reader does not know.
    #[error("not a studio model of a known version")]
    NotModel,
    /// A table runs past the end of the file.
    #[error("{0} out of bounds")]
    OutOfBounds(&'static str),
}

/// One bone of a skeleton.
#[derive(Debug, Clone, PartialEq)]
pub struct Bone {
    /// Its name, as animations of included models know it.
    pub name: String,
    /// Its parent, if it has one.
    pub parent: Option<usize>,
    /// Rest position relative to the parent, Source units.
    pub position: [f32; 3],
    /// Rest orientation relative to the parent, `[x, y, z, w]`.
    pub rotation: [f32; 4],
    /// Rest orientation as Euler angles (radians), which animated rotations
    /// are offsets from.
    pub euler: [f32; 3],
    /// Scale of animated position values.
    pub position_scale: [f32; 3],
    /// Scale of animated rotation values.
    pub rotation_scale: [f32; 3],
}

/// A named point fixed to a bone, such as a gun's muzzle.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    /// Its name (`muzzle`).
    pub name: String,
    /// The bone it moves with.
    pub bone: usize,
    /// Its offset from the bone: a 3×4 matrix, rows of `[x, y, z, translation]`.
    pub local: [[f32; 4]; 3],
}

/// A value the game sets to choose between a sequence's blended
/// animations (`move_yaw`, `aim_pitch`).
#[derive(Debug, Clone, PartialEq)]
pub struct PoseParameter {
    /// Its name.
    pub name: String,
    /// The value at the start of its range.
    pub start: f32,
    /// The value at the end of its range.
    pub end: f32,
    /// The range over which it wraps (360 for an angle), or zero.
    pub loop_range: f32,
}

/// One bone's motion through an animation: a position and a rotation per
/// frame (or one for every frame).
#[derive(Debug, Clone, PartialEq)]
pub struct BoneTrack {
    /// The bone, in the animation's own model.
    pub bone: usize,
    /// Positions relative to the parent.
    pub positions: Vec<[f32; 3]>,
    /// Rotations relative to the parent, `[x, y, z, w]`.
    pub rotations: Vec<[f32; 4]>,
}

/// One animation.
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    /// Its name.
    pub name: String,
    /// Frames per second.
    pub fps: f32,
    /// Frames.
    pub frames: usize,
    /// Whether it loops.
    pub looping: bool,
    /// Whether it is a delta, added on top of another pose.
    pub delta: bool,
    /// Every bone of the model, animated or at rest (for a delta, the
    /// bones it does not move hold no offset).
    pub tracks: Vec<BoneTrack>,
}

/// Something a sequence does at a point of its cycle: a sound, a muzzle
/// flash.
#[derive(Debug, Clone, PartialEq)]
pub struct SequenceEvent {
    /// Where in the cycle, `0..=1`.
    pub cycle: f32,
    /// The event's number (old-style events) or zero.
    pub event: i32,
    /// The event's name (new-style events), possibly empty.
    pub name: String,
    /// Its options: a sound name, a particle...
    pub options: String,
}

/// One sequence: what the game plays.
#[derive(Debug, Clone, PartialEq)]
pub struct Sequence {
    /// Its label (`run_smg1`).
    pub label: String,
    /// Its activity (`ACT_HL2MP_RUN_SMG1`), possibly empty.
    pub activity: String,
    /// Whether it loops.
    pub looping: bool,
    /// Whether it is a delta, layered over another sequence.
    pub delta: bool,
    /// Its animations, by index into [`ModelAnimation::animations`], in a
    /// grid `group[0]` wide and `group[1]` tall.
    pub blends: Vec<usize>,
    /// The blend grid's size.
    pub group: [usize; 2],
    /// The pose parameter of each grid axis, if any.
    pub parameters: [Option<usize>; 2],
    /// Each axis' parameter value at its first and last column or row.
    pub ranges: [(f32, f32); 2],
    /// Seconds over which it fades in and out.
    pub fade: (f32, f32),
    /// Events along its cycle.
    pub events: Vec<SequenceEvent>,
}

/// Everything about a model's skeleton and animation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelAnimation {
    /// The skeleton.
    pub bones: Vec<Bone>,
    /// Points fixed to bones.
    pub attachments: Vec<Attachment>,
    /// Pose parameters.
    pub pose_parameters: Vec<PoseParameter>,
    /// Animations, decoded.
    pub animations: Vec<Animation>,
    /// Sequences.
    pub sequences: Vec<Sequence>,
    /// Models whose sequences this one includes, paths as written.
    pub includes: Vec<String>,
    /// The `.ani` file holding streamed animation blocks, if any.
    pub anim_blocks_file: Option<String>,
    /// Whether the model is a static prop (`STUDIOHDR_FLAGS_STATIC_PROP`).
    pub static_prop: bool,
}

/// Reads a model's skeleton, animations and sequences from its `.mdl`
/// bytes, with the bytes of its `.ani` file if it streams animation
/// blocks. Never panics; animations whose data cannot be read are left
/// out (logged at debug level).
///
/// # Errors
///
/// Fails if the bytes are not a studio model, or its tables run past the
/// end of the file.
pub fn read(mdl: &[u8], ani: Option<&[u8]>) -> Result<ModelAnimation, AnimError> {
    let file = File { bytes: mdl };
    if mdl.len() < header::SIZE || &mdl[..4] != b"IDST" {
        return Err(AnimError::NotModel);
    }
    let version = file.i32(4).ok_or(AnimError::NotModel)?;
    if !(44..=49).contains(&version) {
        return Err(AnimError::NotModel);
    }
    let table = |at: usize, what| -> Result<(usize, usize), AnimError> {
        let count = file.count(at).ok_or(AnimError::OutOfBounds(what))?;
        let offset = file.count(at + 4).ok_or(AnimError::OutOfBounds(what))?;
        Ok((count, offset))
    };
    let bones = read_bones(&file, table(header::BONES, "bones")?)?;
    let attachments = read_attachments(&file, table(header::ATTACHMENTS, "attachments")?)?;
    let pose_parameters =
        read_pose_parameters(&file, table(header::POSE_PARAMETERS, "pose parameters")?)?;
    let blocks = read_blocks(&file, table(header::ANIM_BLOCKS, "animation blocks")?)?;
    let ani = ani.map(|bytes| File { bytes });
    let animations = read_animations(
        &file,
        table(header::ANIMATIONS, "animations")?,
        &bones,
        &blocks,
        ani.as_ref(),
    )?;
    let sequences = read_sequences(
        &file,
        table(header::SEQUENCES, "sequences")?,
        animations.len(),
    )?;
    let (count, offset) = table(header::INCLUDES, "includes")?;
    let includes = (0..count)
        .filter_map(|index| {
            let at = offset + index * 8;
            file.string_at(at, file.i32(at + 4)?)
        })
        .filter(|name| !name.is_empty())
        .map(|name| name.replace('\\', "/"))
        .collect();
    let anim_blocks_file = file
        .count(header::ANIM_BLOCK_NAME)
        .filter(|&offset| offset > 0)
        .and_then(|offset| file.string(offset))
        .filter(|name| !name.is_empty())
        .map(|name| name.replace('\\', "/"));
    Ok(ModelAnimation {
        bones,
        attachments,
        pose_parameters,
        animations,
        sequences,
        includes,
        anim_blocks_file,
        static_prop: file.i32(header::FLAGS).unwrap_or(0) & 0x10 != 0,
    })
}

/// Little-endian reads that fail rather than panic.
struct File<'a> {
    bytes: &'a [u8],
}

impl File<'_> {
    fn array<const N: usize>(&self, at: usize) -> Option<[u8; N]> {
        self.bytes.get(at..at.checked_add(N)?)?.try_into().ok()
    }

    fn i32(&self, at: usize) -> Option<i32> {
        self.array(at).map(i32::from_le_bytes)
    }

    fn i16(&self, at: usize) -> Option<i16> {
        self.array(at).map(i16::from_le_bytes)
    }

    fn u16(&self, at: usize) -> Option<u16> {
        self.array(at).map(u16::from_le_bytes)
    }

    fn f32(&self, at: usize) -> Option<f32> {
        self.array(at).map(f32::from_le_bytes)
    }

    fn u8(&self, at: usize) -> Option<u8> {
        self.bytes.get(at).copied()
    }

    /// A non-negative count or offset.
    fn count(&self, at: usize) -> Option<usize> {
        usize::try_from(self.i32(at)?).ok()
    }

    fn vector(&self, at: usize) -> Option<[f32; 3]> {
        Some([self.f32(at)?, self.f32(at + 4)?, self.f32(at + 8)?])
    }

    fn quaternion(&self, at: usize) -> Option<[f32; 4]> {
        Some([
            self.f32(at)?,
            self.f32(at + 4)?,
            self.f32(at + 8)?,
            self.f32(at + 12)?,
        ])
    }

    fn matrix(&self, at: usize) -> Option<[[f32; 4]; 3]> {
        let row = |row: usize| -> Option<[f32; 4]> {
            let at = at + row * 16;
            Some([
                self.f32(at)?,
                self.f32(at + 4)?,
                self.f32(at + 8)?,
                self.f32(at + 12)?,
            ])
        };
        Some([row(0)?, row(1)?, row(2)?])
    }

    /// The NUL-terminated string at `at`.
    fn string(&self, at: usize) -> Option<String> {
        let rest = self.bytes.get(at..)?;
        let end = rest.iter().position(|&byte| byte == 0)?;
        Some(String::from_utf8_lossy(&rest[..end]).into_owned())
    }

    /// The string at `relative` from a record at `base` (Source's name
    /// offsets are relative to the record holding them).
    fn string_at(&self, base: usize, relative: i32) -> Option<String> {
        if relative == 0 {
            return Some(String::new());
        }
        self.string(base.checked_add_signed(relative as isize)?)
    }

    fn in_bounds(&self, offset: usize, count: usize, size: usize) -> bool {
        count
            .checked_mul(size)
            .and_then(|len| offset.checked_add(len))
            .is_some_and(|end| end <= self.bytes.len())
    }
}

fn read_bones(file: &File, (count, offset): (usize, usize)) -> Result<Vec<Bone>, AnimError> {
    if count > MAX_BONES || !file.in_bounds(offset, count, BONE_SIZE) {
        return Err(AnimError::OutOfBounds("bones"));
    }
    (0..count)
        .map(|index| {
            let at = offset + index * BONE_SIZE;
            let bone = || -> Option<Bone> {
                let parent = file.i32(at + 4)?;
                Some(Bone {
                    name: file.string_at(at, file.i32(at)?)?,
                    parent: usize::try_from(parent)
                        .ok()
                        .filter(|&parent| parent < index),
                    position: file.vector(at + 32)?,
                    rotation: file.quaternion(at + 44)?,
                    euler: file.vector(at + 60)?,
                    position_scale: file.vector(at + 72)?,
                    rotation_scale: file.vector(at + 84)?,
                })
            };
            bone().ok_or(AnimError::OutOfBounds("bones"))
        })
        .collect()
}

fn read_attachments(
    file: &File,
    (count, offset): (usize, usize),
) -> Result<Vec<Attachment>, AnimError> {
    if !file.in_bounds(offset, count, ATTACHMENT_SIZE) {
        return Err(AnimError::OutOfBounds("attachments"));
    }
    Ok((0..count)
        .filter_map(|index| {
            let at = offset + index * ATTACHMENT_SIZE;
            Some(Attachment {
                name: file.string_at(at, file.i32(at)?)?,
                bone: file.count(at + 8)?,
                local: file.matrix(at + 12)?,
            })
        })
        .collect())
}

fn read_pose_parameters(
    file: &File,
    (count, offset): (usize, usize),
) -> Result<Vec<PoseParameter>, AnimError> {
    if !file.in_bounds(offset, count, POSE_PARAMETER_SIZE) {
        return Err(AnimError::OutOfBounds("pose parameters"));
    }
    Ok((0..count)
        .filter_map(|index| {
            let at = offset + index * POSE_PARAMETER_SIZE;
            Some(PoseParameter {
                name: file.string_at(at, file.i32(at)?)?,
                start: file.f32(at + 8)?,
                end: file.f32(at + 12)?,
                loop_range: file.f32(at + 16)?,
            })
        })
        .collect())
}

/// Each animation block's `(start, end)` in the `.ani` file.
fn read_blocks(
    file: &File,
    (count, offset): (usize, usize),
) -> Result<Vec<(usize, usize)>, AnimError> {
    if !file.in_bounds(offset, count, 8) {
        return Err(AnimError::OutOfBounds("animation blocks"));
    }
    Ok((0..count)
        .map(|index| {
            let at = offset + index * 8;
            (file.count(at).unwrap_or(0), file.count(at + 4).unwrap_or(0))
        })
        .collect())
}

fn read_animations(
    file: &File,
    (count, offset): (usize, usize),
    bones: &[Bone],
    blocks: &[(usize, usize)],
    ani: Option<&File>,
) -> Result<Vec<Animation>, AnimError> {
    if !file.in_bounds(offset, count, ANIM_DESC_SIZE) {
        return Err(AnimError::OutOfBounds("animations"));
    }
    Ok((0..count)
        .map(|index| {
            let at = offset + index * ANIM_DESC_SIZE;
            read_animation(file, at, bones, blocks, ani).unwrap_or_else(|| {
                tracing::debug!(index, "unreadable animation; holding the rest pose");
                rest_animation(file, at, bones)
            })
        })
        .collect())
}

/// An animation that holds the rest pose, standing in for one that cannot
/// be read so sequence indices stay valid.
fn rest_animation(file: &File, at: usize, bones: &[Bone]) -> Animation {
    Animation {
        name: file
            .i32(at + 4)
            .and_then(|name| file.string_at(at, name))
            .unwrap_or_default(),
        fps: 30.0,
        frames: 1,
        looping: false,
        delta: false,
        tracks: bones
            .iter()
            .enumerate()
            .map(|(bone, rest)| BoneTrack {
                bone,
                positions: vec![rest.position],
                rotations: vec![rest.rotation],
            })
            .collect(),
    }
}

/// Where an animation's per-bone data for `frame` starts, and the frame
/// within it: the animation's own data, or that of the frame's section,
/// in the `.mdl` or in a block of the `.ani` file.
fn frame_data<'a>(
    file: &'a File<'a>,
    at: usize,
    frame: usize,
    blocks: &[(usize, usize)],
    ani: Option<&'a File<'a>>,
) -> Option<(&'a File<'a>, usize, usize)> {
    let section_frames = file.count(at + 84).unwrap_or(0);
    let (block, index, local) = match frame.checked_div(section_frames) {
        Some(section) => {
            let sections = file.count(at + 80)?;
            let record = at.checked_add(sections)?.checked_add(section * 8)?;
            (
                file.count(record)?,
                file.i32(record + 4)?,
                frame - section * section_frames,
            )
        }
        None => (file.count(at + 52)?, file.i32(at + 56)?, frame),
    };
    if block == 0 {
        let start = at.checked_add_signed(index as isize)?;
        Some((file, start, local))
    } else {
        let (start, _) = *blocks.get(block)?;
        let ani = ani?;
        Some((ani, start.checked_add_signed(index as isize)?, local))
    }
}

fn read_animation(
    file: &File,
    at: usize,
    bones: &[Bone],
    blocks: &[(usize, usize)],
    ani: Option<&File>,
) -> Option<Animation> {
    let name = file.string_at(at, file.i32(at + 4)?)?;
    let fps = file.f32(at + 8)?;
    let animation_flags = file.i32(at + 12)?;
    let frames = file.count(at + 16)?.clamp(1, MAX_FRAMES);
    let delta = animation_flags & flags::DELTA != 0;
    let mut tracks: Vec<BoneTrack> = bones
        .iter()
        .enumerate()
        .map(|(bone, rest)| {
            let (position, rotation) = if delta {
                ([0.0; 3], [0.0, 0.0, 0.0, 1.0])
            } else {
                (rest.position, rest.rotation)
            };
            BoneTrack {
                bone,
                positions: vec![position; frames],
                rotations: vec![rotation; frames],
            }
        })
        .collect();
    for frame in 0..frames {
        let (data, start, local) = frame_data(file, at, frame, blocks, ani)?;
        let mut cursor = start;
        // Each bone record names the offset of the next; zero ends the
        // chain. The bone count bounds a corrupt (cyclic) chain.
        for _ in 0..=bones.len() {
            let bone = usize::from(data.u8(cursor)?);
            let bone_flags = data.u8(cursor + 1)?;
            let next = usize::from(data.u16(cursor + 2)?);
            if let (Some(rest), Some(track)) = (bones.get(bone), tracks.get_mut(bone)) {
                let (position, rotation) = bone_pose(data, cursor + 4, bone_flags, local, rest)?;
                if let Some(position) = position {
                    track.positions[frame] = position;
                }
                if let Some(rotation) = rotation {
                    track.rotations[frame] = rotation;
                }
            }
            if next == 0 {
                break;
            }
            cursor += next;
        }
    }
    // Bones that hold still keep a single value.
    for track in &mut tracks {
        // Bit-identical values: the same stored value decoded again.
        let same = |a: &[f32], b: &[f32]| a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits());
        if track
            .positions
            .windows(2)
            .all(|pair| same(&pair[0], &pair[1]))
        {
            track.positions.truncate(1);
        }
        if track
            .rotations
            .windows(2)
            .all(|pair| same(&pair[0], &pair[1]))
        {
            track.rotations.truncate(1);
        }
    }
    Some(Animation {
        name,
        fps: if fps > 0.0 && fps.is_finite() {
            fps
        } else {
            30.0
        },
        frames,
        looping: animation_flags & flags::LOOPING != 0,
        delta,
        tracks,
    })
}

/// One bone's position and rotation at `frame` from its record's data at
/// `at`; `None` where the record leaves that part alone.
#[allow(clippy::type_complexity)]
fn bone_pose(
    data: &File,
    at: usize,
    bone_flags: u8,
    frame: usize,
    rest: &Bone,
) -> Option<(Option<[f32; 3]>, Option<[f32; 4]>)> {
    let delta = bone_flags & anim_flags::DELTA != 0;
    let (rotation, rotation_size) = if bone_flags & anim_flags::RAW_ROTATION != 0 {
        (Some(quaternion48(data.array(at)?)), 6)
    } else if bone_flags & anim_flags::RAW_ROTATION_64 != 0 {
        (Some(quaternion64(data.array(at)?)), 8)
    } else if bone_flags & anim_flags::ANIMATED_ROTATION != 0 {
        let mut euler = [0.0; 3];
        for (axis, angle) in euler.iter_mut().enumerate() {
            let value = animated_value(data, at, axis, frame)?;
            *angle = value * rest.rotation_scale[axis] + if delta { 0.0 } else { rest.euler[axis] };
        }
        (Some(euler_to_quaternion(euler)), 6)
    } else {
        (None, 0)
    };
    let at = at + rotation_size;
    let position = if bone_flags & anim_flags::RAW_POSITION != 0 {
        let raw: [u8; 6] = data.array(at)?;
        Some(std::array::from_fn(|axis| {
            half_to_f32(u16::from_le_bytes([raw[axis * 2], raw[axis * 2 + 1]]))
        }))
    } else if bone_flags & anim_flags::ANIMATED_POSITION != 0 {
        let mut position = [0.0; 3];
        for (axis, coordinate) in position.iter_mut().enumerate() {
            let value = animated_value(data, at, axis, frame)?;
            *coordinate =
                value * rest.position_scale[axis] + if delta { 0.0 } else { rest.position[axis] };
        }
        Some(position)
    } else {
        None
    };
    Some((position, rotation))
}

/// Component `axis` at `frame` of the value stream whose pointer
/// (`mstudioanim_valueptr_t`, three offsets from itself) is at `pointer`:
/// a run-length encoding of 16-bit values (`mstudioanimvalue_t`). Each run
/// starts with a header of `valid` stored values covering `total` frames;
/// frames past the stored values repeat the last one.
fn animated_value(data: &File, pointer: usize, axis: usize, frame: usize) -> Option<f32> {
    let offset = usize::from(data.u16(pointer + axis * 2)?);
    if offset == 0 {
        return Some(0.0);
    }
    let mut run = pointer + offset;
    let mut frame = frame;
    // Each step skips a run; the frame count bounds a corrupt stream.
    for _ in 0..=MAX_FRAMES {
        let valid = usize::from(data.u8(run)?);
        let total = usize::from(data.u8(run + 1)?);
        if total == 0 {
            return Some(0.0);
        }
        if frame < total {
            let index = if frame < valid { frame + 1 } else { valid };
            return Some(f32::from(data.i16(run + index * 2)?));
        }
        frame -= total;
        run += (valid + 1) * 2;
    }
    None
}

/// Source's `RadianEuler` → quaternion (`AngleQuaternion`): roll about X,
/// pitch about Y, yaw about Z, composed `Rz · Ry · Rx`.
#[must_use]
pub fn euler_to_quaternion([x, y, z]: [f32; 3]) -> [f32; 4] {
    let (sr, cr) = (x * 0.5).sin_cos();
    let (sp, cp) = (y * 0.5).sin_cos();
    let (sy, cy) = (z * 0.5).sin_cos();
    [
        sr * cp * cy - cr * sp * sy,
        cr * sp * cy + sr * cp * sy,
        cr * cp * sy - sr * sp * cy,
        cr * cp * cy + sr * sp * sy,
    ]
}

/// A 48-bit quaternion: x and y in 16 bits, z in 15, the sign of w in one;
/// w's magnitude follows from the unit length.
fn quaternion48(raw: [u8; 6]) -> [f32; 4] {
    let x = u16::from_le_bytes([raw[0], raw[1]]);
    let y = u16::from_le_bytes([raw[2], raw[3]]);
    let zw = u16::from_le_bytes([raw[4], raw[5]]);
    let x = (f32::from(x) - 32768.0) / 32768.0;
    let y = (f32::from(y) - 32768.0) / 32768.0;
    let z = (f32::from(zw & 0x7fff) - 16384.0) / 16384.0;
    let w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
    [x, y, z, if zw & 0x8000 != 0 { -w } else { w }]
}

/// A 64-bit quaternion: x, y and z in 21 bits each, the sign of w in one.
fn quaternion64(raw: [u8; 8]) -> [f32; 4] {
    let bits = u64::from_le_bytes(raw);
    let component = |shift: u32| ((bits >> shift) & 0x1f_ffff) as f32 - 1_048_576.0;
    let x = component(0) / 1_048_576.5;
    let y = component(21) / 1_048_576.5;
    let z = component(42) / 1_048_576.5;
    let w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
    [x, y, z, if bits >> 63 != 0 { -w } else { w }]
}

/// An IEEE half float.
fn half_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let mantissa = f32::from(bits & 0x3ff);
    match exponent {
        0 => sign * mantissa * 2f32.powi(-24),
        31 if mantissa == 0.0 => sign * f32::INFINITY,
        31 => f32::NAN,
        _ => sign * (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15),
    }
}

fn read_sequences(
    file: &File,
    (count, offset): (usize, usize),
    animations: usize,
) -> Result<Vec<Sequence>, AnimError> {
    if !file.in_bounds(offset, count, SEQUENCE_SIZE) {
        return Err(AnimError::OutOfBounds("sequences"));
    }
    Ok((0..count)
        .filter_map(|index| read_sequence(file, offset + index * SEQUENCE_SIZE, animations))
        .collect())
}

fn read_sequence(file: &File, at: usize, animations: usize) -> Option<Sequence> {
    let sequence_flags = file.i32(at + 12)?;
    let group = [file.count(at + 68)?.max(1), file.count(at + 72)?.max(1)];
    let blend_count = group[0].checked_mul(group[1])?.min(256);
    let blends_at = at.checked_add(file.count(at + 60)?)?;
    let blends = (0..blend_count)
        .map(|blend| {
            let index = usize::try_from(file.i16(blends_at + blend * 2)?).ok()?;
            (index < animations).then_some(index)
        })
        .collect::<Option<Vec<usize>>>()?;
    let parameter = |field: usize| usize::try_from(file.i32(at + field)?).ok();
    let events_at = at.checked_add(file.count(at + 28)?)?;
    let events = (0..file.count(at + 24)?.min(64))
        .filter_map(|event| {
            let at = events_at + event * EVENT_SIZE;
            Some(SequenceEvent {
                cycle: file.f32(at)?,
                event: file.i32(at + 4)?,
                options: file.string(at + 12)?,
                name: file.string_at(at, file.i32(at + 76)?).unwrap_or_default(),
            })
        })
        .collect();
    Some(Sequence {
        label: file.string_at(at, file.i32(at + 4)?)?,
        activity: file.string_at(at, file.i32(at + 8)?)?,
        looping: sequence_flags & flags::LOOPING != 0,
        delta: sequence_flags & flags::DELTA != 0,
        blends,
        group,
        parameters: [parameter(76), parameter(80)],
        ranges: [
            (file.f32(at + 84)?, file.f32(at + 92)?),
            (file.f32(at + 88)?, file.f32(at + 96)?),
        ],
        fade: (file.f32(at + 104)?, file.f32(at + 108)?),
        events,
    })
}

#[cfg(test)]
mod tests;
