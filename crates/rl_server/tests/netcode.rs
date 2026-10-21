//! The server and its clients together, in-process, over links with
//! simulated latency, jitter and loss: joining, data agreement, prediction,
//! interpolation, lag compensation, props, leaving — and one game over real
//! UDP on the loopback interface.

#![allow(
    clippy::expect_used,
    reason = "tests: anything missing must fail the test"
)]

use std::sync::Arc;
use std::time::Duration;

use glam::{Quat, Vec3};
use rl_data::GameData;
use rl_level::{Level, LevelProp};
use rl_net::client::{ClientConfig, ClientEvent, NetClient};
use rl_net::conditioner::NetConditions;
use rl_server::{MapLoader, Server, ServerOptions};
use rl_sim::collision::{ConvexHull, WorldBuilder};
use rl_sim::combat::hitscan::Struck;
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::physics::{Motion, PropDesc};
use rl_sim::simulation::{PlayerId, Spawn, TickEvents};
use source_formats::props::{PropKind, PropPlacement};

/// The game id every test client and server agrees on.
const GAME: &str = "hl2dm";

fn data() -> GameData {
    let mut data = GameData::builtin().expect("builtin data");
    // Matches start the moment the first player is there.
    data.rules.warmup = 0.0;
    data
}

/// A big floor with two spawn points facing each other 400 units apart and
/// a crate between them, off to the side.
fn level(data: &GameData) -> Level {
    named_level(data, "test_arena", 0x1234_5678)
}

/// [`level`] under another name and checksum.
fn named_level(data: &GameData, name: &str, checksum: u32) -> Level {
    let world = WorldBuilder::new()
        .solid_box(
            Vec3::new(-4096.0, -4096.0, -64.0),
            Vec3::new(4096.0, 4096.0, 0.0),
        )
        .build();
    let crate_desc = PropDesc {
        pieces: vec![ConvexHull::from_box(
            Vec3::new(-12.0, -12.0, 0.0),
            Vec3::new(12.0, 12.0, 24.0),
        )],
        origin: Vec3::new(60.0, 0.0, 0.0),
        rotation: Quat::IDENTITY,
        motion: Motion::Dynamic,
        mass: 15.0,
        material: *data.physics.materials.get("wood"),
        linear_damping: 0.0,
        angular_damping: 0.0,
        debris: false,
        start_asleep: true,
        grabbable: true,
        free_on_grab: false,
    };
    Level {
        name: name.to_owned(),
        checksum,
        origin: None,
        world: Arc::new(world),
        spawns: vec![
            Spawn {
                origin: Vec3::new(0.0, 0.0, 1.0),
                yaw: 0.0,
            },
            Spawn {
                origin: Vec3::new(400.0, 0.0, 1.0),
                yaw: 180.0,
            },
        ],
        entities: Vec::new(),
        props: vec![LevelProp {
            placement: PropPlacement::new(
                "models/crate.mdl",
                [60.0, 0.0, 0.0],
                [0.0; 3],
                PropKind::Physics,
            ),
            desc: Some(crate_desc),
        }],
    }
}

/// A server and its in-process clients, stepped one tick at a time.
struct Game {
    server: Server,
    clients: Vec<NetClient>,
    level: Level,
    data: GameData,
    now: Duration,
    tick: Duration,
    /// What the server's ticks did, for tests to inspect.
    server_events: Vec<TickEvents>,
}

impl Game {
    fn new(data: GameData) -> Self {
        Self::with_maps(data, Vec::new(), None)
    }

    /// A game whose players vote between `maps` when a match ends.
    fn with_maps(data: GameData, maps: Vec<String>, loader: Option<MapLoader>) -> Self {
        let level = level(&data);
        let server = Server::new(
            &level,
            ServerOptions {
                data: data.clone(),
                games: vec![GAME.to_owned()],
                listen: None,
                dummies: 0,
                maps,
                loader,
            },
        )
        .expect("server");
        Self {
            server,
            clients: Vec::new(),
            tick: Duration::from_secs_f32(data.simulation.tick_interval()),
            level,
            data,
            now: Duration::ZERO,
            server_events: Vec::new(),
        }
    }

