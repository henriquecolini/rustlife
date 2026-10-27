# Player movement

rustlife's players move like Source-engine players: quick ground
acceleration with friction, air control that rewards strafing, bunny-hops,
surfable ramps, stepping up ledges, crouch-jumps, swimming and ladders. The
algorithm follows the publicly documented structure of Source's
`CGameMovement`. It is written afresh in `rl_sim::movement` and
cross-checked against open recreations and write-ups of it.

All tuning lives in [`data/movement.ron`](../data/movement.ron) (see
[`rl_data::MovementConfig`](../crates/rl_data/src/movement.rs)). Units are
Source units (1 unit = 1 inch) and seconds. Angles are degrees, with positive
pitch looking down and yaw 0 facing +X.

## The tick

`rl_sim::movement::simulate(state, cmd, config, dt, world)` advances one
player by one tick (`dt = 1 / tick_rate`, 66 Hz by default, set in
`simulation.ron`). It is a pure function. The same inputs always give
bit-identical outputs, and it allocates nothing. That is what lets a client
predict its own player with the code the server runs. Each tick runs these
steps:

1. **Sanitise.** Every velocity component is clamped to `max_velocity`, and
   non-finite values are zeroed. A player overlapping a prop is then pushed
   out of it, at most `unstick_speed` per second and never into the map
   (see [physics.md](physics.md)).
2. **Duck** (`duck.rs`). Crouching on the ground takes `duck.duck_time`.
   The view lowers over that time and the hull shrinks at the end. Crouching
   in the air is instant and lifts the feet by the difference in hull
   heights, so the head stays put. That is what makes crouch-jumps clear
   higher ledges. Standing up needs room for the standing hull: under a low
   ceiling the player stays down.
3. **Ladders** (`ladder.rs`). See below. A tick on a ladder ends here.
4. **Categorize** (`ground.rs`). The water level comes from the contents at
   the feet, waist and eyes. The player is on the ground if a surface with
   normal Z ≥ `ground.floor_normal_z` lies within 2 units below. A player
   rising faster than 140 units/s is never on the ground, so a jump is not
   cancelled by the floor it leaves.
5. **Swim** (`water.rs`) when at least waist-deep. Otherwise:
6. **Walk or fly**:
   1. Apply half of the tick's gravity.
   2. Jump if the button was freshly pressed (or held, with
      `jump.auto_hop`). The launch speed is `√(2 · gravity · jump.height)`.
   3. On the ground, apply friction and then **walk**: accelerate towards
      the wished velocity and cap at the top speed. Move, stepping up ledges
      if blocked, and stay glued to slopes going down.
   4. In the air, **fly**: apply capped air acceleration, then slide.
   5. Categorize again, then apply the other half of gravity. Splitting
      gravity this way integrates a jump's arc symmetrically.

The tick also reports events: a jump, and a landing with the fall speed
(for sounds, and for fall damage: see [combat.md](combat.md)).

## Friction and acceleration

**Friction** removes `ground.friction` × speed per second. Below
`ground.stop_speed` it is computed as if moving at that speed, so slow
sliding stops completely. With the defaults (friction 4, stop speed 100), a
player running at 320 units/s stops in about half a second.

**Acceleration** adds speed along the wished direction only until the
velocity's component *along that direction* reaches the wished speed:

```
add   = wish_speed − velocity · wish_dir
gain  = min(accelerate · wish_speed · dt, add)
velocity += gain · wish_dir
```

On the ground, `wish_speed` is the top speed: `ground.run_speed`, or
`ground.walk_speed` with walk held, times `duck.speed_scale` while crouched.
After accelerating, ground speed is capped at that top speed.

## Air-strafing and bunny-hopping

In the air, the wished speed used in `add` is capped at `air.speed_cap`
(30 units/s), but `gain` still scales with the full wished speed. Strafing
right while turning the view right keeps the wished direction nearly
perpendicular to the velocity. There, `velocity · wish_dir` is below the
cap, so each tick adds a little speed. The gain along the velocity peaks
when that component is half the cap, at `acos(15 / speed)` from the
velocity: about +1 unit/s per tick at 320 units/s.

A jump taken on the tick of landing happens before ground friction. If the
player hops every time they land, friction never acts, and the speed gained
by strafing carries from hop to hop. That is bunny-hopping. Without auto-hop,
the jump button must be released and pressed again for each hop. A press in
the air counts as "held", so holding jump through a landing does not hop.

## Sliding, stepping and surfing (`slide.rs`)

**Sliding.** This is Source's `TryPlayerMove`. The player moves along the
velocity until something is hit, then removes the velocity component going
into the surface:

```
clipped = v − n · (v · n) · overbounce
```

The rest of the tick is spent moving along the surface, up to four times.
Against two surfaces at once, the only way forward is along their crease
(`n₁ × n₂`). In a corner of three, the player stops. If a clip turns the
velocity back against its original direction, the player also stops, which
avoids jitter in corners.

rustlife clips with an overbounce of **1.001** instead of exactly 1. A
clipped velocity is parallel to the surface only up to rounding, and can
point a sliver *into* it. A hull resting within the clip epsilon of a slope
would then hit it at fraction 0 on every bump and stop dead mid-slide. That
is the "ramp bug" known from Source surf servers. The extra thousandth moves
the player away from a surface by less than a unit per second of contact,
which is invisible.

