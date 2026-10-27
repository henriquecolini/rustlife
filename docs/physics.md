# Prop physics and the Manipulator

Implemented in `crates/rl_sim/src/physics/` (the physics world),
`crates/rl_sim/src/tools/manipulator.rs` (the tool) and
`crates/rl_sim/src/simulation/` (the tick that ties them to players). All
tuning lives in [`data/physics.ron`](../data/physics.ron),
[`data/tools/manipulator.ron`](../data/tools/manipulator.ron) and, for how
often the Manipulator acts, [`data/weapons/manipulator.ron`](../data/weapons/manipulator.ron).

## The physics world

`PhysicsWorld` wraps a [Box3D](https://github.com/larsbrubaker/box3d-rust)
world that works in Source units, like the rest of the simulation: inches,
Z up, kilograms. Box3D's global `length_units_per_meter` is set to `39.37`
(inches per metre) so its tolerances and defaults suit inch-sized geometry.
It holds three kinds of body.

| Body | Kind | Shapes | Collides with |
|------|------|----------|---------------|
| The map | fixed | one convex hull per solid brush, one triangle mesh per displacement | props, debris |
| Props | dynamic or fixed | the convex pieces of the model's `.phy` (or its bounding box) | map, props, debris, players |
| Debris props | dynamic | as props | map, props |
| Players | kinematic | the movement hull's box | props |

**Map geometry.** The map's brushes are the same ones players collide with.
A brush blocks props if it is solid, glass, a grate or a moving brush
entity. Clip brushes block props only when they clip both players and NPCs
(`tools/toolsclip`). A brush's convex hull is the set of corners where three
of its planes meet inside all of them. Displacements become triangle meshes.

**Props.** `prop_desc` turns a placement into a body description:

- *Shape*: the convex pieces of the model's only solid, scaled with the
  model. Models without a usable `.phy` (none, unreadable, or a ragdoll with
  one solid per bone) collide as their bounding box, as does a placement
  that asks for a box (static prop solid type `bbox`, entity `solid 2`).
  Props with no collision (`solid 0`) get no body.
- *Mass*: the `.phy`'s declared mass times the entity's `massScale`. For a
  bounding box it is the material's density times the box's volume.
- *Material*: friction, restitution and density come from `physics.ron`,
  looked up by the solid's `surfaceprop` (else the model's `$surfaceprop`).
  A name falls back to its prefix before the last underscore, so
  `metal_barrel` uses `metal` unless it is listed itself, and then to the
  default.
- *Motion*: `prop_physics*` (and ragdolls, as one body) are dynamic; static
  props and other prop entities are fixed.
- *Spawnflags*: `1` starts asleep, `4` makes the prop debris (players pass
  through it), `8` fixes it in place, `64` frees a fixed prop when it is
  grabbed, and `512` prevents pickup.

Dynamic props use Box3D's continuous collision detection, and props moving
faster than `ccd_min_speed` are marked as bullets for the step, so a thrown
prop does not tunnel through thin walls or other props. After each step
every prop's speed is clamped to `max_speed` and `max_angular_speed`.

### Constraints

Maps hang and hinge props with constraint entities. The lamps of
`dm_steamlab` and `dm_powerhouse` hang from the ceiling on ball sockets.
Each constraint joins the props named by its `attach1` and `attach2`
(their `targetname`). A name that is empty, or that names something other
than a simulated prop, stands for the world. `Level::simulation` adds them
as joints once the props have their bodies, so the server and every
replica hold the same props the same way:

| Entity | Joint |
|--------|-------|
| `phys_ballsocket` | the two turn freely about `origin` |
| `phys_hinge` | they turn only about the line from `origin` to `hingeaxis` |
| `phys_lengthconstraint` | `origin` on the first and `attachpoint` on the second stay at most their distance plus `addlength` apart |
| `phys_constraint` | welded as placed |

A joint holds the bodies as the map placed them, and `No collision until
break` stops the joined pair colliding with each other. Not simulated yet:
constraints that start inactive, breaking under `forcelimit` /
`torquelimit`, `phys_motor`, and anything joined to a `func_physbox`
(which is not simulated at all). Those are logged at debug level when a
level is built.

## Players and props

Players are not simulated by Box3D. They move with rustlife's own
Source-like movement ([movement.md](movement.md)), which must collide with
props exactly as it does with brushes. So `Scene` merges two traces:

