//! The messages client and server exchange, their channels and encoding.
//!
//! # Channels
//!
//! | Channel | Direction | Delivery | Carries |
//! |---------|-----------|----------|---------|
//! | [`Channel::Commands`] | client → server | unreliable | [`CommandPacket`]: the latest commands, repeated |
//! | [`Channel::Control`] | both | reliable, ordered | [`ClientMessage`] / [`ServerMessage`]: joining, data, the roster, kills |
//! | [`Channel::Snapshots`] | server → client | unreliable | [`Snapshot`](crate::snapshot::Snapshot)s of the world |
//!
//! What must arrive — the welcome with the server's data, who joined and
//! left, the kill feed — goes reliably. What is replaced a tick later —
//! commands, snapshots — goes unreliably: a lost command is covered by the
//! copies in the next packets, a lost snapshot by the next one.
//!
//! # Joining
//!
//! 1. The client connects and says [`ClientMessage::Hello`] with the
//!    protocol version and a hash of the gameplay data it already has.
//! 2. The server answers [`ServerMessage::Welcome`]: the map to load (name
//!    and checksum; never the map itself) and its gameplay data, unless the
//!    client's hash shows it has the same. Or [`ServerMessage::Refused`].
//! 3. The client loads the map and says [`ClientMessage::Ready`].
//! 4. The server puts the player in the game and tells them who they are
//!    ([`ServerMessage::Spawned`]) and everyone else that they joined; the
//!    snapshots begin.
//!
//! Every message is encoded with `postcard`.

use std::collections::BTreeMap;

use std::time::Duration;

use renet::{Bytes, ChannelConfig, ConnectionConfig, SendType};
use rl_sim::movement::UserCmd;
use rl_sim::simulation::{Kill, PlayerId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identifies rustlife's traffic to netcode; a server ignores clients of
/// other games.
pub const PROTOCOL_ID: u64 = u64::from_be_bytes(*b"rustlife");

/// Bumped whenever a message changes shape: client and server must agree.
pub const PROTOCOL_VERSION: u32 = 3;

/// Longest player name kept, in characters.
pub const MAX_NAME_CHARS: usize = 32;

/// Most commands one packet carries, whatever the redundancy asked for.
pub const MAX_COMMANDS_PER_PACKET: usize = 32;

/// The channels of a connection; see the [module docs](self).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Client commands, unreliable.
    Commands,
    /// Joining, data, roster and kills, reliable and ordered.
    Control,
    /// World snapshots, unreliable.
    Snapshots,
}

impl From<Channel> for u8 {
    fn from(channel: Channel) -> Self {
        match channel {
            Channel::Commands => 0,
            Channel::Control => 1,
            Channel::Snapshots => 2,
        }
    }
}

/// The renet configuration both ends use.
#[must_use]
pub fn connection_config() -> ConnectionConfig {
    // Reliable messages are resent if unacknowledged this long: a little
    // over a typical round trip.
    let resend_time = Duration::from_millis(200);
    let control = ChannelConfig {
        channel_id: Channel::Control.into(),
        // Room for the welcome (the whole gameplay data set) several times
        // over.
        max_memory_usage_bytes: 4 * 1024 * 1024,
        send_type: SendType::ReliableOrdered { resend_time },
    };
    ConnectionConfig {
        // Snapshots of a busy map stay well below this at 66 Hz.
        available_bytes_per_tick: 256 * 1024,
        client_channels_config: vec![
            ChannelConfig {
                channel_id: Channel::Commands.into(),
                max_memory_usage_bytes: 256 * 1024,
                send_type: SendType::Unreliable,
            },
            control.clone(),
        ],
        server_channels_config: vec![
            control,
            ChannelConfig {
                channel_id: Channel::Snapshots.into(),
                max_memory_usage_bytes: 4 * 1024 * 1024,
                send_type: SendType::Unreliable,
            },
        ],
    }
}

/// What a client tells the server, reliably.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// The first message: who is joining.
    Hello {
        /// The client's [`PROTOCOL_VERSION`].
        version: u32,
        /// The player's name.
        name: String,
        /// [`rl_data::content_hash`] of the gameplay data the client has;
        /// the server sends its own unless they match.
        data_hash: u64,
        /// Ids of the games the client has mounted
        /// (`source_fs::games::GameDescriptor::id`); the server refuses
        /// clients that lack one it requires.
        games: Vec<String>,
    },
    /// The map is loaded: put the player in the game.
    Ready,
    /// Votes for the map to play next, by its index in the current
    /// [`ServerMessage::VoteOpen`]; the latest vote counts.
    Vote(u16),
}

/// A map a server runs, as clients must find it in their own content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapInfo {
    /// Name, without path or extension.
    pub name: String,
    /// CRC-32 of the BSP: the client's copy must match.
    pub checksum: u32,
    /// The workshop item the server's copy came from, so a player missing
    /// the map knows what to subscribe to.
    pub workshop: Option<u64>,
}

