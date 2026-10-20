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
//! - [`physics`]: props as rigid bodies, pushed by and hitting players.
//! - [`scene`]: the map and its props together, as movement sees them.
//! - [`tools`]: the Manipulator, the tool behind the weapon of that name.
//! - [`weapons`]: what players carry, firing, reloading, projectiles.
//! - [`combat`]: health and armor, damage, what shots and blasts hit.
//! - [`rng`]: deterministic randomness (bullet spread, spawn choice).
//! - [`simulation`]: all of the above for a map's players, a tick at a
//!   time.
//!
//! # Determinism
//!
//! Given the same inputs, movement produces bit-identical outputs: it uses
//! only `f32` arithmetic in a fixed order, never iterates hash maps, and
//! reads no clocks or random sources. Client-side prediction depends on
//! this. Prop physics is server-authoritative and repeatable on one build,
//! but not promised to match across machines (see [`physics`]).

#![forbid(unsafe_code)]

pub mod collision;
pub mod combat;
pub mod movement;
pub mod physics;
pub mod rng;
pub mod scene;
pub mod simulation;
pub mod tools;
pub mod weapons;
