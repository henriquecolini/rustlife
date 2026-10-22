# Gameplay data

Everything about how rustlife plays is data: RON files under
[`data/`](../data/), read by the `rl_data` crate. Code implements *kinds*
of things — a hitscan shot, a projectile, a health pickup — and the data
says which exist and how they behave. This page lists the files and walks
through adding a weapon and an item. How the game then uses them is in
[combat.md](combat.md), [movement.md](movement.md),
[physics.md](physics.md), [map-logic.md](map-logic.md) and
[presentation.md](presentation.md).

## Files

| File | Contents |
|------|----------|
| `simulation.ron` | Tick rate |
| `movement.ron` | Player movement physics |
| `physics.ron` | Prop physics, surface materials, pushing, impact damage |
| `tools/manipulator.ron` | The Manipulator's grip and throws |
| `ammo.ron` | Ammo types and how much of each a player carries |
| `weapons/<id>.ron` | One weapon per file; the file name is its id |
| `items/<id>.ron` | One health, armor or ammo pickup per file |
| `entity_map.ron` | What a map's entity classes become |
| `player.ron` | Health, armor, player models and how they animate |
| `rules/deathmatch.ron` | Frag and time limits, respawning, the loadout, fall damage |
| `hud.ron` | HUD tunables (crosshair, kill feed, low-health mark) |
| `net.ron` | Port, snapshot rate, interpolation delay, command redundancy and backlog, lag-compensation window ([netcode.md](netcode.md)) |
| `map_logic.ron` | Use range, wall chargers' aim cone, map explosions, the per-tick event cap ([map-logic.md](map-logic.md)) |
| `sounds.ron` | Footsteps, landings, deaths, explosions, impacts, breakables, buttons ([presentation.md](presentation.md)) |
| `effects.ron` | Muzzle flash, tracer and spark sprites, and how explosions are drawn ([presentation.md](presentation.md)) |

Every file documents its fields in comments, and every type in `rl_data`
documents them in Rust.

## Loading and overriding

The repository's `data/` is compiled into the game, so it always has a
complete set. `--data-dir <dir>` overrides it file by file: a file in that
directory replaces the built-in one of the same path, missing files fall
back to the built-in copies, and new files in `weapons/` and `items/` add
weapons and items. A modded data set is therefore just the files that
differ:

```text
my-data/
  rules/deathmatch.ron      # a different loadout
  weapons/nailgun.ron       # a new weapon
```

On a server, the data is the server's: it sends its whole data set to each
player who joins, unless their copy hashes the same, and they play by it.
A modded server needs no modded clients.

## Validation

Data is checked when loaded, and a bad file stops the game with every
problem it has, each by field:

- **Schema.** Every file is parsed with `deny_unknown_fields`: a misspelt
  field is an error, not a silently ignored key.
- **Values.** Speeds and times must be positive, fractions within 0..1,
  curves sorted, clips big enough for the rounds a shot takes, and so on.
- **References.** A weapon or item cannot name an ammo type `ammo.ron`
  lacks; the entity map and the loadout cannot name missing weapons or
  items; a weapon with a pickup that gives ammo must use some.
- **Ids.** Weapon and item ids (file names) are lower-case letters, digits
  and underscores.

```text
Error: references: weapons/nailgun.ron: no ammo type "nails" in ammo.ron
```

## Adding a weapon

A weapon is composed, not coded. Create `weapons/nailgun.ron`:

```ron
Weapon(
    name: "Nailgun",
    // Number key 3, after the SMG and the rifle.
    slot: 3,
    position: 2,
    // Drawn with models from the mounted game.
    view_model: Some("models/weapons/v_smg1.mdl"),
    world_model: "models/weapons/w_smg1.mdl",
    // A 50-round magazine of a new ammo type, reloaded in 2 s.
    clip: Some((ammo: "nails", size: 50, reload: Whole(time: 2.0))),
    primary: Some((
        interval: 0.06,
        automatic: true,
        ammo: Clip(1),
        // Slow, slightly dropping nails that stick where they land.
        shot: Projectile((
            model: "models/crossbow_bolt.mdl",
            speed: 2000.0,
            lift: 0.0,
            gravity: 0.15,
            size: 1.0,
            bounce: None,
            fuse: None,
            hit_damage: 12.0,
            explosion: None,
            lifetime: 3.0,
        )),
    )),
    secondary: None,
    deploy_time: 0.5,
    pickup: Some((ammo: 50, respawn: 20.0, radius: 24.0)),
    // Players hold it with the smg1 sequences of HL2DM's player models
    // (run_smg1, cidle_smg1, ...).
    hold: Some("smg1"),
    sounds: {"fire": "Weapon_SMG1.Single", "reload": "Weapon_SMG1.Reload"},
)
```

