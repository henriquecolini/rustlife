//! The client's connection to its game: hosting one in-process, or joining
//! a server.
//!
//! Every game the client plays goes through a server, as in Source:
//!
//! - **Hosting** ([`Network::Host`], the default and what the map browser
//!   starts): on entering a map the client loads its level, starts a
//!   [`Server`] in-process ([`Host`]) — listening on UDP too when given a
//!   port, so others can join — and connects to it in memory.
//! - **Joining** ([`Network::Join`]): the client starts in
//!   [`AppState::Connecting`], connects over UDP, and enters the map the
//!   server's welcome names, if its own copy matches (by checksum).
//!
//! Either way the client is a [`NetClient`] ([`Connection`]): each frame,
//! before the fixed ticks, the [`Receive`] systems run the host (if any), take in
//! snapshots and turn their events into [`Ticked`] messages for the HUD
//! and effects. On the welcome it builds the level's simulation under the
//! server's gameplay data and enters. The replica that every view reads
//! lives in the connection's [`Session`]; [`Game`] is the shortcut to it.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bevy::ecs::schedule::SystemCondition;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use rl_data::GameData;
use rl_level::Level;
use rl_net::client::{ClientConfig, ClientEvent, NetClient, Session, Vote};
use rl_net::conditioner::NetConditions;
use rl_net::interpolation::Interpolated;
use rl_net::protocol::Welcome;
use rl_server::{MapLoader, Server, ServerOptions};
use rl_sim::physics::BodyId;
use rl_sim::simulation::{PlayerId, Simulation};
use source_formats::entities::MapEntity;

use crate::content::Content;
use crate::player::{Gameplay, Movement, Ticked};
use crate::{AppState, CurrentMap, GameVfs};

/// How the client plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Network {
    /// Host each map in-process; with a port, also open it to others over
    /// UDP.
    Host {
        /// The UDP port to listen on, if any.
        port: Option<u16>,
    },
    /// Join the server at this address.
    Join(SocketAddr),
}

/// Who the player is and how they play.
#[derive(Resource, Debug, Clone)]
pub struct NetSettings {
    /// Hosting or joining.
    pub network: Network,
    /// The player's name.
    pub name: String,
    /// Simulated latency, jitter and loss (`--net-sim`).
    pub conditions: NetConditions,
    /// Training dummies a hosted game adds.
    pub dummies: usize,
}

/// The connection to the game's server.
#[derive(Resource, Debug)]
pub struct Connection(pub NetClient);

/// The server of a game this client hosts.
#[derive(Resource, Debug)]
pub struct Host(pub Server);

/// The level a hosted game runs, kept for the welcome.
#[derive(Resource, Debug)]
struct HostedLevel(Level);

/// The level's entities, for what the client looks up about them (a
/// door's sounds, a breakable's material).
#[derive(Resource, Debug, Clone, Default)]
pub struct LevelEntities(pub Arc<Vec<MapEntity>>);

/// Each of the level's prop placements' body, by placement index.
#[derive(Resource, Debug, Clone, Default)]
pub struct LevelBodies(pub Vec<Option<BodyId>>);

/// The players' names, by id.
#[derive(Resource, Debug, Clone, Default)]
pub struct Roster(pub BTreeMap<PlayerId, String>);

/// Everything but the local player as this frame draws it: interpolated a
/// little in the past (see [`rl_net::interpolation`]).
#[derive(Resource, Debug, Clone, Default)]
pub struct Drawn(pub Option<Interpolated>);

/// Where the connection is, for menus to show.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub enum NetStatus {
    /// Not connected.
    #[default]
    Idle,
    /// Connecting to this server.
    Connecting(String),
    /// In a game.
    Playing,
    /// The last connection failed, for this reason.
    Failed(String),
}

/// The game as the client knows it: the replica and the local player.
#[derive(SystemParam)]
pub struct Game<'w> {
    connection: Option<Res<'w, Connection>>,
}

impl Game<'_> {
    /// The map session, once entered.
    #[must_use]
    pub fn session(&self) -> Option<&Session> {
        self.connection.as_ref()?.0.session()
    }

    /// The replica simulation.
    #[must_use]
    pub fn sim(&self) -> Option<&Simulation> {
        self.session().map(Session::sim)
    }

    /// The local player, once in the game.
    #[must_use]
    pub fn local(&self) -> Option<PlayerId> {
        self.session()?.player()
    }

    /// The vote for the next map, once the match is over.
    #[must_use]
    pub fn vote(&self) -> Option<&Vote> {
        self.connection.as_ref()?.0.vote()
    }
}

/// Set while the server moves a running game to another map, so leaving
/// [`AppState::InGame`] to load it does not end the connection.
#[derive(Resource, Default)]
struct ReloadingMap(bool);

/// The old map is unloaded; load the new one.
fn finish_map_change(mut next: ResMut<NextState<AppState>>) {
    next.set(AppState::InGame);
}

