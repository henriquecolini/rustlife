//! Building levels: bodies numbered by placement, models' collision, and
//! (with `RUSTLIFE_TEST_GAME_DIR` set and `--ignored`) every stock map.

#![allow(
    clippy::expect_used,
    reason = "tests: anything missing must fail the test"
)]

use std::sync::Arc;

use glam::Vec3;
use rl_data::GameData;
use rl_level::{Level, LevelError, LevelProp, PropModel};
use rl_sim::collision::WorldBuilder;
use rl_sim::simulation::Spawn;
use source_formats::entities::MapEntity;
use source_formats::phy;
use source_formats::props::{PropKind, PropPlacement, PropSolid};
use source_fs::{LooseDirectory, MountOrigin, Vfs};

fn data() -> GameData {
    GameData::builtin().expect("builtin data")
}

/// A crate model: a 32-unit box with a `.phy` of the same box, 20 kg.
fn crate_model() -> PropModel {
    let bytes = phy::testing::boxes_phy(&[([-16.0, -16.0, 0.0], [16.0, 16.0, 32.0])], 20.0, "wood");
    PropModel {
        bounds: ([-16.0, -16.0, 0.0], [16.0, 16.0, 32.0]),
        surface_prop: "wood".to_owned(),
        physics: Some(phy::decode(&bytes).expect("phy")),
    }
}

/// A level on a big floor with three props: a physics crate, a crate that
/// collides with nothing, and a static crate.
fn level(data: &GameData) -> Level {
    let world = WorldBuilder::new()
        .solid_box(
            Vec3::new(-4096.0, -4096.0, -64.0),
            Vec3::new(4096.0, 4096.0, 0.0),
        )
        .build();
    let model = crate_model();
    let placements = [
        PropPlacement::new(
            "models/crate.mdl",
            [100.0, 0.0, 0.0],
            [0.0; 3],
            PropKind::Physics,
        ),
        PropPlacement {
            solid: PropSolid::None,
            ..PropPlacement::new(
                "models/crate.mdl",
                [200.0, 0.0, 0.0],
                [0.0; 3],
                PropKind::Physics,
            )
        },
        PropPlacement::new(
            "models/crate.mdl",
            [300.0, 0.0, 0.0],
            [0.0; 3],
            PropKind::Static,
        ),
    ];
    Level {
        name: "test".to_owned(),
        checksum: 0,
        origin: None,
        world: Arc::new(world),
        spawns: vec![Spawn {
            origin: Vec3::new(0.0, 0.0, 1.0),
            yaw: 90.0,
        }],
        entities: Vec::new(),
        props: placements
            .into_iter()
            .map(|placement| LevelProp {
                desc: model.desc(&placement, &data.physics),
                placement,
            })
            .collect(),
    }
}

#[test]
fn solid_props_get_bodies_in_placement_order() {
    let data = data();
    let built = level(&data).simulation(&data);
    assert_eq!(built.bodies.len(), 3);
    assert!(built.bodies[1].is_none(), "a prop nothing collides with");
    let (Some(first), Some(third)) = (built.bodies[0], built.bodies[2]) else {
        panic!("solid props have bodies: {:?}", built.bodies);
    };
    assert!(first < third);
    let physics = built.simulation.physics();
    assert!(physics.is_dynamic(first));
    assert!(!physics.is_dynamic(third));
    assert_eq!(physics.mass(first), Some(20.0));
    let pose = physics.pose(first).expect("pose");
    assert_eq!(pose.position, Vec3::new(100.0, 0.0, 0.0));
}

#[test]
fn a_named_prop_hangs_on_the_maps_ball_socket() {
    let data = data();
    let mut level = level(&data);
    // Lift the physics crate and name it, as a map names a lamp it hangs.
    let mut placement = level.props[0].placement.clone();
    placement.origin = [100.0, 0.0, 200.0];
    placement.name = Some("lamp_1".to_owned());
    level.props[0] = LevelProp {
        desc: crate_model().desc(&placement, &data.physics),
        placement,
    };
    level.entities =
        vec![MapEntity::new("phys_ballsocket", [100.0, 0.0, 260.0]).with("attach1", "lamp_1")];
    let mut built = level.simulation(&data);
    let lamp = built.bodies[0].expect("a body");
    for _ in 0..300 {
        built.simulation.tick(&[]);
    }
    let hangs = built
        .simulation
        .physics()
        .pose(lamp)
        .expect("pose")
        .position;
    assert!(
        hangs.distance(Vec3::new(100.0, 0.0, 200.0)) < 1.0,
        "{hangs}"
    );
}

