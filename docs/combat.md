# Combat

Implemented in `crates/rl_sim/src/` — `weapons/` (inventories, firing,
projectiles), `combat/` (health, damage, what shots hit), `pickups.rs`,
`hazards.rs`, `rules.rs` and `simulation/` (the tick that runs them) — and
drawn by `crates/rl_client/src/` (`hud.rs`, `actors.rs`, `effects.rs`).
Every number is data: see [data-driven.md](data-driven.md) for the files and
how to add a weapon or an item.

## The tick

`Simulation::tick` runs, in order:

1. **Players**, each with their command: movement, pushing props, fall
   damage, then the weapon in hand (or the Manipulator).
2. **Respawns** of the dead who asked to, or waited long enough.
3. **Physics**: players' boxes move, the physics world steps, props that hit
   players hurt them.
4. **Projectiles** fly, strike and burst.
5. **Pickups** are taken and come back.
6. **Hazards** hurt whoever is inside.
7. **The match** clock runs.

What happened comes back as `TickEvents`: trigger pulls, shots, explosions,
damage, kills, pickups, respawns and match changes. The client turns them
into the kill feed, hit markers, tracers and flashes.

## Weapons

The game data's weapons and ammo types are numbered in id order by the
`Arsenal` (`WeaponId`, `AmmoId`), so the same data numbers them the same
everywhere. Each player has an `Inventory`: carried weapons with their
clips, carried ammo per type (capped by `ammo.ron`), and the weapon in hand,
which is in one of three phases:

| Phase | Ends | Meanwhile |
|-------|------|-----------|
| Deploying | after the weapon's `deploy_time` | nothing fires |
| Reloading | after the clip's reload time | nothing fires, except that firing interrupts a round-by-round reload (with a round loaded) |
| Ready | — | fire modes fire once their `interval` since the last shot has passed |

A fire mode fires while its button is held (`automatic`) or on each fresh
press. A shot spends its `ammo`: rounds from the clip, rounds straight from
the carried ammo, or nothing. Firing with too few rounds starts a reload
instead, and an empty clip reloads by itself; `R` reloads a partial clip.
A whole-clip reload loads what it can after its time; a round-by-round
reload loads one round per step until the clip is full or the ammo runs out.

The command for a tick may choose a weapon (`UserCmd::select`). The client
resolves number keys (cycling through the weapons of a slot), the mouse
wheel (next or previous usable weapon) and `Q` (last weapon) against the
inventory (`Inventory::choose`). Switching deploys the new weapon.

The **Manipulator** is a weapon whose fire modes are `Shot::Manipulator`.
The inventory never fires it: once it is deployed, the simulation runs the
tool every tick with the buttons ([physics.md](physics.md)), and the fire
modes' intervals are its rest after acting.

### Shots

| Shot | What happens |
|------|--------------|
| `Hitscan` | `pellets` rays from the eyes, each turned by up to `spread` degrees (a deterministic draw seeded by tick, player and mode, so prediction will agree). Each stops at the map, a prop or a player's box; a player takes `damage × falloff(distance)`, a prop an impulse of `force` along the ray. |
| `Melee` | A box of half-width `size` swept `range` units along the view: `damage` to a player, `force` to a prop. Reported as a shot, hit or miss. |
| `Projectile` | A box of half-width `size` launched from the eyes at `speed` along the view plus `lift` upwards (see below). |

Bullets and swings stop at what Source's `MASK_SHOT` stops at: solids,
windows, props and players, not grates or clip brushes. Players are hit as
their movement box; there are no per-limb hitboxes until models are
animated (M7).

### Projectiles

Each tick a projectile falls under `gravity` times the world's gravity and
moves, traced as a box against the map, props (`MASK_PROJECTILE`, which
includes grates) and players' boxes:

- Touching a **player** ends it: the player takes `hit_damage`, then it
  bursts if it has an `explosion`. Its thrower is never touched (it starts
  inside their box).
- Touching the **map or a prop**, a projectile with `bounce` rebounds,
  keeping that fraction of its speed, and comes to rest when slower than
  40 units/s on a floor. Any other projectile ends there, bursting if it has
  an explosion.
- A `fuse` bursts it that many seconds after launch, wherever it is.
- Past its `lifetime` it is gone, without a burst.

### Explosions

A blast's strength falls linearly from 1 at its centre to 0 at its
`radius`, measured to the nearest point of a player's box. A player is
reached if the map leaves a clear line from the centre to their box's
centre or nearest point: walls shelter, props do not. Each player reached
takes `damage × strength` and is pushed away from the centre at
`knockback × strength` units/s; each dynamic prop within reach (measured to
its surface) gets an impulse of `force × strength`.

A blast hurts its own thrower by the rules' `self_damage` factor, but
throws them just as hard. With the shipped launcher, a rocket fired at your
feet throws you about 640 units/s upwards for about 50 health: a rocket
jump.

## Health and armor