    /// Connects a client over `conditions` and plays until it is in the
    /// game; returns its index.
    fn join(&mut self, name: &str, conditions: &str, data_hash: u64) -> usize {
        let config = ClientConfig {
            name: name.to_owned(),
            data_hash,
            games: vec![GAME.to_owned()],
            conditions: conditions.parse().expect("conditions"),
        };
        let client = self.server.connect_local(config);
        self.clients.push(client);
        let index = self.clients.len() - 1;
        let mut joined = false;
        for _ in 0..600 {
            for event in self.step_with(|_, _| None) {
                if let (at, ClientEvent::Welcome(welcome)) = event
                    && at == index
                {
                    let data = welcome.data.as_ref().map_or_else(
                        || self.data.clone(),
                        |files| GameData::from_files(files).expect("server data"),
                    );
                    let built = self.level.simulation(&data);
                    self.clients[index].enter(built.simulation, data);
                }
            }
            if self.player(index).is_some()
                && self.clients[index]
                    .session()
                    .is_some_and(|s| s.latest().is_some())
            {
                joined = true;
                break;
            }
        }
        assert!(joined, "{name} never got into the game");
        index
    }

    fn player(&self, client: usize) -> Option<PlayerId> {
        self.clients[client].session()?.player()
    }

    /// Runs one tick: the server, the links, each client's update and its
    /// command from `command` (none for `None`). Returns the clients'
    /// events.
    fn step_with(
        &mut self,
        mut command: impl FnMut(usize, &NetClient) -> Option<UserCmd>,
    ) -> Vec<(usize, ClientEvent)> {
        self.now += self.tick;
        let ticks = self.server.update(self.now);
        self.server_events.extend(ticks);
        let mut events = Vec::new();
        for (index, client) in self.clients.iter_mut().enumerate() {
            self.server.exchange(client);
            events.extend(
                client
                    .update(self.now)
                    .into_iter()
                    .map(|event| (index, event)),
            );
            if let Some(cmd) = command(index, client) {
                client.tick(cmd);
            }
            self.server.exchange(client);
        }
        events
    }

    /// Runs `ticks` ticks with every client sending `cmd`.
    fn run(&mut self, ticks: usize, cmd: UserCmd) {
        for _ in 0..ticks {
            self.step_with(|_, _| Some(cmd));
        }
    }

    /// Where the server has a client's player.
    fn server_origin(&self, client: usize) -> Vec3 {
        let player = self.player(client).expect("in the game");
        self.server
            .sim()
            .player(player)
            .expect("player")
            .state
            .origin
    }

    /// Where a client predicts its own player.
    fn predicted_origin(&self, client: usize) -> Vec3 {
        let session = self.clients[client].session().expect("session");
        let player = session.player().expect("player");
        session.sim().player(player).expect("player").state.origin
    }
}

fn walk(forward: f32, side: f32, yaw: f32) -> UserCmd {
    UserCmd {
        forward,
        side,
        yaw,
        ..UserCmd::default()
    }
}

fn local_hash(data: &GameData) -> u64 {
    rl_data::content_hash(&data.to_files().expect("files"))
}

#[test]
fn clients_join_with_the_servers_data() {
    let data = data();
    let mut game = Game::new(data.clone());
    let alice = game.join("Alice", "0ms", local_hash(&data));
    // A client with other data is sent the server's.
    let bob = game.join("Bob", "0ms", 1);
    let bob_session = game.clients[bob].session().expect("session");
    assert_eq!(bob_session.data(), &data);
    assert_ne!(game.player(alice), game.player(bob));
    game.run(5, UserCmd::default());
    let names: Vec<&String> = game.clients[alice].roster().values().collect();
    assert_eq!(names, ["Alice", "Bob"]);
    assert_eq!(game.server.players().count(), 2);
}

#[test]
fn a_full_server_refuses_more_players() {
    let mut data = data();
    data.net.max_players = 1;
    let mut game = Game::new(data.clone());
    game.join("Alice", "0ms", local_hash(&data));
    let config = ClientConfig {
        name: "Bob".to_owned(),
        data_hash: 0,
        games: vec![GAME.to_owned()],
        conditions: NetConditions::NONE,
    };
    let client = game.server.connect_local(config);
    game.clients.push(client);
    let refused = (0..120)
        .flat_map(|_| game.step_with(|_, _| None))
        .any(|(index, event)| {
            index == 1 && matches!(event, ClientEvent::Refused(ref why) if why.contains("full"))
        });
    assert!(refused);
}