/// While a vote is open, the number keys vote for the maps on offer.
fn vote_keys(keys: Res<ButtonInput<KeyCode>>, connection: Option<ResMut<Connection>>) {
    const DIGITS: [KeyCode; 9] = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    let Some(mut connection) = connection else {
        return;
    };
    if connection.0.vote().is_none() {
        return;
    }
    if let Some(index) = DIGITS.iter().position(|&key| keys.just_pressed(key)) {
        connection
            .0
            .cast_vote(u16::try_from(index).unwrap_or(u16::MAX));
    }
}

/// The system set receiving from the server, before the fixed ticks.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Receive;

/// Connects the client to its games.
pub struct NetPlugin {
    /// How to play.
    pub settings: NetSettings,
}

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        let playing = in_state(AppState::InGame).or_else(in_state(AppState::Connecting));
        app.insert_resource(self.settings.clone())
            .init_resource::<Roster>()
            .init_resource::<Drawn>()
            .init_resource::<LevelBodies>()
            .init_resource::<LevelEntities>()
            .init_resource::<NetStatus>()
            .init_resource::<ReloadingMap>()
            .add_systems(OnEnter(AppState::Connecting), connect)
            .add_systems(OnEnter(AppState::ChangingMap), finish_map_change)
            .add_systems(OnEnter(AppState::InGame), host)
            .add_systems(OnExit(AppState::InGame), leave)
            .add_systems(PreUpdate, receive.in_set(Receive).run_if(playing))
            .add_systems(Update, (pace, vote_keys).run_if(in_state(AppState::InGame)));
    }
}

fn client_config(settings: &NetSettings, data: &GameData, games: Vec<String>) -> ClientConfig {
    let data_hash = data
        .to_files()
        .map(|files| rl_data::content_hash(&files))
        .unwrap_or_default();
    ClientConfig {
        name: settings.name.clone(),
        data_hash,
        games,
        conditions: settings.conditions,
    }
}

/// Joining: connects to the server.
fn connect(
    mut commands: Commands,
    settings: Res<NetSettings>,
    gameplay: Res<Gameplay>,
    content: Res<Content>,
    mut status: ResMut<NetStatus>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Network::Join(address) = settings.network else {
        next.set(AppState::MainMenu);
        return;
    };
    match NetClient::connect(
        address,
        client_config(&settings, &gameplay.0, content.mounted_ids()),
    ) {
        Ok(client) => {
            info!("connecting to {address}");
            *status = NetStatus::Connecting(address.to_string());
            commands.insert_resource(Connection(client));
        }
        Err(error) => {
            warn!("cannot connect to {address}: {error}");
            *status = NetStatus::Failed(error.to_string());
            next.set(AppState::MainMenu);
        }
    }
}

/// Hosting: on entering a map, loads its level, starts the server and
/// connects to it in memory.
fn host(
    mut commands: Commands,
    settings: Res<NetSettings>,
    map: Res<CurrentMap>,
    vfs: Res<GameVfs>,
    gameplay: Res<Gameplay>,
    content: Res<Content>,
    connection: Option<Res<Connection>>,
    mut status: ResMut<NetStatus>,
) {
    let Network::Host { port } = settings.network else {
        return;
    };
    if connection.is_some() {
        return;
    }
    let data = gameplay.0.clone();
    let level = match rl_level::load(&vfs.0, &map.0, &data) {
        Ok(level) => level,
        Err(error) => {
            // A map without collision is still worth flying around.
            warn!("{error}; viewing the map without a game");
            *status = NetStatus::Failed(error.to_string());
            return;
        }
    };
    let listen = port.map(|port| SocketAddr::from(([0, 0, 0, 0], port)));
    let games = content.mounted_ids();
    let loader = {
        let (vfs, gameplay) = (vfs.0.clone(), data.clone());
        MapLoader::new(move |name| {
            rl_level::load(&vfs, name, &gameplay).map_err(|error| error.to_string())
        })
    };
    let options = ServerOptions {
        data: data.clone(),
        games: games.clone(),
        maps: rl_level::maps::deathmatch_maps(&vfs.0),
        loader: Some(loader),
        listen,
        dummies: settings.dummies,
    };
    let mut server = match Server::new(&level, options) {
        Ok(server) => server,
        Err(error) => {
            warn!("cannot host {}: {error}", map.0);
            *status = NetStatus::Failed(error.to_string());
            return;
        }
    };
    if listen.is_some() {
        info!("hosting {} on {:?}", map.0, server.addresses());
    }
    let client = server.connect_local(client_config(&settings, &data, games));
    commands.insert_resource(Host(server));
    commands.insert_resource(HostedLevel(level));
    commands.insert_resource(Connection(client));
}

/// Leaving a map: disconnects, and shuts the hosted server down.
fn leave(
    mut commands: Commands,
    mut reloading: ResMut<ReloadingMap>,
    host: Option<ResMut<Host>>,
    connection: Option<ResMut<Connection>>,
) {
    if std::mem::take(&mut reloading.0) {
        return;
    }
    if let Some(mut connection) = connection {
        connection.0.disconnect();
    }
    if let Some(mut host) = host {
        host.0.shut_down();
    }
    commands.remove_resource::<Connection>();
    commands.remove_resource::<Host>();
    commands.remove_resource::<HostedLevel>();
    commands.insert_resource(LevelBodies::default());
    commands.insert_resource(LevelEntities::default());
    commands.insert_resource(Roster::default());
    commands.insert_resource(Drawn::default());
}

