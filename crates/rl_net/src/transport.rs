//! How a client's messages reach the server: in-process or over UDP.
//!
//! Above this module, a client speaks renet messages on the protocol's
//! channels. Below it are two ways to carry them:
//!
//! - **Local**: the server runs in the same process (a listen server, or a
//!   test). renet's packets are handed across in memory by the server
//!   calling [`NetClient::exchange_local`](crate::client::NetClient::exchange_local),
//!   through a [`DelayLine`] each way, so simulated latency and loss apply
//!   to real packets and renet's own resending is exercised.
//! - **UDP**: netcode's encrypted transport to a remote server. Its packets
//!   cannot be reached, so simulated conditions apply to messages instead:
//!   delayed both ways, unreliable ones dropped.

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use renet::{Bytes, ClientId, RenetClient, RenetServer};
use renet_netcode::{ClientAuthentication, NetcodeClientTransport};
use thiserror::Error;

use crate::conditioner::{DelayLine, Delivery, NetConditions};
use crate::protocol::{Channel, PROTOCOL_ID};

/// Failure to set up a UDP connection.
#[derive(Debug, Error)]
pub enum ConnectError {
    /// The local socket could not be opened.
    #[error("cannot open a UDP socket: {0}")]
    Socket(#[from] std::io::Error),
    /// netcode refused the connection parameters.
    #[error("cannot connect: {0}")]
    Netcode(#[from] renet_netcode::NetcodeError),
}

/// The channels a server sends on, which a client reads.
const SERVER_CHANNELS: [Channel; 2] = [Channel::Control, Channel::Snapshots];

/// How a channel's messages may be treated by a delay line.
fn delivery(channel: u8) -> Delivery {
    if channel == u8::from(Channel::Control) {
        Delivery::Reliable
    } else {
        Delivery::Unreliable
    }
}

/// See the [module docs](self).
#[derive(Debug)]
pub(crate) enum Transport {
    /// In-process, packets through delay lines.
    Local {
        /// The client's id at the server.
        id: ClientId,
        to_server: DelayLine<Bytes>,
        to_client: DelayLine<Bytes>,
    },
    /// UDP through netcode, messages through delay lines.
    Udp {
        netcode: Box<NetcodeClientTransport>,
        outgoing: DelayLine<(u8, Bytes)>,
        incoming: DelayLine<(u8, Bytes)>,
    },
}

impl Transport {
    /// An in-process link for client `id`.
    pub(crate) fn local(id: ClientId, conditions: NetConditions) -> Self {
        Self::Local {
            id,
            to_server: DelayLine::new(conditions, id ^ 0x5eed),
            to_client: DelayLine::new(conditions, id ^ 0xc11e),
        }
    }

    /// A UDP link to `server` from an ephemeral local port.
    pub(crate) fn udp(server: SocketAddr, conditions: NetConditions) -> Result<Self, ConnectError> {
        let bind: SocketAddr = if server.is_ipv4() {
            (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
        } else {
            (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
        };
        let socket = UdpSocket::bind(bind)?;
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        // Unsecured netcode: the client picks its id. The clock's
        // nanoseconds and the port make collisions between players
        // vanishingly unlikely.
        let port = socket.local_addr().map_or(0, |addr| addr.port());
        let client_id = (wall.as_nanos() as u64).rotate_left(16) ^ u64::from(port);
        let authentication = ClientAuthentication::Unsecure {
            protocol_id: PROTOCOL_ID,
            client_id,
            server_addr: server,
            user_data: None,
        };
        let netcode = NetcodeClientTransport::new(wall, authentication, socket)?;
        Ok(Self::Udp {
            netcode: Box::new(netcode),
            outgoing: DelayLine::new(conditions, client_id),
            incoming: DelayLine::new(conditions, !client_id),
        })
    }

    /// Receives what arrived, before the client reads its messages.
    pub(crate) fn receive(&mut self, renet: &mut RenetClient, elapsed: Duration) -> Option<String> {
        if let Self::Udp { netcode, .. } = self
            && let Err(error) = netcode.update(elapsed, renet)
        {
            return Some(error.to_string());
        }
        None
    }

    /// Sends a message on `channel`.
    pub(crate) fn send(
        &mut self,
        renet: &mut RenetClient,
        now: Duration,
        channel: Channel,
        message: Bytes,
    ) {
        let channel = u8::from(channel);
        match self {
            Self::Local { .. } => renet.send_message(channel, message),
            Self::Udp { outgoing, .. } => outgoing.send(now, (channel, message), delivery(channel)),
        }
    }

    /// Every message that has arrived by `now`, with its channel.
    pub(crate) fn messages(&mut self, renet: &mut RenetClient, now: Duration) -> Vec<(u8, Bytes)> {
        let mut arrived = Vec::new();
        for channel in SERVER_CHANNELS {
            while let Some(message) = renet.receive_message(channel) {
                arrived.push((u8::from(channel), message));
            }
        }
        match self {
            Self::Local { .. } => arrived,
            Self::Udp { incoming, .. } => {
                for (channel, message) in arrived {
                    incoming.send(now, (channel, message), delivery(channel));
                }
                incoming.receive(now)
            }
        }
    }

    /// Sends what is due, after the client wrote its messages.
    pub(crate) fn flush(&mut self, renet: &mut RenetClient, now: Duration) -> Option<String> {
        if let Self::Udp {
            netcode, outgoing, ..
        } = self
        {
            for (channel, message) in outgoing.receive(now) {
                renet.send_message(channel, message);
            }
            if let Err(error) = netcode.send_packets(renet) {
                return Some(error.to_string());
            }
        }
        None
    }

    /// Moves packets between an in-process client and `server`; `false`
    /// for UDP clients, or once the server has dropped the client.
    pub(crate) fn exchange_local(
        &mut self,
        renet: &mut RenetClient,
        server: &mut RenetServer,
        now: Duration,
    ) -> bool {
        let Self::Local {
            id,
            to_server,
            to_client,
        } = self
        else {
            return false;
        };
        if renet.is_disconnected() {
            server.remove_connection(*id);
            return false;
        }
        let Ok(packets) = server.get_packets_to_send(*id) else {
            renet.disconnect_due_to_transport();
            return false;
        };
        for packet in packets {
            to_client.send(now, Bytes::from(packet), Delivery::Unreliable);
        }
        for packet in to_client.receive(now) {
            renet.process_packet(&packet);
        }
        for packet in renet.get_packets_to_send() {
            to_server.send(now, Bytes::from(packet), Delivery::Unreliable);
        }
        for packet in to_server.receive(now) {
            if server.process_packet_from(&packet, *id).is_err() {
                renet.disconnect_due_to_transport();
                return false;
            }
        }
        true
    }

    /// Tells the server the client is leaving, where the transport can.
    pub(crate) fn disconnect(&mut self) {
        if let Self::Udp { netcode, .. } = self {
            netcode.disconnect();
        }
    }
}
