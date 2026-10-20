//! The rustlife server: one map's simulation, run authoritatively for the
//! clients connected to it.
//!
//! [`Server`] owns the [`Simulation`] and renet's server. Its owner — the
//! dedicated server's loop, the client's listen server, or a test — calls
//! [`Server::update`] often with the time; the server then:
//!
//! 1. receives: connections and disconnections, clients' joining messages
//!    (see [`rl_net::protocol`]) and their commands, which wait in a queue
//!    per player;
//! 2. ticks the simulation at the data's fixed rate as time allows, running
//!    each player's waiting commands — one a tick, more to catch up after a
//!    stall, within the `command_backlog` of `net.ron`;
//! 3. sends each playing client a snapshot every `snapshot_interval` ticks:
//!    the world as they see it, as the change from the newest state they
//!    acknowledged, with the events since their last one.
//!
//! Players join on UDP (when the server was given an address) or
//! in-process ([`Server::connect_local`]). Commands are sanitised before
//! they run: a client cannot move faster by sending out-of-range input,
//! nor run more commands than the clock allows.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use renet::{ClientId, RenetServer, ServerEvent};
use renet_netcode::{NetcodeServerTransport, ServerAuthentication, ServerConfig as NetcodeConfig};
use rl_data::GameData;
use rl_level::Level;
use rl_net::client::{ClientConfig, NetClient};
use rl_net::events::{self, Event};
use rl_net::protocol::{
    self, Channel, ClientMessage, CommandPacket, MapInfo, PROTOCOL_ID, PROTOCOL_VERSION,
    ServerMessage, Welcome,
};
use rl_net::snapshot::{self, WorldState};
use rl_sim::movement::UserCmd;
use rl_sim::physics::{BodyId, BodyPose};
use rl_sim::simulation::{PlayerId, Simulation, TickEvents};
use source_fs::MountOrigin;
use thiserror::Error;

/// States sent kept per client, to send later snapshots against.
const STATES_KEPT: usize = 64;
/// Ids of in-process clients count down from here, far from the random
/// ids UDP clients pick.
const FIRST_LOCAL_ID: ClientId = u64::MAX;
/// Most ticks the server runs in one update; beyond that it is too far
/// behind (a debugger pause, a suspended laptop) and skips the time.
const MAX_TICKS_PER_UPDATE: u32 = 10;
/// Pitch is clamped to this, degrees.
const MAX_PITCH: f32 = 89.0;
/// A refused client is disconnected this long after being told why, so
/// the message has time to reach them.
const REFUSAL_GRACE: Duration = Duration::from_secs(2);

/// Failure to start a server.
#[derive(Debug, Error)]
pub enum ServerError {
    /// The UDP socket could not be opened.
    #[error("cannot listen on {addr}: {source}")]
    Bind {
        /// The address asked for.
        addr: SocketAddr,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The gameplay data could not be written out for clients.
    #[error("cannot serialise the gameplay data: {0}")]
    Data(#[from] rl_data::DataError),
}

/// How to run a server.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// The gameplay data, sent to every client.
    pub data: GameData,
    /// Where to listen for UDP clients; `None` for in-process clients
    /// only.
    pub listen: Option<SocketAddr>,
    /// Training dummies to add: players who stand still and respawn.
    pub dummies: usize,
}

/// Where a connected client is in joining.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Connected; waiting for their hello.
    Connected,
    /// Welcomed; loading the map.
    Loading,
    /// In the game as this player.
    Playing(PlayerId),
    /// Refused at this time; disconnected once the refusal had time to
    /// arrive.
    Refused(Duration),
}

/// A connected client.
#[derive(Debug)]
struct Client {
    name: String,
    stage: Stage,
    /// Commands received and not yet run, in order.
    commands: VecDeque<UserCmd>,
    /// The highest command number received.
    received: u32,
    /// The highest command number run.
    ran: u32,
    /// Commands the player may run now: one more each tick, up to the
    /// backlog.
    credits: u32,
    /// Commands that were waiting at the start of the last tick.
    queued: u16,
    /// The newest snapshot tick the client acknowledged.
    acked: Option<u64>,
    /// States sent, oldest first.
    sent: VecDeque<WorldState>,
    /// Events since the client's last snapshot.
    events: Vec<Event>,
    /// When the client was last heard from.
    heard: Duration,
}

impl Client {
    fn new(now: Duration) -> Self {
        Self {
            name: String::new(),
            stage: Stage::Connected,
            commands: VecDeque::new(),
            received: 0,
            ran: 0,
            credits: 0,
            queued: 0,
            acked: None,
            sent: VecDeque::new(),
            events: Vec::new(),
            heard: now,
        }
    }

    fn player(&self) -> Option<PlayerId> {
        match self.stage {
            Stage::Playing(player) => Some(player),
            Stage::Connected | Stage::Loading | Stage::Refused(_) => None,
        }
    }
}

