# How the game looks and sounds

The simulation decides what happens. This page covers how the client shows
it: animated players and view models, sound, the parts of the world beyond
plain geometry (3D skybox, visibility, fog, water) and combat effects. All
of it lives in `rl_client`, fed by loaders in `rl_assets` and parsers in
`source_formats`. None of it changes gameplay, so none of it runs on the
server.

## Animation

### Reading it

`source_formats::anim` decodes a model's skeleton and animations from its
`.mdl` (and its `.ani`, when animations are streamed from one), following
the Source SDK's `studio.h`:

- **Bones**: each has a parent and a rest position and rotation.
- **Animations**: per animated bone, a value per frame, either raw (48-bit
  vectors, 48- or 64-bit quaternions) or as run-length encoded 16-bit
  streams scaled by the bone's `posscale` / `rotscale`. Long animations
  are split into sections.
- **Sequences**: what the game plays, with a label (`run_smg1`), an
  activity (`ACT_VM_RELOAD`), looping and delta flags, and a grid of
  animations blended by pose parameters (`move_yaw` for a run's eight
  directions).
- **Included models** (`$includemodel`) lend their sequences. HL2DM's
  player models take theirs from `models/player/male_anims.mdl`, matched
  by bone name.

The model loader also reads each vertex's bone weights, so meshes are
skinned. `rl_assets::skeleton` turns all of it into Bevy terms: joints with
rest transforms and inverse bind poses, and one `AnimationClip` per blend
of each sequence, keyed by joint name.

### Playing it

A skeletal model is spawned with an entity per joint, which its meshes are
skinned to, and its attachments (`muzzle`, `eyes`) as children of their
joints. Its root gets Bevy's `AnimationPlayer` over a graph of every clip,
and a `SequencePlayer` through which the game says what to play: a
sequence, a value on its blend axis and a rate. The sequence player keeps
time, weighs the clips of a blend and fades one sequence into the next over
`player.ron`'s `fade` seconds.

**Other players** play the sequence of their stance with the weapon in
hand. The stance is idle, run, crouch, crouch-walk or jump, and the hold
comes from the weapon's data (`hold: Some("shotgun")` gives `run_shotgun`,
`cidle_shotgun`, ...). A run is blended by the direction of movement
relative to the facing (`move_yaw`) and paced by speed against
`run_speed`. The held weapon's world model is joined to the player's
skeleton at the first bone they share, so it moves with the hand.

**The view model**, the local player's weapon in first person, is drawn by
a second camera with a narrower field of view (Source's 54° across 4:3)
and a depth buffer of its own, so it never sinks into walls. It plays
`ACT_VM_DRAW` when the weapon comes up, `ACT_VM_PRIMARYATTACK` /
`SECONDARYATTACK` (a swing plays `HITCENTER` or `MISSCENTER`) on firing,
`ACT_VM_RELOAD` on reloading, and `ACT_VM_IDLE` in between. These events
come from the local player's prediction, so they play without waiting for
the server.

## Sound

Sounds are named as the mounted game names them, as soundscript entries
(`Weapon_Shotgun.Single`) or files under `sound/`. The client reads the
game's soundscripts (`scripts/game_sounds_manifest.txt`, plus the map's
`maps/<map>_level_sounds.txt`). Playing an entry picks one of its waves,
and a volume and pitch within its ranges. `source_formats::wav` decodes
PCM and Microsoft ADPCM waves, with their loop points; MP3s play as they
are.

Each sound plays from an emitter at a place: panned between the
listener's ears and fading with distance as its sound level says (a
gunshot carries further than a footstep). The local player's own sounds
play at the listener. What plays, by source:

