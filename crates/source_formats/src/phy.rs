//! Physics collision models (`.phy`).
//!
//! A model that can be simulated ships a `.phy` beside its `.mdl`: the
//! collision shape the physics engine uses, which is much simpler than the
//! rendered mesh, plus the physical properties the modeller gave it (mass,
//! surface material, damping). Static props use it too, for what players
//! and objects collide with.
//!
//! The file holds one *solid* per physics bone (props have one; ragdolls one
//! per limb), each an IVP *compact surface*: a set of convex hulls
//! ("ledges") made of triangles, indexed by a bounding-sphere tree. After the
//! solids comes a KeyValues text section describing each solid. The layout
//! is documented in `docs/formats/phy.md`.
//!
//! The decoder returns each solid's convex pieces in Source space (Z up,
//! inches, the model's own frame) and its text properties. Like every
//! decoder in this crate it never panics: every read is bounds-checked and
//! malformed input is an error.

use source_fs::keyvalues::{self, Block};
use thiserror::Error;

/// Size of the file header.
const HEADER_SIZE: usize = 16;
/// Size of the `VPHY` header that precedes each solid's compact surface in
/// files written by the 2006+ tools.
const VPHY_HEADER_SIZE: usize = 28;
/// The `VPHY` magic, little-endian.
const VPHY_MAGIC: &[u8; 4] = b"VPHY";
/// `VPHY` model type of an IVP compact surface (the alternative, MOPP, is
/// never written by the Source tools).
const MODEL_TYPE_COMPACT_SURFACE: i16 = 0;
/// Size of `IVP_Compact_Surface`.
const SURFACE_HEADER_SIZE: usize = 48;
/// Size of `IVP_Compact_Ledge`.
const LEDGE_HEADER_SIZE: usize = 16;
/// Size of `IVP_Compact_Triangle`.
const TRIANGLE_SIZE: usize = 16;
/// Size of `IVP_Compact_Poly_Point`: x, y, z and a padding float.
const POINT_SIZE: usize = 16;
/// Size of `IVP_Compact_Ledgetree_Node`.
const TREE_NODE_SIZE: usize = 28;

/// IVP works in metres; Source in inches.
const INCHES_PER_METRE: f32 = 1.0 / 0.0254;

/// Most solids accepted: far more than any ragdoll has bones.
const MAX_SOLIDS: usize = 256;
/// Most tree nodes walked per solid, which also bounds corrupt (cyclic)
/// trees.
const MAX_TREE_NODES: usize = 1 << 16;

/// Errors from [`decode`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PhyError {
    /// A structure extends past the end of the file.
    #[error("truncated .phy: {0} lies outside the file")]
    Truncated(&'static str),
    /// The file is structurally invalid.
    #[error("invalid .phy: {0}")]
    Invalid(String),
}

/// A convex piece of a solid: a closed triangle mesh enclosing a convex
/// volume.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvexPiece {
    /// Corner points, Source space, model frame.
    pub vertices: Vec<[f32; 3]>,
    /// Triangles as indices into [`vertices`](Self::vertices). The winding
    /// is not normalised; derive outward normals from the piece's centre.
    pub triangles: Vec<[u16; 3]>,
}

/// One solid of a physics model: the shape of one physics bone.
#[derive(Debug, Clone, PartialEq)]
pub struct PhysicsSolid {
    /// The convex pieces whose union is the solid.
    pub pieces: Vec<ConvexPiece>,
    /// Centre of mass computed by the compiler, Source space.
    pub mass_center: [f32; 3],
    /// Properties from the text section.
    pub params: SolidParams,
}

/// The text properties of a solid.
#[derive(Debug, Clone, PartialEq)]
pub struct SolidParams {
    /// Mass, kilograms.
    pub mass: f32,
    /// Surface material (`$surfaceprop`), e.g. `metal_barrel`; empty if
    /// unset.
    pub surface_prop: String,
    /// Linear damping (fraction of speed lost per second).
    pub damping: f32,
    /// Angular damping.
    pub rot_damping: f32,
    /// Scale applied to the computed inertia.
    pub inertia: f32,
}

