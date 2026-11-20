//! The browser half, run in a real (headless) browser:
//! `cargo test --target wasm32-unknown-unknown -p rl_web` (see `cargo xtask web-test`).

#![cfg(target_arch = "wasm32")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use rl_web::browser::{
    InstallStatus, MountError, OpfsSource, PickedFolder, Progress, install_status, mount_installed,
    read_manifest, remove_install, run_import, write_bytes, write_manifest,
};
use rl_web::import::{ImportRules, plan};
use rl_web::manifest::Manifest;
use source_fs::byte_source::ByteSource;
use source_fs::testing::{SAMPLE_GAMEINFO, VpkBuilder};
use source_fs::{Error, Vfs};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// A pattern whose every window is distinctive, so a misplaced read shows.
fn pattern(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(31) ^ (i >> 8)) as u8)
        .collect()
}

#[wasm_bindgen_test]
async fn written_files_read_back_in_ranges() {
    remove_install().await.unwrap();
    let data = pattern(1000);
    write_bytes("tests/deep/a.bin", &data).await.unwrap();
    let source = OpfsSource::new("tests/deep/a.bin", 1000);
    assert_eq!(source.len(), 1000);
    assert_eq!(source.read_at(10, 20).await.unwrap(), data[10..30]);
    assert_eq!(source.read_at(0, 1000).await.unwrap(), data);
    assert_eq!(source.read_at(1000, 0).await.unwrap(), b"");
    let error = source.read_at(990, 20).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    // A file that was never written is an error, not a panic.
    assert!(
        OpfsSource::new("tests/none.bin", 4)
            .read_at(0, 4)
            .await
            .is_err()
    );
}

#[wasm_bindgen_test]
async fn a_file_larger_than_a_chunk_is_copied_whole() {
    remove_install().await.unwrap();
    // 20 MB crosses two 8 MB chunk boundaries.
    let data = pattern(20 * 1024 * 1024);
    write_bytes("big.bin", &data).await.unwrap();
    let source = OpfsSource::new("big.bin", data.len() as u64);
    let around = 8 * 1024 * 1024 - 5;
    assert_eq!(
        source.read_at(around, 10).await.unwrap(),
        data[around as usize..][..10]
    );
    let end = data.len() - 7;
    assert_eq!(source.read_at(end as u64, 7).await.unwrap(), data[end..]);
}

#[wasm_bindgen_test]
async fn an_empty_file_is_copied() {
    remove_install().await.unwrap();
    write_bytes("empty.bin", &[]).await.unwrap();
    let source = OpfsSource::new("empty.bin", 0);
    assert_eq!(source.read_at(0, 0).await.unwrap(), b"");
}

#[wasm_bindgen_test]
async fn the_manifest_is_stored_and_read_back() {
    remove_install().await.unwrap();
    assert_eq!(read_manifest().await.unwrap(), None);
    assert_eq!(install_status().await.unwrap(), InstallStatus::Empty);
    let mut manifest = Manifest::new();
    manifest.record("hl2mp/gameinfo.txt", 12);
    write_manifest(&manifest).await.unwrap();
    assert_eq!(read_manifest().await.unwrap(), Some(manifest.clone()));
    assert_eq!(
        install_status().await.unwrap(),
        InstallStatus::Partial {
            files: 1,
            bytes: 12
        }
    );
    manifest.complete();
    write_manifest(&manifest).await.unwrap();
    assert_eq!(
        install_status().await.unwrap(),
        InstallStatus::Complete {
            files: 1,
            bytes: 12
        }
    );
    remove_install().await.unwrap();
    assert_eq!(install_status().await.unwrap(), InstallStatus::Empty);
}

/// A tiny install the shape the real one has: a mod folder with
/// `gameinfo.txt`, loose maps, and a VPK set.
fn small_install() -> Vec<(String, Vec<u8>)> {
    let pak = VpkBuilder::new()
        .file("materials/shared.vmt", b"mod_pak")
        .file("materials/mod_only.vmt", b"mod_pak")
        .build();
    vec![
        (
            "mod/gameinfo.txt".to_owned(),
            SAMPLE_GAMEINFO.as_bytes().to_vec(),
        ),
        ("mod/maps/dm_base.bsp".to_owned(), pattern(5000)),
        ("mod/mod_pak_dir.vpk".to_owned(), pak.dir),
        ("mod/mod_pak_000.vpk".to_owned(), pak.archive.unwrap()),
    ]
}

fn small_rules() -> ImportRules {
    ImportRules::parse(
        r#"(mod_dir: "mod", groups: [(id: "g", title: "All", required: true, default_on: true, include: ["mod/**"], exclude: [])])"#,
    )
    .unwrap()
}