#[test]
fn a_client_without_the_servers_game_is_refused() {
    let data = data();
    let mut game = Game::new(data.clone());
    let config = ClientConfig {
        name: "Bob".to_owned(),
        data_hash: local_hash(&data),
        games: vec!["other".to_owned()],
        conditions: NetConditions::NONE,
    };
    let client = game.server.connect_local(config);
    game.clients.push(client);
    let reason = (0..120)
        .flat_map(|_| game.step_with(|_, _| None))
        .find_map(|(_, event)| match event {
            ClientEvent::Refused(why) => Some(why),
            _ => None,
        })
        .expect("refused");
    assert!(reason.contains(GAME), "{reason}");
    assert_eq!(game.server.players().count(), 0);
}

#[test]
fn prediction_converges_over_a_bad_link() {
    let data = data();
    let mut game = Game::new(data.clone());
    let client = game.join("Alice", "100ms,10ms,2%", local_hash(&data));
    // Well clear of the crate: a pushed prop moves on the server before
    // the client hears of it, which prediction cannot foresee.
    let player = game.player(client).expect("player");
    game.server
        .sim_mut()
        .teleport_player(player, Vec3::new(-1500.0, -1500.0, 1.0));
    game.run(60, UserCmd::default());
    let settled = game.clients[client]
        .session()
        .expect("session")
        .corrections();
    // Run, strafe, jump and turn for a few seconds.
    let mut widest = 0.0f32;
    for tick in 0..400u32 {
        let t = tick as f32;
        let cmd = UserCmd {
            forward: if tick % 120 < 80 { 1.0 } else { -1.0 },
            side: (t * 0.05).sin(),
            yaw: t * 0.8,
            buttons: if tick % 60 == 0 {
                Buttons::JUMP
            } else {
                Buttons::NONE
            },
            ..UserCmd::default()
        };
        game.step_with(|_, _| Some(cmd));
        // While moving, the prediction runs ahead of the server's latest
        // word but never wanders off.
        let gap = game
            .predicted_origin(client)
            .distance(game.server_origin(client));
        assert!(gap < 64.0, "tick {tick}: {gap}");
        widest = widest.max(gap);
    }
    assert!(
        widest > 8.0,
        "the client runs ahead of the server: {widest}"
    );
    let session = game.clients[client].session().expect("session");
    assert_eq!(
        session.corrections(),
        settled,
        "nothing but the player moves them: no mispredictions"
    );
    // Standing still, the client settles exactly where the server has it.
    game.run(120, UserCmd::default());
    assert_eq!(game.predicted_origin(client), game.server_origin(client));
    let error = game.clients[client]
        .session()
        .expect("session")
        .prediction_error();
    assert!(error.length() < 0.5, "{error}");
}

#[test]
fn the_server_corrects_a_wrong_prediction() {
    let data = data();
    let mut game = Game::new(data.clone());
    let client = game.join("Alice", "80ms,5ms", local_hash(&data));
    let player = game.player(client).expect("player");
    game.run(30, walk(1.0, 0.0, 0.0));
    // Something the client could not foresee: the server moves the player.
    game.server
        .sim_mut()
        .teleport_player(player, Vec3::new(-1000.0, 500.0, 1.0));
    game.run(60, walk(1.0, 0.0, 0.0));
    game.run(60, UserCmd::default());
    let session = game.clients[client].session().expect("session");
    assert!(session.corrections() > 0);
    assert_eq!(game.predicted_origin(client), game.server_origin(client));
    assert!(
        game.server_origin(client)
            .distance(Vec3::new(-1000.0, 500.0, 1.0))
            < 400.0
    );
}