impl Default for SolidParams {
    fn default() -> Self {
        Self {
            mass: 1.0,
            surface_prop: String::new(),
            damping: 0.0,
            rot_damping: 0.0,
            inertia: 1.0,
        }
    }
}

/// A decoded `.phy` file.
#[derive(Debug, Clone, PartialEq)]
pub struct PhysicsModel {
    /// The model checksum the file was compiled for (it matches the
    /// `.mdl`'s).
    pub checksum: u32,
    /// The solids, in file order (solid `i` has text `index` `i`).
    pub solids: Vec<PhysicsSolid>,
    /// Total mass from the `editparams` block, kilograms, when given.
    pub total_mass: Option<f32>,
    /// The pieces the model breaks into (`break` lines of the text), each
    /// a model drawn in this model's own frame, so spawning it at the
    /// broken model's origin puts it where it belongs.
    pub breaks: Vec<BreakPiece>,
}

/// One of the models a breakable model breaks into.
#[derive(Debug, Clone, PartialEq)]
pub struct BreakPiece {
    /// The piece's model path, lower case with a `models/` prefix and
    /// `.mdl` suffix (`models/props_junk/wood_crate001a_chunk01.mdl`).
    pub model: String,
    /// Seconds it lingers before fading away (`fadetime`); `0` if unsaid.
    pub fade_time: f32,
    /// How hard it is thrown out (`burst`); `0` if unsaid.
    pub burst: f32,
}

impl PhysicsModel {
    /// The model's mass: the declared total, else the sum of its solids.
    #[must_use]
    pub fn mass(&self) -> f32 {
        self.total_mass
            .unwrap_or_else(|| self.solids.iter().map(|solid| solid.params.mass).sum())
    }

    /// Axis-aligned bounds of every piece of every solid, or `None` if the
    /// model has no vertices.
    #[must_use]
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut vertices = self
            .solids
            .iter()
            .flat_map(|solid| &solid.pieces)
            .flat_map(|piece| &piece.vertices);
        let first = *vertices.next()?;
        Some(vertices.fold((first, first), |(min, max), v| {
            (
                [min[0].min(v[0]), min[1].min(v[1]), min[2].min(v[2])],
                [max[0].max(v[0]), max[1].max(v[1]), max[2].max(v[2])],
            )
        }))
    }
}

/// Decodes a `.phy` file.
///
/// # Errors
///
/// Returns a [`PhyError`] if the file is truncated or structurally invalid.
/// A solid whose shape cannot be read fails the whole file: a model missing
/// part of its collision would behave wrongly in subtle ways, whereas a
/// failed load falls back to the model's bounding box.
pub fn decode(bytes: &[u8]) -> Result<PhysicsModel, PhyError> {
    let file = Bytes(bytes);
    let header_size = file.u32(0, "file header")? as usize;
    let solid_count = file.u32(8, "file header")? as usize;
    let checksum = file.u32(12, "file header")?;
    if header_size < HEADER_SIZE {
        return Err(PhyError::Invalid(format!("header size {header_size}")));
    }
    if solid_count > MAX_SOLIDS {
        return Err(PhyError::Invalid(format!("{solid_count} solids")));
    }

    let mut shapes = Vec::with_capacity(solid_count);
    let mut at = header_size;
    for _ in 0..solid_count {
        let size = file.u32(at, "solid size")? as usize;
        let start = at + 4;
        let data = start
            .checked_add(size)
            .and_then(|end| bytes.get(start..end))
            .ok_or(PhyError::Truncated("solid"))?;
        shapes.push(solid_shape(data)?);
        at = start + size;
    }

    let text = bytes.get(at..).unwrap_or_default();
    let (params, total_mass, breaks) = text_params(text, solid_count);
    let solids = shapes
        .into_iter()
        .zip(params)
        .map(|((pieces, mass_center), params)| PhysicsSolid {
            pieces,
            mass_center,
            params,
        })
        .collect();
    Ok(PhysicsModel {
        checksum,
        solids,
        total_mass,
        breaks,
    })
}

