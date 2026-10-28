# rustlife

A multiplayer first-person shooter written in Rust with [Bevy](https://bevyengine.org),
which plays on the maps and with the assets of **your own copy of
Half-Life 2: Deathmatch**.

rustlife contains **no Valve content**. Like OpenMW or Xash3D, it is an engine
that reads the original game files (BSP maps, VPK archives, VTF textures, MDL
models, sounds) directly from a legally purchased install at runtime. Without
an install it will not start. Installed Steam Workshop content is picked up
the same way.

The gameplay is rustlife's own: Source-style movement (air-strafing,
bunny-hopping, surfing), a fully data-driven arsenal including a physics
*Gravity Gun* that grabs and launches props, and a free-for-all deathmatch
mode, all tunable through RON files in [`data/`](data/).

> **Status:** early development. See the roadmap below.

## Requirements

* Rust 1.90 or newer (`rustup update`).
* Half-Life 2: Deathmatch installed through Steam (app 320).
* Linux, Windows or macOS with a Vulkan, DX12 or Metal capable GPU.

## Finding your game

No command-line arguments are needed. rustlife *mounts* games the way Garry's
Mod does: the content of an installed Source game (today Half-Life 2:
Deathmatch, id `hl2dm`) becomes available for maps, models and sounds. To find
the install it searches every Steam library (native, Flatpak and Snap installs,
the libraries listed in `libraryfolders.vdf`, and libraries on secondary drives
under `/mnt`, `/media` and the like). A server and its players must have the
same games mounted to play together.

For installs it cannot find, add the folder containing `hl2mp/` or a Steam
library folder to the config file:

### Config file

`~/.config/rustlife/config.ron` (Linux), `%APPDATA%\rustlife\config\config.ron`
(Windows), `~/Library/Application Support/rustlife/config.ron` (macOS):

```ron
(
    content: (
        games: {
            "hl2dm": (enabled: true, dir: Some("/path/to/Half-Life 2 Deathmatch")),
        },
        library_paths: ["/mnt/games/SteamLibrary"],   // extra Steam libraries
        workshop: (
            enabled: true,
            extra_apps: [220],          // also mount Half-Life 2 workshop items
            disabled_items: [],         // workshop item ids to skip
        ),
    ),
)
```

Developers can override the Half-Life 2: Deathmatch folder with `--game-dir`
or the `HL2DM_DIR` environment variable (in that order of precedence, above
the config file).

### Dedicated server

`rustlife-server` needs no arguments either. Everything is in one file,
`server.ron`, next to the config above; a commented default is written the
first time the server runs. It sets the mounted games, the map list, the
port, the player limit, the match length and the respawn delay.

To host one on a server machine, `docker compose up -d` builds and runs it in
a container that downloads the Half-Life 2: Deathmatch Dedicated Server
itself; [docs/deploy.md](docs/deploy.md) covers that and deploying it with
Coolify.

## Running

```sh
cargo run --release -p rustlife                          # the main menu
cargo run --release -p rustlife -- --map dm_lockdown     # straight into a map (a developer shortcut)
```

The main menu is how players start: **Play** (pick a map to host, optionally
open to others), **Join a server**, **Mounted games** (which Source games
supply content, and where they are) and **Settings** (video, audio, controls
with rebindable keys, your name). `Esc` in a game opens the pause menu; the
match keeps running behind it. In a map you play a
free-for-all deathmatch from a spawn point, with Source-style movement
([docs/movement.md](docs/movement.md)), rustlife's own arsenal and pickups
([docs/combat.md](docs/combat.md)) and the Gravity Gun, a tool that grabs
and throws the map's physics props ([docs/physics.md](docs/physics.md)):

| Input | Action |
|-------|--------|
| Mouse | Look |
| W A S D | Move |
| Space | Jump (swim up in water) |
| Left Ctrl | Crouch |
| Left Shift | Walk |
| E | Use: press a button, open a door, draw from a wall charger; grab or let go of a ladder |
| Left / right mouse | Primary / secondary fire |
| R | Reload |
| 1 – 9 | Choose a weapon by slot |
| Mouse wheel | Browse the weapon list; either mouse button confirms the pick, Esc closes it |
| Q | Last weapon |
| G | Gravity Gun; again to go back to the weapon held before it |
| Tab | Scoreboard |
| Right mouse (Gravity Gun) | Grab a prop (hold to pull a distant one in); press again to drop it |
| Left mouse (Gravity Gun) | Throw the held prop, or punt the one under the crosshair |
| V | Toggle noclip (fly freely; Space / Left Ctrl rise and sink, Shift is faster) |
| Esc | Pause menu: resume, settings, leave the match |
| F11 | Switch between a window and fullscreen (anywhere, menus included) |
| 1 – 4 | Vote for the next map while the scoreboard is up |

Keys are the defaults; rebind them in Settings. F11 is reserved and cannot be
rebound; it changes the same Window mode setting Settings does, so the game
opens the way you left it.

Every game runs on a server. Playing a map from the menu or with
`--map` hosts it in-process; to play with others:

```sh
cargo run --release -p rustlife -- --map dm_lockdown --listen --name Gordon   # host, open on UDP 27015
cargo run --release -p rustlife -- --connect 192.168.1.20:27015 --name Alyx   # join
cargo run --release -p rustlife-server                                        # dedicated server (see server.ron)
```

Each player needs the server's games mounted and their own copy of the map; the server checks both match
and sends its gameplay data, never game files. `--port` picks another UDP
port, and `--net-sim 100ms,10ms,2%` simulates a bad connection. See
[docs/netcode.md](docs/netcode.md).

Other options:

- `--spawn <n>` starts at a different spawn point (when hosting).
- `--dummies <n>` adds training dummies to a hosted game: players who stand at their spawn
  points and respawn when killed.
- `--data-dir <dir>` overrides the built-in gameplay data file by file, e.g.
  with a tweaked `movement.ron`.
- `--screenshot <file.png>` renders one frame non-interactively, saves it and
  exits, which is useful for visual checks. Add `--walk-forward <secs>` to
  capture the player mid-walk, `--grab-prop` to pick up the prop nearest the
  spawn, `--grab-prop --throw-after <secs>` to throw it, `--weapon <id>`
  with `--shoot-for <secs>` to fire at the nearest dummy, or
  `--visit-pickup` to look at the nearest pickup, `--use-mover <class>`
  to press use on the nearest door or button of that class, and
  `--teleport x,y,z[,pitch,yaw]` with `--wait <secs>` to look at a spot a
  while later. `--log-fps` logs the frame rate.
- `--silent` (alias `--mute`) plays no sound without touching the volume
  settings; screenshots and scripted captures are always silent.

Maps' own logic runs: doors, buttons, lifts, teleporters and traps work
as their mappers wired them ([docs/map-logic.md](docs/map-logic.md)).
Players and view models are animated, the game is heard through its own
sounds and soundscapes, and maps' 3D skyboxes, fog and water are drawn
([docs/presentation.md](docs/presentation.md)).

Weapons, items, the rules, sounds, effects and the HUD are all data: see
[docs/data-driven.md](docs/data-driven.md) to change them or add your own.

## Workshop content

Workshop items you are subscribed to for HL2DM (app 320) are mounted
automatically. Items for other Source games can be opted into with
`extra_apps` or `--workshop-app <id>`. Maps from the workshop show up in the
map list labelled with their item id.

## Tools

```sh
cargo run -p rustlife-inspect -- mounts      # search path order
cargo run -p rustlife-inspect -- maps        # available maps and their origin
cargo run -p rustlife-inspect -- ls 'materials/concrete/*.vtf'
cargo run -p rustlife-server -- --map dm_lockdown --dummies 2   # dedicated server
```

## Development

```sh
cargo dev                   # run the client with Bevy linked dynamically (fast rebuilds)
cargo xtask ci              # fmt, clippy -D warnings, tests, docs, audit, asset check
cargo xtask content-tests   # tests that need a real HL2DM install
cargo bench -p rl_sim       # trace and movement benchmarks
cargo build --profile dist  # fully optimised (LTO) client for playing and shipping
```

`cargo dev` builds Bevy as a shared library, so a change to rustlife relinks
only its own code. Use a plain `cargo build` or `cargo run` for a
self-contained binary.

The content tests read their inputs from environment variables and skip
silently when unset: `RUSTLIFE_TEST_GAME_DIR` (an HL2DM install),
`RUSTLIFE_TEST_BSP` (a map: collision, movement and physics checks among
others), `RUSTLIFE_TEST_VTF` and `RUSTLIFE_TEST_PHY` (single files) and
`RUSTLIFE_TEST_SKY` (a directory holding a sky's six `<suffix>.vtf` faces).

See [CONTRIBUTING.md](CONTRIBUTING.md) and [ARCHITECTURE.md](ARCHITECTURE.md).

## Roadmap

| Milestone | Scope | State |
|-----------|-------|-------|
| M0 | Workspace, CI, content discovery, VPK/VFS, inspect CLI | done |
| M1 | World viewer: BSP, textures, lightmaps, displacements, sky, map browser | done |
| M2 | Props and models: static props, prop entities, brush entities, baked model lighting | done |
| M3 | Collision and Source-style movement: walking, air-strafing, bunny-hopping, surfing, crouching, swimming, ladders | done |
| M4 | Physics props and the Manipulator: `.phy` collision, pushing, impact damage, grab/pull/launch/punt | done |
| M5 | Data-driven combat: weapons, items, pickups, damage, health and armor, deathmatch rules, HUD | done |
| M6 | Networking: dedicated/listen servers, prediction, interpolation, lag compensation, data sync | done |
| M7 | Map logic (doors, buttons, triggers, traps, chargers), animation, audio, 3D skybox, PVS, fog, water, sprite effects | done |

## Legal

rustlife is licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE) at your option. This covers rustlife's code and
data only. Half-Life 2, Half-Life 2: Deathmatch and their content are
trademarks and copyright of Valve Corporation; rustlife is not affiliated with
or endorsed by Valve. Never commit or redistribute game files: the CI asset
check rejects them.