`Vitals` holds health and armor (`player.ron`). Damage is split by armor
first: while it lasts, armor takes `absorb` of each hit, losing `cost`
points per point taken; health takes the rest. A player at zero health or
below is dead. Health and armor pickups top up to their maximum, and a
player with nothing to gain leaves a pickup where it is.

Damage carries its **cause** (a weapon, a prop, a fall, a hazard) and its
**attacker**, if any: the player who fired, or who threw, punted or dropped
the prop within `prop_credit` seconds.

| Damage | Amount |
|--------|--------|
| Weapons | as above |
| Props | `physics.ron`'s impact damage ([physics.md](physics.md)) |
| Falls | `fall_damage`: none up to `safe_speed`, rising linearly to `fatal_damage` at `fatal_speed`. A plain jump lands at about 270 units/s; a 210-unit drop reaches the safe speed. |
| Hazards | a `trigger_hurt`'s `damage` per second, in two doses a second |

Nobody is hurt between matches.

## Death and respawning

A dying player drops what they hold, their box leaves the physics world,
and the match scores the death. The camera drops to the floor where they
fell; the HUD names the killer and counts down. After the respawn `delay`,
fire or jump brings them back; after `force` they come back anyway.

Spawn points are ranked by distance from the nearest other living player,
and one of the best `candidates` is picked at random (seeded by the tick,
so it is repeatable). Points with someone standing on them, or without
room to stand, are skipped. A respawned player gets fresh health and armor
and the loadout: its weapons with full clips, its ammo, its active weapon
in hand.

## Pickups and hazards

When a map loads, `entity_map.ron` turns its entities into pickups: each
listed classname becomes one of rustlife's weapons or items, placed where
the entity is. A pickup is taken by a living player whose box comes within
its `radius`, if they can use it:

- a **weapon** they lack is added with a full clip; a weapon's pickup also
  gives rounds of its main ammo type, so a weapon already carried is still
  worth taking for its ammo;
- **health** and **armor** up to the maximum;
- **ammo** of each listed type, up to each type's maximum.

Taken, it is gone for its `respawn` seconds. Weapon and item classnames the
map uses but the entity map does not list are logged once.

`trigger_hurt` brush entities become hazards: their brush model's bounding
box, with their `damage` per second. Disabled ones (`StartDisabled`), which
maps switch on through entity I/O, stay off until I/O is implemented (M7);
in the stock maps that leaves one of sixteen active.

## The match

`rules::Match` keeps every player's frags and deaths and the clock. A kill
scores the killer a frag; a death with no other killer (your own blast, a
fall, a hazard, your own prop) costs the victim `suicide_penalty`. The match
ends when someone reaches `frag_limit` or `time_limit` runs out; the player
alone at the top wins (a tie has no winner). During the `intermission` the
scores show and nothing moves; then scores reset, everyone respawns,
pickups return and projectiles vanish.

## The HUD

`hud.ron` places each element by a screen anchor and an offset: health and
armor, the ammo of the weapon in hand (clip / carried, plus the carried
rounds of a secondary fire with its own ammo), the carried weapons by slot,
frags and the clock, the kill feed, and messages (who killed you, the
respawn countdown, pickups, the match result). The crosshair turns to the
warning colour when your shot lands, the screen flashes red when you are
hurt, and `Tab` shows the scoreboard (always shown between matches).

Tracers, impact marks and explosion flashes are drawn without content for
now; the game's own sprites and particles come with M7.

## Trying it

```sh
rustlife --map dm_lockdown --dummies 3
```

adds three training dummies: players who stand at their spawn points and
respawn when killed. For scripted captures, `--weapon <id>` puts a weapon
in hand with plenty of ammo, `--shoot-for <secs>` goes to the nearest dummy
and fires at it, and `--visit-pickup` goes to look at the nearest pickup:

```sh
rustlife --map dm_lockdown --dummies 3 --weapon launcher --shoot-for 0.3 --screenshot rocket.png
```

## Tests

| Test | Covers |
|------|--------|
| `rl_data` unit tests | Schema validation per file, references between files, adding weapons by file, falloff, blast and armor arithmetic |
| `rl_sim/src/weapons/tests.rs` | Loadouts, automatic and semi-automatic fire, reloads (whole, round by round, interrupted), reserve ammo, deploying, the Manipulator as a tool, weapon choice |
| `rl_sim/src/weapons/projectile.rs` | Rockets ending at walls, striking players but never their thrower, grenades bouncing, settling and fusing, expiry |
| `rl_sim/tests/combat.rs` | Bullets, armor, swings, rockets, rocket jumps, shelter, deaths and frags, forced and requested respawns, pickups, falls, hazards, thrown-prop kills, the match ending and restarting, bullets shoving props, determinism |
| `rl_sim/tests/real_map.rs` | Every stock map stocked with pickups (`RUSTLIFE_TEST_BSP`) |
| `rl_client/tests/local_play.rs` | The running client: dummies, pickups, firing and weapon keys (`RUSTLIFE_TEST_GAME_DIR`) |