/// The convex pieces and mass centre of one solid's data.
fn solid_shape(data: &[u8]) -> Result<(Vec<ConvexPiece>, [f32; 3]), PhyError> {
    // Files from the 2006+ tools wrap the surface in a small header; older
    // ones store the IVP surface directly.
    let surface = if data.get(..4) == Some(VPHY_MAGIC) {
        let model_type = Bytes(data).i16(6, "VPHY header")?;
        if model_type != MODEL_TYPE_COMPACT_SURFACE {
            return Err(PhyError::Invalid(format!(
                "unsupported collision model type {model_type}"
            )));
        }
        data.get(VPHY_HEADER_SIZE..)
            .ok_or(PhyError::Truncated("VPHY header"))?
    } else {
        data
    };
    let surface = Bytes(surface);
    let mass_center = surface.point(0, "surface header")?;
    let tree_root = surface.i32(32, "surface header")?;
    surface.require(SURFACE_HEADER_SIZE, "surface header")?;

    let ledges = if tree_root == 0 {
        // No tree: the surface is one ledge right after the header.
        vec![SURFACE_HEADER_SIZE]
    } else {
        tree_ledges(&surface, offset(0, tree_root, "ledge tree")?)?
    };
    let pieces = ledges
        .into_iter()
        .map(|ledge| read_ledge(&surface, ledge))
        .collect::<Result<Vec<_>, _>>()?;
    if pieces.is_empty() {
        return Err(PhyError::Invalid("solid without ledges".to_owned()));
    }
    Ok((pieces, mass_center))
}

/// The offsets of the ledges at the leaves of the ledge tree rooted at
/// `root`. Inner nodes also point at a ledge — a hull around everything
/// below them, for coarse tests — which is not part of the shape.
fn tree_ledges(surface: &Bytes<'_>, root: usize) -> Result<Vec<usize>, PhyError> {
    let mut ledges = Vec::new();
    let mut stack = vec![root];
    let mut visited = 0;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_TREE_NODES {
            return Err(PhyError::Invalid("ledge tree too large".to_owned()));
        }
        surface.require(node + TREE_NODE_SIZE, "ledge tree node")?;
        let right = surface.i32(node, "ledge tree node")?;
        if right == 0 {
            let ledge = surface.i32(node + 4, "ledge tree node")?;
            ledges.push(offset(node, ledge, "ledge")?);
        } else {
            // The left child immediately follows its parent.
            stack.push(offset(node, right, "ledge tree node")?);
            stack.push(node + TREE_NODE_SIZE);
        }
    }
    // Pushing right before left visits left first; keep file order anyway.
    ledges.sort_unstable();
    ledges.dedup();
    Ok(ledges)
}

/// Reads the ledge at `ledge`: its triangles and the points they use.
fn read_ledge(surface: &Bytes<'_>, ledge: usize) -> Result<ConvexPiece, PhyError> {
    let points = offset(ledge, surface.i32(ledge, "ledge")?, "ledge points")?;
    let triangle_count = surface.i16(ledge + 12, "ledge")?;
    let triangle_count = usize::try_from(triangle_count)
        .ok()
        .filter(|&count| count >= 4)
        .ok_or_else(|| PhyError::Invalid(format!("ledge with {triangle_count} triangles")))?;

    // Points are shared by every ledge of the surface; remap the ones this
    // ledge uses to a compact list of its own.
    let mut remap: Vec<(u16, u16)> = Vec::new();
    let mut vertices = Vec::new();
    let mut triangles = Vec::with_capacity(triangle_count);
    for triangle in 0..triangle_count {
        let at = ledge + LEDGE_HEADER_SIZE + triangle * TRIANGLE_SIZE;
        let mut corners = [0u16; 3];
        for (edge, corner) in corners.iter_mut().enumerate() {
            let point = (surface.u32(at + 4 + edge * 4, "ledge triangle")? & 0xFFFF) as u16;
            if let Some(&(_, local)) = remap.iter().find(|(file, _)| *file == point) {
                *corner = local;
                continue;
            }
            let local = u16::try_from(vertices.len())
                .map_err(|_| PhyError::Invalid("ledge with too many points".to_owned()))?;
            vertices.push(surface.point(points + usize::from(point) * POINT_SIZE, "ledge point")?);
            remap.push((point, local));
            *corner = local;
        }
        triangles.push(corners);
    }
    Ok(ConvexPiece {
        vertices,
        triangles,
    })
}

