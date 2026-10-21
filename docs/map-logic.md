# Map logic

Source maps are programmed, not just built. A mapper wires entities
together: a button's `OnPressed` output sends `Trigger` to a relay, whose
`OnTrigger` opens a door and, four seconds later, enables a `trigger_hurt`
that crushes whoever is still in the corridor. rustlife runs that wiring,
so a map's doors, buttons, lifts, traps and teleporters work as the mapper
meant them to.

The logic lives in `crates/rl_sim/src/logic/`: `io.rs` (connections and the
event queue), `mover.rs` (doors, buttons, moving brushes), `volume.rs`
(triggers and filters) and `inputs.rs` (what each entity does with an
input). It runs on the server only, as part of the tick
(`simulation/map.rs`). Clients get what they need to draw and hear it.

## Entity I/O

An entity's outputs are keys of its BSP entity, each a connection
`target,input,parameter,delay,times` (with the ESC character in newer
compilers). When an entity fires an output, every connection on it queues
an event: at `delay` seconds from now, deliver `input` with `parameter` to
every entity named `target` (a trailing `*` matches any name with that
prefix, and `!self` / `!caller` the entity firing). Inputs to
`!activator` or `!player` act on players (their health, their speed),
which rustlife's rules do not let maps do; they are dropped. A connection with `times` greater than zero is used up after that
many firings.

Each tick, the queue delivers the events that are due, in the order they
were queued. An input may fire more outputs; those with no delay are
delivered in the same tick. `map_logic.ron`'s `max_events_per_tick` caps a
tick's deliveries, so two relays triggering each other forever cannot hang
the server; the rest wait for the next tick.

The activator, whoever set the chain off, travels with every event down
the chain.

## What runs

| Entity | Does |
|--------|------|
| `logic_auto` | fires `OnMapSpawn` (and `OnMultiNewMap` / `OnMultiNewRound`) as the match starts |
| `logic_relay` | passes `Trigger` on as `OnTrigger`, while enabled |
| `logic_timer` | fires `OnTimer` every `RefireTime` seconds, or at random between two bounds |
| `math_counter` | adds, subtracts and sets a value, firing `OutValue`, `OnHitMin`, `OnHitMax` |
| `func_door`, `func_door_rotating`, `func_movelinear` | open and close |
| `func_button` | pressed by use, touch, damage or `Press` |
| `func_brush`, `func_wall_toggle` | shown and hidden, solid or not |
| `func_breakable`, `func_breakable_surf` | break when shot enough, or on `Break` |
| `trigger_multiple`, `trigger_once` | fire `OnTrigger`, `OnStartTouch`, `OnEndTouch` |
| `trigger_teleport`, `trigger_push` | move the players inside |
| `trigger_hurt` | the simulation's hazards, switched by `Enable` / `Disable` |
| `env_explosion` | an explosion, on `Explode` |
| `ambient_generic` | a sound playing at a place, for clients to hear |
| `filter_activator_name` / `_class`, `filter_multi` | who sets triggers off |

Every entity also accepts `Kill` and `FireUser1`–`4`. Inputs sent to
entities rustlife does not run (lights, sprites, NPC spawners) are dropped
and logged once each, so a map that relies on one shows what is missing.

## Movers

Doors, buttons and `func_movelinear`s are **movers**: brush models that
travel between a closed and an open pose, sliding along their move
direction by their size less `lip`, or turning by `distance` degrees for
rotating doors. A mover's spawnflags and keys say how it is set off and
how it behaves:

- **Activation.** Doors open when used if their `Use Opens` flag is set,
  when touched if `Touch Opens` is, and on `Open`, `Close` and `Toggle`.
  Buttons press on use, on touch or damage when their flags say so, and on
  `Press`. Locked movers ignore players and fire `OnUseLocked` instead.
- **Returning.** After `wait` seconds open, a door closes again; with
  `wait` at `-1` it stays until told otherwise.
