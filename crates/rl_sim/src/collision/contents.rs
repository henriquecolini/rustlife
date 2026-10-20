//! `CONTENTS_*` flags: what a brush or a point of space is made of, and the
//! masks that select which contents block a trace.
//!
//! The values are Source's, since they come straight from the map's brushes.

/// Nothing: open space.
pub const EMPTY: u32 = 0;
/// Solid world.
pub const SOLID: u32 = 0x1;
/// Glass and other see-through solids.
pub const WINDOW: u32 = 0x2;
/// Alpha-tested grates: block movement, let bullets through.
pub const GRATE: u32 = 0x8;
/// Slime.
pub const SLIME: u32 = 0x10;
/// Water.
pub const WATER: u32 = 0x20;
/// Moving brush entities (doors, platforms).
pub const MOVEABLE: u32 = 0x4000;
/// Invisible walls that block only players.
pub const PLAYER_CLIP: u32 = 0x1_0000;
/// Invisible walls that block NPCs (and, together with [`PLAYER_CLIP`],
/// physics props too).
pub const MONSTER_CLIP: u32 = 0x2_0000;
/// Other players and creatures (never on a brush).
pub const MONSTER: u32 = 0x200_0000;
/// Brush ladders (CS-style; HL2 maps use ladder entities instead).
pub const LADDER: u32 = 0x2000_0000;

/// Everything that blocks a player's movement.
pub const MASK_PLAYER_SOLID: u32 = SOLID | WINDOW | GRATE | MOVEABLE | PLAYER_CLIP | MONSTER;
/// Every liquid a player can swim in.
pub const MASK_WATER: u32 = WATER | SLIME;
