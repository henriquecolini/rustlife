//! Particle systems from the game's `.pcf` files.
//!
//! HL2DM's impacts, muzzle flashes, explosions and beams are defined in
//! `particles/*.pcf`, not in code. [`sim`] is the engine-independent half:
//! it steps a system's emitters, initializers, operators, forces and
//! constraints. Drawing them is the other half.

mod draw;
mod render;
pub mod sim;

pub(crate) use render::Pcf;
pub use render::{PcfPlugin, PcfSpawn};