/// See the [crate docs](crate).
#[derive(Debug)]
pub struct Server {
    renet: RenetServer,
    netcode: Option<NetcodeServerTransport>,
    sim: Simulation,
    data: GameData,
    data_files: BTreeMap<String, String>,
    data_hash: u64,
    map: MapInfo,
    /// Where the level placed its moving props.
    initial: BTreeMap<BodyId, BodyPose>,
    clients: BTreeMap<ClientId, Client>,
    next_local: ClientId,
    /// The owner's clock at the last update.
    now: Duration,
    /// Time not yet ticked.
    accumulated: Duration,
    tick_interval: Duration,
}

impl Server {
    /// A server running `level` under `options`.
    ///
    /// # Errors
    ///
    /// Fails if the UDP address cannot be bound.
    pub fn new(level: &Level, options: ServerOptions) -> Result<Self, ServerError> {
        let mut built = level.simulation(&options.data);
        for _ in 0..options.dummies {
            built.simulation.join();
        }
        let data_files = options.data.to_files()?;
        let netcode = options
            .listen
            .map(|addr| udp_transport(addr, &options.data))
            .transpose()?;
        let workshop = match level.origin {
            Some(MountOrigin::Workshop { item_id, .. }) => Some(item_id),
            _ => None,
        };
        Ok(Self {
            renet: RenetServer::new(protocol::connection_config()),
            netcode,
            initial: snapshot::initial_props(&built.simulation),
            sim: built.simulation,
            data_hash: rl_data::content_hash(&data_files),
            data_files,
            tick_interval: Duration::from_secs_f32(options.data.simulation.tick_interval()),
            data: options.data,
            map: MapInfo {
                name: level.name.clone(),
                checksum: level.checksum,
                workshop,
            },
            clients: BTreeMap::new(),
            next_local: FIRST_LOCAL_ID,
            now: Duration::ZERO,
            accumulated: Duration::ZERO,
        })
    }

    /// The simulation, authoritative.
    #[must_use]
    pub fn sim(&self) -> &Simulation {
        &self.sim
    }

    /// The simulation, to change (scripted tests and captures).
    pub fn sim_mut(&mut self) -> &mut Simulation {
        &mut self.sim
    }

    /// The map being played.
    #[must_use]
    pub fn map(&self) -> &MapInfo {
        &self.map
    }

    /// The UDP addresses the server listens on, if any.
    #[must_use]
    pub fn addresses(&self) -> Vec<SocketAddr> {
        self.netcode
            .as_ref()
            .map(NetcodeServerTransport::addresses)
            .unwrap_or_default()
    }

    /// The players in the game, with their names.
    pub fn players(&self) -> impl Iterator<Item = (PlayerId, &str)> {
        self.clients
            .values()
            .filter_map(|client| Some((client.player()?, client.name.as_str())))
    }

    /// A client in this process, joining with `config`; keep it moving
    /// with [`Server::exchange`].
    pub fn connect_local(&mut self, config: ClientConfig) -> NetClient {
        let id = self.next_local;
        self.next_local -= 1;
        self.renet.add_connection(id);
        NetClient::local(id, config)
    }

    /// Moves packets between an in-process client and the server; `false`
    /// once the client is gone.
    pub fn exchange(&mut self, client: &mut NetClient) -> bool {
        client.exchange_local(&mut self.renet)
    }

    /// Receives, ticks as time allows, and sends, at `now` (the owner's
    /// clock, which only runs forwards). Returns the events of each tick
    /// run.
    pub fn update(&mut self, now: Duration) -> Vec<TickEvents> {
        let elapsed = now.saturating_sub(self.now);
        self.now = now;
        self.renet.update(elapsed);
        if let Some(netcode) = &mut self.netcode
            && let Err(error) = netcode.update(elapsed, &mut self.renet)
        {
            tracing::warn!(%error, "network transport error");
        }
        self.handle_connections();
        self.receive();
        self.drop_silent();

        self.accumulated += elapsed;
        let mut ticks = Vec::new();
        while self.accumulated >= self.tick_interval {
            self.accumulated -= self.tick_interval;
            if ticks.len() as u32 >= MAX_TICKS_PER_UPDATE {
                tracing::warn!("server running behind; skipping time");
                self.accumulated = Duration::ZERO;
                break;
            }
            ticks.push(self.tick());
        }
        if let Some(netcode) = &mut self.netcode {
            netcode.send_packets(&mut self.renet);
        }
        ticks
    }

    /// Disconnects everyone, telling UDP clients at once.
    pub fn shut_down(&mut self) {
        match &mut self.netcode {
            Some(netcode) => netcode.disconnect_all(&mut self.renet),
            None => self.renet.disconnect_all(),
        }
    }

