//! A client's end of the connection: joining, commands, snapshots,
//! prediction and interpolation, independent of any engine.
//!
//! [`NetClient`] is driven by its owner — the Bevy client, or a test — with
//! the time:
//!
//! - [`NetClient::update`] every frame: receives messages, rebuilds world
//!   states from snapshots, writes the newest into the replica simulation
//!   and replays the commands the server has not run yet, and reports
//!   what the owner must act on ([`ClientEvent`]).
//! - On [`ClientEvent::Welcome`] the owner loads the map with the server's
//!   data and hands over the level's simulation ([`NetClient::enter`]).
//! - [`NetClient::tick`] at the simulation's tick rate, with the player's
//!   command: it is numbered, run at once for the local player
//!   (prediction), and sent with the few before it.
//!
//! Everything but the local player is drawn from
//! [`Session::interpolated`], a little in the past.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use glam::Vec3;
use renet::{Bytes, ClientId, RenetClient, RenetServer};
use rl_data::GameData;
use rl_sim::movement::UserCmd;
use rl_sim::physics::{BodyId, BodyPose};
use rl_sim::simulation::{Kill, PlayerId, Prediction, Simulation, TickEvents};

use crate::clock::ServerClock;
use crate::conditioner::NetConditions;
use crate::events;
use crate::interpolation::{Interpolated, StateBuffer};
use crate::prediction::Predictor;
use crate::protocol::{
    self, Channel, ClientMessage, CommandPacket, MAX_COMMANDS_PER_PACKET, PROTOCOL_VERSION,
    ServerMessage, Welcome,
};
use crate::snapshot::{self, Snapshot, WorldState};
use crate::transport::{ConnectError, Transport};

/// World states kept: a little over a second at 66 Hz.
const STATES_KEPT: usize = 96;
/// A correction bigger than this is a teleport, not an error to smooth
/// away, units.
const MAX_SMOOTHED_ERROR: f32 = 64.0;
/// Corrections smaller than this are rounding, not mispredictions, units.
const CORRECTION_EPSILON: f32 = 0.01;
/// Seconds over which a prediction error fades to a third.
const ERROR_FADE_SECS: f32 = 0.1;
/// Commands the client tries to keep waiting at the server, to ride out
/// jitter without adding much delay.
const TARGET_QUEUE: f32 = 1.5;
/// Most the client speeds up or slows down its ticks to reach that queue.
const MAX_PACE: f32 = 0.03;

/// Who is joining, and how the link should misbehave.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// The player's name.
    pub name: String,
    /// [`rl_data::content_hash`] of the gameplay data the client has.
    pub data_hash: u64,
    /// Simulated latency, jitter and loss (none for a real game).
    pub conditions: NetConditions,
}

/// Something the client's owner must act on.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    /// The server accepted the client: load the map, with the server's data
    /// if it sent some, then [`NetClient::enter`].
    Welcome(Box<Welcome>),
    /// The server would not have the client, and said why.
    Refused(String),
    /// The connection is gone, for this reason.
    Disconnected(String),
}

/// A client in a map: the replica, prediction and interpolation.
#[derive(Debug)]
pub struct Session {
    sim: Simulation,
    data: GameData,
    player: Option<PlayerId>,
    /// Where the level placed its moving props, for full snapshots.
    initial: BTreeMap<BodyId, BodyPose>,
    states: StateBuffer,
    predictor: Predictor,
    clock: ServerClock,
    /// The tick being drawn, sent with commands for lag compensation.
    view_tick: Option<f64>,
    /// Commands waiting at the server, smoothed.
    queued: f32,
    /// How far the drawn view is from the predicted one, fading away.
    error: Vec3,
    /// Snapshots that moved the predicted player.
    corrections: u32,
}

impl Session {
    fn new(sim: Simulation, data: GameData) -> Self {
        Self {
            initial: snapshot::initial_props(&sim),
            clock: ServerClock::new(data.simulation.tick_rate, data.net.interpolation_delay),
            sim,
            data,
            player: None,
            states: StateBuffer::new(STATES_KEPT),
            predictor: Predictor::default(),
            view_tick: None,
            queued: TARGET_QUEUE,
            error: Vec3::ZERO,
            corrections: 0,
        }
    }

    /// The replica: the server's world as of the newest snapshot, with the
    /// local player run ahead.
    #[must_use]
    pub fn sim(&self) -> &Simulation {
        &self.sim
    }

    /// The gameplay data, the server's.
    #[must_use]
    pub fn data(&self) -> &GameData {
        &self.data
    }

    /// The local player, once the server put them in the game.
    #[must_use]
    pub fn player(&self) -> Option<PlayerId> {
        self.player
    }

    /// How far to shift the drawn view from the predicted one: what is left
    /// of the last corrections, fading to nothing.
    #[must_use]
    pub fn prediction_error(&self) -> Vec3 {
        self.error
    }

    /// How many snapshots found the prediction wrong and moved the local
    /// player: a measure of how well prediction works.
    #[must_use]
    pub fn corrections(&self) -> u32 {
        self.corrections
    }

