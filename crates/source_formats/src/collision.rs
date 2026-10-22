//! The collision model of a Source BSP map: what players and traces collide
//! with.
//!
//! Source does not collide against the rendered faces. It collides against
//! the *brushes* the level was built from: convex volumes, each the
//! intersection of the half-spaces behind its sides' planes, tagged with
//! `CONTENTS_*` flags (solid, player clip, water, ladder, ...). The compiler
//! also adds *bevel* sides — extra axis-aligned and edge planes that do not
//! change a brush's shape, but make sweeping a box against it exact, since a
//! box swept against a convex volume only needs to be tested against the
//! volume's planes pushed out by the box. The BSP tree partitions space into
//! leaves, and each leaf lists the brushes that touch it, so a trace only
//! tests the brushes along its path.
//!
//! Displacements (terrain) are not brushes. They collide as their tessellated
//! triangles, which are extracted here per displacement with their bounds.
//! The brush a displacement was built on survives compilation as a solid
//! brush (its brush-side `dispinfo` field is not filled in by the compiler),
//! but Source collides displacements only through their surface: otherwise
//! the flat base face would sit as a lid over terrain that dips below it. Those
//! source brushes are found geometrically — a brush with a side on the
//! displacement's base face plane whose corners are the face's corners — and
//! made non-solid here.
//!
//! Brush entities (`func_door`, `func_brush`, ...) own a sub-model with its
//! own tree root inside the shared node and leaf arrays; its planes are in the
//! entity's local frame and are placed by the entity's origin and angles. Which
//! entity classes are solid is decided by the simulation, not here.
//!
//! The ladders of HL2-era maps are entities (`func_useableladder`) rather than
//! brushes: a segment between two mount points, with `info_ladder_dismount`
//! points nearby where a climber may step off. Both are extracted too.
//!
//! Indices stored here have been validated against the arrays they refer to
//! (anything out of range in the file is dropped or redirected during
//! extraction), but consumers should still index defensively: the types are
//! plain data and can be built by hand.

use vbsp::Bsp;

use crate::bsp::{BspError, parse_vector};
use crate::leaves;

/// A plane `normal · p = dist`, Source space. The "front" is the side the
/// normal points to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CollisionPlane {
    /// Unit normal.
    pub normal: [f32; 3],
    /// Distance of the plane from the origin along `normal`.
    pub dist: f32,
}

/// An interior node of the BSP tree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CollisionNode {
    /// The splitting plane.
    pub plane: CollisionPlane,
    /// Children in front of (`[0]`) and behind (`[1]`) the plane. A child
    /// `c >= 0` is a node index; a negative child is the leaf `!c`.
    pub children: [i32; 2],
}

/// A leaf of the BSP tree: a convex cell of space and the brushes touching it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollisionLeaf {
    /// `CONTENTS_*` flags of the cell.
    pub contents: u32,
    /// First entry of the leaf's range in [`MapCollision::leaf_brushes`].
    pub first_brush: u32,
    /// Number of entries in the range.
    pub brush_count: u32,
}

/// One side of a brush: the brush lies behind its plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CollisionSide {
    /// The side's plane, normal pointing out of the brush.
    pub plane: CollisionPlane,
    /// Whether the side is a bevel: added by the compiler for box traces, and
    /// ignored by point (ray) traces, whose hit normal would otherwise be the
    /// bevel's rather than the visible surface's.
    pub bevel: bool,
    /// The side's material, as an index into [`MapCollision::materials`]:
    /// what the surface looks and sounds like. `None` for a side without
    /// one (a bevel, or a malformed reference).
    pub material: Option<u16>,
}

/// A convex brush: the intersection of its sides' back half-spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollisionBrush {
    /// `CONTENTS_*` flags.
    pub contents: u32,
    /// First side in [`MapCollision::sides`].
    pub first_side: u32,
    /// Number of sides.
    pub side_count: u32,
}

/// The tessellated surface of one displacement, for collision.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplacementMesh {
    /// `CONTENTS_*` flags (normally `CONTENTS_SOLID`).
    pub contents: u32,
    /// The surface's material, as an index into
    /// [`MapCollision::materials`].
    pub material: Option<u16>,
    /// Bounds of every triangle, Source space.
    pub mins: [f32; 3],
    /// Bounds of every triangle, Source space.
    pub maxs: [f32; 3],
    /// The triangles, Source space.
    pub triangles: Vec<[[f32; 3]; 3]>,
}