#[test]
fn every_machine_numbers_the_bodies_alike() {
    let data = data();
    let level = level(&data);
    assert_eq!(
        level.simulation(&data).bodies,
        level.simulation(&data).bodies
    );
}

#[test]
fn players_join_at_the_levels_spawns() {
    let data = data();
    let mut built = level(&data).simulation(&data);
    let player = built.simulation.join();
    let state = built.simulation.player(player).expect("player").state;
    assert!(
        state.origin.distance(Vec3::new(0.0, 0.0, 1.0)) < 2.0,
        "{}",
        state.origin
    );
}

#[test]
fn models_without_a_physics_model_collide_as_their_box() {
    let data = data();
    let model = PropModel {
        physics: None,
        ..crate_model()
    };
    let placement = PropPlacement::new("models/box.mdl", [0.0; 3], [0.0; 3], PropKind::Physics);
    let desc = model.desc(&placement, &data.physics).expect("a box");
    assert_eq!(desc.pieces.len(), 1);
    assert!(desc.mass > 0.0);
}

#[test]
fn a_missing_map_is_reported_by_name() {
    let data = data();
    let error = rl_level::load(&Vfs::new(), "dm_nowhere", &data.physics).expect_err("missing");
    assert!(matches!(error, LevelError::NotFound { ref map } if map == "dm_nowhere"));
    assert_eq!(error.to_string(), "map dm_nowhere not found");
}

#[test]
fn a_broken_map_is_an_error_not_a_panic() {
    let data = data();
    let dir = std::env::temp_dir().join(format!("rl_level_broken_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("maps")).expect("mkdir");
    std::fs::write(dir.join("maps/dm_broken.bsp"), b"VBSP but not really").expect("write");
    let vfs = Vfs::new();
    vfs.mount_back(
        LooseDirectory::open(&dir).expect("open"),
        MountOrigin::Custom,
    );
    let error = rl_level::load(&vfs, "dm_broken", &data.physics).expect_err("broken");
    std::fs::remove_dir_all(&dir).ok();
    assert!(matches!(error, LevelError::Bsp { .. }), "{error}");
}

/// Loads every stock map from the install at `RUSTLIFE_TEST_GAME_DIR`,
/// builds its simulation and checks props got their bodies.
#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn every_stock_map_loads_as_a_level() {
    let Ok(dir) = std::env::var("RUSTLIFE_TEST_GAME_DIR") else {
        return;
    };
    let options = source_fs::content::ContentOptions {
        game_dir: Some(dir.into()),
        ..Default::default()
    };
    let game = source_fs::content::mount_game(&options).expect("mount the game");
    let data = data();
    for map in [
        "dm_lockdown",
        "dm_overwatch",
        "dm_powerhouse",
        "dm_resistance",
        "dm_runoff",
        "dm_steamlab",
        "dm_underpass",
    ] {
        let level = rl_level::load(&game.vfs, map, &data.physics).expect("load level");
        let built = level.simulation(&data);
        let bodies = built.bodies.iter().flatten().count();
        let dynamic = built
            .bodies
            .iter()
            .flatten()
            .filter(|&&body| built.simulation.physics().is_dynamic(body))
            .count();
        eprintln!(
            "{map}: crc {:08x}, {} props, {bodies} bodies ({dynamic} dynamic), {} spawns, {} pickups",
            level.checksum,
            level.props.len(),
            level.spawns.len(),
            built.simulation.pickups().len()
        );
        assert_eq!(level.origin, Some(MountOrigin::BaseGame));
        assert!(!level.spawns.is_empty(), "{map} has spawns");
        assert!(dynamic > 0, "{map} has physics props");
        assert!(!built.simulation.pickups().is_empty(), "{map} has pickups");
    }
}
