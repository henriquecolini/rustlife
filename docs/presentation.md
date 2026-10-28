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
`run_speed`; walking is the run sequence played slower, as HL2DM's models
have no separate walk. The held weapon's world model is joined to the
player's skeleton at the first bone they share, so it moves with the hand.

When a player dies, their model becomes a ragdoll: the animation stops and
the pelvis, spine, chest, neck, head and limb bones become rigid bodies with
capsule colliders, mass and angular limits at their joints, simulated by
Box3D against the map in Source space. Fingers, toes, clavicles and the
middle spine bones follow the nearest simulated bone. The joint tuning is
the engine's human ragdoll sample's: spherical hips, shoulders and spine
with cone and twist limits, revolute knees and elbows, joint friction, and a
soft spring (1 Hz, damped) toward the pose of death, so a corpse crumples
without going limp. The bones of a corpse collide with each other and with
the map, so an arm lies on the chest and does not pass through it. Pairs that
touch by design are left out: a bone and its parent, and any two bones whose
colliders already overlap in the pose of death, which would otherwise be shoved
apart against the springs.
A corpse is kept stable by limits rather than luck: bones have a sensible
mass (a hand is never a featherweight beside a pelvis), no bone moves faster
than 16 m/s or spins faster than 14 rad/s, overlaps with the map are eased
apart at 1 m/s, and a blow is limited by the speed it would give the bone it
hits, not only by its size, so a bullet nudges a hand and does not fling it.
A corpse that still goes wrong (a bone not a number, or torn from the body)
is frozen where it lay instead of being simulated further.
The pose starts from the one the player died in; the fatal shot pushes the
bone it struck, and a nearby blast throws the whole body outwards. A corpse
is visual only, lies for 25 seconds, and at most eight are kept.

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
| `sounds.ron` | fallback footsteps, hard landings, weapon pickups, deaths, explosions, fallback bullet impacts, breakables by material, buttons by sound number |
| `surfaceproperties.txt` | footstep and bullet-impact sounds from the surface underfoot or struck: concrete, metal, wood, glass, flesh, dirt and the rest |
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
`effects.ron`, on quads facing the camera (explosions excepted):

| Effect | When | Drawn |
|--------|------|-------|
| Muzzle flash | a gun fires | one of `muzzle_flash.sprites` at random, briefly, rolled at random, at the gun's `muzzle` attachment: the view model's for the local player (on the view model's layer), the held world model's for others |
| Tracer | a bullet flies | a streak of `tracer.length` flying at `tracer.speed` from the muzzle to where the bullet stopped, turned about its line to face the camera |
| Impact spray | a bullet or swing strikes the map or a prop | the surface class's sprites (concrete flecks, metal sparks, wood chips, glass shards, water splash) thrown off the surface |
| Bullet decal | a bullet strikes a surface | one of the surface class's decal materials, projected onto the triangles around the hit (so it bends around corners and sticks to props) and multiplied into them |
| Blood | a bullet strikes a player | lit blood sprites thrown out of the wound, and a large blood decal on the floor or wall behind the player |
| Explosion | something blows up | particles the game makes itself (below) |
| Grenade dot | a grenade is in flight | a red dot on its top that flashes with every tick of its fuse and dims between, in step with the tick sound (`grenade.dot_*`) |
| Grenade trail | a grenade is in flight | a red ribbon of where it has been, turned to face the camera, tapering and fading over `grenade.trail_life` |

### Explosions

An explosion uses no mounted-game content. `rl_client::particles` makes the
textures itself (billowing puffs with swirling detail, a bright dot, a
streak, a thin ring) and draws a blast as quads facing the camera whose size
and colour change over their life along a three-stop gradient: a white-hot
flash and an expanding shockwave ring; a fireball of puffs flung out hard
that brake quickly, climb and cool from yellow-white through orange to
soot; black smoke boiling up behind; dust rolling out along the floor when
there is one close below; sparks as streaks along their motion that bounce
off the map; and embers that drift. Fire, sparks, embers, flash and ring are
added to what is behind them, smoke and dust blended over it. A blast
centred on the floor is lifted so the floor does not cut its fireball in
half; one on a wall or in mid-air is centred where it is.

Anyone near a blast also gets the view shaken (fading to nothing at three
times its radius) and, very near, a brief wash of white. A barrel that is
on fire gives off flames, smoke and embers that grow fiercer as its fuse
burns down. `effects.ron`'s `explosion` sets the fireball's size (a share
of the radius) and lift, how many puffs of each kind, how long fire and
smoke last, and how hard the shake is. At most 900 particles live at once.

### Pieces

A breakable prop does not just vanish. Its `.phy` lists, in `break` lines,
the models it breaks into (a crate's wooden chunks, a barrel's sheet-metal
pieces, a bottle's glass shards). Those pieces are modelled in the prop's own
frame, so they are spawned at its origin with its orientation, where they
fit together as the whole did, and then fly apart with the prop's own
velocity plus a burst (harder for a blast), tumble, bounce off the map and
fade after the time the model gives. Props whose model names no pieces
scatter their class's generic gibs (`scripts/propdata.txt`) instead.

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
Screenshots and scripted captures play no sound, so they can be run
unattended; `--silent` (or its alias `--mute`) does the same for any other
run. The volume settings are not changed: every sound's gain is zeroed as
it plays, so a silent run that changes some other setting still saves the
player's own volumes.

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
| `rl_client/src/grenades.rs` | The model's middle on the body's, the dot riding a turned grenade's top, flashing and dimming, fuses keeping time between snapshots, the trail's taper and fade |
| `rl_client/src/fog.rs`, `effects.rs` | Fog from entity keys, sprite frames, tracers' stretch and turn, view model points seen by the main camera |
