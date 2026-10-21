//! Decoding Valve Texture Format (VTF) images into plain RGBA8 pixels.
//!
//! VTF stores textures in a handful of GPU and CPU formats (DXT1/3/5, BGRA8,
//! I8, ...), optionally with a mip chain, several animation frames and cubemap
//! faces. [`decode`] expands frame 0 of the full-resolution image to
//! straight, non-premultiplied RGBA8. For the block-compressed formats
//! (DXT1/3/5) that most textures use, [`compressed`] instead hands out the
//! stored blocks of every mip level as they are, for a GPU that samples them
//! directly: a quarter to an eighth of the memory, and no decoding.
//!
//! Whether a texture is colour data (sampled as sRGB) or a normal map (sampled
//! as linear) is not something the pixels can tell us: Source records that in
//! the *material*, not the texture. We surface the VTF `NORMAL` flag as a hint,
//! but the authoritative decision belongs to the [`crate::vmt`] layer, which
//! knows the shader a texture is bound to.

use thiserror::Error;

/// `TEXTUREFLAGS_NORMAL` from the VTF header flags: the texture is a
/// tangent-space normal map and should be sampled as linear data. Documented on
/// the Valve Developer Community wiki.
const TEXTUREFLAGS_NORMAL: u32 = 0x0080;

/// A VTF decoded to tightly packed, top-to-bottom RGBA8 pixels, ready to hand
/// to a GPU texture upload without further conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedTexture {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes of non-premultiplied RGBA8, row-major from the
    /// top-left pixel.
    pub rgba8: Vec<u8>,
    /// Whether the VTF `NORMAL` flag was set. A hint only: the material layer
    /// has the final say on sRGB-versus-linear sampling.
    pub is_normal_map: bool,
}

/// A block-compressed format a GPU can sample directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockFormat {
    /// DXT1 (BC1): 8 bytes per 4×4 block, with or without 1-bit alpha.
    Dxt1,
    /// DXT3 (BC2): 16 bytes per block, explicit 4-bit alpha.
    Dxt3,
    /// DXT5 (BC3): 16 bytes per block, interpolated alpha.
    Dxt5,
}

impl BlockFormat {
    /// Bytes per 4×4 block.
    #[must_use]
    pub fn block_size(self) -> usize {
        match self {
            Self::Dxt1 => 8,
            Self::Dxt3 | Self::Dxt5 => 16,
        }
    }

    /// The format of a VTF format number, if it is a block format.
    fn of(format: i32) -> Option<Self> {
        match format {
            // DXT1, and DXT1 with one-bit alpha.
            13 | 20 => Some(Self::Dxt1),
            14 => Some(Self::Dxt3),
            15 => Some(Self::Dxt5),
            _ => None,
        }
    }
}

/// A texture's stored blocks, every mip level of frame 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressedTexture {
    /// The block format.
    pub format: BlockFormat,
    /// Width of the largest level, pixels.
    pub width: u32,
    /// Height of the largest level, pixels.
    pub height: u32,
    /// Each level's blocks, the largest first, each half the size of the
    /// one before (down to one block).
    pub mips: Vec<Vec<u8>>,
    /// Whether the VTF `NORMAL` flag was set (see [`DecodedTexture`]).
    pub is_normal_map: bool,
}

/// VTF header fields.
mod header {
    pub(super) const HEADER_SIZE: usize = 12;
    pub(super) const WIDTH: usize = 16;
    pub(super) const FLAGS: usize = 20;
    pub(super) const FRAMES: usize = 24;
    pub(super) const FORMAT: usize = 52;
    pub(super) const MIPS: usize = 56;
    pub(super) const LOW_FORMAT: usize = 57;
    pub(super) const LOW_WIDTH: usize = 61;
    pub(super) const DEPTH: usize = 63;
    pub(super) const RESOURCE_COUNT: usize = 68;
    pub(super) const RESOURCES: usize = 80;
}