    /// The newest world state received.
    #[must_use]
    pub fn latest(&self) -> Option<&WorldState> {
        self.states.latest()
    }

    /// The server tick being drawn.
    #[must_use]
    pub fn view_tick(&self) -> Option<f64> {
        self.view_tick
    }

    /// Everything but the local player, at the tick being drawn.
    #[must_use]
    pub fn interpolated(&self) -> Option<Interpolated> {
        self.states.sample(self.view_tick?)
    }

    /// How much faster (above 1) or slower than the tick rate the client
    /// should tick, to keep a small queue of its commands at the server.
    #[must_use]
    pub fn pace(&self) -> f32 {
        1.0 + ((TARGET_QUEUE - self.queued) * 0.02).clamp(-MAX_PACE, MAX_PACE)
    }

    /// Runs the clocks on by `elapsed`.
    fn advance(&mut self, elapsed: Duration) {
        self.clock.advance(elapsed);
        self.view_tick = self.clock.render_tick();
        self.error *= (-elapsed.as_secs_f32() / ERROR_FADE_SECS).exp();
    }

    /// Takes a snapshot in: rebuilds its state from the baseline, keeps it,
    /// and if it is the newest writes it into the replica and replays the
    /// commands the server has not run. Returns its events.
    fn receive(&mut self, snapshot: &Snapshot) -> Option<Vec<events::Event>> {
        let baseline = snapshot.baseline.and_then(|tick| self.states.get(tick));
        let state = match WorldState::apply(baseline, snapshot, &self.initial) {
            Ok(state) => state,
            Err(error) => {
                tracing::debug!(%error, "dropped a snapshot");
                return None;
            }
        };
        self.clock.observe(state.tick);
        let newest = self
            .states
            .latest()
            .is_none_or(|latest| state.tick > latest.tick);
        if !self.states.insert(state.clone()) {
            return None;
        }
        if newest {
            self.queued += (f32::from(snapshot.queued) - self.queued) * 0.1;
            self.reconcile(&state, snapshot.ack);
        }
        Some(snapshot.events.clone())
    }

    /// Writes the server's newest state into the replica and replays the
    /// commands it has not run yet on top, keeping the difference to the
    /// old prediction as an error to fade out.
    fn reconcile(&mut self, state: &WorldState, ack: u32) {
        let eye = |sim: &Simulation, player| sim.player(player).map(|player| player.state.origin);
        let before = self.player.and_then(|player| eye(&self.sim, player));
        state.restore(&mut self.sim);
        self.predictor.acknowledge(ack);
        let Some(player) = self.player else {
            return;
        };
        let pending: Vec<UserCmd> = self.predictor.pending().copied().collect();
        for cmd in &pending {
            self.sim.predict(player, cmd);
        }
        if let (Some(before), Some(after)) = (before, eye(&self.sim, player)) {
            let correction = before - after;
            if correction.length_squared() > CORRECTION_EPSILON * CORRECTION_EPSILON {
                self.corrections += 1;
            }
            self.error = if (self.error + correction).length() < MAX_SMOOTHED_ERROR {
                self.error + correction
            } else {
                Vec3::ZERO
            };
        }
    }
}

/// See the [module docs](self).
#[derive(Debug)]
pub struct NetClient {
    renet: RenetClient,
    transport: Transport,
    config: ClientConfig,
    /// The owner's clock.
    now: Duration,
    said_hello: bool,
    disconnected: bool,
    roster: BTreeMap<PlayerId, String>,
    session: Option<Session>,
    /// What happened, for the owner to show.
    events: Vec<TickEvents>,
}

impl NetClient {
    /// A client of a server in this process, with id `id` there; the server
    /// must add the connection and call [`NetClient::exchange_local`].
    #[must_use]
    pub fn local(id: ClientId, config: ClientConfig) -> Self {
        let mut renet = RenetClient::new(protocol::connection_config());
        renet.set_connected();
        Self::with(renet, Transport::local(id, config.conditions), config)
    }

    /// A client connecting to the server at `server` over UDP.
    ///
    /// # Errors
    ///
    /// Fails if no local socket can be opened.
    pub fn connect(server: SocketAddr, config: ClientConfig) -> Result<Self, ConnectError> {
        let transport = Transport::udp(server, config.conditions)?;
        let mut renet = RenetClient::new(protocol::connection_config());
        renet.set_connecting();
        Ok(Self::with(renet, transport, config))
    }

    fn with(renet: RenetClient, transport: Transport, config: ClientConfig) -> Self {
        Self {
            renet,
            transport,
            config,
            now: Duration::ZERO,
            said_hello: false,
            disconnected: false,
            roster: BTreeMap::new(),
            session: None,
            events: Vec::new(),
        }
    }

