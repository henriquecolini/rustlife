//! How each class of entity turns its `angles` into what is drawn and
//! simulated, as one table.
//!
//! Source's `angles` key is `pitch yaw roll` in degrees, composed as
//! `Rz(yaw) · Ry(pitch) · Rx(roll)`, so that a model's +X (its forward) ends up
//! along the entity's forward. Getting there takes a little more than that:
//!
//! | Class | Angles used | Model drawn from |
//! |-------|-------------|------------------|
//! | `weapon_*`, `item_*`, `item_*charger` | all three | its idle pose |
//! | `prop_physics`, `prop_dynamic` | all three | its idle pose |
//! | static props | all three | the vertices as stored |
//! | brush entities | all three | the map's own faces, relative to the origin |
//! | player spawns | yaw only | none: it faces the view |
//! | `info_teleport_destination` | pitch and yaw (players have no roll) | none |
//!
//! Vertices are stored in a model's rest pose, but the game draws a model as
//! the first frame of its first sequence places its bones, and the model
//! compiler turns the root bone there, a quarter turn about Z for most. Drawing
//! such a model at rest lays it a quarter turn off (`ebc3fa2`). `$staticprop`
//! models have no bones applied.

use bevy::prelude::*;
use rl_assets::coords;
use rl_data::GameData;
use rl_sim::collision::rotation_from_angles;
use rl_sim::pickups;
use rl_sim::weapons::Arsenal;
use source_formats::anim::{self, ModelAnimation};
use source_formats::entities::MapEntity;
use source_formats::props::{PropKind, PropPlacement};

/// Half of √2, a quaternion component of a quarter turn.
const H: f32 = std::f32::consts::FRAC_1_SQRT_2;
/// What the model compiler's idle pose does to most models' root bone: a
/// quarter turn clockwise about Z (`[x, y, z, w]`), taking a mesh authored
/// facing +Y to face +X.
const IDLE_ROOT: [f32; 4] = [0.0, 0.0, -H, H];
/// The way such a mesh faces at rest, Source space.
const AUTHORED_FORWARD: Vec3 = Vec3::Y;

/// A one-bone model, identity at rest, whose first frame holds [`IDLE_ROOT`].
fn compiled_model() -> ModelAnimation {
    ModelAnimation {
        bones: vec![anim::Bone {
            name: "root".into(),
            parent: None,
            position: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            euler: [0.0; 3],
            position_scale: [1.0; 3],
            rotation_scale: [1.0; 3],
        }],
        animations: vec![anim::Animation {
            name: "@idle".into(),
            fps: 30.0,
            frames: 1,
            looping: false,
            delta: false,
            tracks: vec![anim::BoneTrack {
                bone: 0,
                positions: vec![[0.0; 3]],
                rotations: vec![IDLE_ROOT],
            }],
        }],
        sequences: vec![anim::Sequence {
            label: "idle".into(),
            activity: String::new(),
            looping: false,
            delta: false,
            blends: vec![0],
            group: [1, 1],
            parameters: [None, None],
            ranges: [(0.0, 0.0); 2],
            fade: (0.0, 0.0),
            events: Vec::new(),
        }],
        ..Default::default()
    }
}

/// What a class does with an entity's angles, and what it starts from.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Path {
    /// A pickup: the simulation keeps the angles for the client to draw.
    Pickup,
    /// A model entity, in the idle pose of its bones.
    Prop,
    /// A static prop, its vertices as stored.
    Static,
    /// A brush entity, its faces in its own frame.
    Brush,
    /// A spawn: the player faces its yaw, level.
    Spawn,
    /// A teleport's end: the player's eyes take its pitch and yaw.
    TeleportEnd,
}

/// One entity of the table: what it says, and where the thing it places should
/// face, Source space.
struct Row {
    class: &'static str,
    path: Path,
    angles: [f32; 3],
    forward: Vec3,
}

const TABLE: &[Row] = &[
    Row {
        class: "weapon_shotgun",
        path: Path::Pickup,
        angles: [0.0, 90.0, -90.0],
        forward: Vec3::Y,
    },
    Row {
        class: "item_healthkit",
        path: Path::Pickup,
        angles: [0.0, 180.0, 0.0],
        forward: Vec3::NEG_X,
    },
    Row {
        class: "item_healthcharger",
        path: Path::Pickup,
        angles: [0.0, 270.0, 0.0],
        forward: Vec3::NEG_Y,
    },
    Row {
        class: "item_suitcharger",
        path: Path::Pickup,
        angles: [0.0, 0.0, 0.0],
        forward: Vec3::X,
    },
    Row {
        class: "prop_physics",
        path: Path::Prop,
        angles: [90.0, 0.0, 0.0],
        forward: Vec3::NEG_Z,
    },
    Row {
        class: "prop_dynamic",
        path: Path::Prop,
        angles: [0.0, 90.0, 0.0],
        forward: Vec3::Y,
    },
    Row {
        class: "prop_static",
        path: Path::Static,
        angles: [0.0, 90.0, 0.0],
        forward: Vec3::Y,
    },
    Row {
        class: "func_door",
        path: Path::Brush,
        angles: [0.0, 90.0, 0.0],
        forward: Vec3::Y,
    },
    Row {
        class: "info_player_deathmatch",
        path: Path::Spawn,
        // Pitch and roll say nothing to a spawn.
        angles: [45.0, 90.0, 30.0],
        forward: Vec3::Y,
    },
    Row {
        class: "info_teleport_destination",
        path: Path::TeleportEnd,
        // Roll says nothing to eyes.
        angles: [90.0, 90.0, 30.0],
        forward: Vec3::NEG_Z,
    },
];