/// `TEXTUREFLAGS_ENVMAP`: a cubemap (six faces), which [`compressed`]
/// leaves to [`decode`].
const TEXTUREFLAGS_ENVMAP: u32 = 0x4000;
/// The resource tag of the high-resolution image in VTF 7.3+.
const HIGH_RES_TAG: [u8; 3] = [0x30, 0, 0];

/// The stored blocks of a DXT texture's every mip level, frame 0, or
/// `None` for any other texture (uncompressed formats, cubemaps, volume
/// textures, sizes not a multiple of the block size) and for malformed
/// files. Never panics.
#[must_use]
pub fn compressed(bytes: &[u8]) -> Option<CompressedTexture> {
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
    };
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    if bytes.get(..4)? != b"VTF\0" {
        return None;
    }
    let minor = u32_at(8)?;
    let (width, height) = (
        u32::from(u16_at(header::WIDTH)?),
        u32::from(u16_at(header::WIDTH + 2)?),
    );
    let flags = u32_at(header::FLAGS)?;
    let frames = usize::from(u16_at(header::FRAMES)?.max(1));
    let format = BlockFormat::of(i32::from_le_bytes(
        bytes
            .get(header::FORMAT..header::FORMAT + 4)?
            .try_into()
            .ok()?,
    ))?;
    let mips = usize::from(*bytes.get(header::MIPS)?).max(1);
    let depth = if minor >= 2 {
        u16_at(header::DEPTH)?.max(1)
    } else {
        1
    };
    if flags & TEXTUREFLAGS_ENVMAP != 0
        || depth != 1
        || width % 4 != 0
        || height % 4 != 0
        || width == 0
        || height == 0
    {
        return None;
    }
    let level_size = |level: usize| -> usize {
        let blocks = |side: u32| ((side >> level).max(1) as usize).div_ceil(4);
        blocks(width) * blocks(height) * format.block_size()
    };
    // Where the high-resolution data starts: a resource entry in 7.3+, else
    // right after the header and the low-resolution thumbnail.
    let start = if minor >= 3 {
        let count = u32_at(header::RESOURCE_COUNT)? as usize;
        (0..count.min(32)).find_map(|index| {
            let at = header::RESOURCES + index * 8;
            (bytes.get(at..at + 3)? == HIGH_RES_TAG).then(|| u32_at(at + 4))?
        })? as usize
    } else {
        let low = i32::from_le_bytes(
            bytes
                .get(header::LOW_FORMAT..header::LOW_FORMAT + 4)?
                .try_into()
                .ok()?,
        );
        let low_size = match BlockFormat::of(low) {
            Some(low_format) => {
                let side = |at: usize| {
                    bytes
                        .get(at)
                        .map_or(0, |&side| usize::from(side).div_ceil(4))
                };
                side(header::LOW_WIDTH) * side(header::LOW_WIDTH + 1) * low_format.block_size()
            }
            None => 0,
        };
        u32_at(header::HEADER_SIZE)? as usize + low_size
    };
    // Levels are stored smallest first, each with every frame.
    let mut offsets = vec![0; mips];
    let mut at = start;
    for level in (0..mips).rev() {
        offsets[level] = at;
        at = at.checked_add(level_size(level).checked_mul(frames)?)?;
    }
    if at > bytes.len() {
        return None;
    }
    let levels = (0..mips)
        .map(|level| bytes[offsets[level]..offsets[level] + level_size(level)].to_vec())
        .collect();
    Some(CompressedTexture {
        format,
        width,
        height,
        mips: levels,
        is_normal_map: flags & TEXTUREFLAGS_NORMAL != 0,
    })
}

/// Errors from [`decode`].
#[derive(Debug, Error)]
pub enum VtfError {
    /// The bytes are not a valid VTF, or use a pixel format this decoder does
    /// not support. The wrapped message comes from the underlying `vtf` crate
    /// and names the specific cause (bad signature, unknown format, truncated
    /// data).
    #[error("could not decode VTF: {0}")]
    Decode(String),
}

