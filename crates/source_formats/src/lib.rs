//! Typed wrappers and gap-fills over the Source engine format parsers.
//!
//! rustlife reads content directly from the player's own Half-Life 2:
//! Deathmatch install (see the `source_fs` crate). This crate turns the raw
//! bytes those mounts hand back into typed, engine-independent values: decoded
//! textures ([`vtf`]), parsed materials ([`vmt`]), map geometry ([`bsp`]),
//! models ([`mdl`]) and their collision ([`phy`]), their placements
//! ([`props`]), the baked light that models are lit by ([`ambient`],
//! [`lights`], [`vhv`]), what players collide with ([`collision`]) and the
//! map's entities ([`entities`]), and the game's sounds ([`wav`],
//! [`soundscript`], [`soundscape`]).
//!
//! Nothing here depends on Bevy. Every decoder is a pure function over bytes,
//! so the format layer can be unit-tested without an `App`, a window or a GPU,
//! and the same code is reusable by the headless server and the content
//! inspector. Where the upstream crates (`vtf`, `vmt-parser`, `vbsp`, `vmdl`)
//! are missing a piece rustlife needs, the gap is filled here rather than in
//! the renderer, so the higher layers only ever see clean Rust types.
//!
//! # Robustness
//!
//! Community content is frequently malformed. Decoders in this crate therefore
//! never panic on bad input: they return an error, and the caller is expected
//! to log it and fall back to a placeholder (checker texture, error material).

#![forbid(unsafe_code)]

pub mod ambient;
pub mod bsp;
pub mod collision;
pub mod entities;
mod leaves;
pub mod lightmap;
pub mod lights;
pub mod mdl;
pub mod phy;
pub mod props;
pub mod skybox;
pub mod soundscape;
pub mod soundscript;
pub mod vhv;
pub mod vmt;
mod vmt_fallback;
pub mod vtf;
pub mod wav;
