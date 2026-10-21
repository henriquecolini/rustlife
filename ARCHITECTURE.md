# Architecture

rustlife is a Cargo workspace. Lower layers know nothing about higher ones,
and nothing below `rl_client` depends on rendering.

```
bins/rustlife ─────────► rl_client ──► rl_server ──┬──► rl_net ──► rl_sim ──► rl_data
bins/rustlife-server ──────────────────► rl_server │                 ▲
                                                   └──► rl_level ────┘
                         rl_client ──► rl_assets          │
                                           │              │
                                           ▼              ▼
                                      source_formats ──► source_fs
bins/rustlife-inspect ──► rl_config ─────────────────────► source_fs
```

| Crate | Responsibility |
|-------|----------------|
| `source_fs` | Steam discovery, `gameinfo.txt`, VPK / loose / zip mounts, the layered `Vfs`, KeyValues parser. No Bevy. |
| `rl_config` | User config file schema and command-line overrides. |
| `source_formats` | Pure parsers for Source file formats. No Bevy. |
| `rl_assets` | Bevy `AssetSource` over the `Vfs` and loaders that turn Source formats into Bevy assets. |
| `rl_data` | Schema, validation and loading of rustlife's own gameplay data (`data/*.ron`). |
| `rl_sim` | Shared simulation: collision, deterministic movement, prop physics (rapier3d), weapons and the Manipulator, damage, pickups, deathmatch rules. Runs headless; no Bevy. |
| `rl_level` | A map's simulation (collision, spawns, entities, prop bodies) from mounted content, without rendering. |
| `rl_net` | Protocol, snapshots and deltas, events, transports, the client endpoint with prediction and interpolation. No Bevy. |
| `rl_server` | The authoritative server: joining, command queues, the tick, snapshots. No Bevy. |
| `rl_client` | The Bevy app: rendering, input, HUD, hosting or joining a game. |

Crates are introduced by the milestone that needs them. As of M7 the workspace
has `source_fs`, `rl_config`, `source_formats`, `rl_assets`, `rl_data`,
`rl_sim`, `rl_level`, `rl_net`, `rl_server`, `rl_client`, the `rustlife`,
`rustlife-server` and `rustlife-inspect` binaries, and `xtask`. Everything up
to `rl_server` runs without Bevy, so the dedicated server is headless and the
simulation and netcode are tested without an app; the client renders what
its connection's replica holds.

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

The client is a state machine (`AppState::MainMenu` / `Connecting` /
`InGame` / `ChangingMap`); everything a map creates is torn down on leaving it, so maps can
be switched freely.

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

## Collision and movement (M3)

1. **Collision model.** `source_formats::collision` reads what players
   collide with: brushes with their bevel sides, the node tree and leaf lump,
   leaf brush lists, displacement triangles, sub-models, brush entities and
   `func_useableladder` segments with their dismount points. The BSP loader
   attaches it to the `MapAsset` (Source space).
2. **Collision world.** `rl_sim::collision::CollisionWorld` answers
   `trace_hull(start, end, hull, mask)` exactly as Source does for brushes,
   plus displacement prisms and solid brush entities, and `point_contents`
   for water.
3. **Data.** `rl_data` loads `data/movement.ron` and `data/simulation.ron`:
   compiled-in defaults, overridable file by file with `--data-dir`, and
   validated with every problem reported by field path.
4. **Movement.** `rl_sim::movement::simulate` advances a `PlayerState` by
   one `UserCmd` tick; see [docs/movement.md](docs/movement.md).
5. **Local play.** `rl_client::player` builds the collision world when the
   map loads, places the player at the start spawn, samples input every
   frame (latching presses until the next tick), simulates in Bevy's
   `FixedUpdate` at the data's tick rate, and puts the camera at the eyes,
   interpolated between the last two ticks. `V` switches to the fly camera
   (noclip) and back. (Since M6 the player's commands go through a
   server and the client predicts them; see Networking below.)

## Physics and the Manipulator (M4)

1. **Collision models.** `source_formats::phy` decodes a model's `.phy`
   (IVP compact surfaces) into convex pieces in Source space, plus each
   solid's mass and surface property. The model loader reads it beside the
   `.mdl`, and placements carry how each prop collides (none, box, hull),
   its spawnflags and its mass scale.
2. **Physics world.** `rl_sim::physics::PhysicsWorld` wraps rapier3d in
   Source units. The map's solid brushes are convex hulls and displacements
   triangle meshes, both fixed. Props are bodies built by `prop_desc` from
   placement plus model, and players are kinematic boxes.
3. **Players vs props.** `rl_sim::scene::Scene` merges the map's trace with
   one against the props' pieces, clipped exactly as brushes are with face
   and bevel planes rebuilt as props move. Movement collides with props
   unchanged. It reports the props it ran into, and they are pushed. Props
   that start touching a player hard report impact damage (`physics.ron`).
4. **The Manipulator** (`rl_sim::tools::manipulator`) grabs, pulls, carries,
   launches and punts props as `tools/manipulator.ron` says.
