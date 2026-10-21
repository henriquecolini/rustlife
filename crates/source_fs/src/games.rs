//! The Source games rustlife knows how to mount.
//!
//! A *game* is a content source: an installed Source title whose maps, models,
//! sounds and so on become available to rustlife, like a Garry's Mod mount.
//! Each has a stable [`GameDescriptor::id`] that players and servers exchange
//! to agree they have the same content.
//!
//! Only Half-Life 2: Deathmatch is supported today; supporting another title
//! means adding a descriptor to [`KNOWN_GAMES`].

use crate::content::{HL2DM_APP_ID, HL2DM_MOD_DIR};

/// A mountable Source game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameDescriptor {
    /// Stable identifier used in configuration files and on the network.
    pub id: &'static str,
    /// Human-readable title.
    pub name: &'static str,
    /// Steam app id, used to find the install.
    pub app_id: u32,
    /// Directory holding `gameinfo.txt`, inside the install folder.
    pub mod_dir: &'static str,
}

/// Half-Life 2: Deathmatch.
pub const HL2DM: GameDescriptor = GameDescriptor {
    id: "hl2dm",
    name: "Half-Life 2: Deathmatch",
    app_id: HL2DM_APP_ID,
    mod_dir: HL2DM_MOD_DIR,
};

/// Every game rustlife can mount.
pub const KNOWN_GAMES: &[GameDescriptor] = &[HL2DM];

/// Looks a game up by [`GameDescriptor::id`].
#[must_use]
pub fn find_game(id: &str) -> Option<&'static GameDescriptor> {
    KNOWN_GAMES.iter().find(|game| game.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_resolvable() {
        for game in KNOWN_GAMES {
            assert_eq!(find_game(game.id), Some(game));
            assert_eq!(
                KNOWN_GAMES
                    .iter()
                    .filter(|other| other.id == game.id)
                    .count(),
                1,
                "duplicate id {}",
                game.id
            );
        }
        assert_eq!(find_game("nope"), None);
    }
}