/// The server's answer to [`ClientMessage::Hello`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    /// Ids of the games whose content the server runs on.
    pub games: Vec<String>,
    /// The map to load.
    pub map: MapInfo,
    /// The server's gameplay data as files ([`rl_data::GameData::to_files`]),
    /// or `None` when the client's hash showed it has the same.
    pub data: Option<BTreeMap<String, String>>,
    /// Who is playing already.
    pub roster: Vec<(PlayerId, String)>,
}

/// What the server tells a client, reliably.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerMessage {
    /// The client may join; see [`Welcome`].
    Welcome(Box<Welcome>),
    /// The client may not join, and why. The server disconnects them.
    Refused(String),
    /// The client's player is in the game, with this id.
    Spawned(PlayerId),
    /// Another player joined.
    Joined {
        /// Their id.
        player: PlayerId,
        /// Their name.
        name: String,
    },
    /// A player left.
    Left(PlayerId),
    /// Someone died, for the kill feed.
    Kill(Kill),
    /// The match is over and the next map is up for a vote.
    VoteOpen {
        /// The maps to choose from, by name.
        options: Vec<String>,
    },
    /// The votes so far for each of the open vote's options.
    VoteTally(Vec<u16>),
}

/// A client's latest commands, sent unreliably every tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandPacket {
    /// The newest snapshot tick the client has, which the server may send
    /// the next snapshot as a change from.
    pub ack: Option<u64>,
    /// The latest commands, oldest first; the server skips those it has
    /// seen.
    pub commands: Vec<UserCmd>,
}

/// A message that could not be decoded.
#[derive(Debug, Error)]
#[error("malformed message: {0}")]
pub struct ProtocolError(#[from] postcard::Error);

/// Encodes a message.
#[must_use]
pub fn encode<T: Serialize>(message: &T) -> Bytes {
    // Encoding into a Vec fails only for types serde cannot represent,
    // which none of the protocol's are.
    Bytes::from(postcard::to_stdvec(message).unwrap_or_default())
}

/// Decodes a message.
///
/// # Errors
///
/// Fails on bytes that are not a message of type `T`.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ProtocolError> {
    Ok(postcard::from_bytes(bytes)?)
}

/// A player name made safe to show: trimmed, without control characters,
/// at most [`MAX_NAME_CHARS`] long; `fallback` if nothing is left.
#[must_use]
pub fn clean_name(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        fallback.to_owned()
    } else {
        cleaned.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use rl_sim::movement::Buttons;

    use super::*;

    #[test]
    fn messages_round_trip() {
        let hello = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            name: "Gordon".to_owned(),
            data_hash: 42,
            games: vec!["hl2dm".to_owned()],
        };
        assert_eq!(
            decode::<ClientMessage>(&encode(&hello)).expect("decode"),
            hello
        );
        let packet = CommandPacket {
            ack: Some(9),
            commands: vec![UserCmd {
                forward: 1.0,
                buttons: Buttons::JUMP,
                number: 3,
                view_tick: Some(7.5),
                ..UserCmd::default()
            }],
        };
        assert_eq!(
            decode::<CommandPacket>(&encode(&packet)).expect("decode"),
            packet
        );
        let welcome = ServerMessage::Welcome(Box::new(Welcome {
            games: vec!["hl2dm".to_owned()],
            map: MapInfo {
                name: "dm_test".to_owned(),
                checksum: 0xdead_beef,
                workshop: Some(123),
            },
            data: Some(BTreeMap::from([("a.ron".to_owned(), "A()".to_owned())])),
            roster: vec![(PlayerId::from_index(2), "Alyx".to_owned())],
        }));
        assert_eq!(
            decode::<ServerMessage>(&encode(&welcome)).expect("decode"),
            welcome
        );
        for message in [
            ServerMessage::VoteOpen {
                options: vec!["dm_a".to_owned(), "dm_b".to_owned()],
            },
            ServerMessage::VoteTally(vec![2, 0]),
        ] {
            assert_eq!(
                decode::<ServerMessage>(&encode(&message)).expect("decode"),
                message
            );
        }
        let vote = ClientMessage::Vote(1);
        assert_eq!(
            decode::<ClientMessage>(&encode(&vote)).expect("decode"),
            vote
        );
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(decode::<ServerMessage>(&[0xff, 0xff, 0xff]).is_err());
        assert!(decode::<CommandPacket>(&[]).is_err());
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  Gordon\n", "player"), "Gordon");
        assert_eq!(clean_name("\u{7}\t ", "player"), "player");
        assert_eq!(clean_name(&"x".repeat(100), "player").len(), MAX_NAME_CHARS);
    }

    #[test]
    fn channels_are_distinct_per_direction() {
        let config = connection_config();
        let ids = |channels: &[ChannelConfig]| {
            let mut ids: Vec<u8> = channels.iter().map(|channel| channel.channel_id).collect();
            ids.sort_unstable();
            ids.dedup();
            ids.len() == channels.len()
        };
        assert!(ids(&config.client_channels_config));
        assert!(ids(&config.server_channels_config));
    }
}