    fn handle_connections(&mut self) {
        while let Some(event) = self.renet.get_event() {
            match event {
                ServerEvent::ClientConnected { client_id } => {
                    tracing::info!(client = client_id, "client connected");
                    self.clients.insert(client_id, Client::new(self.now));
                }
                ServerEvent::ClientDisconnected { client_id, reason } => {
                    tracing::info!(client = client_id, %reason, "client disconnected");
                    self.forget(client_id);
                }
            }
        }
    }

    /// A client is gone: their player leaves the game.
    fn forget(&mut self, id: ClientId) {
        let Some(client) = self.clients.remove(&id) else {
            return;
        };
        if let Some(player) = client.player() {
            self.sim.leave(player);
            tracing::info!(name = client.name, "player left");
            self.broadcast(&ServerMessage::Left(player));
        }
    }

    /// Disconnects clients silent for longer than the timeout, and refused
    /// ones whose refusal had time to arrive.
    fn drop_silent(&mut self) {
        let timeout = Duration::from_secs_f32(self.data.net.timeout);
        let now = self.now;
        let dropped: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, client)| match client.stage {
                Stage::Refused(at) => now.saturating_sub(at) > REFUSAL_GRACE,
                _ => now.saturating_sub(client.heard) > timeout,
            })
            .map(|(&id, _)| id)
            .collect();
        for id in dropped {
            tracing::info!(client = id, "dropping client");
            self.renet.disconnect(id);
        }
    }

    fn receive(&mut self) {
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for id in ids {
            while let Some(message) = self.renet.receive_message(id, Channel::Control) {
                self.touch(id);
                match protocol::decode::<ClientMessage>(&message) {
                    Ok(message) => self.control(id, message),
                    Err(error) => {
                        tracing::warn!(client = id, %error, "bad message; disconnecting");
                        self.renet.disconnect(id);
                    }
                }
            }
            while let Some(message) = self.renet.receive_message(id, Channel::Commands) {
                self.touch(id);
                if let Ok(packet) = protocol::decode::<CommandPacket>(&message) {
                    self.commands(id, packet);
                }
            }
        }
    }

    fn touch(&mut self, id: ClientId) {
        if let Some(client) = self.clients.get_mut(&id) {
            client.heard = self.now;
        }
    }

    fn control(&mut self, id: ClientId, message: ClientMessage) {
        let Some(stage) = self.clients.get(&id).map(|client| client.stage) else {
            return;
        };
        match (message, stage) {
            (
                ClientMessage::Hello {
                    version,
                    name,
                    data_hash,
                },
                Stage::Connected,
            ) => self.hello(id, version, &name, data_hash),
            (ClientMessage::Ready, Stage::Loading) => self.ready(id),
            (message, stage) => {
                tracing::debug!(client = id, ?message, ?stage, "message out of turn ignored");
            }
        }
    }

    fn hello(&mut self, id: ClientId, version: u32, name: &str, data_hash: u64) {
        let playing = self.players().count();
        let refusal = if version != PROTOCOL_VERSION {
            Some(format!(
                "the server speaks protocol {PROTOCOL_VERSION}, the client {version}"
            ))
        } else if playing >= self.data.net.max_players as usize {
            Some("the server is full".to_owned())
        } else {
            None
        };
        if let Some(reason) = refusal {
            tracing::info!(client = id, reason, "refused a client");
            self.send(id, &ServerMessage::Refused(reason));
            if let Some(client) = self.clients.get_mut(&id) {
                client.stage = Stage::Refused(self.now);
            }
            return;
        }
        let name = protocol::clean_name(name, &format!("player{}", self.clients.len()));
        let welcome = Welcome {
            map: self.map.clone(),
            data: (data_hash != self.data_hash).then(|| self.data_files.clone()),
            roster: self
                .players()
                .map(|(player, name)| (player, name.to_owned()))
                .collect(),
        };
        tracing::info!(
            client = id,
            name,
            sends_data = welcome.data.is_some(),
            "welcomed"
        );
        if let Some(client) = self.clients.get_mut(&id) {
            client.name = name;
            client.stage = Stage::Loading;
        }
        self.send(id, &ServerMessage::Welcome(Box::new(welcome)));
    }

    fn ready(&mut self, id: ClientId) {
        let player = self.sim.join();
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        client.stage = Stage::Playing(player);
        let name = client.name.clone();
        tracing::info!(name, ?player, "player joined");
        self.send(id, &ServerMessage::Spawned(player));
        self.broadcast(&ServerMessage::Joined { player, name });
    }

    fn commands(&mut self, id: ClientId, packet: CommandPacket) {
        let backlog = self.data.net.command_backlog as usize;
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        if client.player().is_none() {
            return;
        }
        if packet.ack > client.acked {
            client.acked = packet.ack;
        }
        for cmd in packet.commands {
            if cmd.number > client.received {
                client.received = cmd.number;
                client.commands.push_back(sanitize(cmd));
            }
        }
        // A client far ahead of the clock loses its oldest commands rather
        // than banking them.
        while client.commands.len() > backlog * 4 {
            client.commands.pop_front();
        }
    }

    /// One tick: every player's waiting commands within their credits, the
    /// simulation, then snapshots.
    fn tick(&mut self) -> TickEvents {
        let backlog = self.data.net.command_backlog;
        let mut commands = Vec::new();
        for client in self.clients.values_mut() {
            let Some(player) = client.player() else {
                continue;
            };
            client.credits = (client.credits + 1).min(backlog);
            client.queued = u16::try_from(client.commands.len()).unwrap_or(u16::MAX);
            let run = (client.credits as usize).min(client.commands.len());
            for cmd in client.commands.drain(..run) {
                client.ran = cmd.number;
                commands.push((player, cmd));
            }
            client.credits -= run as u32;
        }
        let events = self.sim.tick(&commands);
        for kill in &events.kills {
            self.broadcast(&ServerMessage::Kill(*kill));
        }
        for client in self.clients.values_mut() {
            if let Some(player) = client.player() {
                client
                    .events
                    .extend(events::from_tick_events(&events, Some(player)));
            }
        }
        if self
            .sim
            .tick_count()
            .is_multiple_of(u64::from(self.data.net.snapshot_interval.max(1)))
        {
            self.send_snapshots();
        }
        events
    }

    fn send_snapshots(&mut self) {
        for (&id, client) in &mut self.clients {
            let Some(player) = client.player() else {
                continue;
            };
            let state = WorldState::capture(&self.sim, Some(player));
            let baseline = client
                .acked
                .and_then(|tick| client.sent.iter().find(|sent| sent.tick == tick));
            let snapshot = state.diff(
                baseline,
                &self.initial,
                client.ran,
                client.queued,
                std::mem::take(&mut client.events),
            );
            self.renet
                .send_message(id, Channel::Snapshots, protocol::encode(&snapshot));
            client.sent.push_back(state);
            while client.sent.len() > STATES_KEPT {
                client.sent.pop_front();
            }
        }
    }

    fn send(&mut self, id: ClientId, message: &ServerMessage) {
        self.renet
            .send_message(id, Channel::Control, protocol::encode(message));
    }

    /// Sends `message` to every client in the game or joining it.
    fn broadcast(&mut self, message: &ServerMessage) {
        let bytes = protocol::encode(message);
        for (&id, client) in &self.clients {
            if matches!(client.stage, Stage::Loading | Stage::Playing(_)) {
                self.renet.send_message(id, Channel::Control, bytes.clone());
            }
        }
    }
}