/// A BSP model: the world (model 0) or a brush entity's geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubModel {
    /// Root of the model's tree, encoded like [`CollisionNode::children`].
    pub root: i32,
    /// Bounds of the model in its own frame.
    pub mins: [f32; 3],
    /// Bounds of the model in its own frame.
    pub maxs: [f32; 3],
}

/// A brush entity and the placement of its sub-model.
#[derive(Debug, Clone, PartialEq)]
pub struct BrushEntityCollision {
    /// The entity class, e.g. `func_door`.
    pub classname: String,
    /// Index into [`MapCollision::models`] (never 0, the world).
    pub model: usize,
    /// Entity origin, Source space.
    pub origin: [f32; 3],
    /// Entity orientation, `[pitch, yaw, roll]` in degrees.
    pub angles: [f32; 3],
    /// The entity's `Solidity` key (`func_brush`: 0 toggle, 1 never,
    /// 2 always), if present.
    pub solidity: Option<i32>,
    /// Whether the entity starts disabled (`StartDisabled 1`).
    pub start_disabled: bool,
    /// The entity's name, which map logic addresses it by, if it has one.
    pub targetname: Option<String>,
    /// The name of the entity it moves with, if it has one.
    pub parentname: Option<String>,
}

/// A climbable ladder (`func_useableladder`): the segment a climber's feet
/// move along.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LadderSegment {
    /// The lower mount point (a climber's feet at the bottom), Source space.
    pub bottom: [f32; 3],
    /// The upper mount point, Source space.
    pub top: [f32; 3],
    /// Whether the ladder starts disabled.
    pub start_disabled: bool,
}

/// Everything a map's occupants collide with or move along.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapCollision {
    /// Interior nodes of every model's tree.
    pub nodes: Vec<CollisionNode>,
    /// Leaves of every model's tree, in tree order.
    pub leaves: Vec<CollisionLeaf>,
    /// Brush indices, ranged into by [`CollisionLeaf`].
    pub leaf_brushes: Vec<u32>,
    /// Every brush.
    pub brushes: Vec<CollisionBrush>,
    /// Brush sides, ranged into by [`CollisionBrush`].
    pub sides: Vec<CollisionSide>,
    /// Material (texture) names, as the BSP's texture-info table indexes
    /// them; sides and displacements refer to these.
    pub materials: Vec<String>,
    /// Displacement surfaces (all belong to the world).
    pub displacements: Vec<DisplacementMesh>,
    /// BSP models; model 0 is the world.
    pub models: Vec<SubModel>,
    /// Brush entities that own a sub-model.
    pub brush_entities: Vec<BrushEntityCollision>,
    /// Ladders.
    pub ladders: Vec<LadderSegment>,
    /// Points where a climber may step off a ladder (`info_ladder_dismount`).
    pub ladder_dismounts: Vec<[f32; 3]>,
}

/// Reads the collision model of a BSP map.
///
/// # Errors
///
/// Returns [`BspError::Parse`] if the data is not a valid BSP or its leaf lump
/// cannot be read. Like the other decoders in this crate, it never panics on
/// malformed input.
pub fn load(bytes: &[u8]) -> Result<MapCollision, BspError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load_inner(bytes))).unwrap_or_else(
        |_| {
            Err(BspError::Parse(
                "BSP collision extraction panicked on malformed data".to_owned(),
            ))
        },
    )
}