#[wasm_bindgen_test]
async fn an_import_copies_the_picked_files_and_the_game_mounts_from_them() {
    remove_install().await.unwrap();
    let install = small_install();
    let files: Vec<(String, &[u8])> = install
        .iter()
        .map(|(path, bytes)| (format!("Half-Life 2 Deathmatch/{path}"), bytes.as_slice()))
        .collect();
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    let picked = PickedFolder::from_memory(&borrowed).unwrap();

    let rules = small_rules();
    let plan = plan(&rules, &picked.candidates(), &BTreeSet::new()).unwrap();
    assert_eq!(plan.root, "Half-Life 2 Deathmatch");
    let seen: Rc<RefCell<Vec<Progress>>> = Rc::default();
    let sink = Rc::clone(&seen);
    let done = run_import(&plan, &picked, Manifest::new(), move |progress| {
        sink.borrow_mut().push(progress.clone());
    })
    .await
    .unwrap();

    assert_eq!(done.len(), 4);
    assert_eq!(read_manifest().await.unwrap(), Some(done));
    let seen = seen.borrow().clone();
    assert!(!seen.is_empty(), "progress was reported");
    let last = seen.last().unwrap();
    assert_eq!(last.bytes_total, plan.total_bytes());
    assert!(last.bytes_done <= last.bytes_total);
    assert!(
        seen.windows(2).all(|w| w[0].bytes_done <= w[1].bytes_done),
        "progress never goes back"
    );

    // And the game mounts from what was copied.
    let vfs = Vfs::new();
    let mounted = mount_installed(&vfs, "mod", 1 << 20).await.unwrap();
    assert!(mounted.mounts >= 2);
    assert_eq!(
        vfs.read_async("materials/mod_only.vmt")
            .await
            .unwrap()
            .unwrap(),
        b"mod_pak"
    );
    assert_eq!(
        vfs.read_async("maps/dm_base.bsp").await.unwrap().unwrap(),
        pattern(5000)
    );
    // Synchronous reads need the files resident first.
    assert!(matches!(
        vfs.read("maps/dm_base.bsp"),
        Err(Error::NotResident { .. })
    ));
    vfs.make_resident(&["maps/dm_base.bsp", "materials/mod_only.vmt"], false)
        .await
        .unwrap();
    assert_eq!(
        vfs.read("maps/dm_base.bsp").unwrap().unwrap(),
        pattern(5000)
    );
    assert_eq!(vfs.take_misses(), ["maps/dm_base.bsp"]);
}

#[wasm_bindgen_test]
async fn an_interrupted_import_resumes_without_copying_again() {
    remove_install().await.unwrap();
    let install = small_install();
    let borrowed: Vec<(&str, &[u8])> = install
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    let picked = PickedFolder::from_memory(&borrowed).unwrap();
    let plan = plan(&small_rules(), &picked.candidates(), &BTreeSet::new()).unwrap();

    // The first two files were copied by an earlier run.
    let mut done = Manifest::new();
    for file in &plan.files[..2] {
        let bytes = &install.iter().find(|(p, _)| *p == file.dest).unwrap().1;
        write_bytes(&file.dest, bytes).await.unwrap();
        done.record(&file.dest, file.size);
    }
    let first_progress: Rc<RefCell<Option<Progress>>> = Rc::default();
    let sink = Rc::clone(&first_progress);
    let finished = run_import(&plan, &picked, done, move |progress| {
        sink.borrow_mut().get_or_insert_with(|| progress.clone());
    })
    .await
    .unwrap();
    assert_eq!(finished.len(), 4);
    let first = first_progress.borrow().clone().unwrap();
    assert_eq!(first.files_done, 2, "the two copied earlier count as done");
    assert!(first.bytes_done > 0);
    assert_eq!(
        install_status().await.unwrap(),
        InstallStatus::Complete {
            files: 4,
            bytes: plan.total_bytes()
        }
    );
}

#[wasm_bindgen_test]
async fn mounting_says_why_it_cannot() {
    remove_install().await.unwrap();
    let vfs = Vfs::new();
    assert!(matches!(
        mount_installed(&vfs, "mod", 1 << 20).await,
        Err(MountError::NothingImported)
    ));
    let mut partial = Manifest::new();
    partial.record("mod/gameinfo.txt", 1);
    write_manifest(&partial).await.unwrap();
    assert!(matches!(
        mount_installed(&vfs, "mod", 1 << 20).await,
        Err(MountError::Incomplete)
    ));
    // Complete on paper but with no gameinfo.txt: not a game.
    let mut empty = Manifest::new();
    empty.record("elsewhere/file.txt", 1);
    empty.complete();
    write_manifest(&empty).await.unwrap();
    assert!(matches!(
        mount_installed(&vfs, "mod", 1 << 20).await,
        Err(MountError::Game(Error::GameNotFound(_)))
    ));
    remove_install().await.unwrap();
}