- **Blocking.** A mover that meets a player it cannot push aside deals its
  `dmg` every tick and fires `OnBlockedOpening` / `OnBlockedClosing`. A
  door that waits goes back the way it came; one that waits for ever, or
  has `forceclosed`, keeps pressing.
- **Riding.** A mover that sweeps into a player pushes them along, so
  platforms lift whoever stands on them. Movers parented to another
  (`parentname`) are carried with it.

Movers are solid to movement, bullets, projectiles and physics props
alike: the collision world and the physics world both move each brush
model to its pose every tick.

**Use.** Pressing use (`E`) traces from the eyes through movers and the
map, out to `use_range`. The first mover it meets that use sets off is
used. Wall chargers have no brush to aim at, so one is used when it lies
within `use_cone` degrees of the view and within range.

## Triggers and filters

A trigger is the volume of its brush model. Each tick the logic compares
who is inside with who was inside last tick and fires `OnStartTouch`,
`OnEndTouch` and `OnTrigger` (repeating every `wait` seconds for
`trigger_multiple`; once for `trigger_once`, which is then removed). A
trigger only counts the players its filter (`filtername`) passes:
`filter_activator_name` and `_class` match by name and class, possibly
negated, and `filter_multi` combines filters with AND or OR.

`trigger_teleport` moves the players inside to its target's origin, facing
its angles, and stops them. `trigger_push` brings the speed of those inside
up to its own along its push direction, as wind does: someone already
faster that way keeps their speed. A disabled trigger (`StartDisabled`,
or after `Disable`) does nothing until enabled, and that is how most traps
are built: a `trigger_hurt` waiting for a button.

## Wall chargers

A map's `item_healthcharger` and `item_suitcharger` become stations
through `entity_map.ron`: items with a `station` (see
[data-driven.md](data-driven.md)). Holding use at one draws its charge at
the station's `rate` per second until the player is full or the charge is
spent; then it recharges over its `respawn` time. Stations stay visible
while empty.

## What clients see

The logic's state that matters on screen — every mover's position,
rotation and visibility, and which ambient sounds are playing — is part of
the snapshot (`LogicState`), sent as a delta like everything else and
interpolated between snapshots. The tick's map events (a button pressed, a
breakable breaking, a door blocked) travel with the other events, so
clients can play their sounds. A client's own replica never runs the logic.
It takes the server's word for where the doors are. Prediction treats
movers as the replica last saw them, and the next snapshot corrects any
difference.

## Tuning

`data/map_logic.ron` holds the numbers a map cannot give:

```ron
MapLogic(
    use_range: 80.0,          // units from the eyes
    use_cone: 30.0,           // degrees, for wall chargers
    explosion: (              // env_explosion
        radius_per_damage: 2.5,
        knockback_per_damage: 7.0,
        force_per_damage: 700.0,
    ),
    max_events_per_tick: 1024,
)
```

## Trying it

```sh
# Walk to the nearest button and press it; shoot 3 seconds later.
cargo run --release -p rustlife -- --map dm_lockdown --use-mover func_button --wait 3 --screenshot /tmp/button.png
```

`--use-mover <class>` goes to the nearest mover of that class and presses
use on it, and `--teleport x,y,z[,pitch,yaw]` stands the player somewhere
to watch.

## Tests

| Test | Covers |
|------|--------|
| `rl_sim/src/logic/tests.rs` | Delayed chains from `logic_auto`, relays, connections' `times`, timers (random ones repeatably), counters, triggers and their touch outputs, filters, buttons opening doors through relays, locking, doors that stay open, damage breaking breakables and pressing buttons, explosions, teleports, sounds, a looping relay spending the tick budget, a new match resetting everything, children riding their parent, replicas taking on the server's state |
| `rl_sim/tests/map_logic.rs` | In built maps: a button opening the door in the way, a platform carrying the player up, crushing, a blocked door going back, a trap switched on, a teleport, glass stopping bullets until it breaks, a replica colliding with doors where the server has them, drawing a charger |
| `rl_sim/tests/real_map.rs` | A stock map's logic running for two minutes (needs an install) |