- the map's, through the BSP (`CollisionWorld::trace_hull`);
- the props', where each convex piece is turned into clip planes and swept
  with the same exact box clip as brushes, `DIST_EPSILON` included. The
  planes are the piece's faces, plus bevels on every other separating axis
  of box and hull: the world axes, and each hull edge crossed with them. The
  bevels depend on the world axes, so the planes are rebuilt (in world
  space) after every step in which a prop moved. Box3D's broad phase picks
  the props near a sweep.

The nearer hit wins, and the trace reports which body it hit. Movement uses
that to report the props it ran into: their side, not their top. Each tick
the simulation then **pushes** those props. The player counts as a body of
`push.player_mass` meeting the prop in a perfectly inelastic collision along
the contact normal:

```text
Δv = min(v_into · M / (M + m), max_speed_change)
```

So a 5 kg crate is shoved along at walking speed and a 200 kg dumpster
creeps.

Players also have a **kinematic box** in the physics world, moved to the
player's position each tick. Props land on players and bump into them
instead of passing through.

## Impact damage

A prop that *starts* touching a player during a step has hit them. The
closing speed is the prop's velocity before the step, relative to the
player, along the contact normal. `physics.ron` turns speed and mass into
damage:

```text
E = ½ · m · (v · 0.0254)²          (joules)
damage = clamp((E − min_energy) · damage_per_joule, 0, max_damage)   if v ≥ min_speed
```

Box3D reports the contact's approach speed and normal. With the shipped
values, a 30 kg barrel thrown at 1000 units/s deals about 110 damage. A
barrel rolling into someone at walking pace deals none. Props held by
someone deal no damage. The player who just let go of a prop is never hit
by it: a filter joint keeps their contacts apart until their bounds
separate (see below). Damage is reported as `Impact` events and
taken from the player's health; a prop someone threw, punted or dropped in
the last `prop_credit` seconds (`rules/deathmatch.ron`) is theirs, so a kill
with it is their frag (see [combat.md](combat.md)).

## Breakable props and explosive barrels

