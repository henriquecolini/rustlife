# Networking

rustlife plays every game through a server, as Source does. The server runs
the [`Simulation`](../crates/rl_sim/src/simulation/mod.rs) authoritatively at a
fixed tick (66 Hz, `simulation.ron`). Clients send it their input and draw
what it sends back. The networking design follows the publicly described
techniques of the Source and Quake 3 engines: client-side prediction,
entity interpolation, lag compensation and delta-compressed snapshots.
The implementation is rustlife's own.

| Crate | Holds |
|-------|-------|
| `rl_level` | A map's simulation, loaded without rendering, so the server and clients build identical ones. |
| `rl_net` | The protocol, snapshots, events, prediction, interpolation, the simulated-network conditioner, and the client endpoint `NetClient`. |
| `rl_server` | `Server`: joining, command queues, the tick, snapshots. |
| `rl_client::net` | The Bevy side: hosting in-process or joining, feeding the views. |
| `rustlife-server` | The dedicated server binary. |

Nothing below `rl_client` depends on Bevy. A test can run a server and
clients in one thread, stepping time by hand, as
[`crates/rl_server/tests/netcode.rs`](../crates/rl_server/tests/netcode.rs)
does.

## Playing

```sh
rustlife --map dm_lockdown                 # host a game, alone (in-process server)
rustlife --map dm_lockdown --listen        # host it for others too (UDP, --port)
rustlife-server                          # a dedicated server, configured by server.ron
rustlife --connect 127.0.0.1:27015 --name Alyx
rustlife --connect 127.0.0.1:27015 --net-sim 100ms,10ms,2%
```

`--net-sim LATENCY[,JITTER[,LOSS]]` adds that much round-trip time (half
each way), random jitter and loss to the client's link, to see how the game
copes.

## Transport and channels

renet 2 provides connections, channels, reliability and message slicing.
`renet_netcode` carries them over UDP (unsecured netcode: there is no
matchmaker to hand out tokens). Messages are serde types encoded with
`postcard`.

| Channel | Direction | Delivery | Carries |
|---------|-----------|----------|---------|
| Commands | client → server | unreliable | the latest few commands |
| Control | both | reliable, ordered | hello, welcome, ready, roster, kills |
| Snapshots | server → client | unreliable | world snapshots with events |

A client hosting its own game connects to its in-process server through
memory. The server hands renet's packets across directly, through a
`DelayLine` each way, so `--net-sim` applies to real packets and renet's
resending is exercised. netcode's transport would otherwise send every
connection's packets, so the server sets the in-process clients' packets
aside first.

## Joining

1. The client connects and says **Hello**: protocol version, name, and a
   hash (`rl_data::content_hash`) of its gameplay data.
2. The server answers **Welcome**: the map's name, the CRC-32 of its BSP
   and, for a workshop map, the item id. It includes its own gameplay data
   as RON files, unless the client's hash shows it already has the same.
   The current roster comes too. A client of another protocol version, or
   one beyond `max_players`, is **Refused** with a reason instead.
3. The client loads the map from its own content. The map file itself is
   never sent: it is Valve's or a workshop author's content. A missing map,
   or one whose checksum differs, ends the attempt with a message naming
   the workshop item when there is one. The client builds the level's
   simulation under the server's data, validated exactly as data files on
   disk are, and says **Ready**.
4. The server joins the player into the simulation and sends them
   **Spawned** with their id. Everyone hears **Joined**, and snapshots
   begin.

Both sides build the simulation with `rl_level`, which adds the map's
props in placement order. Every prop therefore has the same `BodyId` on
every machine, and snapshots can name props by body.

## Commands

Each fixed tick the client builds a `UserCmd` from its input and numbers
it. It runs the command at once for its own player (prediction) and sends
it, together with the few before it (`command_redundancy`), so a lost
packet costs nothing. Each command also carries the tick the client was
drawing (`view_tick`), for lag compensation.

The server queues each player's commands, skipping those it has seen, and
sanitises them: movement within ±1, the view in range, nothing non-finite.
Each tick it runs one command per player, or more to catch up after a
stall: every tick earns a credit, up to `command_backlog`. A player without
a command that tick stands still. A client cannot run faster than the
clock however many commands it sends.

The simulation accepts several commands from one player in a tick and
moves them once for each. Bullet spread is drawn from the command's number,
so the client's predicted shot sprays exactly like the server's.

To keep a small queue at the server (about one and a half commands, enough
to ride out jitter without adding delay), each snapshot says how many of
the client's commands were waiting. The client eases its own fixed tick
rate by up to 3 % to steer that number.

## Snapshots and deltas

Every `snapshot_interval` ticks, the server captures for each client a
`WorldState`, the world as that client sees it:

- every player's motion (movement state, view, weapon in hand) and life;
- for the client's own player only, health, armor, inventory and the
  Manipulator (`Private`). Other players' private state is never sent;
- the pose of every prop that can move;
- which pickups are there, the projectiles in flight, and the match.

It sends the state as a `Snapshot`: the change from the newest state the
client acknowledged (each command packet carries the newest snapshot tick
the client has). Players, props and pickups that did not change are left
out, and so are the parts of a player that did not change. A resting prop
costs nothing. Without a usable baseline, everything is sent, with props
measured against where the level placed them. Both ends keep about a second
of states, so the client rebuilds each full state from its copy of the
baseline and keeps the result as a later baseline.

Each snapshot also carries the events since the client's last one: shots,
explosions, damage, pickups, respawns, tool actions and the match's end.
They are only for show, so losing one with its snapshot is acceptable. The
client's own trigger pulls and bullets are left out, because it predicted
and showed them already. Kills, which the kill feed must not miss, go on
the reliable channel.

## Prediction and reconciliation

The client keeps a replica `Simulation` and never ticks it. Each snapshot
overwrites it: players, props, pickups, projectiles, the match and the tick
count (`WorldState::restore`). Every view reads this replica: the HUD, the
actors and the props.

Only the local player is run ahead, with `Simulation::predict`: movement
and the weapon in hand, through the very code the server runs, against the
map and the props where the server last put them. Prediction never hurts
anyone, never pushes props and leaves the Manipulator to the server.

When the newest snapshot arrives, the client restores the server's state
of its player as of the last command the server ran (`ack`). It drops the
commands up to that one and replays the rest on top. If the result differs
from what it had predicted, it keeps the difference as an error that fades
over about 0.1 s and is added to the camera, so a correction glides rather
than snaps. Differences over 64 units are teleports and are taken at once.

Movement and weapons are deterministic. Without outside influence, the
client is never corrected: the netcode tests run, strafe and jump for
several seconds over 100 ms of lag with jitter and 2 % loss and see no
corrections. Things the client cannot foresee do correct it: another
player's blast, being teleported, a prop pushed on the server before the
client hears of it.

## Interpolation

Everything else is drawn `interpolation_delay` (0.1 s) in the past, between
the two received states either side of that moment. Positions are blended
linearly and orientations along the shortest turn. A player who died,
respawned or moved more than 256 units between the two states is shown at
the newer one rather than sliding there.

The client's estimate of the server's newest tick runs on at the tick rate
between snapshots. It is pulled towards each snapshot's tick by a tenth of
the difference, so arrival jitter does not shake it, and jumps of more than
a quarter second are taken at once. The render tick is that estimate less
the delay, and it never runs backwards.

The prop the local player holds is the exception. It is drawn where the
newest snapshot has it relative to the player's eyes and view, carried to
the predicted eyes and current view, so carrying feels immediate rather
than trailing a round trip behind.

## Lag compensation

By the time a command reaches the server, the target the player aimed at
has moved on. The server keeps every living player's box for each of the
last `max_rewind` seconds of ticks (`simulation::History`). Hitscan shots
and swings are traced against the boxes at the command's `view_tick`,
interpolated between the two ticks around it, exactly as the client drew
them. Props are not rewound, and neither are blasts or the Manipulator's
grab: they act on the present.

The netcode tests check both directions. A shooter on a 150 ms link lands
every shot at a runner where its client draws them. With rewinding turned
off, the same shots all miss.

## Tuning (`data/net.ron`)

| Field | Meaning |
|-------|---------|
| `port` | Default UDP port. |
| `max_players` | Players a server admits. |
| `snapshot_interval` | Ticks between snapshots (1: every tick). |
| `interpolation_delay` | How far in the past others are drawn, seconds. |
| `command_redundancy` | Latest commands repeated in each packet. |
| `command_backlog` | Most ticks of a player's commands run at once. |
| `max_rewind` | How far back hit detection looks, seconds. |
| `timeout` | Seconds of silence before a client is dropped. |

The server sends this file with the rest of its data, so clients follow
the server's values.

## Testing

- `rl_net` unit tests: messages round-trip, delta snapshots rebuild states
  exactly, the delay line's loss, jitter and ordering, the clock never runs
  backwards, interpolation is monotonic.
- `rl_sim/tests/network.rs`: players leaving and rejoining, several
  commands in a tick, spread by command number, rewound hits and the
  rewind window, prediction matching the server tick for tick.
- `rl_server/tests/netcode.rs`: a server and clients over simulated
  conditions: joining with the server's data, a full server, prediction
  over a bad link, corrections, smooth interpolation, lag-compensated hits
  (and misses without rewinding), a thrown prop seen in the same place by
  everyone, leaving, the kill feed, and a game shared over real UDP with
  an in-process player.