fn load_inner(bytes: &[u8]) -> Result<MapCollision, BspError> {
    let bsp = Bsp::read(bytes).map_err(|error| BspError::Parse(error.to_string()))?;
    let records = leaves::read_all(bytes)
        .ok_or_else(|| BspError::Parse("unreadable leaf lump".to_owned()))?;
    let materials: Vec<String> = (0..bsp.textures_info.len())
        .map(|index| {
            bsp.texture_info(index)
                .map(|texture| texture.name().to_owned())
                .unwrap_or_default()
        })
        .collect();

    let (mut brushes, sides) = brushes(&bsp, materials.len());
    for brush in displacement_source_brushes(&bsp, &brushes, &sides) {
        brushes[brush].contents = 0;
    }
    let leaf_brushes: Vec<u32> = bsp
        .leaf_brushes
        .iter()
        .map(|entry| u32::from(entry.brush))
        .filter(|&brush| (brush as usize) < brushes.len())
        .collect();
    // Filtering can only shorten the list if the file is corrupt; ranges are
    // clamped to it either way.
    let leaves = records
        .iter()
        .map(|record| {
            let first = u32::from(record.first_brush).min(leaf_brushes.len() as u32);
            let count = u32::from(record.brush_count).min(leaf_brushes.len() as u32 - first);
            CollisionLeaf {
                contents: record.contents as u32,
                first_brush: first,
                brush_count: count,
            }
        })
        .collect::<Vec<_>>();
    let nodes = nodes(&bsp, leaves.len());
    let models = bsp
        .models
        .iter()
        .map(|model| SubModel {
            root: if usize::try_from(model.head_node).is_ok_and(|node| node < nodes.len()) {
                model.head_node
            } else {
                // A model without a valid tree collides with nothing: point
                // it at leaf 0, the map's outside leaf, which lists no brushes.
                !0
            },
            mins: [model.mins.x, model.mins.y, model.mins.z],
            maxs: [model.maxs.x, model.maxs.y, model.maxs.z],
        })
        .collect::<Vec<_>>();

    Ok(MapCollision {
        brush_entities: brush_entities(&bsp, models.len()),
        displacements: displacements(&bsp, materials.len()),
        ladders: ladders(&bsp),
        ladder_dismounts: entity_origins(&bsp, "info_ladder_dismount"),
        nodes,
        leaves,
        leaf_brushes,
        brushes,
        sides,
        materials,
        models,
    })
}

/// Every brush with its sides. A brush whose sides reference missing planes
/// keeps its place (leaf brush indices refer to it) but loses its contents,
/// so it collides with nothing.
fn brushes(bsp: &Bsp, material_count: usize) -> (Vec<CollisionBrush>, Vec<CollisionSide>) {
    let material = |texture_info: i16| {
        usize::try_from(texture_info)
            .ok()
            .filter(|&index| index < material_count)
            .and_then(|index| u16::try_from(index).ok())
    };
    let mut sides = Vec::with_capacity(bsp.brush_sides.len());
    let brushes = bsp
        .brushes
        .iter()
        .map(|brush| {
            let first = brush.brush_side as usize;
            let range = bsp
                .brush_sides
                .get(first..first.saturating_add(brush.num_brush_sides as usize));
            let planes = range.and_then(|range| {
                range
                    .iter()
                    .map(|side| {
                        let plane = bsp.planes.get(usize::from(side.plane))?;
                        Some(CollisionSide {
                            plane: CollisionPlane {
                                normal: [plane.normal.x, plane.normal.y, plane.normal.z],
                                dist: plane.dist,
                            },
                            // Newer compilers pack a `thin` flag into the
                            // high byte; only the low byte means "bevel".
                            bevel: side.bevel & 0xff != 0,
                            material: material(side.texture_info),
                        })
                    })
                    .collect::<Option<Vec<_>>>()
            });
            let first_side = sides.len() as u32;
            match planes {
                Some(planes) => {
                    let count = planes.len() as u32;
                    sides.extend(planes);
                    CollisionBrush {
                        contents: brush.flags.bits(),
                        first_side,
                        side_count: count,
                    }
                }
                None => CollisionBrush {
                    contents: 0,
                    first_side,
                    side_count: 0,
                },
            }
        })
        .collect();
    (brushes, sides)
}

/// Distance within which a face corner counts as lying on a brush side's plane
/// or inside the brush.
const ON_PLANE_EPSILON: f32 = 0.1;
/// Minimum dot product of two unit normals that count as the same direction.
const SAME_NORMAL_DOT: f32 = 0.999;

