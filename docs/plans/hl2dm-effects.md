# Plan: HL2DM visual effects, Gravity Gun and physics fidelity

Status: not started. Everything in `docs/combat.md` (weapon numbers, timings,
mechanics) is done; this plan covers what is left of "make it identical to
HL2DM". Work in small commits; run `cargo run -q -p xtask -- ci` before each.

## Ground rules (learned the hard way)
- Content: `--game-dir "/mnt/jupiter/Games/SteamLibrary/steamapps/common/Half-Life 2 Deathmatch"` (not auto-discovered). Inspect files with `target/release/rustlife-inspect --game-dir ... ls|cat|extract|sequences`.
- Particle systems live in `particles/*.pcf` (binary DMX) in the mounted game; materials in `materials/particle/**`, `materials/effects/**`, `materials/sprites/**`.
- Reading the game's code: Ghidra/REA times out on `hl2mp/bin/linux64/{server,client}.so` (stripped, 18 MB). What worked: `objdump -d --no-show-raw-insn -M intel X.so > X.asm`, then find a string's address with `strings -t x`, grep `# <addr> ` in the asm for its users, and walk vtables via RTTI (typeinfo name -> typeinfo -> `R_X86_64_RELATIVE` relocs from `readelf -rW` -> vtable slots). ConVar defaults: pairs of `lea rdx,[default]` / `lea rsi,[name]` before `call` in static initialisers. Scripts were scratch files; rewrite them (small Python) if needed.
- Visual checks: `target/release/rustlife --game-dir ... --map dm_lockdown --weapon <id> --teleport=-2843,1928,3,20,84 --fire-for 0.3 --wait 1.0 --screenshot out.png` (build with `cargo build --release -p rustlife`). `--all-weapons` gives everything with infinite ammo.
- Existing client effects are procedural: `crates/rl_client/src/{particles,effects,decals,grenades,laser,zoom}.rs`, config in `data/effects.ron`. Materials: `rl_assets/src/{material,decal,sprite}.rs`, `rl_client/src/world.rs::standard_from`.
- Style: clippy pedantic, docs on public items, tests, data-driven (RON), conventional commits ending with the Co-Authored-By line.

## 1. PCF particle engine (prerequisite for most items below)
1. `source_formats::pcf`: parse DMX binary (versions 1-5, string dictionary, element/attribute types) into particle systems: operators (renderers, initializers, emitters, operators, forces, constraints), children, and their attributes. Test on `particles/*.pcf` from the game (ignored content tests with `RUSTLIFE_TEST_GAME_DIR`).
2. `rl_client::pcf_particles`: a CPU particle runtime for the operators HL2DM's effects actually use (list them by scanning every system named below; implement only those). Renderers: sprite/animated sprite/rope/beam/trail as Bevy billboards or ribbons with the VMT's blend mode (additive, alpha, `$selfillum`). Respect `Exposure`/`Tonemapping::None` look (see world.rs).
3. Spawn by system name from events (explosion, impact, muzzle flash, ...) so `effects.ron` maps names -> events.

## 2. Effects to reproduce (find each system name in the .pcf files / client.so strings)
- AR2 / Pulse Rifle: muzzle flash, tracer, impact (`AR2Impact`), view-model effects; energy ball (`models/effects/combineball.mdl`, `prop_combine_ball`: glow sprites, trails, bounce flashes, explosion `cball_explode`, dissolve of the victim: `env_entity_dissolver` look).
- Gravity Gun: view-model animations (`ACT_VM_*` incl. `hold_idle`, prongs `prong_blend` pose parameter opening on pull/hold), glow sprites, beams from the muzzle to the held/pulled prop (`sprites/physbeam.vmt`, `sprites/orangelight1*`), launch/punt flashes and sounds (`Weapon_PhysCannon.*`), object glow while held.
- Explosions and fire: `ExplosionCreate` look (fireball, smoke, sparks, dust, shockwave, screen shake/flash), burning props (flames on props, `Burning` in particles.rs today), barrel explosions.
- Bullet impacts per surface material (metal sparks, concrete dust, wood chips, flesh blood...), `g_pEffects->Sparks`, ricochet effects.
- Decals: bullet-hole decals on props (parented, per-surface), blood; check `decals.rs` against `scripts/decals.txt`; finish the metal decal alpha (the 2x modulate is done in rl_assets/decal.rs; bright rims are clipped because Bevy multiply cannot brighten: consider an additive second pass).
- Crossbow: bolt glow sprite at the tail (`sprites/light_glow02_noz.vmt`, scale 0.2, alpha 128), the bolt trail, orange-hot tip (selfillum is supported; check the alpha mask: only the glow texels should emit).

## 3. Gravity Gun strength
- Known (server.so ConVars): `physcannon_tracelength 250`, `maxmass 250`, `pullforce 4000`, `minforce 700`, `maxforce 1500`, `cone 0.97`, `ball_cone 0.997`, `chargetime 2`. `data/tools/manipulator.ron` pull_range is 250 now; launch/punt speeds are the old guesses (1400/1000 with mass curves).
- Derive from the code: disassemble `CWeaponPhysCannon` primary/secondary attack, `PuntVPhysics`, `LaunchObject`/`AttachObject`, `CGrabController` (vtable via RTTI, see above): force-to-velocity (is `force/mass` an impulse? what mass clamp?), pull acceleration, hold distance, max speed, drop/break distances, punt cooldowns (0.5 s?), the secondary refire, and the user's claim that HL2DM pulls objects from very far away (check `FindObject`/`Attract` distance logic, including objects beyond tracelength).
- Then retune `data/tools/manipulator.ron` and the manipulator tests; document in `docs/physics.md`.

## 4. Physics parameters in general
- Audit `data/physics.ron` / `movement.ron` against the game: `sv_*` ConVar defaults in server.so (accelerate 10, airaccelerate 10, friction 4, maxspeed 320, stopspeed, gravity 600 done, bounce 0, maxvelocity 3500), `surfaceproperties.txt` (friction, elasticity, density per material), prop mass/damping from `.phy`/propdata, player mass 85 kg, pushaway (`sv_pushaway_*`), fall damage, jump height (45 units), duck/unduck timings, water.
- Compare behaviours by test scenarios (jump height/air time, friction stop time, prop push speeds) against known HL2DM figures.

## 5. Smaller open items
- Selection sounds: soundscripts say `PITCH_NORM` and the client passes no pitch, so no pitch change was found. Ask the user what exactly they hear (or inspect `CHudWeaponSelection` in client.so for pitch arguments) before changing anything. HL2DM's default is `hud_fastswitch 0` (number key opens the list, fire confirms); ours switches at once.
- Remote players' reload-step and empty-click sounds (Noise events exist; reload steps play per step).
- SLAM: satchels do not stick to walls/players; add view-model detonator draw/holster animations (`ACT_SLAM_DETONATOR_*`) and the SLAM's world-model skins.
- Combine-ball exact bounce/speed behaviour (`prop_combine_ball` collision code), AR2 view-model fire/charge animations.