**Stepping.** When a walking move is blocked, it is tried twice and the
version that got farther horizontally wins:

- as-is;
- lifted by `ground.step_size`, moved, then dropped back down.

A ledge up to the step size is climbed. If the lifted move ends somewhere
that is not a floor, the plain move is used. The lifted move keeps the plain
move's vertical speed, so stepping never launches the player.

**Sticking to slopes.** After a walking move, if a floor lies within a step
below, the player moves down onto it. Walking down stairs or slopes then
stays grounded instead of turning into a series of small falls.

**Surfing** needs no special code. A surface steeper than
`ground.floor_normal_z` is never a floor, so a player on one is in the air.
Sliding redirects their velocity along it, gravity only bends the path, and
strafing into the ramp holds them on it.

## Swimming (`water.rs`)

At least waist-deep, there is no gravity and no floor:

- **Direction.** The player swims in the full 3D direction of the view, at
  `water.speed_scale` × top speed.
- **Resistance.** Water friction removes `water.friction` × speed per
  second.
- **Drift.** Without input, the player sinks at `water.sink_speed`.
- **Jump** swims straight up. Once the waist clears the surface, normal
  movement takes over. A wading player (feet in water) who is not rising may
  jump even off the ground, which lets them climb out of a pool.

## Ladders (`ladder.rs`)

HL2-era maps place ladders as `func_useableladder` entities: a segment
between two feet positions, plus `info_ladder_dismount` points nearby.

- **Grab** with *use* within `ladder.mount_distance` of the ladder's axis.
  The player snaps onto the axis if there is room.
- **Climb** with forward/back at `ladder.climb_speed`. Looking down by more
  than 45° makes forward climb down. There is no gravity on a ladder.
- **Let go** with *use*. The player steps onto the nearest free dismount
  point within `ladder.dismount_range`, or drops where they are. Climbing
  past the top does the same, or steps forward if no dismount point is
  near. Climbing past the bottom lets go there.
- **Jump off** with *jump*. The player is pushed backwards, away from the
  ladder they face, at `ladder.jump_off_speed`.

## Collision (`rl_sim::collision`)

Movement only asks one question: if this box moved from here to there, how
far would it get and what would it hit? `trace_hull` answers it the way the
Source engine does:

- **Brushes.** The box descends the BSP tree along its path. At each node,
  the splitting plane is thickened by the box's support distance
  `|n|·extents`. A sweep wholly on one side descends that side; one that
  straddles the plane is split, near half first. In each leaf, the box is
  clipped against the leaf's brushes. A box swept through a convex brush is
  the box's centre swept through the brush grown by the box. Each plane
  moves out by the support distance, and the sweep enters at the last
  entering crossing and leaves at the first leaving one. The compiler's
  *bevel* planes make this exact at edges and corners; rays skip them.
- **The epsilon.** Crossings are pulled back by 1/32 unit
  (`DIST_EPSILON`), so a trace stops just short of a surface. A box that
  starts closer to a plane than that is treated as hitting at the start.
  Otherwise, on a short move the pulled-back crossing lands far behind the
  start, is discarded, and the box sinks in. Source has the same fix.
- **Displacements** collide through their tessellated triangles. Each is a
  1-unit-thick prism, given every separating axis of box against triangle
  as a plane: its faces, axis-aligned bevels, and edge × axis bevels. The
  brush a displacement was built on survives compilation as a solid brush.
  It is found geometrically and made non-solid, so terrain that dips below
  its base face has no invisible lid over it.
- **Brush entities** of solid classes (`func_door`, `func_brush`, ...) are
  traced as their own sub-trees, in their own frame.
- **Spawns** that mappers left touching a wall are nudged to the nearest
  free spot (`find_free_position`).

- **Props** (static and physics) collide too. Movement runs against a
  `Scene`, which merges the map's trace with one against the props' convex
  pieces, clipped exactly like brushes. The props a move runs into are
  reported (`MoveEvents::touches`) so they can be pushed. See
  [physics.md](physics.md).

## Testing

- **Scenarios** (`crates/rl_sim/tests/movement.rs`), in synthetic rooms built
  with `WorldBuilder`:
  - friction stopping time;
  - run and walk speeds;
  - jump height;
  - jump latching and auto-hop;
  - air-strafe and bunny-hop gains;
  - steps of 16 (climbed) and 24 (blocked) units;
  - staying grounded down a slope;
  - surfing a 60° ramp, and the ramp-bug regression;
  - crouching, a low ceiling, crouch-jump height;
  - swimming;
  - ladders;
  - bit-identical determinism over 600 scripted ticks;
  - non-finite input.
- **Collision** (`crates/rl_sim/tests/collision.rs`): landing, contents,
  stuck detection, ramp edges and displacements. Property tests check that
  fractions stay in `[0, 1]`, that traces from outside never end inside,
  that chains of tiny moves from just off a surface never sink in, and that
  normals face the mover.
- **Real maps** (ignored; set `RUSTLIFE_TEST_BSP`): every spawn has a floor,
  no displacement can be fallen through, and players walking, strafing,
  jumping and crouching out of every spawn never get stuck or fall out of
  the world. These pass on every stock HL2DM map.
- **Benchmarks** (`cargo bench -p rl_sim`): about 140 ns per movement-sized
  hull trace, and about 0.9 µs per movement tick.
