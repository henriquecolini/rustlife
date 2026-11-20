//! Checks the import rules against a real install: importing what the plan
//! selects must leave everything the game reads. `#[ignore]`d; run with
//! `RUSTLIFE_TEST_GAME_DIR=<install> cargo test -p rl_web --test real_install
//! -- --ignored --nocapture`.

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use futures_lite::future::block_on;
use rl_web::import::{Candidate, ImportRules, plan};
use source_fs::content::{ContentOptions, mount_game};
use source_fs::remote::{DirectoryTree, RemoteTree, mount_remote_game};
use source_fs::{ByteSource, Vfs};

/// The part of an install an import copies.
#[derive(Debug)]
struct Selected {
    inner: DirectoryTree,
    keep: BTreeSet<String>,
}

impl RemoteTree for Selected {
    fn files(&self) -> Vec<String> {
        self.inner
            .files()
            .into_iter()
            .filter(|path| self.keep.contains(path))
            .collect()
    }

    fn open(&self, path: &str) -> Option<Arc<dyn ByteSource>> {
        self.keep.contains(path).then(|| self.inner.open(path))?
    }
}

#[test]
#[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
fn importing_the_plan_leaves_everything_the_game_reads() {
    let Ok(dir) = std::env::var("RUSTLIFE_TEST_GAME_DIR") else {
        return;
    };
    let tree = DirectoryTree::open(Path::new(&dir)).expect("index the install");
    let candidates: Vec<Candidate> = tree
        .files()
        .into_iter()
        .map(|path| Candidate {
            size: tree.open(&path).map_or(0, |source| source.len()),
            path: format!("Half-Life 2 Deathmatch/{path}"),
        })
        .collect();
    let rules = ImportRules::builtin();
    let planned = plan(&rules, &candidates, &rules.default_enabled()).expect("plan the import");
    eprintln!(
        "{} files, {} MB selected of {} files",
        planned.files.len(),
        planned.total_bytes() / 1_048_576,
        candidates.len()
    );
    let selected = Selected {
        keep: planned.files.iter().map(|file| file.dest.clone()).collect(),
        inner: tree,
    };

    let imported = Vfs::new();
    block_on(mount_remote_game(&imported, &selected, "hl2mp", 1 << 20)).expect("mount the import");
    let native = mount_game(&ContentOptions {
        game_dir: Some(dir.into()),
        ..ContentOptions::default()
    })
    .expect("mount the install");

    // What the game reads from content. The optional voice-over and the
    // campaign are not among it.
    let wanted = [
        "scripts/**",
        "maps/*.bsp",
        "materials/**",
        "models/**",
        "particles/**",
        "sound/**",
    ];
    let mut missing = Vec::new();
    let mut total = 0;
    for pattern in wanted {
        for file in native.vfs.list(pattern) {
            total += 1;
            // Voice-over is optional and off by default.
            if file.path.starts_with("sound/vo/") || file.path.starts_with("sound/npc/") {
                continue;
            }
            if !imported.exists(&file.path) {
                missing.push(file.path);
            }
        }
    }
    missing.sort();
    missing.dedup();
    eprintln!("{total} files checked, {} missing", missing.len());
    for path in missing.iter().take(25) {
        eprintln!("  missing: {path}");
    }
    assert!(
        missing.is_empty(),
        "{} files the game reads are not imported",
        missing.len()
    );
}
