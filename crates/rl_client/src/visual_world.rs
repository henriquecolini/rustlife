//! Static geometry for the client's visual-only Box3D worlds.
//!
//! Ragdolls and gibs are simulated on the client with Box3D, but never touch
//! the server's authoritative prop world. Each gets a small world of its own
//! and only the map's solid geometry: brushes as convex hulls and
//! displacements as a triangle mesh, in Source units.

use b3::body::create_body;
use b3::math_functions::Vec3 as B3Vec3;
use b3::mesh::{MeshDef, create_mesh};
use b3::shape::{create_hull_shape, create_mesh_shape};
use b3::types::{BodyType, Filter, ShapeDef};
use b3::world::{World, world_set_gravity};
use bevy::prelude::Vec3;
use box3d_rust as b3;
use rl_sim::collision::CollisionWorld;
use rl_sim::collision::contents::MASK_PLAYER_SOLID;
use rl_sim::physics::{UNITS_PER_METRE, use_source_units};
use rl_sim::simulation::Simulation;

/// The map.
pub(crate) const WORLD_GROUP: u64 = 1 << 0;
/// Ragdoll bones.
pub(crate) const RAGDOLL_GROUP: u64 = 1 << 1;
/// Gibs and other debris.
pub(crate) const DEBRIS_GROUP: u64 = 1 << 2;

/// Gravity in Source units/s², the same as movement.
pub(crate) const GRAVITY: f32 = -9.81 * UNITS_PER_METRE;

/// A collision filter with the given category and mask.
pub(crate) fn filter(category: u64, mask: u64) -> Filter {
    Filter {
        category_bits: category,
        mask_bits: mask,
        group_index: 0,
    }
}

/// A world holding the map's static geometry, colliding only with bodies in
/// `mask`.
pub(crate) fn build_static_world(sim: &Simulation, mask: u64) -> World {
    use_source_units();
    let mut def = b3::default_world_def();
    def.gravity = b3::Vec3 {
        x: 0.0,
        y: 0.0,
        z: GRAVITY,
    };
    let mut world = World::new(&def);
    world_set_gravity(&mut world, def.gravity);

    let mut body_def = b3::default_body_def();
    body_def.type_ = BodyType::Static;
    body_def.user_data = u64::MAX;
    let body = create_body(&mut world, &body_def);

    let mut shape_def = b3::default_shape_def();
    shape_def.density = 0.0;
    shape_def.update_body_mass = false;
    shape_def.base_material.friction = 0.8;
    shape_def.base_material.restitution = 0.05;
    shape_def.filter = filter(WORLD_GROUP, mask);

    add_map_geometry(&mut world, body, &shape_def, sim.world());
    world
}

fn add_map_geometry(world: &mut World, body: b3::BodyId, shape: &ShapeDef, map: &CollisionWorld) {
    for solid in map.convex_solids(MASK_PLAYER_SOLID) {
        let points: Vec<B3Vec3> = solid.iter().map(|point| b3v(Vec3::from(*point))).collect();
        if let Some(hull) = b3::hull::create_hull(&points, b3::constants::MAX_HULL_VERTICES) {
            create_hull_shape(world, body, shape, &hull);
        }
    }

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for triangles in map.displacement_triangles(MASK_PLAYER_SOLID) {
        for triangle in triangles {
            let first = i32::try_from(vertices.len()).unwrap_or(i32::MAX);
            for point in triangle {
                vertices.push(b3v(Vec3::from(*point)));
            }
            indices.extend([first, first + 1, first + 2]);
        }
    }
    if !vertices.is_empty() {
        let mesh_def = MeshDef {
            vertices,
            indices,
            ..MeshDef::default()
        };
        if let Some(mesh) = create_mesh(&mesh_def, None) {
            create_mesh_shape(world, body, shape, &mesh, b3::VEC3_ONE);
        }
    }
}

fn b3v(value: Vec3) -> B3Vec3 {
    B3Vec3 {
        x: value.x,
        y: value.y,
        z: value.z,
    }
}