/// Decodes frame 0 of the full-resolution image in `bytes` to RGBA8.
///
/// The returned pixels are independent of the input format: DXT blocks are
/// expanded, and BGR/BGRA channel orders are normalised to RGBA.
///
/// # Errors
///
/// Returns [`VtfError::Decode`] if the signature, header or pixel format is
/// invalid, unsupported or truncated. Never panics, however malformed the
/// input (see the proptest in this module).
pub fn decode(bytes: &[u8]) -> Result<DecodedTexture, VtfError> {
    // The upstream `vtf` crate indexes into the input slice without bounds
    // checks and *panics* on truncated or internally inconsistent files
    // (observed in its `image::get_frame`). rustlife treats all content as
    // untrusted, so we contain any such panic here and turn it into an ordinary
    // error; the caller then falls back to a placeholder texture. This is the
    // gap-fill the crate docs promise for malformed input.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode_inner(bytes))).unwrap_or_else(
        |_| {
            Err(VtfError::Decode(
                "decoder panicked on malformed data".to_owned(),
            ))
        },
    )
}

/// The actual decode, which may panic deep inside the `vtf` crate on bad input;
/// [`decode`] wraps it to guarantee the no-panic contract.
fn decode_inner(bytes: &[u8]) -> Result<DecodedTexture, VtfError> {
    let parsed = vtf::from_bytes(bytes).map_err(|error| VtfError::Decode(error.to_string()))?;
    let is_normal_map = parsed.header.flags & TEXTUREFLAGS_NORMAL != 0;

    // `decode` expands whatever the stored format is into an `image` crate
    // `DynamicImage`; `to_rgba8` then gives us a uniform RGBA8 buffer.
    let image = parsed
        .highres_image
        .decode(0)
        .map_err(|error| VtfError::Decode(error.to_string()))?;
    let rgba = image.to_rgba8();
    Ok(DecodedTexture {
        width: rgba.width(),
        height: rgba.height(),
        rgba8: rgba.into_raw(),
        is_normal_map,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Rgba, RgbaImage};
    use proptest::prelude::*;

    /// Silences the backtrace dump from panics that originate *inside* the `vtf`
    /// crate (which [`decode`] catches on purpose), while leaving panics from
    /// anywhere else — a genuine test failure — reported as usual. Keeps the
    /// "never panics" tests from spamming the test log.
    fn silence_vtf_panics() {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let default = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let from_vtf = info
                    .location()
                    .is_some_and(|loc| loc.file().contains("vtf-0.4.1"));
                if !from_vtf {
                    default(info);
                }
            }));
        });
    }

    /// Builds a VTF byte stream from an RGBA pattern, via the `vtf` crate, so
    /// the format tests need no Valve content on disk. Dimensions must be powers
    /// of two, which VTF requires.
    fn synthetic_vtf(width: u32, height: u32, format: vtf::ImageFormat) -> Vec<u8> {
        let image = RgbaImage::from_fn(width, height, |x, y| {
            Rgba([(x * 17) as u8, (y * 17) as u8, (x + y) as u8, 0xff])
        });
        vtf::create(DynamicImage::ImageRgba8(image), format).expect("encode synthetic VTF")
    }

    #[test]
    fn decodes_uncompressed_rgba_losslessly() {
        let bytes = synthetic_vtf(4, 4, vtf::ImageFormat::Rgba8888);
        let decoded = decode(&bytes).expect("decode");
        assert_eq!((decoded.width, decoded.height), (4, 4));
        assert_eq!(decoded.rgba8.len(), 4 * 4 * 4);
        // Pixel (1,1) in the generator is (17, 17, 2, 255); an uncompressed
        // format must round-trip it exactly. Index = (row * width + col) * 4.
        let (row, col, width) = (1usize, 1usize, 4usize);
        let index = (row * width + col) * 4;
        assert_eq!(&decoded.rgba8[index..index + 4], &[17, 17, 2, 0xff]);
    }

    /// Decodes a real texture (set `RUSTLIFE_TEST_VTF`) and checks it has real
    /// detail rather than collapsing to a flat colour, which would indicate a
    /// wrong mip or format being read.
    #[test]
    #[ignore = "requires a VTF file; set RUSTLIFE_TEST_VTF"]
    fn decodes_a_real_texture_with_detail() {
        let Ok(path) = std::env::var("RUSTLIFE_TEST_VTF") else {
            return;
        };
        let decoded = decode(&std::fs::read(&path).expect("read vtf")).expect("decode");
        let luma: Vec<f64> = decoded
            .rgba8
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| (f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])) / 3.0)
            .collect();
        let mean = luma.iter().sum::<f64>() / luma.len() as f64;
        let deviation =
            (luma.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / luma.len() as f64).sqrt();
        eprintln!(
            "{path}: {}x{}, mean luma {mean:.1}, std dev {deviation:.1}",
            decoded.width, decoded.height
        );
        assert!(deviation > 2.0, "texture should have visible detail");
    }

    /// A VTF 7.2 of DXT1 blocks, `width`×`height` with a full mip chain,
    /// each level's blocks filled with its level number: no thumbnail, so
    /// the data follows the 80-byte header, smallest level first.
    fn dxt1_vtf(width: u16, height: u16, mips: u8) -> Vec<u8> {
        let mut bytes = vec![0u8; 80];
        bytes[..4].copy_from_slice(b"VTF\0");
        bytes[4..8].copy_from_slice(&7u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&80u32.to_le_bytes());
        bytes[16..18].copy_from_slice(&width.to_le_bytes());
        bytes[18..20].copy_from_slice(&height.to_le_bytes());
        bytes[24..26].copy_from_slice(&1u16.to_le_bytes());
        bytes[52..56].copy_from_slice(&13i32.to_le_bytes());
        bytes[56] = mips;
        bytes[57..61].copy_from_slice(&(-1i32).to_le_bytes());
        bytes[63..65].copy_from_slice(&1u16.to_le_bytes());
        for level in (0..mips).rev() {
            let blocks = |side: u16| usize::from((side >> level).max(1)).div_ceil(4);
            bytes.extend(std::iter::repeat_n(
                level,
                blocks(width) * blocks(height) * 8,
            ));
        }
        bytes
    }

    #[test]
    fn dxt_textures_hand_out_their_stored_mips() {
        let texture = compressed(&dxt1_vtf(16, 8, 5)).expect("a DXT1 texture");
        assert_eq!(
            (texture.format, texture.width, texture.height),
            (BlockFormat::Dxt1, 16, 8)
        );
        let sizes: Vec<usize> = texture.mips.iter().map(Vec::len).collect();
        // 16×8, 8×4, then single blocks for 4×2, 2×1 and 1×1.
        assert_eq!(sizes, [4 * 2 * 8, 2 * 8, 8, 8, 8]);
        for (level, blocks) in texture.mips.iter().enumerate() {
            assert!(
                blocks.iter().all(|&byte| usize::from(byte) == level),
                "level {level} read in place"
            );
        }
        assert!(
            compressed(&dxt1_vtf(6, 8, 1)).is_none(),
            "not a multiple of the block size"
        );
        let truncated = dxt1_vtf(16, 8, 5);
        assert!(compressed(&truncated[..truncated.len() - 1]).is_none());
        assert!(
            compressed(&synthetic_vtf(4, 4, vtf::ImageFormat::Rgba8888)).is_none(),
            "uncompressed"
        );
        assert!(compressed(b"not a vtf").is_none());
    }

    #[test]
    fn rejects_non_vtf_bytes() {
        let error = decode(b"not a vtf at all").unwrap_err();
        assert!(matches!(error, VtfError::Decode(_)));
    }

    proptest! {
        /// No byte string, however short or malformed, may panic the decoder.
        #[test]
        fn never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            silence_vtf_panics();
            let _ = decode(&bytes);
        }

        /// Truncating a real VTF at any point must error, never panic — even
        /// though the underlying crate would panic on these inputs.
        #[test]
        fn never_panics_on_truncated_vtf(cut in 0usize..200) {
            silence_vtf_panics();
            let full = synthetic_vtf(4, 4, vtf::ImageFormat::Rgba8888);
            let end = cut.min(full.len());
            let _ = decode(&full[..end]);
            let dxt = dxt1_vtf(8, 8, 4);
            let _ = compressed(&dxt[..cut.min(dxt.len())]);
        }
    }
}
