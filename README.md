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
*Manipulator* that grabs and launches props, and a free-for-all deathmatch
mode, all tunable through RON files in [`data/`](data/).

> **Status:** early development. See the roadmap below.

## Requirements

* Rust 1.90 or newer (`rustup update`).
* Half-Life 2: Deathmatch installed through Steam (app 320).
* Linux, Windows or macOS with a Vulkan, DX12 or Metal capable GPU.

## Finding your game

rustlife looks for the game in this order:

1. `--game-dir <folder>` on the command line;
2. the `HL2DM_DIR` environment variable;
3. `content.game_dir` in the config file;
4. every Steam library listed in `libraryfolders.vdf` (native, Flatpak and
   Snap Steam installs are detected automatically).

The folder to point at is the one containing `hl2mp/`, e.g.
`~/.local/share/Steam/steamapps/common/Half-Life 2 Deathmatch`.

### Config file

`~/.config/rustlife/config.ron` (Linux), `%APPDATA%\rustlife\config\config.ron`
(Windows), `~/Library/Application Support/rustlife/config.ron` (macOS):

```ron
(
    content: (
        game_dir: Some("/path/to/Half-Life 2 Deathmatch"),
        workshop: (
            enabled: true,
            extra_apps: [220],          // also mount Half-Life 2 workshop items
            disabled_items: [],         // workshop item ids to skip
        ),
    ),
)
```

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
```

## Development

```sh
cargo xtask ci              # fmt, clippy -D warnings, tests, docs, audit, asset check
cargo xtask content-tests   # tests that need a real HL2DM install
```

See [CONTRIBUTING.md](CONTRIBUTING.md) and [ARCHITECTURE.md](ARCHITECTURE.md).

## Roadmap

| Milestone | Scope | State |
|-----------|-------|-------|
| M0 | Workspace, CI, content discovery, VPK/VFS, inspect CLI | done |
| M1 | World viewer: BSP, textures, lightmaps, displacements, sky | |
| M2 | Static props and models | |
| M3 | Source-style movement | |
| M4 | Physics props and the Manipulator | |
| M5 | Data-driven weapons, items and rules | |
| M6 | Networking: dedicated/listen servers, prediction, lag compensation | |
| M7 | Animation, audio, doors, 3D skybox, water | |

## Legal

rustlife is licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE) at your option. This covers rustlife's code and
data only. Half-Life 2, Half-Life 2: Deathmatch and their content are
trademarks and copyright of Valve Corporation; rustlife is not affiliated with
or endorsed by Valve. Never commit or redistribute game files: the CI asset
check rejects them.
