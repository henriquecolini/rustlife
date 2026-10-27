//! Sprite-sheet data of particle textures.
//!
//! A particle material's VTF holds a grid of frames in one image, and a
//! `sheet` resource (tag `0x10`) says which rectangle of the image each frame
//! of each *sequence* is: a sequence is one animation (a puff of smoke, an
//! impact), and a particle names the one it plays. [`parse`] reads it from a
//! whole VTF file.
//!
//! The resource is a little-endian list: a version (`0` stores one rectangle
//! per frame, `1` four, for the engine's blended layers), the number of
//! sequences, then per sequence its id, a clamp flag, its frame count and
//! total time, then per frame a duration and the rectangles
//! (`u0 v0 u1 v1`, in `0..1` of the image).

/// The resource tag of the sheet in VTF 7.3+.
const SHEET_TAG: [u8; 3] = [0x10, 0, 0];
/// `RSRCF_HAS_NO_DATA_CHUNK`: the resource's offset field is its value.
const NO_DATA_CHUNK: u8 = 0x02;
/// Most sequences read from one sheet.
const MAX_SEQUENCES: usize = 1024;
/// Most frames read from one sequence.
const MAX_FRAMES: usize = 4096;

/// A rectangle of the image, as fractions of its width and height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left.
    pub u0: f32,
    /// Top.
    pub v0: f32,
    /// Right.
    pub u1: f32,
    /// Bottom.
    pub v1: f32,
}

/// One frame of a sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Seconds the frame lasts at the sequence's own speed.
    pub duration: f32,
    /// Where it is in the image.
    pub rect: Rect,
}

/// One animation of a sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct Sequence {
    /// The number particles name it by.
    pub id: u32,
    /// Whether the animation holds its last frame rather than looping.
    pub clamp: bool,
    /// Seconds for the whole sequence at its own speed.
    pub total_time: f32,
    /// Its frames in order.
    pub frames: Vec<Frame>,
}

/// All the sequences of a texture.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sheet {
    /// In file order.
    pub sequences: Vec<Sequence>,
}

impl Sheet {
    /// The sequence particles call `id`, or the first when there is none of
    /// that number.
    #[must_use]
    pub fn sequence(&self, id: u32) -> Option<&Sequence> {
        self.sequences
            .iter()
            .find(|s| s.id == id)
            .or_else(|| self.sequences.first())
    }