/// A command made safe to run: movement within `-1..=1`, the view within
/// range, nothing non-finite.
fn sanitize(cmd: UserCmd) -> UserCmd {
    let finite = |value: f32| if value.is_finite() { value } else { 0.0 };
    UserCmd {
        forward: finite(cmd.forward).clamp(-1.0, 1.0),
        side: finite(cmd.side).clamp(-1.0, 1.0),
        pitch: finite(cmd.pitch).clamp(-MAX_PITCH, MAX_PITCH),
        yaw: finite(cmd.yaw).rem_euclid(360.0),
        view_tick: cmd.view_tick.filter(|tick| tick.is_finite()),
        ..cmd
    }
}

fn udp_transport(addr: SocketAddr, data: &GameData) -> Result<NetcodeServerTransport, ServerError> {
    let bind = |source| ServerError::Bind { addr, source };
    let socket = UdpSocket::bind(addr).map_err(bind)?;
    let public = socket.local_addr().map_err(bind)?;
    let config = NetcodeConfig {
        current_time: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default(),
        // netcode admits at most 1024; the server refuses players beyond
        // its own limit after they say hello.
        max_clients: (data.net.max_players as usize + 4).min(1024),
        protocol_id: PROTOCOL_ID,
        public_addresses: vec![public],
        authentication: ServerAuthentication::Unsecure,
    };
    NetcodeServerTransport::new(config, socket).map_err(bind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_sanitised() {
        let cmd = sanitize(UserCmd {
            forward: 9.0,
            side: f32::NAN,
            pitch: -200.0,
            yaw: -90.0,
            view_tick: Some(f64::INFINITY),
            number: 7,
            ..UserCmd::default()
        });
        assert_eq!(cmd.forward, 1.0);
        assert_eq!(cmd.side, 0.0);
        assert_eq!(cmd.pitch, -MAX_PITCH);
        assert_eq!(cmd.yaw, 270.0);
        assert_eq!(cmd.view_tick, None);
        assert_eq!(cmd.number, 7);
    }
}
