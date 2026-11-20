//! The browser build of rustlife.
//!
//! The game reads the player's own Half-Life 2: Deathmatch files, and a
//! browser has no Steam folder to read them from. So the player picks their
//! install folder once, and its needed files are copied into the browser's
//! private storage, where later visits mount them (see `source_fs::remote`).
//!
//! * [`import`] plans which files to copy, from data (`data/import.ron`);
//! * [`manifest`] records what has been copied.
//!
//! Both are plain Rust, tested natively. The code that talks to the browser
//! lives in the `browser` module, which exists only for `wasm32`.

pub mod import;
pub mod manifest;

#[cfg(target_arch = "wasm32")]
pub mod browser;