#[test]
fn others_are_drawn_moving_smoothly() {
    let data = data();
    let mut game = Game::new(data.clone());
    let walker = game.join("Walker", "0ms", local_hash(&data));
    let watcher = game.join("Watcher", "60ms,10ms,5%", local_hash(&data));
    let walker_id = game.player(walker).expect("walker");
    let mut seen = Vec::new();
    for _ in 0..200 {
        game.step_with(|index, _| {
            (index == walker)
                .then(|| walk(0.0, 0.0, 0.0))
                .or(Some(UserCmd::default()))
        });
    }
    for _ in 0..150 {
        game.step_with(|index, _| {
            Some(if index == walker {
                walk(0.0, 1.0, 90.0)
            } else {
                UserCmd::default()
            })
        });
        let drawn = game.clients[watcher]
            .session()
            .and_then(rl_net::Session::interpolated)
            .and_then(|world| world.players.get(&walker_id).copied())
            .expect("the walker is drawn");
        seen.push(drawn.feet.x);
    }
    // Strafing right while facing +Y walks along +X: never backwards.
    assert!(
        seen.windows(2).all(|pair| pair[1] >= pair[0] - 1e-3),
        "{seen:?}"
    );
    assert!(seen.last().expect("seen") - seen[0] > 100.0, "{seen:?}");
}

/// A shooter on a 150 ms link fires the hand cannon four times at a runner
/// crossing in front of them, aiming each time where its client draws the
/// runner. Returns how many shots the server found hit the runner.
fn shots_at_a_runner(data: GameData) -> usize {
    let mut game = Game::new(data.clone());
    let shooter = game.join("Shooter", "150ms", local_hash(&data));
    let runner = game.join("Runner", "0ms", local_hash(&data));
    let runner_id = game.player(runner).expect("runner");
    let shooter_id = game.player(shooter).expect("shooter");
    let magnum = game
        .server
        .sim()
        .arsenal()
        .find_weapon("magnum")
        .expect("magnum");
    {
        let sim = game.server.sim_mut();
        let arsenal = sim.arsenal().clone();
        let ammo = arsenal
            .weapon(magnum)
            .and_then(rl_data::WeaponConfig::ammo_type)
            .and_then(|ammo| arsenal.find_ammo(ammo))
            .expect("ammo");
        let inventory = &mut sim.player_mut(shooter_id).expect("shooter").inventory;
        inventory.give_weapon(&arsenal, magnum);
        inventory.give_ammo(&arsenal, ammo, 100);
        sim.teleport_player(shooter_id, Vec3::new(0.0, -600.0, 1.0));
        sim.teleport_player(runner_id, Vec3::new(-400.0, 0.0, 1.0));
        // Enough health to take every shot.
        sim.player_mut(runner_id).expect("runner").vitals.health = 1000.0;
    }
    let select = UserCmd {
        select: Some(magnum),
        ..UserCmd::default()
    };
    game.step_with(|index, _| (index == shooter).then_some(select));
    game.run(90, UserCmd::default());

    let mut hits = 0;
    for tick in 0..300 {
        // Once the runner is at full speed, aim where the shooter's client
        // draws them and fire every so often.
        let aim = game.clients[shooter].session().and_then(|session| {
            let drawn = session.interpolated()?.players.get(&runner_id).copied()?;
            let eye = session.sim().eye_position(shooter_id)?;
            let to = drawn.feet + Vec3::Z * 36.0 - eye;
            let yaw = to.y.atan2(to.x).to_degrees();
            let pitch = -(to.z.atan2(to.truncate().length())).to_degrees();
            Some((pitch, yaw))
        });
        let fire = tick >= 60 && tick % 70 == 0;
        let (pitch, yaw) = aim.expect("the runner is drawn");
        game.step_with(|index, _| {
            Some(if index == runner {
                walk(1.0, 0.0, 0.0)
            } else {
                UserCmd {
                    pitch,
                    yaw,
                    buttons: if fire { Buttons::ATTACK } else { Buttons::NONE },
                    ..UserCmd::default()
                }
            })
        });
        hits += game
            .server_events
            .drain(..)
            .flat_map(|events| events.shots)
            .filter(|shot| {
                shot.player == shooter_id && shot.trace.struck == Struck::Player(runner_id)
            })
            .count();
    }
    hits
}

#[test]
fn shots_land_where_a_lagging_shooter_aimed() {
    assert_eq!(shots_at_a_runner(data()), 4);
}