    /// The frame counts by sequence id, with ids that are skipped counting
    /// as one frame; for lifetimes taken from a sequence.
    #[must_use]
    pub fn frame_counts(&self) -> Vec<f32> {
        let len = self.sequences.iter().map(|s| s.id as usize + 1).max();
        let mut counts = vec![1.0; len.unwrap_or(0)];
        for s in &self.sequences {
            if let Some(slot) = counts.get_mut(s.id as usize) {
                *slot = s.frames.len() as f32;
            }
        }
        counts
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Option<u32> {
        let v = u32::from_le_bytes(self.bytes.get(self.at..self.at + 4)?.try_into().ok()?);
        self.at += 4;
        Some(v)
    }

    fn f32(&mut self) -> Option<f32> {
        self.u32().map(f32::from_bits)
    }
}

/// The sheet of the VTF file `bytes`, or `None` if it has none or it is
/// malformed. Never panics.
#[must_use]
pub fn parse(bytes: &[u8]) -> Option<Sheet> {
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    if bytes.get(..4)? != b"VTF\0" || u32_at(8)? < 3 {
        return None;
    }
    let count = u32_at(68)? as usize;
    let entry = (0..count.min(32)).find_map(|index| {
        let at = 80 + index * 8;
        (bytes.get(at..at + 3)? == SHEET_TAG).then_some(at)
    })?;
    if *bytes.get(entry + 3)? & NO_DATA_CHUNK != 0 {
        return None;
    }
    let chunk = u32_at(entry + 4)? as usize;
    let length = u32_at(chunk)? as usize;
    let data = bytes.get(chunk + 4..chunk.checked_add(4)?.checked_add(length)?)?;
    parse_data(data)
}

fn parse_data(data: &[u8]) -> Option<Sheet> {
    let mut r = Reader { bytes: data, at: 0 };
    let version = r.u32()?;
    let layers = match version {
        0 => 1,
        1 => 4,
        _ => return None,
    };
    let count = r.u32()? as usize;
    let mut sequences = Vec::new();
    for _ in 0..count.min(MAX_SEQUENCES) {
        let id = r.u32()?;
        let clamp = r.u32()? != 0;
        let frame_count = r.u32()? as usize;
        let total_time = r.f32()?;
        let mut frames = Vec::new();
        for _ in 0..frame_count.min(MAX_FRAMES) {
            let duration = r.f32()?;
            let mut first = None;
            for _ in 0..layers {
                let rect = Rect {
                    u0: r.f32()?,
                    v0: r.f32()?,
                    u1: r.f32()?,
                    v1: r.f32()?,
                };
                first.get_or_insert(rect);
            }
            frames.push(Frame {
                duration,
                rect: first?,
            });
        }
        sequences.push(Sequence {
            id,
            clamp,
            total_time,
            frames,
        });
    }
    Some(Sheet { sequences })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn pushf(out: &mut Vec<u8>, v: f32) {
        push(out, v.to_bits());
    }

    /// A 7.3 VTF with only a header and one sheet resource.
    fn vtf_with(sheet: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 88];
        out[..4].copy_from_slice(b"VTF\0");
        out[8..12].copy_from_slice(&3u32.to_le_bytes());
        out[68..72].copy_from_slice(&1u32.to_le_bytes());
        out[80..83].copy_from_slice(&SHEET_TAG);
        out[84..88].copy_from_slice(&88u32.to_le_bytes());
        out.extend_from_slice(&(sheet.len() as u32).to_le_bytes());
        out.extend_from_slice(sheet);
        out
    }

    fn two_sequences(version: u32) -> Vec<u8> {
        let mut s = Vec::new();
        push(&mut s, version);
        push(&mut s, 2);
        for (id, frames) in [(0u32, 2u32), (3, 1)] {
            push(&mut s, id);
            push(&mut s, 1);
            push(&mut s, frames);
            pushf(&mut s, frames as f32 * 0.1);
            for f in 0..frames {
                pushf(&mut s, 0.1);
                for layer in 0..if version == 0 { 1 } else { 4 } {
                    for v in [f as f32 * 0.5, 0.0, f as f32 * 0.5 + 0.5, 1.0] {
                        pushf(&mut s, v + layer as f32 * 100.0);
                    }
                }
            }
        }
        s
    }

    #[test]
    fn reads_sequences_and_frames() {
        for version in [0, 1] {
            let sheet = parse(&vtf_with(&two_sequences(version))).unwrap();
            assert_eq!(sheet.sequences.len(), 2);
            assert_eq!(sheet.sequences[0].frames.len(), 2);
            assert_eq!(sheet.sequences[0].frames[1].rect.u0, 0.5);
            assert!(sheet.sequences[1].clamp);
            assert_eq!(sheet.sequence(3).unwrap().frames.len(), 1);
            assert_eq!(sheet.sequence(9).unwrap().id, 0, "falls back to the first");
            assert_eq!(sheet.frame_counts(), [2.0, 1.0, 1.0, 1.0]);
        }
    }

    #[test]
    fn textures_without_a_sheet_or_truncated_ones_give_none() {
        assert!(parse(b"").is_none());
        assert!(parse(b"VTF\0").is_none());
        let good = vtf_with(&two_sequences(0));
        for cut in 0..good.len() {
            let _ = parse(&good[..cut]);
        }
        assert!(parse(&good[..good.len() - 4]).is_none());
        let mut plain = good;
        plain[80] = 0x30;
        assert!(parse(&plain).is_none());
    }
}