/// `base + relative`, rejecting results before the start of the data.
fn offset(base: usize, relative: i32, what: &'static str) -> Result<usize, PhyError> {
    base.checked_add_signed(relative as isize)
        .ok_or(PhyError::Truncated(what))
}

/// Each solid's text parameters (defaults for any not described) and the
/// declared total mass. The text section is best-effort: models with a
/// broken one still collide, with default properties.
fn text_params(
    text: &[u8],
    solid_count: usize,
) -> (Vec<SolidParams>, Option<f32>, Vec<BreakPiece>) {
    let mut params = vec![SolidParams::default(); solid_count];
    let text = String::from_utf8_lossy(text);
    let text = text.trim_end_matches('\0');
    let Ok(document) = keyvalues::parse(text) else {
        tracing::debug!("unreadable .phy text section; using default properties");
        return (params, None, Vec::new());
    };
    for pair in document.root.iter() {
        let Some(block) = pair.value.as_block() else {
            continue;
        };
        if !pair.key.eq_ignore_ascii_case("solid") {
            continue;
        }
        let index = block
            .get_str("index")
            .and_then(|index| index.trim().parse::<usize>().ok());
        if let Some(slot) = index.and_then(|index| params.get_mut(index)) {
            *slot = solid_params(block);
        }
    }
    let total_mass = document
        .root
        .get_block("editparams")
        .and_then(|block| number(block, "totalmass"))
        .filter(|mass| *mass > 0.0);
    let breaks = document
        .root
        .iter()
        .filter(|pair| pair.key.eq_ignore_ascii_case("editparams"))
        .filter_map(|pair| pair.value.as_block())
        .chain(std::iter::once(&document.root))
        .flat_map(Block::iter)
        .filter(|pair| pair.key.eq_ignore_ascii_case("break"))
        .filter_map(|pair| break_piece(pair.value.as_block()?))
        .collect();
    (params, total_mass, breaks)
}

/// A `break` line: `"model" "props_junk/crate_chunk01" "fadetime" "20"`.
fn break_piece(block: &Block) -> Option<BreakPiece> {
    let name = block
        .get_str("model")?
        .trim()
        .replace('\\', "/")
        .to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let name = name.strip_prefix("models/").unwrap_or(&name);
    let name = name.strip_suffix(".mdl").unwrap_or(name);
    Some(BreakPiece {
        model: format!("models/{name}.mdl"),
        fade_time: number(block, "fadetime").unwrap_or(0.0).max(0.0),
        burst: number(block, "burst").unwrap_or(0.0).max(0.0),
    })
}

fn solid_params(block: &Block) -> SolidParams {
    let defaults = SolidParams::default();
    SolidParams {
        mass: number(block, "mass")
            .filter(|mass| *mass > 0.0)
            .unwrap_or(defaults.mass),
        surface_prop: block
            .get_str("surfaceprop")
            .map(|name| name.trim().to_ascii_lowercase())
            .unwrap_or_default(),
        damping: number(block, "damping").unwrap_or(defaults.damping),
        rot_damping: number(block, "rotdamping").unwrap_or(defaults.rot_damping),
        inertia: number(block, "inertia")
            .filter(|inertia| *inertia > 0.0)
            .unwrap_or(defaults.inertia),
    }
}

