//! rustlife's shared simulation: the game rules that the server runs
//! authoritatively and every client runs again to predict its own player.
//!
//! The crate is headless and engine-independent: no Bevy, no rendering, no
//! I/O. Everything is a plain function or type over Source-space values (Z up,
//! units of inches), so it can be unit-tested directly and gives identical
//! results on the server and the client.
//!
//! - [`collision`]: the map's collision world and box traces through it.
//! - [`movement`]: Source-like player movement, one tick at a time.
//!
//! # Determinism
//!
//! Given the same inputs, the simulation produces bit-identical outputs: it
//! uses only `f32` arithmetic in a fixed order, never iterates hash maps, and
//! reads no clocks or random sources. Client-side prediction depends on this.

#![forbid(unsafe_code)]

pub mod collision;
pub mod movement;
