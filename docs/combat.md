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
resolves number keys (cycling through the weapons of a slot), `Q` (last
weapon) and a pick from the weapon list (`Choice::Weapon`) against the
inventory (`Inventory::choose`). Switching deploys the new weapon.

The mouse wheel does not switch by itself: it opens a weapon list above the
ammo counter and moves a cursor through the usable weapons
(`Inventory::cycle`). Primary fire confirms the pick (and does not fire);
after a second without scrolling the list fades and the pick is forgotten,
so the fire button fires as usual. Moving the cursor, picking and the list
timing out each play a sound (`weapon_select` in `sounds.ron`), and each row
shows the weapon's icon, a glyph of the game's icon font.

The **Manipulator** is a weapon whose fire modes are `Shot::Manipulator`.
The inventory never fires it: once it is deployed, the simulation runs the
tool every tick with the buttons ([physics.md](physics.md)), and the fire
modes' intervals are its rest after acting.

### Shots

| Shot | What happens |
|------|--------------|
| `Hitscan` | `pellets` rays from the eyes, each turned by up to `spread` degrees (a deterministic draw seeded by the command's number, player and mode, so the shooter's prediction sprays exactly as the server does; other players are hit where the shooter saw them, see [netcode.md](netcode.md)). Each stops at the map, a prop or a player's box; a player takes `damage × falloff(distance)`, a prop an impulse of `force` along the ray. |
| `Melee` | A box of half-width `size` swept `range` units along the view: `damage` to a player, `force` to a prop. Reported as a shot, hit or miss. |
| `Projectile` | A box of half-width `size` launched from the eyes at `speed` along the view plus `lift` upwards (see below). |

Bullets and swings stop at what Source's `MASK_SHOT` stops at: solids,
windows, props and players, not grates or clip brushes. Players are hit as
their movement box; there are no per-limb hitboxes (models are animated
for show only, on clients).

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

### Grenades

A projectile whose weapon gives it a `body` is not flown as a box but thrown
into the physics world (`rl_sim/src/simulation/grenades.rs`): a rigid
**cylinder** (`radius`, `height`, standing along the model's Z axis) whose
mass is at its middle, so it tumbles, bounces, scrapes and rolls on
`surface`'s friction, is shoved by blasts and knocked about by players and
props, until its `fuse` runs out. It is simulated only on the server; clients
are sent its pose and body (`GrenadeView`) and draw the model with its own
middle (`model_center`) laid on the body's, so it is drawn where it collides.

The throw is the *player's* action, not the weapon's: a fire mode with
`on_release` is **primed** while its button is held (the weapon is in
`Phase::Priming`: the arm is drawn back, nothing is used up; switching
away cancels the throw) and goes off when the button is let go, from where the
player then looks. Primary throws overhand (`speed`, `lift`); secondary,
whose body is `rolled`, sets the grenade down at the feet and sends it along
the floor on its side at the flat view's heading, turning as it goes: a
gentle roll.

A `consumable` weapon is its own ammo. It is carried only while rounds of its
ammo are: with the last throw it stays in hand to play out the throw, then
is put away for good and the weapon before it is brought up. Picking up
another brings it back.

Its `blip` is the fuse ticking: a tick as it leaves the hand, then every
`interval` seconds, every `fast_interval` once `warn_time` is left. The
schedule is a function of the grenade's age (`BlipConfig::last`), so the
sound (the weapon's `blip` sound) and the red dot's flash (`effects.ron`'s
`grenade`) agree.

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
box, with their `damage` per second. Disabled ones (`StartDisabled`) wait
for the map's logic to switch them on, which is how traps are built (see
[map-logic.md](map-logic.md)).

## The match

`rules::Match` keeps every player's points, kills and deaths, and the clock,
through the match's life:

1. **Waiting** until `min_players` have joined. Everyone may move and shoot,
   but nobody is hurt and nothing scores.
2. **Warmup**: a `warmup` countdown with the same freedom.
3. **Playing**: a kill scores the killer points by how it was done
   (`points` in `rules/deathmatch.ron`: a default per kill, a value per
   weapon id, and values for props, barrels and the environment); a death
   with no other killer (your own blast, a fall, a hazard, your own prop)
   costs the victim `suicide_penalty`. The match ends when someone reaches
   `score_limit` or `time_limit` runs out; the player alone at the top wins
   (a tie has no winner).
4. **Intermission**: the scoreboard shows and the players vote for the next
   map (see [netcode.md](netcode.md)); nothing moves. Then the next match
   starts: scores reset, everyone respawns, pickups return and projectiles
   vanish.

An emptied server forgets the match and goes back to waiting.

## The HUD

The HUD is drawn with the client's UI theme (see [presentation.md](presentation.md)):
health and armor with bars that drain behind a pale ghost of what was lost,
the ammo of the weapon in hand (clip, carried rounds and the carried rounds of
a secondary fire with its own ammo), the strip of carried weapons, your
points and the clock with the match phase (waiting for players, the
countdown), the kill feed, and a message in the middle (who killed you, the
respawn countdown, the match result). The screen edges glow red when you are
hurt and the crosshair turns red when your shot lands. The scoreboard shows
while the scoreboard key (`Tab`) is held and between matches, when it is
joined by the vote for the next map (click a map, or press its number).
`hud.ron` holds only the tunables: when health counts as low, the crosshair
and the kill feed's length and duration.

Muzzle flashes, tracers, sparks and explosions are drawn with the game's
own sprites (`effects.ron`, see [presentation.md](presentation.md)).

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
| `rl_sim/src/weapons/tests.rs` | Grenades primed while held, thrown on release, rolled with secondary, cancelled by switching away while primed, thrown one at a time, the last one retiring the weapon |
| `rl_sim/tests/combat.rs` | Bullets, armor, swings, rockets, rocket jumps, shelter, grenades as centred cylinders thrown only on release and rolled along the floor, deaths and frags, forced and requested respawns, pickups, falls, hazards, thrown-prop kills, the match ending and restarting, bullets shoving props, determinism |
| `rl_sim/tests/real_map.rs` | Every stock map stocked with pickups (`RUSTLIFE_TEST_BSP`) |
| `rl_client/tests/local_play.rs` | The running client: dummies, pickups, firing and weapon keys (`RUSTLIFE_TEST_GAME_DIR`) |
