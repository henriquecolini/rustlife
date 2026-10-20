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

Crates are introduced by the milestone that needs them. As of M2 the workspace
has `source_fs`, `rl_config`, `source_formats`, `rl_assets`, `rl_client`, the
`rustlife` and `rustlife-inspect` binaries, and `xtask`.

## Content pipeline

1. `source_fs::content::mount_game` locates the install and builds a `Vfs`
   whose mounts mirror the engine's search path order, with workshop items
   below `custom/`.
2. `rl_assets` registers the `Vfs` as the Bevy asset source `source://`, so
   any system can `asset_server.load("source://materials/foo.vmt")`.
3. Loaders convert Source formats to Bevy assets on the asset server's worker
   threads; failures produce placeholder assets and a log line, never a panic.

## Loading a map (M1)

1. **Mount the pakfile.** On entering a map, `rl_client` reads the BSP from
   the `Vfs`, extracts lump 40 (an embedded zip) and mounts it *above* every
   other search path. Most world materials in stock maps are per-map
   cubemap *patch* materials that live only there. Leaving the map unmounts it.
2. **Extract geometry.** `rl_assets`' `BspLoader` calls
   `source_formats::bsp::load_geometry`, which in one pass over the visible
   faces produces triangles grouped by material (Source space), lightmap UVs,
   displacement blend alphas, spawn points and the sky name.
3. **Bake to Bevy assets.** The loader converts positions to Bevy space, builds
   one `Mesh` per material, uploads the packed lightmap atlas
   (`Rgba16Float`) and assembles the six sky faces into a cubemap.
4. **Resolve materials.** The client spawns every surface with a placeholder
   material and requests `materials/<name>.vmt`. The `VmtLoader` follows
   `patch` materials to their base and loads the textures, producing a
   `SourceMaterial` (texture handles and flags, no `bevy_pbr`). The client then
   builds the `StandardMaterial` and swaps it in. `WorldVertexTransition`
   surfaces get a second, alpha-blended layer for `$basetexture2`, faded by
   vertex alpha.
5. **Light and frame.** Each surface gets Bevy's `Lightmap` component over the
   shared atlas; the sun and ambient light are excluded from lightmapped
   surfaces so baked light is not counted twice. Lightmap and sky brightness
   divide out the camera exposure, so a luxel of 1.0 renders the texture's own
   colour, as Source's `LightmappedGeneric` does. To match that shader, world
   materials have no specular (reflectance 0) and the camera does no
   tone-mapping.

The client is a two-state app (`AppState::Browser` / `InGame`); everything a
map creates is torn down on leaving it, so maps can be switched freely.

## Props, brush entities and model lighting (M2)

1. **Placements.** `load_geometry` also returns every static prop (`sprp`
   game lump) and `prop_*` entity as a `PropPlacement`, and splits geometry
   by BSP model: model 0 is the world, each `*N` model of a brush entity
   becomes a `BrushEntity`. Brush-entity faces are stored relative to the
   entity's origin, so the client spawns each as a root entity placed by its
   origin and angles, with lightmapped surfaces as children.
2. **Models.** `MdlLoader` reads a `.mdl` with its `.vvd` and `.vtx` into a
   `ModelAsset`: one mesh per model mesh and, per skin, the first existing
   VMT of each material slot (material names share the world's cache). The
   client spawns each prop as a root entity with one child per mesh.
3. **Lighting.** Models carry no lightmaps. Like the world, every prop is
   drawn unlit as `albedo × baked light`, with the light in its vertex
   colours (per instance):
   - **Static props** use `vrad`'s per-vertex bake, `sp_<index>.vhv` in the
     pakfile: direct light and shadows included. Colours are stored per
     LOD0 strip-group vertex and mapped to model vertices.
   - **Other props** are lit as the engine lights dynamic models: the BSP
     leaf's ambient cube at the model's centre, plus every world light
     (`worldlights` lump: point, spot, sun) with a clear line of sight,
     folded into the cube by direction. Line of sight is a segment trace
     through the BSP tree; the sun counts only where the trace ends in a sky
     brush. Each vertex then receives `Σ nᵢ² · cube[side]`.
   - As a fallback, the ambient cubes are also uploaded as a Bevy irradiance
     volume (the same ambient-cube representation) that lights any
     non-lightmapped mesh without its own light, and leaves lightmapped
     surfaces alone.

   There are no real-time lights: all light comes from `vrad`'s bake, so
   nothing is counted twice.

Format notes live in [`docs/formats/`](docs/formats/).

## Coordinates and units

The simulation works in Source units (inches) with Z up, so movement constants
read like their Source counterparts. Conversion to Bevy's Y-up space happens
in exactly one place at the render boundary.