    /// Whether the connection is established.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.renet.is_connected()
    }

    /// The round-trip time renet measures, seconds.
    #[must_use]
    pub fn rtt(&self) -> f64 {
        self.renet.rtt()
    }

    /// The map session, once entered.
    #[must_use]
    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// The players' names, by id.
    #[must_use]
    pub fn roster(&self) -> &BTreeMap<PlayerId, String> {
        &self.roster
    }

    /// What happened since this was last called: the events of the
    /// snapshots received, with kills.
    pub fn take_events(&mut self) -> Vec<TickEvents> {
        std::mem::take(&mut self.events)
    }

    /// Receives and handles everything that arrived by `now` (the owner's
    /// clock, which only runs forwards).
    pub fn update(&mut self, now: Duration) -> Vec<ClientEvent> {
        let elapsed = now.saturating_sub(self.now);
        self.now = now;
        let mut out = Vec::new();
        if self.disconnected {
            return out;
        }
        self.renet.update(elapsed);
        if let Some(error) = self.transport.receive(&mut self.renet, elapsed) {
            self.renet.disconnect_due_to_transport();
            tracing::warn!(%error, "network transport failed");
        }
        if let Some(reason) = self.renet.disconnect_reason() {
            self.disconnected = true;
            out.push(ClientEvent::Disconnected(reason.to_string()));
            return out;
        }
        if self.renet.is_connected() && !self.said_hello {
            self.said_hello = true;
            let hello = ClientMessage::Hello {
                version: PROTOCOL_VERSION,
                name: self.config.name.clone(),
                data_hash: self.config.data_hash,
            };
            self.send(Channel::Control, protocol::encode(&hello));
        }
        if let Some(session) = &mut self.session {
            session.advance(elapsed);
        }
        let mut kills = Vec::new();
        let mut heard = Vec::new();
        for (channel, message) in self.transport.messages(&mut self.renet, now) {
            if channel == u8::from(Channel::Control) {
                match protocol::decode::<ServerMessage>(&message) {
                    Ok(message) => self.control(message, &mut out, &mut kills),
                    Err(error) => tracing::warn!(%error, "bad control message"),
                }
            } else if let Some(session) = &mut self.session {
                match protocol::decode::<Snapshot>(&message) {
                    Ok(snapshot) => heard.extend(session.receive(&snapshot).unwrap_or_default()),
                    Err(error) => tracing::debug!(%error, "bad snapshot"),
                }
            }
        }
        if !heard.is_empty() || !kills.is_empty() {
            self.events.push(events::to_tick_events(&heard, &kills));
        }
        self.flush();
        out
    }

    fn control(
        &mut self,
        message: ServerMessage,
        out: &mut Vec<ClientEvent>,
        kills: &mut Vec<Kill>,
    ) {
        match message {
            ServerMessage::Welcome(welcome) => {
                self.roster = welcome.roster.iter().cloned().collect();
                out.push(ClientEvent::Welcome(welcome));
            }
            ServerMessage::Refused(reason) => {
                self.disconnected = true;
                self.renet.disconnect();
                out.push(ClientEvent::Refused(reason));
            }
            ServerMessage::Spawned(player) => {
                if let Some(session) = &mut self.session {
                    session.player = Some(player);
                }
            }
            ServerMessage::Joined { player, name } => {
                self.roster.insert(player, name);
            }
            ServerMessage::Left(player) => {
                self.roster.remove(&player);
            }
            ServerMessage::Kill(kill) => kills.push(kill),
        }
    }

    /// Enters the map the server welcomed the client to, with the level's
    /// simulation built under `data` (the server's), and tells the server
    /// the client is ready to play.
    pub fn enter(&mut self, sim: Simulation, data: GameData) {
        self.session = Some(Session::new(sim, data));
        self.send(Channel::Control, protocol::encode(&ClientMessage::Ready));
        self.flush();
    }

    /// Issues the local player's command for this tick: numbers it, runs it
    /// ahead of the server and sends it with the latest few. Returns what
    /// the prediction did, for immediate effects; `None` before the player
    /// is in the game.
    pub fn tick(&mut self, cmd: UserCmd) -> Option<Prediction> {
        let session = self.session.as_mut()?;
        let player = session.player?;
        let cmd = session.predictor.issue(UserCmd {
            view_tick: session.view_tick,
            ..cmd
        });
        let prediction = session.sim.predict(player, &cmd);
        let redundancy =
            (session.data.net.command_redundancy as usize).min(MAX_COMMANDS_PER_PACKET);
        let packet = CommandPacket {
            ack: session.states.latest().map(|state| state.tick),
            commands: session.predictor.latest(redundancy),
        };
        self.send(Channel::Commands, protocol::encode(&packet));
        self.flush();
        Some(prediction)
    }

    /// For a client of a server in this process: moves packets between it
    /// and `server`. Returns `false` for UDP clients and once the
    /// connection is gone.
    pub fn exchange_local(&mut self, server: &mut RenetServer) -> bool {
        self.transport
            .exchange_local(&mut self.renet, server, self.now)
    }

    /// Leaves the server.
    pub fn disconnect(&mut self) {
        self.renet.disconnect();
        self.transport.disconnect();
        self.disconnected = true;
    }

    fn send(&mut self, channel: Channel, message: Bytes) {
        self.transport
            .send(&mut self.renet, self.now, channel, message);
    }

    fn flush(&mut self) {
        if let Some(error) = self.transport.flush(&mut self.renet, self.now) {
            tracing::warn!(%error, "network transport failed");
            self.renet.disconnect_due_to_transport();
        }
    }
}