#[test]
fn without_lag_compensation_the_same_shots_miss() {
    let mut data = data();
    data.net.max_rewind = 0.0;
    assert_eq!(shots_at_a_runner(data), 0);
}

#[test]
fn everyone_sees_a_thrown_prop_land_in_the_same_place() {
    let data = data();
    let mut game = Game::new(data.clone());
    let thrower = game.join("Thrower", "80ms,10ms,2%", local_hash(&data));
    let watcher = game.join("Watcher", "40ms,5ms,2%", local_hash(&data));
    let thrower_id = game.player(thrower).expect("thrower");
    let manipulator = game
        .server
        .sim()
        .arsenal()
        .find_weapon("manipulator")
        .expect("manipulator");
    game.server
        .sim_mut()
        .teleport_player(thrower_id, Vec3::new(0.0, 0.0, 1.0));
    let select = UserCmd {
        select: Some(manipulator),
        ..UserCmd::default()
    };
    game.step_with(|index, _| (index == thrower).then_some(select));
    game.run(90, UserCmd::default());
    // Look down at the crate 60 units ahead, grab it, then throw it.
    let look = |buttons| UserCmd {
        pitch: 45.0,
        buttons,
        ..UserCmd::default()
    };
    for (buttons, ticks) in [
        (Buttons::ATTACK2, 1),
        (Buttons::NONE, 60),
        (Buttons::ATTACK, 1),
        (Buttons::NONE, 400),
    ] {
        for _ in 0..ticks {
            game.step_with(|index, _| {
                Some(if index == thrower {
                    look(buttons)
                } else {
                    UserCmd::default()
                })
            });
        }
    }
    let body = game
        .server
        .sim()
        .physics()
        .props()
        .next()
        .expect("the crate");
    let server_pose = game.server.sim().physics().pose(body).expect("pose");
    assert!(
        server_pose.position.distance(Vec3::new(60.0, 0.0, 0.0)) > 32.0,
        "the crate was thrown: {server_pose:?}"
    );
    for client in [thrower, watcher] {
        let session = game.clients[client].session().expect("session");
        let drawn = session.interpolated().expect("drawn").props[&body];
        assert!(
            drawn.position.distance(server_pose.position) < 0.01,
            "{drawn:?}"
        );
        let replica = session.sim().physics().pose(body).expect("replica");
        assert!(replica.position.distance(server_pose.position) < 0.01);
    }
}

#[test]
fn players_who_leave_are_gone_for_everyone() {
    let data = data();
    let mut game = Game::new(data.clone());
    let stayer = game.join("Stayer", "20ms", local_hash(&data));
    let leaver = game.join("Leaver", "20ms", local_hash(&data));
    let leaver_id = game.player(leaver).expect("leaver");
    game.run(10, UserCmd::default());
    game.clients[leaver].disconnect();
    game.run(60, UserCmd::default());
    assert!(game.server.sim().player(leaver_id).is_none());
    assert_eq!(game.server.players().count(), 1);
    assert!(!game.clients[stayer].roster().contains_key(&leaver_id));
    let replica = game.clients[stayer].session().expect("session").sim();
    assert!(replica.player(leaver_id).is_none());
}

#[test]
fn kills_reach_every_client() {
    let data = data();
    let mut game = Game::new(data.clone());
    let a = game.join("A", "30ms,5ms,10%", local_hash(&data));
    let b = game.join("B", "30ms", local_hash(&data));
    let victim = game.player(b).expect("b");
    game.server
        .sim_mut()
        .player_mut(victim)
        .expect("victim")
        .vitals
        .health = 1.0;
    let fall = game
        .server
        .sim_mut()
        .teleport_player(victim, Vec3::new(0.0, 900.0, 600.0));
    assert!(fall);
    let mut kills = 0;
    for _ in 0..200 {
        game.step_with(|_, _| Some(UserCmd::default()));
        kills += game.clients[a]
            .take_events()
            .iter()
            .map(|events| events.kills.len())
            .sum::<usize>();
    }
    assert_eq!(kills, 1, "the kill feed hears of the fall once");
}