/// Runs the host, receives from the server and acts on what it says.
fn receive(
    time: Res<Time<Real>>,
    mut host: Option<ResMut<Host>>,
    connection: Option<ResMut<Connection>>,
    mut ticked: MessageWriter<Ticked>,
    mut roster: ResMut<Roster>,
    mut drawn: ResMut<Drawn>,
    mut welcome: WelcomeParams,
) {
    let Some(mut connection) = connection else {
        return;
    };
    let now = time.elapsed();
    if let Some(host) = &mut host {
        host.0.update(now);
        host.0.exchange(&mut connection.0);
    }
    for event in connection.0.update(now) {
        match event {
            ClientEvent::Welcome(message) => welcome.enter(&mut connection.0, &message),
            ClientEvent::Refused(reason) | ClientEvent::Disconnected(reason) => {
                welcome.fail(&reason);
            }
        }
    }
    if let Some(host) = &mut host {
        host.0.exchange(&mut connection.0);
    }
    if roster.0 != *connection.0.roster() {
        roster.0.clone_from(connection.0.roster());
    }
    for events in connection.0.take_events() {
        ticked.write(Ticked(events));
    }
    drawn.0 = connection.0.session().and_then(Session::interpolated);
}

/// What entering a map on the server's welcome needs.
#[derive(SystemParam)]
struct WelcomeParams<'w> {
    vfs: Res<'w, GameVfs>,
    hosted: Option<Res<'w, HostedLevel>>,
    gameplay: ResMut<'w, Gameplay>,
    movement: ResMut<'w, Movement>,
    bodies: ResMut<'w, LevelBodies>,
    entities: ResMut<'w, LevelEntities>,
    map: ResMut<'w, CurrentMap>,
    reloading: ResMut<'w, ReloadingMap>,
    status: ResMut<'w, NetStatus>,
    state: Res<'w, State<AppState>>,
    next: ResMut<'w, NextState<AppState>>,
}

impl WelcomeParams<'_> {
    /// Enters the welcome's map: checks the local copy, takes the server's
    /// data and builds the level's simulation.
    fn enter(&mut self, client: &mut NetClient, welcome: &Welcome) {
        let data = match &welcome.data {
            Some(files) => match GameData::from_files(files) {
                Ok(data) => data,
                Err(error) => {
                    self.fail(&format!("the server's gameplay data is invalid: {error}"));
                    client.disconnect();
                    return;
                }
            },
            None => self.gameplay.0.clone(),
        };
        let map = &welcome.map;
        let level = match &self.hosted {
            Some(hosted) if hosted.0.name == map.name => Ok(hosted.0.clone()),
            _ => rl_level::load(&self.vfs.0, &map.name, &data),
        };
        let level = match level {
            Ok(level) if level.checksum == map.checksum => level,
            Ok(_) => {
                self.fail(&format!(
                    "your copy of {} differs from the server's",
                    map.name
                ));
                client.disconnect();
                return;
            }
            Err(error) => {
                let hint = map.workshop.map_or_else(String::new, |item| {
                    format!(" (subscribe to workshop item {item} to get it)")
                });
                self.fail(&format!("cannot load {}: {error}{hint}", map.name));
                client.disconnect();
                return;
            }
        };
        let built = level.simulation(&data);
        self.bodies.0 = built.bodies;
        self.entities.0 = Arc::new(level.entities.clone());
        self.movement.0 = data.movement.clone();
        self.gameplay.0 = data.clone();
        client.enter(built.simulation, data);
        *self.status = NetStatus::Playing;
        if *self.state.get() != AppState::InGame {
            self.map.0.clone_from(&map.name);
            self.next.set(AppState::InGame);
        } else if self.map.0 != map.name {
            // The server moved the running game to another map.
            self.map.0.clone_from(&map.name);
            self.reloading.0 = true;
            self.next.set(AppState::ChangingMap);
        }
        info!("entered {}", map.name);
    }

    /// The connection failed: back to the map browser, saying why.
    fn fail(&mut self, reason: &str) {
        warn!("left the game: {reason}");
        *self.status = NetStatus::Failed(reason.to_owned());
        self.next.set(AppState::MainMenu);
    }
}

/// Eases the fixed tick rate so a small queue of commands waits at the
/// server: a client ahead of the server ticks a little slower, one behind a
/// little faster.
fn pace(game: Game, gameplay: Res<Gameplay>, mut fixed: ResMut<Time<Fixed>>) {
    let Some(session) = game.session() else {
        return;
    };
    let base = f64::from(gameplay.0.simulation.tick_interval());
    let wanted = Duration::from_secs_f64(base / f64::from(session.pace()));
    if fixed.timestep() != wanted {
        fixed.set_timestep(wanted);
    }
}
