# Prop physics and the Manipulator

Implemented in `crates/rl_sim/src/physics/` (the physics world),
`crates/rl_sim/src/tools/manipulator.rs` (the tool) and
`crates/rl_sim/src/simulation.rs` (the tick that ties them to players). All
tuning lives in [`data/physics.ron`](../data/physics.ron) and
[`data/tools/manipulator.ron`](../data/tools/manipulator.ron).

## The physics world

`PhysicsWorld` wraps a [rapier3d](https://rapier.rs) world that works in
Source units, like the rest of the simulation: inches, Z up, kilograms.
Rapier's tolerances are scaled with `length_unit = 39.37` (inches per
metre) so they suit inch-sized geometry. It holds three kinds of body.

| Body | Kind | Collider | Collides with |
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
Building dm_lockdown's 2246 colliders takes about 20 ms.

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

Dynamic props use continuous collision detection, so a thrown prop does not
tunnel through thin walls. After each step every prop's speed is clamped to
`max_speed` and `max_angular_speed`.

## Players and props

Players are not simulated by rapier. They move with rustlife's own
Source-like movement ([movement.md](movement.md)), which must collide with
props exactly as it does with brushes. So `Scene` merges two traces:

- the map's, through the BSP (`CollisionWorld::trace_hull`);
- the props', where each convex piece is turned into clip planes and swept
  with the same exact box clip as brushes, `DIST_EPSILON` included. The
  planes are the piece's faces, plus bevels on every other separating axis
  of box and hull: the world axes, and each hull edge crossed with them. The
  bevels depend on the world axes, so the planes are rebuilt (in world
  space) after every step in which a prop moved. Rapier's broad phase picks
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

With the shipped values, a 30 kg barrel thrown at 1000 units/s deals about
110 damage. A barrel rolling into someone at walking pace deals none. Props
held by someone deal no damage. The player who just let go of a prop is
never hit by it (see below). Damage is reported as `Impact` events. Health
arrives with M5.

## The Manipulator

Each player carries a `Manipulator`. It aims along the view from the eyes,
and its aiming ray stops at the map or at the first prop.

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

Movement is deterministic and allocation-free, so clients will predict it
(M6). Physics runs only on the server, so it is repeatable on one build but
not guaranteed to be bit-identical across machines. Clients will interpolate
props from snapshots.

## In the client

- Every solid prop gets a body once its model, with the model's `.phy`, has
  loaded.
- Props that can move follow their body. Their transform is interpolated
  between the last two ticks' poses, like the camera.
- A moving prop lit by the engine's model lighting (ambient cube plus world
  lights) is relit once it has moved 16 units or turned about 15°, at most
  five times a second.

For scripted checks, `rustlife --grab-prop` goes to the grabbable prop
nearest the spawn and picks it up, and `--throw-after <secs>` throws it.
With `--screenshot`, the shot shows the prop held, or in flight just after
the throw.

## Tests

| Test | Covers |
|------|--------|
| `rl_sim/tests/physics.rs` | Props settling, sliding off a ramp, blocking and carrying players, pushing light vs heavy props, a falling prop hurting a player once, held props, release without self-hits, debris, rotated props, freeing on grab, speed caps |
| `rl_sim/tests/manipulator.rs` | Grabbing within range and mass, pulling, carrying and turning, launching and punting at the configured speeds, dropping, the grip breaking, a thrown prop hurting another player |
| `rl_sim/tests/simulation.rs` | Spawning clear of props, walking while carrying, repeatable runs |
| `rl_sim/tests/real_map.rs` | A real map's physics world, with crates dropped at every spawn landing on the floor (`RUSTLIFE_TEST_BSP`) |
| `rl_client/tests/local_play.rs` | A real map's props becoming bodies in the running client (`RUSTLIFE_TEST_GAME_DIR`) |

`cargo bench -p rl_sim -- simulation` times a whole tick: one player
running into 64 tumbling crates takes about 35 µs (Ryzen desktop, release
build), far inside a 15 ms tick.