/// The world rotation a [`Row`] draws, Bevy space, and the one the simulation
/// holds, Source space, of the row's *entity* (before any model pose).
#[expect(
    clippy::expect_used,
    reason = "test helper: a class that does not place a pickup must fail the test"
)]
fn turned(row: &Row) -> (Quat, Quat) {
    let entity_turn = |angles: [f32; 3]| {
        (
            coords::rotation(angles),
            Quat::from_mat3(&rotation_from_angles(angles)),
        )
    };
    match row.path {
        Path::Pickup => {
            let mut entity = MapEntity::new(row.class, [0.0; 3]);
            entity.angles = row.angles;
            let data = GameData::builtin().expect("built-in data");
            let arsenal = Arsenal::new(&data);
            let placed = pickups::from_entities(&[entity], &data, &arsenal);
            let pickup = placed.first().expect("the class is a pickup");
            entity_turn(pickup.angles)
        }
        Path::Prop | Path::Static => {
            let kind = if row.path == Path::Static {
                PropKind::Static
            } else {
                PropKind::Dynamic
            };
            let placement = PropPlacement::new("m.mdl".to_owned(), [0.0; 3], row.angles, kind);
            entity_turn(placement.angles)
        }
        Path::Brush => entity_turn(row.angles),
        Path::Spawn => {
            let turn = Quat::from_rotation_y(coords::yaw(row.angles[1]));
            // The view looks along Bevy's -Z when unturned; bring that to the
            // +X the rest of the table measures from.
            let from_x = Quat::from_rotation_arc(Vec3::X, Vec3::NEG_Z);
            (turn * from_x, entity_turn([0.0, row.angles[1], 0.0]).1)
        }
        Path::TeleportEnd => entity_turn([row.angles[0], row.angles[1], 0.0]),
    }
}

/// The direction the model's authored forward ends up in on `row`, Source
/// space, with the pose the class draws its model in.
fn model_forward(row: &Row, drawn: Quat) -> Option<Vec3> {
    let source = |bevy: Vec3| Vec3::from(coords::to_source_direction(bevy));
    match row.path {
        Path::Pickup | Path::Prop => {
            let idle = compiled_model().idle_pose()[0].1;
            let bone = Quat::from_array(idle);
            let in_entity = bone * AUTHORED_FORWARD;
            Some(source(drawn * coords::direction(in_entity.to_array())))
        }
        // No bones applied: forward is the stored mesh's +X.
        Path::Static | Path::Brush => Some(source(drawn * coords::direction([1.0, 0.0, 0.0]))),
        Path::Spawn | Path::TeleportEnd => None,
    }
}

#[test]
fn every_class_faces_where_its_angles_say() {
    let source = |bevy: Vec3| Vec3::from(coords::to_source_direction(bevy));
    for row in TABLE {
        let (drawn, simulated) = turned(row);
        let drawn_forward = source(drawn * Vec3::X);
        assert!(
            drawn_forward.abs_diff_eq(row.forward, 1e-5),
            "{}: drawn facing {drawn_forward}, not {}",
            row.class,
            row.forward
        );
        // The world's collision and what is drawn agree.
        let simulated_forward = simulated * Vec3::X;
        let flat = Vec3::new(row.forward.x, row.forward.y, 0.0);
        let expected = if row.path == Path::Spawn {
            flat
        } else {
            row.forward
        };
        assert!(
            simulated_forward.abs_diff_eq(expected, 1e-5),
            "{}: simulated facing {simulated_forward}",
            row.class
        );
        if let Some(model) = model_forward(row, drawn) {
            assert!(
                model.abs_diff_eq(row.forward, 1e-5),
                "{}: its model faces {model}, not {}",
                row.class,
                row.forward
            );
        }
    }
}

#[test]
fn drawn_at_rest_a_compiled_model_would_lie_a_quarter_turn_off() {
    // The bug `ebc3fa2` fixed: at rest, the mesh keeps its authored forward.
    let row = &TABLE[3];
    let (drawn, _) = turned(row);
    let at_rest = drawn * coords::direction(AUTHORED_FORWARD.to_array());
    let at_rest = Vec3::from(coords::to_source_direction(at_rest));
    assert!(at_rest.abs_diff_eq(Vec3::Y, 1e-5), "{at_rest}");
    assert!(
        at_rest.dot(row.forward).abs() < 1e-5,
        "square to its entity"
    );
}

#[test]
fn every_class_in_the_module_docs_has_a_row() {
    for path in [
        Path::Pickup,
        Path::Prop,
        Path::Static,
        Path::Brush,
        Path::Spawn,
        Path::TeleportEnd,
    ] {
        assert!(TABLE.iter().any(|row| row.path == path), "{path:?}");
    }
}
