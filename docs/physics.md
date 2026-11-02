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
- *Spawnflags*: `1` starts asleep, `8` fixes it in place, `64` frees a fixed prop when it is
  grabbed, and `512` prevents pickup. Source's `4` (debris, which players
  walk through) is ignored: maps flag props that look solid, such as
  cinder blocks, chairs and plastic crates, and players should not pass
  through them. Only pickups lie as debris.

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

Other players block a player's movement too. `Simulation` hands each
player's movement the boxes of the other living players (`scene::PlayerBox`),
and `Scene` sweeps against them with the same exact box clip, so players
stop at one another and can stand on each other's heads. Players overlapping
(spawning on the same point, standing up beside someone) are pushed apart by
the same push-out as for props; two players exactly on top of each other
leave in opposite directions. Dead players do not block. The boxes are not
part of the physics world: they never touch props through it, and nothing
pushes a player by walking into them.

Props act on players back in two ways.

- **Shoves.** A prop in contact with a player and moving into them gives
  them speed, as the player gives it to a prop: the prop meets the player as
  a body of its own mass against the much lighter `push.shove_mass` (so
  even a small crate moves a player), in a perfectly inelastic collision
  along the contact normal, capped by `push.max_shove_change` per tick. A rolling barrel carries a player along; a crate pushed by someone
  else's Manipulator nudges them; a prop at rest or moving away does
  nothing. The change goes to the player's velocity at the end of the tick
  (`Simulation::shove_player`), so clients see it as a correction, like
  knockback.