/// A finite number under `key`.
fn number(block: &Block, key: &str) -> Option<f32> {
    block
        .get_str(key)?
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

/// IVP space (metres, Y down) to Source space (inches, Z up).
fn ivp_to_source([x, y, z]: [f32; 3]) -> [f32; 3] {
    [x, z, -y].map(|value| value * INCHES_PER_METRE)
}

/// Bounds-checked little-endian reads.
struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn array<const N: usize>(&self, at: usize, what: &'static str) -> Result<[u8; N], PhyError> {
        at.checked_add(N)
            .and_then(|end| self.0.get(at..end))
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(PhyError::Truncated(what))
    }

    fn require(&self, end: usize, what: &'static str) -> Result<(), PhyError> {
        if end <= self.0.len() {
            Ok(())
        } else {
            Err(PhyError::Truncated(what))
        }
    }

    fn u32(&self, at: usize, what: &'static str) -> Result<u32, PhyError> {
        self.array(at, what).map(u32::from_le_bytes)
    }

    fn i32(&self, at: usize, what: &'static str) -> Result<i32, PhyError> {
        self.array(at, what).map(i32::from_le_bytes)
    }

    fn i16(&self, at: usize, what: &'static str) -> Result<i16, PhyError> {
        self.array(at, what).map(i16::from_le_bytes)
    }

    fn f32(&self, at: usize, what: &'static str) -> Result<f32, PhyError> {
        let value = self.array(at, what).map(f32::from_le_bytes)?;
        if value.is_finite() {
            Ok(value)
        } else {
            Err(PhyError::Invalid(format!("non-finite value in {what}")))
        }
    }

    /// An IVP point at `at`, converted to Source space.
    fn point(&self, at: usize, what: &'static str) -> Result<[f32; 3], PhyError> {
        Ok(ivp_to_source([
            self.f32(at, what)?,
            self.f32(at + 4, what)?,
            self.f32(at + 8, what)?,
        ]))
    }
}

