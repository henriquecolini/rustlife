# Architecture

rustlife is a Cargo workspace. Lower layers know nothing about higher ones,
and nothing below `rl_client` depends on rendering.

```
            bins/rustlife   bins/rustlife-server   bins/rustlife-inspect
                 │                 │                       │
            rl_client          rl_server                   │
                 └──────┬──────────┘                       │
                     rl_net                                 │
                        │                                   │
                     rl_sim ── rl_data                      │
                        │                                   │
                    rl_assets (Bevy asset loaders)          │
                        │                                   │
                  source_formats (BSP, VTF, VMT, MDL, PHY)  │
                        │                                   │
                    source_fs ─────────── rl_config ────────┘
```

| Crate | Responsibility |
|-------|----------------|
| `source_fs` | Steam discovery, `gameinfo.txt`, VPK / loose / zip mounts, the layered `Vfs`, KeyValues parser. No Bevy. |
| `rl_config` | User config file schema and command-line overrides. |
| `source_formats` | Pure parsers for Source file formats. No Bevy. |
| `rl_assets` | Bevy `AssetSource` over the `Vfs` and loaders that turn Source formats into Bevy assets. |
| `rl_data` | Schema, validation and loading of rustlife's own gameplay data (`data/*.ron`). |
| `rl_sim` | Deterministic shared simulation: collision, movement, physics, weapons, rules. Runs headless. |
| `rl_net` | Protocol, transport, prediction, interpolation, lag compensation. |
| `rl_client` / `rl_server` | App composition for each side. |

Crates beyond `source_fs` and `rl_config` are introduced by the milestone that
needs them.

## Content pipeline

1. `source_fs::content::mount_game` locates the install and builds a `Vfs`
   whose mounts mirror the engine's search path order, with workshop items
   below `custom/`.
2. `rl_assets` registers the `Vfs` as the Bevy asset source `source://`, so
   any system can `asset_server.load("source://materials/foo.vmt")`.
3. Loaders convert Source formats to Bevy assets on the asset server's worker
   threads; failures produce placeholder assets and a log line, never a panic.

## Coordinates and units

The simulation works in Source units (inches) with Z up, so movement constants
read like their Source counterparts. Conversion to Bevy's Y-up space happens
in exactly one place at the render boundary.