/// Indices of the brushes displacements were built on: each has a side lying
/// in a displacement's base face plane, and every corner of that face is one
/// of the brush's corners.
fn displacement_source_brushes(
    bsp: &Bsp,
    brushes: &[CollisionBrush],
    sides: &[CollisionSide],
) -> Vec<usize> {
    let brush_sides = |brush: &CollisionBrush| {
        let first = brush.first_side as usize;
        sides
            .get(first..first + brush.side_count as usize)
            .unwrap_or_default()
    };
    let mut found = Vec::new();
    for displacement in &bsp.displacements {
        let Some(face) = bsp.face(usize::from(displacement.map_face)) else {
            continue;
        };
        let normal = face.normal();
        let normal = [normal.x, normal.y, normal.z];
        let corners: Vec<[f32; 3]> = face
            .vertices()
            .map(|vertex| [vertex.position.x, vertex.position.y, vertex.position.z])
            .collect();
        let Some(&first_corner) = corners.first() else {
            continue;
        };
        let dist = dot(normal, first_corner);
        let source = brushes.iter().position(|brush| {
            let sides = brush_sides(brush);
            let has_face_side = sides.iter().any(|side| {
                dot(side.plane.normal, normal) > SAME_NORMAL_DOT
                    && (side.plane.dist - dist).abs() < ON_PLANE_EPSILON
            });
            // A displacement covers its brush's whole face, so every corner
            // of the face is a corner of the brush (on three of its planes).
            // A larger brush whose face merely contains the displacement's
            // is a different brush.
            has_face_side
                && corners.iter().all(|&corner| {
                    let mut on_planes = 0;
                    for side in sides {
                        let distance = dot(side.plane.normal, corner) - side.plane.dist;
                        if distance > ON_PLANE_EPSILON {
                            return false;
                        }
                        if distance.abs() <= ON_PLANE_EPSILON {
                            on_planes += 1;
                        }
                    }
                    on_planes >= 3
                })
        });
        if let Some(brush) = source {
            found.push(brush);
        }
    }
    found
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The tree's interior nodes. Children that point past the node or leaf
/// arrays are redirected to leaf 0 — in a compiled map the "outside" leaf,
/// which lists no brushes — so a corrupt reference collides with nothing
/// instead of indexing out of bounds.
fn nodes(bsp: &Bsp, leaf_count: usize) -> Vec<CollisionNode> {
    let node_count = bsp.nodes.len();
    bsp.nodes
        .iter()
        .map(|node| {
            let plane = usize::try_from(node.plane_index)
                .ok()
                .and_then(|index| bsp.planes.get(index));
            let plane = plane.map_or(
                CollisionPlane {
                    normal: [0.0, 0.0, 1.0],
                    dist: 0.0,
                },
                |plane| CollisionPlane {
                    normal: [plane.normal.x, plane.normal.y, plane.normal.z],
                    dist: plane.dist,
                },
            );
            let children = node.children.map(|child| {
                let valid = if child >= 0 {
                    (child as usize) < node_count
                } else {
                    ((!child) as usize) < leaf_count
                };
                if valid { child } else { !0 }
            });
            CollisionNode { plane, children }
        })
        .collect()
}

/// Each displacement's triangles, material and bounds.
fn displacements(bsp: &Bsp, material_count: usize) -> Vec<DisplacementMesh> {
    (0..bsp.displacements.len())
        .filter_map(|index| {
            let displacement = bsp.displacement(index)?;
            let material = bsp
                .faces
                .get(usize::from(displacement.map_face))
                .and_then(|face| usize::try_from(face.texture_info).ok())
                .filter(|&texture| texture < material_count)
                .and_then(|texture| u16::try_from(texture).ok());
            let vertices: Vec<[f32; 3]> = displacement
                .triangulated_displaced_vertices()
                .map(|vertex| [vertex.x, vertex.y, vertex.z])
                .collect();
            let triangles: Vec<[[f32; 3]; 3]> = vertices.as_chunks::<3>().0.to_vec();
            if triangles.is_empty() {
                return None;
            }
            let mut mins = [f32::INFINITY; 3];
            let mut maxs = [f32::NEG_INFINITY; 3];
            for vertex in &vertices {
                for axis in 0..3 {
                    mins[axis] = mins[axis].min(vertex[axis]);
                    maxs[axis] = maxs[axis].max(vertex[axis]);
                }
            }
            Some(DisplacementMesh {
                contents: displacement.contents as u32,
                material,
                mins,
                maxs,
                triangles,
            })
        })
        .collect()
}

/// Every entity that owns a sub-model (`model "*N"`, N ≥ 1).
fn brush_entities(bsp: &Bsp, model_count: usize) -> Vec<BrushEntityCollision> {
    bsp.entities
        .iter()
        .filter_map(|entity| {
            let model: usize = entity.prop("model")?.strip_prefix('*')?.parse().ok()?;
            if model == 0 || model >= model_count {
                return None;
            }
            let name = |key| {
                entity
                    .prop(key)
                    .filter(|name: &&str| !name.is_empty())
                    .map(str::to_owned)
            };
            Some(BrushEntityCollision {
                classname: entity.prop("classname").unwrap_or_default().to_owned(),
                model,
                origin: entity
                    .prop("origin")
                    .and_then(parse_vector)
                    .unwrap_or_default(),
                angles: entity
                    .prop("angles")
                    .and_then(parse_vector)
                    .unwrap_or_default(),
                solidity: entity.prop("Solidity").and_then(|value| value.parse().ok()),
                start_disabled: entity.prop("StartDisabled") == Some("1"),
                targetname: name("targetname"),
                parentname: name("parentname"),
            })
        })
        .collect()
}

/// Every `func_useableladder` with both mount points, ordered bottom to top.
fn ladders(bsp: &Bsp) -> Vec<LadderSegment> {
    bsp.entities
        .iter()
        .filter(|entity| entity.prop("classname") == Some("func_useableladder"))
        .filter_map(|entity| {
            let a = parse_vector(entity.prop("point0")?)?;
            let b = parse_vector(entity.prop("point1")?)?;
            let (bottom, top) = if a[2] <= b[2] { (a, b) } else { (b, a) };
            Some(LadderSegment {
                bottom,
                top,
                start_disabled: entity.prop("StartDisabled") == Some("1"),
            })
        })
        .collect()
}

/// Origins of every entity of `classname`.
fn entity_origins(bsp: &Bsp, classname: &str) -> Vec<[f32; 3]> {
    bsp.entities
        .iter()
        .filter(|entity| entity.prop("classname") == Some(classname))
        .filter_map(|entity| parse_vector(entity.prop("origin")?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_bsp_bytes_error_without_panicking() {
        assert!(load(b"not a bsp").is_err());
        assert!(load(&[]).is_err());
    }

    /// Checks the extracted model of a real map. Set `RUSTLIFE_TEST_BSP` to a
    /// `.bsp` path and run with `--ignored`.
    #[test]
    #[ignore = "requires a BSP file; set RUSTLIFE_TEST_BSP"]
    fn loads_real_map_collision() {
        let Ok(path) = std::env::var("RUSTLIFE_TEST_BSP") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("read bsp");
        let collision = load(&bytes).expect("extract collision");
        let bevels = collision.sides.iter().filter(|side| side.bevel).count();
        let solid = collision
            .brushes
            .iter()
            .filter(|brush| brush.contents & 1 != 0)
            .count();
        eprintln!(
            "{path}: {} nodes, {} leaves, {} brushes ({solid} solid), {} sides ({bevels} bevels), \
             {} displacements, {} brush entities, {} ladders, {} dismounts",
            collision.nodes.len(),
            collision.leaves.len(),
            collision.brushes.len(),
            collision.sides.len(),
            collision.displacements.len(),
            collision.brush_entities.len(),
            collision.ladders.len(),
            collision.ladder_dismounts.len(),
        );
        assert!(solid > 0, "a real map has solid brushes");
        assert!(bevels > 0, "compiled brushes carry bevel sides");
        assert!(!collision.models.is_empty());
        assert!(!collision.materials.is_empty(), "a real map names textures");
        assert!(collision.sides.iter().any(|side| side.material.is_some()));
        for side in &collision.sides {
            let [x, y, z] = side.plane.normal;
            assert!(((x * x + y * y + z * z).sqrt() - 1.0).abs() < 1e-3);
        }
        for leaf in &collision.leaves {
            assert!((leaf.first_brush + leaf.brush_count) as usize <= collision.leaf_brushes.len());
        }
    }
}