/// Builders for synthetic `.phy` files, shared with other crates' tests.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use super::{
        INCHES_PER_METRE, LEDGE_HEADER_SIZE, POINT_SIZE, SURFACE_HEADER_SIZE, TREE_NODE_SIZE,
    };

    /// The 12 triangles of a box over corners indexed `x | y << 1 | z << 2`.
    const BOX_TRIANGLES: [[u16; 3]; 12] = [
        [0, 2, 1],
        [1, 2, 3],
        [4, 5, 6],
        [5, 7, 6],
        [0, 1, 4],
        [1, 5, 4],
        [2, 6, 3],
        [3, 6, 7],
        [0, 4, 2],
        [2, 4, 6],
        [1, 3, 5],
        [3, 7, 5],
    ];

    /// A `.phy` with one solid made of one axis-aligned box per entry of
    /// `boxes` (Source-space `(mins, maxs)`), and a text section declaring
    /// `mass` and `surface_prop`.
    #[must_use]
    pub fn boxes_phy(boxes: &[([f32; 3], [f32; 3])], mass: f32, surface_prop: &str) -> Vec<u8> {
        let surface = compact_surface(boxes);
        let mut solid = Vec::new();
        solid.extend_from_slice(b"VPHY");
        solid.extend_from_slice(&0x100i16.to_le_bytes());
        solid.extend_from_slice(&0i16.to_le_bytes());
        solid.extend_from_slice(&(surface.len() as i32).to_le_bytes());
        solid.extend_from_slice(&[0; 16]);
        solid.extend_from_slice(&surface);

        let mut file = Vec::new();
        for word in [16u32, 0, 1, 0xC0FF_EE00] {
            file.extend_from_slice(&word.to_le_bytes());
        }
        file.extend_from_slice(&(solid.len() as u32).to_le_bytes());
        file.extend_from_slice(&solid);
        file.extend_from_slice(
            format!(
                "solid {{\n\"index\" \"0\"\n\"name\" \"test\"\n\"mass\" \"{mass}\"\n\
                 \"surfaceprop\" \"{surface_prop}\"\n\"damping\" \"0.1\"\n}}\n\
                 editparams {{\n\"totalmass\" \"{mass}\"\n}}\n\0"
            )
            .as_bytes(),
        );
        file
    }

    /// An IVP compact surface: header, one ledge per box, the shared point
    /// array, then a ledge tree whose leaves are the boxes.
    fn compact_surface(boxes: &[([f32; 3], [f32; 3])]) -> Vec<u8> {
        let ledge_size = LEDGE_HEADER_SIZE + BOX_TRIANGLES.len() * 16;
        let points_at = SURFACE_HEADER_SIZE + boxes.len() * ledge_size;
        let tree_at = points_at + boxes.len() * 8 * POINT_SIZE;

        let mut surface = vec![0u8; SURFACE_HEADER_SIZE];
        surface[32..36].copy_from_slice(&(tree_at as i32).to_le_bytes());
        surface[44..48].copy_from_slice(b"IVPS");
        for index in 0..boxes.len() {
            let ledge_at = SURFACE_HEADER_SIZE + index * ledge_size;
            let first_point = (index * 8) as u32;
            surface.extend_from_slice(&((points_at - ledge_at) as i32).to_le_bytes());
            surface.extend_from_slice(&0i32.to_le_bytes());
            surface.extend_from_slice(&(((ledge_size / 16) as u32) << 8).to_le_bytes());
            surface.extend_from_slice(&(BOX_TRIANGLES.len() as i16).to_le_bytes());
            surface.extend_from_slice(&0i16.to_le_bytes());
            for triangle in BOX_TRIANGLES {
                surface.extend_from_slice(&0u32.to_le_bytes());
                for corner in triangle {
                    surface.extend_from_slice(&(first_point + u32::from(corner)).to_le_bytes());
                }
            }
        }
        for (mins, maxs) in boxes {
            for corner in 0..8 {
                let pick = |axis: usize| {
                    if corner >> axis & 1 == 1 {
                        maxs[axis]
                    } else {
                        mins[axis]
                    }
                };
                // Source (x, y, z) inches back to IVP (x, -z, y) metres.
                let ivp = [pick(0), -pick(2), pick(1)].map(|v| v / INCHES_PER_METRE);
                for value in ivp.into_iter().chain([0.0]) {
                    surface.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        tree(&mut surface, boxes.len(), ledge_size);
        surface
    }

    /// Appends a balanced ledge tree over ledges `0..count`.
    fn tree(surface: &mut Vec<u8>, count: usize, ledge_size: usize) {
        fn node(surface: &mut Vec<u8>, first: usize, count: usize, ledge_size: usize) {
            let at = surface.len();
            surface.extend_from_slice(&[0; TREE_NODE_SIZE]);
            if count == 1 {
                let ledge = SURFACE_HEADER_SIZE + first * ledge_size;
                let relative = ledge as i32 - at as i32;
                surface[at + 4..at + 8].copy_from_slice(&relative.to_le_bytes());
            } else {
                let half = count / 2;
                node(surface, first, half, ledge_size);
                let right = surface.len() - at;
                surface[at..at + 4].copy_from_slice(&(right as i32).to_le_bytes());
                node(surface, first + half, count - half, ledge_size);
            }
        }
        node(surface, 0, count, ledge_size);
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::testing::boxes_phy;
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-3)
    }

    #[test]
    fn decodes_a_box_with_its_properties() {
        let phy = boxes_phy(&[([-8.0, -4.0, 0.0], [8.0, 4.0, 30.0])], 25.0, "Wood_Crate");
        let model = decode(&phy).expect("decode");
        assert_eq!(model.checksum, 0xC0FF_EE00);
        assert_eq!(model.solids.len(), 1);
        let solid = &model.solids[0];
        assert_eq!(solid.pieces.len(), 1);
        assert_eq!(solid.pieces[0].vertices.len(), 8);
        assert_eq!(solid.pieces[0].triangles.len(), 12);
        let (min, max) = model.bounds().expect("bounds");
        assert!(close(min, [-8.0, -4.0, 0.0]), "{min:?}");
        assert!(close(max, [8.0, 4.0, 30.0]), "{max:?}");
        assert_eq!(solid.params.mass, 25.0);
        assert_eq!(solid.params.surface_prop, "wood_crate");
        assert!((solid.params.damping - 0.1).abs() < 1e-6);
        assert_eq!(model.mass(), 25.0);
    }

    #[test]
    fn walks_every_leaf_of_the_ledge_tree() {
        let boxes: Vec<_> = (0..5)
            .map(|i| {
                let x = i as f32 * 20.0;
                ([x, 0.0, 0.0], [x + 10.0, 10.0, 10.0])
            })
            .collect();
        let model = decode(&boxes_phy(&boxes, 10.0, "metal")).expect("decode");
        let pieces = &model.solids[0].pieces;
        assert_eq!(pieces.len(), 5);
        for (piece, (mins, _)) in pieces.iter().zip(&boxes) {
            let min_x = piece
                .vertices
                .iter()
                .map(|v| v[0])
                .fold(f32::INFINITY, f32::min);
            assert!((min_x - mins[0]).abs() < 1e-3, "pieces stay in file order");
        }
    }

    #[test]
    fn converts_ivp_axes_to_source() {
        // IVP's Y points down, Z forward; one metre is 39.37 inches.
        let source = ivp_to_source([1.0, -2.0, 3.0]);
        assert!(
            close(source, [39.370_08, 118.110_24, 78.740_16]),
            "{source:?}"
        );
    }

    #[test]
    fn missing_text_falls_back_to_defaults() {
        let mut phy = boxes_phy(&[([0.0; 3], [1.0; 3])], 5.0, "metal");
        let text = phy
            .windows(5)
            .position(|window| window == b"solid")
            .expect("text section");
        phy.truncate(text);
        let model = decode(&phy).expect("decode");
        assert_eq!(model.solids[0].params, SolidParams::default());
        assert_eq!(model.total_mass, None);
        assert_eq!(model.mass(), 1.0);
    }

    #[test]
    fn rejects_bad_files() {
        assert_eq!(decode(b""), Err(PhyError::Truncated("file header")));
        let mut phy = boxes_phy(&[([0.0; 3], [1.0; 3])], 5.0, "metal");
        phy[8..12].copy_from_slice(&1000u32.to_le_bytes());
        assert!(matches!(decode(&phy), Err(PhyError::Invalid(_))));
        let mut phy = boxes_phy(&[([0.0; 3], [1.0; 3])], 5.0, "metal");
        // Model type 1 (MOPP).
        phy[26..28].copy_from_slice(&1i16.to_le_bytes());
        assert!(matches!(decode(&phy), Err(PhyError::Invalid(_))));
    }

    proptest! {
        /// No byte string may panic the decoder.
        #[test]
        fn never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
            let _ = decode(&bytes);
        }

        /// Truncated or corrupted files error rather than panic.
        #[test]
        fn never_panics_on_damaged_files(cut in 0usize..1200, flip in 0usize..1200, value: u8) {
            let mut phy = boxes_phy(&[([0.0; 3], [4.0; 3]), ([8.0; 3], [9.0; 3])], 5.0, "metal");
            if let Some(byte) = phy.get_mut(flip) {
                *byte = value;
            }
            let end = cut.min(phy.len());
            let _ = decode(&phy[..end]);
        }
    }

    /// Decodes a real `.phy`. Set `RUSTLIFE_TEST_PHY` to its path.
    #[test]
    #[ignore = "requires a .phy file; set RUSTLIFE_TEST_PHY"]
    fn decodes_a_real_model() {
        let Ok(path) = std::env::var("RUSTLIFE_TEST_PHY") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("read phy");
        let model = decode(&bytes).expect("decode");
        let pieces: usize = model.solids.iter().map(|solid| solid.pieces.len()).sum();
        eprintln!(
            "{path}: {} solids, {pieces} pieces, {} kg, bounds {:?}, surfaceprop {:?}",
            model.solids.len(),
            model.mass(),
            model.bounds(),
            model.solids[0].params.surface_prop,
        );
        assert!(pieces > 0 && model.mass() > 0.0);
    }

    #[test]
    fn break_lines_name_the_pieces_a_model_breaks_into() {
        let text = br#"solid { "index" "0" "mass" "30" }
            editparams {
                "totalmass" "30"
                break {"model" "props_junk/Crate_Chunk01" "fadetime" "20"  }
                break {"model" "models\props_c17\chunk.mdl" "burst" "100" "fadetime" "10" }
                break {"fadetime" "5"}
            }"#;
        let (_, mass, breaks) = text_params(text, 1);
        assert_eq!(mass, Some(30.0));
        assert_eq!(
            breaks,
            [
                BreakPiece {
                    model: "models/props_junk/crate_chunk01.mdl".to_owned(),
                    fade_time: 20.0,
                    burst: 0.0,
                },
                BreakPiece {
                    model: "models/props_c17/chunk.mdl".to_owned(),
                    fade_time: 10.0,
                    burst: 100.0,
                },
            ],
            "a break without a model is skipped"
        );
    }
}
