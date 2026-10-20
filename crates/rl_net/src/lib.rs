//! rustlife's network protocol and the client's end of it.
//!
//! The server runs the [`Simulation`](rl_sim::simulation::Simulation)
//! authoritatively at a fixed tick; clients send it their commands and draw
//! what it sends back. This crate holds everything both ends must agree on,
//! and the client logic that hides the network's delay:
//!
//! - [`protocol`]: channels, messages and their encoding; the join
//!   handshake.
//! - [`snapshot`]: the world as one client sees it, sent as the change
//!   from a state the client has.
//! - [`events`]: what happened, for sounds and effects.
//! - [`prediction`] and [`client`]: the client runs its own player ahead
//!   with the shared movement and weapon code, and replays what the server
//!   has not answered yet when a snapshot corrects it.
//! - [`interpolation`] and [`clock`]: everything else is drawn a little in
//!   the past, between two snapshots.
//! - [`conditioner`]: simulated latency, jitter and loss for testing.
//! - `transport`: in-process or UDP (renet over netcode).
//!
//! `docs/netcode.md` walks through how the pieces fit. The server itself is
//! `rl_server`. Nothing here depends on an engine: the Bevy client drives a
//! [`client::NetClient`] each frame, as the tests do.

#![forbid(unsafe_code)]

pub mod client;
pub mod clock;
pub mod conditioner;
pub mod events;
pub mod interpolation;
pub mod prediction;
pub mod protocol;
pub mod snapshot;
mod transport;

pub use client::{ClientConfig, ClientEvent, NetClient, Session};
pub use conditioner::NetConditions;
pub use transport::ConnectError;
