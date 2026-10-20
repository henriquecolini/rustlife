//! Decoding Valve Texture Format (VTF) images into plain RGBA8 pixels.
//!
//! VTF stores textures in a handful of GPU and CPU formats (DXT1/3/5, BGRA8,
//! I8, ...), optionally with a mip chain, several animation frames and cubemap
//! faces. For the world viewer we only need frame 0 of the full-resolution
//! image decoded to straight, non-premultiplied RGBA8; block-compressed GPU
//! upload with the full mip chain is a later optimisation and does not change
//! this crate's interface.
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
        }
    }
}