- **Pushing out.** A prop can still end up overlapping a player (a rotating
  prop's corner, a prop dropped by the Manipulator, a squeeze between two
  props), and a player inside a prop cannot move at all, since every trace
  starts in solid. So the first thing movement does each tick is to find
  the way out. Each prop piece's clip planes, grown by the player's box,
  bound the volume the player's centre must stay out of; inside it, leaving
  along any of the planes works, and the shortest is not always possible (a
  slab resting on a player's head leaves "down", through the floor, the
  shortest). So every plane is a candidate exit (as is every face of another
  player's box), shortest first, and the player takes the first that the
  map leaves open and that ends in the clear (`PhysicsWorld::penetrations`,
  `Scene::penetration`). Exits that lift the player more than 24 units are
  skipped: a player under a prop leaves sideways, not over the top.
  A prop that overlaps only the player's head (its lowest point above the
  player's centre) is left alone: it neither pushes nor blocks, so a player
  can carry a prop on their head and walk out from under it. If none
  qualifies, the one that gets farthest is taken. Movement is at most
  `movement.unstick_speed` per second, so nobody is flung through a wall.
  Movement runs on clients too, so prediction does the same.

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
of: a crate takes more from a club than a bullet. Weapons do
`breakables.weapon_toughness` times less than the model's `dmg.*` says, as
Source's `prop_data` values are tuned for a lighter damage model, so bottles,
boxes and sinks take a few solid hits rather than one. A new prop's first
contacts while it settles into the map (pushing itself out of a floor or a
neighbouring prop), and a server's first seconds before anyone arrives, do not
damage it, so a tightly placed crate does not break at map load.

**Impacts** hurt a prop by its `damage_table` (`prop_data`; only `Glass.Small`,
`Glass.Window` and `Pottery.Small` name one, `glass`; the rest use `npc`). The
tables and the rule are HL2DM's `CalculatePhysicsImpactDamage`, in
`breakables.impact.tables`:

```text
E = (Δv² · m + Δv_other² · m_other · large_mass) / m · energy_scale
damage = the last step of `linear` that E reaches (no interpolation)
```

Source's Δv is each body's speed before minus after. Box3D only gives a
contact's approach speed, and a body's velocities span a whole step of all its
contacts, so the two speed changes here are the closing speed shared out by
mass: all of it for a body meeting something immovable, else in inverse
proportion to the masses. Bounces take nothing off the blow. Heavy things hit
harder (`large_mass_min`, `_scale`, `_falling_scale`), a small mass's damage is
capped, and what a player holds neither hurts anything nor is hurt by the map.
The tables' angular steps are carried but unused: HL2DM reads them only for
objects flagged as slicing, and none is.

`energy_scale` is Source's `physdamagescale`, which defaults to 0.1. Glass keeps
that: a 5 hp bottle or vase breaks at 79 units/s, a drop of about 5 units (8
units certainly). The other tables have 0.5, our deviation: at 0.1 a 30 hp
crate needs a fall of over 1000 units. At 0.5 a crate takes 5 from a 40-unit
drop and nothing from a shove into a wall at running pace (190 units/s), and
breaks at 495 units/s: a fall of about 204 units, or any throw or punt.

Only a new hit hurts: `Box3D` goes on reporting a contact while the two press
together, so a hit between surfaces that hit within `repeat_seconds` does no
harm (rubbing, or a prop held against a wall), and of one step's hits between
two bodies only the hardest counts (a crate landing across two brushes takes
one blow). Slower contacts than `report_speed` are not heard, but are still
judged by their tables, down to the slowest any table notices (8 units/s).

A prop whose health runs out *breaks*: it leaves the physics world (its body id
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
| thrown or punted with the Manipulator | for `launched_seconds`, explodes on the first thing it touches faster than `breakables.impact.report_speed`: the map, another prop, a pickup or a player (not the one who launched it, which it ignores until they have come apart), hurt or not |

Whoever lit the fuse is credited with the blast and its kills, so
barrels set off by other barrels chain, each credited to the first
shooter. The server sends `PropIgnite` when a barrel catches fire so
clients can draw the flames (`rl_client::particles`).

## The Manipulator

Each player carries a `Manipulator`, a weapon in slot 1 (see
[combat.md](combat.md)). While it is in hand and deployed it aims along the
view from the eyes, and its aim stops at the map or at the first prop. Putting
it away lets go of the held prop. After each action it rests
for its fire mode's `interval` from `weapons/manipulator.ron`: primary
after a launch, punt or dry fire, secondary after a grab or drop.

| Input | Action |
|-------|--------|
| Secondary fire, held | Grab a grabbable prop within `grab.range` weighing at most `grab.max_mass`. Pull one within `grab.pull_range` (1000, four times HL2's `physcannon_tracelength`) towards you, picking up speed at `grab.pull_accel` to `grab.pull_speed`, until it is close enough to grab: far props gravitate slowly and arrive fast. |
| Secondary fire, held, on a pickup the player can use | Pull it all the way in, however close, rather than grabbing it: the player takes it on touching it. One they cannot use (a medkit at full health) is grabbed like any prop, and never taken while held. |
| Secondary fire, pressed while holding | Let go. Secondary fire must then be released before it reaches for another prop. |
| Primary fire while holding | Launch the prop along the view at `launch.speed × mass_curve(mass)`. |
| Primary fire otherwise | Punt the dynamic prop in the aim's way within `punt.range`: an impulse at the point hit, for a speed change of `punt.speed × mass_curve(mass)`; harder than a throw. Or smash what is fixed to the world, below. Nothing to act on is a dry fire. |

The **mass curves** are `(kg, factor)` points, interpolated linearly and
held flat past the ends. Heavier props leave slower.

**Punting what is fixed to the world.** HL2DM's punt moves a frozen prop only if
the map lets the physcannon free it, and does 1 damage to a brush
(`PuntNonVPhysics`), which is mostly nothing. Here it goes further, by design:

| In the aim's way | A punt |
|---|---|
| A frozen `prop_physics` that can break (the stock maps' vent grates: `props_junk/vent001.mdl`, spawnflags 8 and 64, `prop_data` health) | Breaks it, whatever its health (`punt.brush_damage`), with the punt's effect and sounds |
| A frozen `prop_physics` that cannot break, with spawnflag 64 (enable motion on physcannon) | Frees it and knocks it away, as HL2DM |
| A frozen `prop_physics` that can do neither, a static prop, or a brush that takes no damage | Dry fire |
| A brush entity that takes damage | Does `punt.brush_damage` to it ([map-logic.md](map-logic.md)) |

A `prop_dynamic` breaks only if its entity gives it health; static props are the
world's.

**The aim** is thicker than a line, so that small props need not be under the
very crosshair. Looking for a prop to take (`find_target`, HL2DM's `FindObject`
and `FindObjectInCone`) tries a ray, then a box 4 units to each side along the
same line, then the nearest prop to take whose centre is within `grab.aim_cone`
(the cosine, about 14 degrees) of the view and `grab.aim_cone_range` of the eyes,
and which the line of sight to its centre reaches first. Ties go to the lowest
body id. A prop the tool cannot take (too heavy, say) does not end the search. A
punt uses a box 8 units to each side and then a ray, without a cone.

**The claws.** The client opens the gun's claws, with their sound, on a prop it
could lift within `grab.open_range` (250, how far HL2DM's `CheckForTarget`
looks): a little past grab range and far short of what secondary fire reaches.
Past that they stay shut until a pull starts. Having opened on a prop they stay
open for `grab.ready_hold_secs` after the view leaves it, so sweeping across
props does not snap them open and shut (HL2DM waits half a second); dropping or
throwing something closes them at once. They sound opening on something, and
letting go of, throwing or punting something, but not closing on nothing.

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
interpolated from snapshots, but for the one the local player holds, which they
steer ahead of the server with the same formula ([netcode.md](netcode.md)).

## In the client

- Every solid prop has a body, made when the level was loaded (`rl_level`,
  from the model's header and `.phy`) and numbered by placement, so the
  server and every client agree on it.
- Props that can move follow their body as the server's snapshots place
  it, interpolated a little in the past. The prop the player holds is run ahead
  of the server with them instead, so carrying feels immediate, and eases back
  on being let go of ([netcode.md](netcode.md)).
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
| `rl_sim/tests/manipulator.rs` | Grabbing within range and mass, pulling, carrying and turning, launching and punting at the configured speeds, punting frozen props (breaking, freeing, a dry fire), dropping, the grip breaking, a thrown prop hurting another player |
| `rl_sim/tests/simulation.rs` | Spawning clear of props, walking while carrying, repeatable runs |
| `rl_sim/tests/real_map.rs` | A real map's physics world, with crates dropped at every spawn landing on the floor (`RUSTLIFE_TEST_BSP`) |
| `rl_client/tests/local_play.rs` | A real map's props becoming bodies in the running client (`RUSTLIFE_TEST_GAME_DIR`) |

`cargo bench -p rl_sim -- simulation` times a whole tick with one player
running into 64 tumbling crates.

## Gravity Gun launch speed (from server.so)

The physcannon's force routine returns `direction * force`, where `force` is
`physcannon_maxforce` (1500) for a prop up to 100 kg, easing (smoothstep over
`(mass - 100) * 0.002`) down to `physcannon_minforce` (700) at 600 kg. A held
prop thrown with primary fire leaves at that speed. `data/tools/manipulator.ron`
encodes it as `speed: 1500` and a mass curve. The punt uses the same curve at a
higher speed (2600), on purpose: HL2's punt hits much harder than a throw.

Reading `server.so` further (pull as an impulse every tick, a grip that breaks
at a running error of 12 units, a hold point from the prop's extent) gave a pull
that hurt the player and a grip that let go when the view was shaken, so those
numbers were not used. The hold, grip and pull here are rustlife's own; what is
HL2's is the reach (a look four times the trace length, so far props are drawn
in), the mass limit and the launch speeds.