Add the ammo type to `ammo.ron`:

```ron
"nails": (name: "Nails", max: 200),
```

Then make it appear: put it in the loadout (`rules/deathmatch.ron`), or map
an entity class to it so maps place it (`entity_map.ron`):

```ron
"weapon_ar2": Weapon("nailgun"),
```

### The building blocks

| Field | Choices |
|-------|---------|
| `clip` | `None` (fire modes take from carried ammo or need none), or `(ammo, size, reload)` with `reload` `Whole(time)` or `PerRound(start, each)` |
| fire mode `ammo` | `Unlimited`, `Clip(n)` (rounds from the clip), `Reserve(ammo, count)` (rounds from carried ammo) |
| fire mode `on_release` | `true`: the shot goes off when the button is let go, the weapon primed while it is held (a grenade) |
| `consumable` | `true`: the weapon is its own ammo, carried only while it has rounds and put away with the last |
| `icon` | The letter of the weapon's icon in the HUD's icon font (`weapon_icon_font` in `hud.ron`, by default the game's `resource/halflife2.ttf`); the weapon list shows its name alone without one |
| fire mode `shot` | `Hitscan((pellets, damage, spread, range, falloff, force))`, `Melee((range, size, damage, force))`, `Projectile((...))`, `Manipulator` |
| `falloff` | `None`, or `(start, end, min_scale)`: full damage to `start`, linear to `min_scale` at `end` |
| projectile `body` | `None`, or a rigid cylinder `(radius, height, model_center, surface, rolled, blip, mass, friction, spin, damping)`; `blip` is `None` or `(interval, fast_interval, warn_time)` |
| projectile `explosion` | `None`, or `(radius, damage, knockback, force)` |
| `hold` | The suffix of the player-model sequences it is held with (`pistol`, `smg1`, `ar2`, `shotgun`, `crossbow`, `rpg`, `grenade`, `melee`, `physgun`); `None` takes `player.ron`'s `default_hold` |
| `sounds` | Soundscript entries or files by purpose: `fire`, `fire2`, `hit`, `reload`, `deploy`, `pickup`, `drop`, `launch`, `punt`, `dryfire`, `blip` (a fuse ticking) |

The ten built-in weapons cover every block: the Gravity Gun (a tool, the
`Manipulator` shot), the Crowbar (melee), 9MM Pistol and .357 Magnum
(single bullets), SMG (automatic, with a reserve-fed grenade launcher),
Pulse Rifle (with a bouncing, fused orb), Shotgun (pellets,
round-by-round reload, a two-shell secondary), Crossbow (a fast
projectile), RPG (rockets) and Grenade (a thrown, bouncing, fused
grenade).

## Adding an item

Create `items/megahealth.ron`:

```ron
Item(
    name: "Mega health",
    model: "models/items/healthkit.mdl",
    gives: Health(100.0),       // or Armor(50.0), or Ammo({"smg": 45, "buckshot": 6})
    respawn: 60.0,
    radius: 24.0,
    sounds: {"pickup": "HealthKit.Touch"},
)
```

and place it through the entity map, e.g. `"item_healthkit":
Item("megahealth")`. Health and armor top up to `player.ron`'s maximums.

An item with a `station` is a wall charger: it is not touched but used,
and gives its `gives` a little at a time, at `rate` points a second while
use is held, recharging over `respawn` seconds once spent. Its `use` sound
loops while someone draws from it:

```ron
Item(
    name: "Health station",
    model: "models/props_combine/health_charger001.mdl",
    gives: Health(50.0),
    respawn: 60.0,
    radius: 40.0,
    station: Some((rate: 25.0)),
    sounds: {"use": "WallHealth.LoopingContinueCharge"},
)
```

## Mapping a map's entities

Maps made for Half-Life 2: Deathmatch place that game's entities
(`weapon_shotgun`, `item_healthkit`, ...). `entity_map.ron` turns each class
into a rustlife weapon or item, or ignores it:

```ron
EntityMap(
    classes: {
        "weapon_shotgun": Weapon("scattergun"),
        "item_healthkit": Item("medkit"),
        "item_ammo_crate": Ignore,
    },
)
```

So a different arsenal plays on the same maps: change what the classes
map to. Weapon and item classes a map uses that are not listed are logged
when it loads.