#[test]
fn a_game_over_udp_shared_with_an_in_process_player() {
    let data = data();
    let level = level(&data);
    let mut server = Server::new(
        &level,
        ServerOptions {
            data: data.clone(),
            games: vec![GAME.to_owned()],
            listen: Some("127.0.0.1:0".parse().expect("address")),
            dummies: 1,
            maps: Vec::new(),
            loader: None,
        },
    )
    .expect("server");
    let address = server.addresses()[0];
    let config = |name: &str| ClientConfig {
        name: name.to_owned(),
        data_hash: local_hash(&data),
        games: vec![GAME.to_owned()],
        conditions: NetConditions::NONE,
    };
    // The host plays in-process while a remote player joins over UDP:
    // netcode must not take the in-process player's packets.
    let mut host = server.connect_local(config("Host"));
    let mut remote = NetClient::connect(address, config("Remote")).expect("connect");
    let start = std::time::Instant::now();
    let joined = |client: &NetClient| {
        client
            .session()
            .and_then(rl_net::Session::latest)
            .is_some_and(|state| state.players.len() == 3)
    };
    while start.elapsed() < Duration::from_secs(10) && !(joined(&host) && joined(&remote)) {
        let now = start.elapsed();
        server.update(now);
        server.exchange(&mut host);
        for client in [&mut host, &mut remote] {
            for event in client.update(now) {
                if let ClientEvent::Welcome(welcome) = event {
                    assert!(welcome.data.is_none(), "same data, not resent");
                    assert_eq!(welcome.map.name, "test_arena");
                    client.enter(level.simulation(&data).simulation, data.clone());
                }
            }
            client.tick(walk(1.0, 0.0, 0.0));
        }
        server.exchange(&mut host);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(joined(&host), "the in-process host sees everyone");
    assert!(joined(&remote), "the UDP player sees everyone");
    assert_eq!(server.players().count(), 2, "the dummy is not a client");
    server.shut_down();
}

#[test]
fn players_vote_for_the_next_map_and_move_to_it() {
    let mut data = data();
    data.rules.time_limit = 1.0;
    data.rules.intermission = 2.0;
    let loader_data = data.clone();
    let loader = MapLoader::new(move |name| Ok(named_level(&loader_data, name, 0xabcd)));
    let maps = vec!["test_arena".to_owned(), "other_arena".to_owned()];
    let mut game = Game::with_maps(data.clone(), maps, Some(loader));
    let alice = game.join("Alice", "0ms", local_hash(&data));
    let bob = game.join("Bob", "0ms", local_hash(&data));

    // The match ends and the vote opens for both.
    for _ in 0..200 {
        game.step_with(|_, _| Some(UserCmd::default()));
        if game.clients.iter().all(|client| client.vote().is_some()) {
            break;
        }
    }
    for client in &game.clients {
        let vote = client.vote().expect("a vote is open");
        assert_eq!(
            vote.options,
            ["other_arena"],
            "the current map is not offered"
        );
    }
    game.clients[alice].cast_vote(0);
    game.clients[bob].cast_vote(0);
    game.run(10, UserCmd::default());
    assert_eq!(game.clients[alice].vote().expect("vote").tally, [2]);
    assert_eq!(game.clients[alice].vote().expect("vote").chosen, Some(0));

    // The scoreboard ends: the server changes map and welcomes everyone.
    let mut welcomed = [false; 2];
    for _ in 0..400 {
        for (index, event) in game.step_with(|_, _| Some(UserCmd::default())) {
            if let ClientEvent::Welcome(welcome) = event {
                assert_eq!(welcome.map.name, "other_arena");
                assert!(welcome.data.is_none(), "the data is already there");
                let built = game.level.simulation(&game.data);
                game.clients[index].enter(built.simulation, game.data.clone());
                welcomed[index] = true;
            }
        }
        if welcomed == [true; 2] && game.server.players().count() == 2 {
            break;
        }
    }
    assert_eq!(welcomed, [true; 2], "both players were welcomed to the map");
    assert_eq!(game.server.map().name, "other_arena");
    assert_eq!(game.server.players().count(), 2, "both rejoined");
    assert!(game.clients[alice].vote().is_none(), "the vote is over");
    game.run(30, UserCmd::default());
    assert!(game.player(alice).is_some() && game.player(bob).is_some());
}
