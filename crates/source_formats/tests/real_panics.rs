//! Runs every decoder over every file of a real install and reports the ones
//! that make a parsing dependency panic. `#[ignore]`d; run with
//! `RUSTLIFE_TEST_GAME_DIR=<install> cargo test -p source_formats --test
//! real_panics -- --ignored --nocapture`.
//!
//! The decoders contain such panics (`catch_unwind`) and report them as
//! errors, which is enough on a desktop. A browser build aborts on any panic
//! instead, so a file that panics a parser there would end the game. This
//! sweep says whether any such file exists in the stock content.

use std::collections::BTreeMap;
use std::path::PathBuf;

use source_formats::{bsp, collision, mdl, vtf};
use source_fs::Vfs;
use source_fs::content::{ContentOptions, mount_game};

fn game_dir() -> Option<PathBuf> {
    std::env::var_os("RUSTLIFE_TEST_GAME_DIR")
        .or_else(|| std::env::var_os("HL2DM_DIR"))
        .map(PathBuf::from)
}

/// Failures that were contained panics, by decoder, with the files.
#[derive(Default)]
struct Panics(BTreeMap<&'static str, Vec<String>>);

impl Panics {
    fn check<T, E: std::fmt::Display>(
        &mut self,
        decoder: &'static str,
        path: &str,
        result: Result<T, E>,
    ) {
        if let Err(error) = result
            && error.to_string().contains("panicked")
        {
            self.0.entry(decoder).or_default().push(path.to_owned());
        }
    }
}

fn read(vfs: &Vfs, path: &str) -> Option<Vec<u8>> {
    vfs.read(path).ok().flatten()
}

#[test]
#[ignore = "requires a Half-Life 2: Deathmatch install"]
fn no_stock_file_panics_a_decoder() {
    let options = ContentOptions {
        game_dir: game_dir(),
        ..ContentOptions::default()
    };
    let Ok(game) = mount_game(&options) else {
        eprintln!("skipping: no install");
        return;
    };
    let vfs = &game.vfs;
    let mut panics = Panics::default();

    let models = vfs.list("models/**/*.mdl");
    for file in &models {
        let Some(mdl_bytes) = read(vfs, &file.path) else {
            continue;
        };
        panics.check("mdl header", &file.path, mdl::read_header(&mdl_bytes));
        let stem = file.path.trim_end_matches(".mdl");
        if let (Some(vtx), Some(vvd)) = (
            read(vfs, &format!("{stem}.dx90.vtx")),
            read(vfs, &format!("{stem}.vvd")),
        ) {
            panics.check(
                "mdl model",
                &file.path,
                mdl::load_model(&mdl_bytes, &vtx, &vvd),
            );
        }
    }
    let textures = vfs.list("materials/**/*.vtf");
    for file in &textures {
        if let Some(bytes) = read(vfs, &file.path) {
            panics.check("vtf", &file.path, vtf::decode(&bytes));
        }
    }
    let maps = vfs.list("maps/*.bsp");
    for file in &maps {
        if let Some(bytes) = read(vfs, &file.path) {
            panics.check("bsp collision", &file.path, collision::load(&bytes));
            panics.check("bsp contents", &file.path, bsp::load_contents(&bytes));
            panics.check("bsp geometry", &file.path, bsp::load_geometry(&bytes));
        }
    }

    eprintln!(
        "swept {} models, {} textures, {} maps",
        models.len(),
        textures.len(),
        maps.len()
    );
    for (decoder, files) in &panics.0 {
        eprintln!("{decoder}: {} files panicked, e.g.", files.len());
        for path in files.iter().take(8) {
            eprintln!("    {path}");
        }
    }
    if panics.0.is_empty() {
        eprintln!("no decoder panicked on any stock file");
    }
}