A physics prop whose model says it breaks (`prop_data` in the model, or a
class in `scripts/propdata.txt`; a map entity's own `health` wins) has
health. Blows (a bullet, a swing, a blast, a hard impact) queue up as the
tick runs and are applied at its end, each scaled by what the prop is made
of: a crate takes more from a club than a bullet. Model health is
multiplied by `breakables.health_scale` — Source's `prop_data` values are
tuned for a lighter damage model — so bottles, boxes and sinks take a few
solid hits rather than one. A new prop's first contacts while it settles
into the map — pushing itself out of a floor or a neighbouring prop — do not
damage it, so a tightly placed crate does not break at map load. A prop
whose health runs out *breaks*: it leaves the physics world (its body id
stays), everyone is
told (`PropBreak`, and the replicated `broken` set for those who join
later), the client scatters its gibs as small Box3D bodies (the chunk's
`.phy` convex pieces, or its bounds) that collide with the map and nothing
else, and the prop comes back where it was
if it respawns (`prop_physics_respawnable`).

**Explosive barrels** do not use health. Bullets and knocks cannot break
one, so shoving one off a ledge, or dropping it, does nothing. Instead a
barrel has a fuse (`breakables.barrels` in `physics.ron`):

| What happens to it | Result |
|--------------------|--------|
| shot once | primed; nothing shows |
| shot while primed | set on fire: it explodes after `burn_seconds` |
| shot while burning, or hit with a crowbar | explodes at once |
| a blast of `instant_damage` or more at the barrel | explodes at once |
| a weaker blast | set on fire, with a fuse between `longest_fuse` (barely reached) and `shortest_fuse` (just short of instant); a burning barrel keeps the sooner fuse |
| thrown or punted with the Manipulator | explodes on the first thing it touches, for `launched_seconds` |

Whoever lit the fuse is credited with the blast and its kills, so
barrels set off by other barrels chain, each credited to the first
shooter. The server sends `PropIgnite` when a barrel catches fire so
clients can draw the flames (`rl_client::particles`).

## The Manipulator

Each player carries a `Manipulator`, a weapon in slot 1 (see
[combat.md](combat.md)). While it is in hand and deployed it aims along the
view from the eyes, and its aiming ray stops at the map or at the first
prop. Putting it away lets go of the held prop. After each action it rests
for its fire mode's `interval` from `weapons/manipulator.ron`: primary
after a launch, punt or dry fire, secondary after a grab or drop.

| Input | Action |
|-------|--------|
| Secondary fire, held | Grab a grabbable prop within `grab.range` weighing at most `grab.max_mass`. Pull one within `grab.pull_range` towards you at `grab.pull_speed` until it is close enough. |
| Secondary fire, pressed while holding | Let go. Secondary fire must then be released before it reaches for another prop. |
| Primary fire while holding | Launch the prop along the view at `launch.speed × mass_curve(mass)`. |
| Primary fire otherwise | Punt the dynamic prop under the crosshair within `punt.range`: an impulse at the point hit, for a speed change of `punt.speed × mass_curve(mass)`. |

The **mass curves** are `(kg, factor)` points, interpolated linearly and
held flat past the ends. Heavier props leave slower.

**Holding.** While held, a prop ignores gravity and does not collide with its
holder. Its centre of mass is steered to the hold point, `hold.distance` plus
the prop's radius in front of the eyes. Its orientation follows the view's
yaw, keeping the orientation it had relative to the player when grabbed. The
steering sets the prop's velocity each tick rather than moving it:

```text
v = clamp((target − centre) / follow_time, max_speed)
ω = clamp(axis · angle / follow_time, max_angular_speed)
```

So the held prop still collides. It shoves other props aside and cannot pass
through walls. If something holds it more than `hold.break_distance` from
the hold point for `hold.break_time`, the grip breaks.

**Letting go.** A prop is usually held overlapping its holder's box, for
example when looking down. Once let go, the prop and its former holder keep
ignoring each other until their bounds come apart. So a prop thrown from up
close neither shoves the thrower nor hits them.

## The tick

`Simulation::tick` runs, for each player with a command:

1. **Move**, against the map and its props. The prop the player holds is
   ignored.
2. **Push** the props the player ran into.
3. Run the player's **Manipulator** from where they now stand.

It then moves every player's kinematic box and **steps** physics, collecting
impacts.

Movement is deterministic and allocation-free, so clients predict it.
Physics runs only on the server, so it is repeatable on one build but not
guaranteed to be bit-identical across machines. Clients draw props
interpolated from snapshots ([netcode.md](netcode.md)).

## In the client

- Every solid prop has a body, made when the level was loaded (`rl_level`,
  from the model's header and `.phy`) and numbered by placement, so the
  server and every client agree on it.
- Props that can move follow their body as the server's snapshots place
  it, interpolated a little in the past. The prop the player holds is drawn
  relative to their current view instead, so carrying feels immediate.
- A moving prop lit by the engine's model lighting (ambient cube plus world
  lights) is relit once it has moved 16 units or turned about 15°, at most
  five times a second.

For scripted checks, `rustlife --grab-prop` goes to the grabbable prop
nearest the spawn and picks it up, and `--throw-after <secs>` throws it.
With `--screenshot`, the shot shows the prop held, or in flight just after
the throw. These scripts move the player on the hosted server.

## Tests

| Test | Covers |
|------|--------|
| `rl_sim/tests/physics.rs` | Props settling, sliding off a ramp, blocking and carrying players, pushing light vs heavy props, a falling prop hurting a player once, held props, release without self-hits, debris, rotated props, freeing on grab, speed caps, a lamp hanging on a ball socket and swinging back when hit, a rope's length, joints to missing bodies refused |
| `rl_sim/src/physics/joints.rs` | Reading ball sockets (attached to the world either way), hinges, length constraints, and what is left out |
| `rl_level/tests/level.rs` | A named prop hanging on the map's ball socket through `Level::simulation` |
| `rl_sim/tests/manipulator.rs` | Grabbing within range and mass, pulling, carrying and turning, launching and punting at the configured speeds, dropping, the grip breaking, a thrown prop hurting another player |
| `rl_sim/tests/simulation.rs` | Spawning clear of props, walking while carrying, repeatable runs |
| `rl_sim/tests/real_map.rs` | A real map's physics world, with crates dropped at every spawn landing on the floor (`RUSTLIFE_TEST_BSP`) |
| `rl_client/tests/local_play.rs` | A real map's props becoming bodies in the running client (`RUSTLIFE_TEST_GAME_DIR`) |

`cargo bench -p rl_sim -- simulation` times a whole tick with one player
running into 64 tumbling crates.