| Source | Sounds |
|--------|--------|
| Weapons (their data's `sounds`) | `fire`, `fire2`, `hit`, `reload`, `deploy`, `pickup`, `drop`, `launch`, `punt`, `dryfire` |
| Items (their data's `sounds`) | `pickup`, and `use` looping while a station is drawn from |
| `sounds.ron` | footsteps, hard landings, weapon pickups, deaths, explosions, bullet impacts on surfaces and flesh, breakables by material, buttons by sound number |
| The map's logic | `ambient_generic`s as the logic switches them; doors' move and stop sounds (`noise1`, `noise2`), carried with them |
| Soundscapes | the ambience of the nearest `env_soundscape` the listener can see and is within the radius of: its loops, and its random sounds every so often |

## The world

### 3D skybox

Source maps build their distant scenery as a miniature, usually a sixteenth
of full size, in a sealed room holding a `sky_camera`. The engine draws
that room behind the world as if seen from the sky camera's position plus
the eye's position divided by the scale. rustlife does the same with a
second camera. It draws first, sees only the skybox's faces and props, and
has the 2D sky behind it. The main camera then draws the world over it
without clearing, so the skybox shows wherever the world leaves sky.

### Visibility

The BSP's visibility lump tells, for each cluster of leaves, which clusters
can possibly be seen from it (the PVS). The world is drawn in chunks, one
per material in each 1024-unit cell, each knowing the clusters its faces
lie in. As the camera moves into another cluster, chunks no cluster it can
see holds are hidden. Props stay drawn, and from outside the map (noclip
in the void) everything shows.

### Fog and water

An `env_fog_controller` fogs the main view, and the `sky_camera`'s own
fog keys fog the skybox, scaled down to it. Water surfaces are drawn
translucent and unlit, their texture tinted by their material's fog
colour. With the eyes under
water, the view fogs to that colour over the water material's distances.

### Textures

DXT-compressed textures stay block-compressed on the GPU (BC1–3) where it
supports that, with their own mip chains: a quarter to an eighth of the
memory of decoding them, which is what lets the larger maps fit in a 2 GB
card. Alpha-tested materials (grates, foliage) are drawn with alpha to
coverage, so fine holes stay smooth rather than shimmering.

## Effects

Combat effects draw the game's own sprite materials, named in
`effects.ron`, on quads facing the camera:

| Effect | When | Drawn |
|--------|------|-------|
| Muzzle flash | a gun fires | one of `muzzle_flash.sprites` at random, briefly, rolled at random, at the gun's `muzzle` attachment: the view model's for the local player (on the view model's layer), the held world model's for others |
| Tracer | a bullet flies | a streak of `tracer.length` flying at `tracer.speed` from the muzzle to where the bullet stopped, turned about its line to face the camera |
| Sparks | a bullet or swing strikes the map or a prop | the `impact` sprite just off the surface |
| Explosion | something blows up | the `explosion` sprite played through its frames at `fps`, sized by the blast's radius and lifted by `rise` |

`rl_assets::SpriteLoader` loads a sprite material with every frame of its
texture, laid side by side in one image, and whether it adds its colour to
what is behind it (`$additive`, or the `Sprite` shader). A tracer starting
at the view model's muzzle is moved to where the main camera sees that
point on screen, since the view model is drawn with a narrower view.

## Trying it

```sh
cargo run --release -p rustlife -- --map dm_lockdown --dummies 1 --weapon smg --shoot-for 1 --screenshot /tmp/smg.png
cargo run --release -p rustlife -- --map dm_lockdown --dummies 1 --weapon launcher --shoot-for 0.2 --wait 0.5 --screenshot /tmp/blast.png
```

`--teleport x,y,z,pitch,yaw` stands the player at a spot (to look at a
skybox, or into water), and `--log-fps` logs the frame rate every second.

## Tests

| Test | Covers |
|------|--------|
| `source_formats/src/anim/tests.rs` | Skeletons, animations and sequences read from a built model, run-length streams, packed quaternions, garbage refused |
| `rl_assets/src/skeleton.rs` | Rest poses converted and inverted, one clip per animation shared between sequences, blends split between neighbours |
| `rl_client/src/animation.rs` | Looping and holding sequences, fading one into the next |
| `rl_client/src/actors.rs`, `viewmodel.rs` | Stances following movement, runs blended by direction, view models facing the view, swings playing hit or miss |
| `source_formats/src/wav.rs` | 8- and 16-bit PCM, Microsoft ADPCM, loops from cue points, garbage refused |
| `source_formats/src/soundscript.rs`, `soundscape.rs` | Entries' waves, ranges and levels, manifests, soundscapes with their children flattened |
| `rl_client/tests/sounds.rs` | Every sound the data names resolving and decoding (needs an install) |
| `source_formats/src/visibility.rs` | PVS decompression, faces in their leaves' clusters, and a real map's surfaces, clusters and sky camera (needs a map) |
| `rl_client/src/fog.rs`, `effects.rs` | Fog from entity keys, sprite frames, tracers' stretch and turn, view model points seen by the main camera |