5. **The tick.** `rl_sim::simulation::Simulation` runs movement, pushing and
   tools for every player, then steps physics. The server runs it each
   fixed tick (the client did until M6). Props with bodies follow their
   poses, interpolated between snapshots and relit as they move.

See [docs/physics.md](docs/physics.md).

## Combat (M5)

1. **Data.** `rl_data` loads the arsenal and the rules: `ammo.ron`, one
   file per weapon (`weapons/`) and per item (`items/`),
   `entity_map.ron`, `player.ron`, `rules/deathmatch.ron` and `hud.ron`,
   checking references between them. Weapons are composed of a clip and
   fire modes whose shots are hitscan, melee, projectile or the
   Manipulator. See [docs/data-driven.md](docs/data-driven.md).
2. **Map entities.** `source_formats::entities` hands every entity of the
   BSP to the simulation, which turns weapon and item classes into pickups
   through the entity map, and `trigger_hurt` into hazards.
3. **The simulation** gives each player health and armor (`combat`) and an
   inventory (`weapons`): the weapon in hand deploys, reloads and fires;
   bullets and swings trace through the map, props and players' boxes,
   projectiles fly and burst, blasts reach whoever the walls do not shelter.
   Deaths score frags in the match (`rules`); the dead respawn away from
   everyone with the loadout. Randomness is seeded (spread by the
   command's number, spawns by the tick), so it is deterministic.
4. **The client** sends weapon choices and reload with each command, draws
   pickups, projectiles and other players from the simulation (`actors`),
   effects from the tick's events (`effects`, with sprites since M7) and
   the HUD, drawn with the UI theme and tuned by `hud.ron` (`hud`).

See [docs/combat.md](docs/combat.md).

## Networking (M6)

1. **Levels.** `rl_level::load` reads what gameplay needs from a map — its
   collision, spawns, entities, and each prop's `.mdl` header and `.phy` —
   and `Level::simulation` adds the props in placement order, so every
   machine numbers their bodies alike.
2. **The server** (`rl_server::Server`) runs the simulation at the fixed
   tick. It admits players (Hello, Welcome with the map's checksum and the
   server's gameplay data, Ready), runs their queued commands within a
   per-tick budget, and sends each player a snapshot of the world as they
   see it, as a delta from a state they acknowledged, with the tick's
   events. `rustlife-server` serves a map over UDP.
3. **The client endpoint** (`rl_net::NetClient`) keeps a replica
   simulation that snapshots overwrite. It predicts its own player with
   the shared movement and weapon code, replays unacknowledged commands
   after each correction, and interpolates everything else between
   snapshots a little in the past. Commands carry the tick being drawn, so
   the server rewinds players for hitscan (lag compensation).
4. **The Bevy client** (`rl_client::net`) hosts each map in-process (also
   on UDP with `--listen`) or joins one with `--connect`. It runs the
   connection before the fixed ticks and issues commands in them. Views read
   the replica (`Game`) and the interpolated world (`Drawn`).

See [docs/netcode.md](docs/netcode.md).

## Map logic, animation, sound and the world (M7)

1. **Map logic.** `rl_sim::logic` runs the map's entity I/O on the server:
   outputs queue events to named entities' inputs, which open doors, press
   buttons, switch triggers, hazards and brushes, teleport, push and blow
   up. Movers (doors, buttons, moving brushes) move in both the collision
   and physics worlds and push or crush the players in their way. Their
   poses and the ambient sounds playing replicate as `LogicState`; the
   tick's map events travel with the others. See
   [docs/map-logic.md](docs/map-logic.md).
2. **Animation.** `source_formats::anim` decodes skeletons, animations and
   sequences (with included models and `.ani` blocks); `rl_assets` turns
   them into joints, inverse bind poses and `AnimationClip`s and skins the
   meshes. The client plays stance sequences on other players, holding
   their weapons' world models, and activities on the view model, drawn by
   its own camera.
3. **Sound.** `source_formats` reads WAV files, soundscripts and
   soundscapes; `rl_assets` loads sounds as Bevy audio. The client plays
   the tick's events, the map's ambient and door sounds, soundscapes and
   footsteps from spatial emitters.
4. **The world.** The BSP loader reads the PVS, leaf clusters and areas
   and the sky camera, and chunks surfaces by material and cell with the
   clusters they lie in. The client culls chunks by the PVS, draws the 3D
   skybox with a camera of its own behind the world, fogs the views and
   draws water. DXT textures stay block-compressed on the GPU.
5. **Effects.** Sprite materials load with all their frames
   (`SpriteLoader`); muzzle flashes, tracers, sparks and explosions draw
   them as `effects.ron` says.

See [docs/presentation.md](docs/presentation.md).

Format notes live in [`docs/formats/`](docs/formats/).

## Coordinates and units

The simulation works in Source units (inches) with Z up, so movement constants
read like their Source counterparts. Conversion to Bevy's Y-up space happens
in exactly one place at the render boundary.
