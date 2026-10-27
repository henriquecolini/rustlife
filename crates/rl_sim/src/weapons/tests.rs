//! Inventory behaviour against the built-in weapon data.

use rl_data::GameData;

use super::*;

const DT: f32 = 1.0 / 66.0;

fn arsenal() -> Arsenal {
    Arsenal::new(&GameData::builtin().expect("builtin data"))
}

fn id(arsenal: &Arsenal, name: &str) -> WeaponId {
    arsenal.find_weapon(name).expect("weapon exists")
}

fn ammo(arsenal: &Arsenal, name: &str) -> AmmoId {
    arsenal.find_ammo(name).expect("ammo exists")
}

fn holding(buttons: Buttons) -> UserCmd {
    UserCmd {
        buttons,
        ..UserCmd::default()
    }
}

/// An inventory with just `weapon` in hand and `reserve` rounds for it.
fn with_weapon(arsenal: &Arsenal, weapon: &str, reserve: u32) -> Inventory {
    let mut inventory = Inventory::empty(arsenal);
    let weapon = id(arsenal, weapon);
    inventory.give_weapon(arsenal, weapon);
    if let Some(ammo) = arsenal
        .weapon(weapon)
        .and_then(WeaponConfig::ammo_type)
        .and_then(|name| arsenal.find_ammo(name))
    {
        inventory.give_ammo(arsenal, ammo, reserve);
    }
    inventory
}

/// Runs `ticks` ticks of `cmd`, counting shots fired.
fn run(inventory: &mut Inventory, arsenal: &Arsenal, cmd: &UserCmd, ticks: usize) -> usize {
    (0..ticks)
        .filter(|_| inventory.tick(arsenal, cmd, DT).fired.is_some())
        .count()
}

/// Presses a button for one tick, then lets it go.
fn click(inventory: &mut Inventory, arsenal: &Arsenal, button: Buttons) -> WeaponTick {
    let tick = inventory.tick(arsenal, &holding(button), DT);
    inventory.tick(arsenal, &holding(Buttons::NONE), DT);
    tick
}

/// Idles for `seconds`.
fn wait(inventory: &mut Inventory, arsenal: &Arsenal, seconds: f32) {
    run(
        inventory,
        arsenal,
        &holding(Buttons::NONE),
        (seconds / DT).ceil() as usize,
    );
}

#[test]
fn the_loadout_is_hl2dms_with_the_gravity_gun_in_hand() {
    let arsenal = arsenal();
    let data = GameData::builtin().expect("data");
    let inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
    let (manipulator, pistol, smg) = (
        id(&arsenal, "manipulator"),
        id(&arsenal, "sidearm"),
        id(&arsenal, "smg"),
    );
    assert_eq!(inventory.active(), Some(manipulator));
    assert_eq!(inventory.clip(pistol), Some(18));
    assert_eq!(inventory.clip(smg), Some(45));
    // 255 pistol rounds are given; 150 is all a player carries.
    assert_eq!(inventory.ammo(ammo(&arsenal, "pistol")), 150);
    assert_eq!(inventory.ammo(ammo(&arsenal, "smg")), 45);
    assert_eq!(inventory.ammo(ammo(&arsenal, "buckshot")), 6);
    assert_eq!(inventory.ammo(ammo(&arsenal, "magnum")), 6);
    assert_eq!(inventory.ammo(ammo(&arsenal, "frag")), 1);
    assert_eq!(inventory.phase(), Phase::Ready);
    assert_eq!(
        inventory.weapons().count(),
        data.rules.loadout.weapons.len()
    );
    assert!(inventory.has(id(&arsenal, "stunstick")));
}

#[test]
fn automatic_weapons_fire_at_their_rate() {
    let arsenal = arsenal();
    let mut inventory = with_weapon(&arsenal, "smg", 0);
    // 0.075 s between shots: one second of fire is 14 shots (the first at
    // once), short of the 45-round clip.
    let shots = run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 66);
    assert!((13..=15).contains(&shots), "{shots}");
    assert_eq!(inventory.clip(id(&arsenal, "smg")), Some(45 - shots as u32));
}

#[test]
fn the_beat_carries_over_between_ticks() {
    let arsenal = arsenal();
    let mut inventory = with_weapon(&arsenal, "smg", 0);
    // Ten seconds of fire at 0.075 s a shot is 133 rounds, not the 126 that
    // rounding every shot up to a whole tick would give (5 ticks each);
    // the clip empties first, so count what a 45-round clip allows.
    let shots = run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 66 * 4);
    assert_eq!(shots, 45, "the clip");
    let mut inventory = with_weapon(&arsenal, "smg", 0);
    let first_45_took = (0..400)
        .find(|_| {
            inventory
                .tick(&arsenal, &holding(Buttons::ATTACK), DT)
                .fired
                .is_some()
                && inventory.clip(id(&arsenal, "smg")) == Some(0)
        })
        .expect("emptied");
    // 44 beats of 0.075 s = 3.3 s = 217.8 ticks: tick 218, not 44 * 5 = 220.
    assert!((217..=219).contains(&first_45_took), "{first_45_took}");
}

#[test]
fn a_held_pistol_fires_every_half_second_but_clicks_fire_faster() {
    let arsenal = arsenal();
    let mut held = with_weapon(&arsenal, "sidearm", 0);
    // Two seconds held: shots at 0, 0.5, 1.0 and 1.5 s.
    assert_eq!(run(&mut held, &arsenal, &holding(Buttons::ATTACK), 132), 4);

    let mut clicked = with_weapon(&arsenal, "sidearm", 0);
    let mut shots = 0;
    for _ in 0..6 {
        // 0.076 s up, 0.06 s down: faster than half a second, slower than
        // the 0.1 s the pistol needs between clicks.
        shots += run(&mut clicked, &arsenal, &holding(Buttons::NONE), 5);
        shots += run(&mut clicked, &arsenal, &holding(Buttons::ATTACK), 4);
    }
    assert_eq!(shots, 6, "every click fires");
    // A click too soon after the last shot is not heard.
    let mut hasty = with_weapon(&arsenal, "sidearm", 0);
    let mut shots = run(&mut hasty, &arsenal, &holding(Buttons::ATTACK), 1);
    shots += run(&mut hasty, &arsenal, &holding(Buttons::NONE), 2);
    shots += run(&mut hasty, &arsenal, &holding(Buttons::ATTACK), 2);
    assert_eq!(shots, 1);
}

#[test]
fn the_pistols_cone_opens_with_rapid_fire_and_closes_when_rested() {
    let arsenal = arsenal();
    let mut inventory = with_weapon(&arsenal, "sidearm", 0);
    let first = click(&mut inventory, &arsenal, Buttons::ATTACK);
    assert_eq!(
        first.penalty, 0.0,
        "the first shot is as accurate as it gets"
    );
    let mut worst = 0.0_f32;
    for _ in 0..6 {
        wait(&mut inventory, &arsenal, 0.11);
        worst = worst.max(click(&mut inventory, &arsenal, Buttons::ATTACK).penalty);
    }
    assert!(worst > 0.5, "six quick shots widen the cone: {worst}");
    assert!(inventory.penalty() > 0.8);
    wait(&mut inventory, &arsenal, 3.0);
    assert_eq!(inventory.penalty(), 0.0, "rested");
}

#[test]
fn an_empty_clip_reloads_from_carried_ammo() {
    let arsenal = arsenal();
    let magnum = id(&arsenal, "magnum");
    let mut inventory = with_weapon(&arsenal, "magnum", 4);
    for _ in 0..6 {
        click(&mut inventory, &arsenal, Buttons::ATTACK);
        wait(&mut inventory, &arsenal, 0.8);
    }
    // Emptied, it reloaded what was carried: 4 of 6 rounds, in 3.667 s.
    wait(&mut inventory, &arsenal, 3.0);
    assert!(matches!(inventory.phase(), Phase::Reloading(_)));
    wait(&mut inventory, &arsenal, 1.0);
    assert_eq!(inventory.clip(magnum), Some(4));
    assert_eq!(inventory.ammo(ammo(&arsenal, "magnum")), 0);
    assert_eq!(inventory.phase(), Phase::Ready);
}

#[test]
fn the_reload_button_tops_up_a_partial_clip_in_the_animations_time() {
    let arsenal = arsenal();
    let sidearm = id(&arsenal, "sidearm");
    let mut inventory = with_weapon(&arsenal, "sidearm", 30);
    click(&mut inventory, &arsenal, Buttons::ATTACK);
    wait(&mut inventory, &arsenal, 0.5);
    let started = inventory.tick(&arsenal, &holding(Buttons::RELOAD), DT);
    assert!(started.reloading);
    // Reloading takes 1.433 s; firing does nothing meanwhile.
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 80),
        0
    );
    assert!(matches!(inventory.phase(), Phase::Reloading(_)));
    assert_eq!(inventory.clip(sidearm), Some(17), "not yet");
    run(&mut inventory, &arsenal, &holding(Buttons::NONE), 20);
    assert_eq!(inventory.clip(sidearm), Some(18));
    assert_eq!(inventory.ammo(ammo(&arsenal, "pistol")), 29);
}

#[test]
fn a_trigger_pulled_on_an_empty_clip_clicks_then_reloads() {
    let arsenal = arsenal();
    let magnum = id(&arsenal, "magnum");
    let mut inventory = with_weapon(&arsenal, "magnum", 6);
    for _ in 0..6 {
        click(&mut inventory, &arsenal, Buttons::ATTACK);
        wait(&mut inventory, &arsenal, 0.8);
    }
    // The empty weapon is already reloading by itself: let it finish, then
    // empty it again without ammo to reload from.
    wait(&mut inventory, &arsenal, 4.0);
    assert_eq!(inventory.clip(magnum), Some(6));

    let mut inventory = with_weapon(&arsenal, "magnum", 6);
    for _ in 0..6 {
        click(&mut inventory, &arsenal, Buttons::ATTACK);
        wait(&mut inventory, &arsenal, 0.8);
        if inventory.clip(magnum) == Some(0) {
            break;
        }
    }
    // With the trigger held to the end, the idle reload never gets its
    // chance: once the gun is ready the first pull clicks, the next reloads.
    let mut inventory = with_weapon(&arsenal, "magnum", 6);
    for _ in 0..5 {
        click(&mut inventory, &arsenal, Buttons::ATTACK);
        wait(&mut inventory, &arsenal, 0.8);
    }
    let last = inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
    assert!(last.fired.is_some());
    assert_eq!(inventory.clip(magnum), Some(0));
    let ticks: Vec<WeaponTick> = (0..70)
        .map(|_| inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT))
        .collect();
    let clicked = ticks
        .iter()
        .position(|tick| tick.noise == Some(Noise::Dry))
        .expect("clicked");
    assert!((48..=51).contains(&clicked), "after 0.75 s: {clicked}");
    assert_eq!(inventory.dry_count(), 1);
    assert!(ticks[clicked + 1].reloading, "{:?}", ticks[clicked + 1]);
    assert_eq!(
        ticks
            .iter()
            .filter(|tick| tick.noise == Some(Noise::Dry))
            .count(),
        1
    );
}

#[test]
fn a_weapon_with_nothing_left_gives_way_to_the_heaviest_with_ammo() {
    let arsenal = arsenal();
    let data = GameData::builtin().expect("data");
    let mut inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
    let magnum = id(&arsenal, "magnum");
    inventory.give_weapon(&arsenal, magnum);
    inventory.select(&arsenal, magnum);
    let magnum_rounds = ammo(&arsenal, "magnum");
    // Six rounds loaded and six more carried: spend them all.
    for _ in 0..12 {
        wait(&mut inventory, &arsenal, 5.0);
        click(&mut inventory, &arsenal, Buttons::ATTACK);
        if inventory.active() != Some(magnum) {
            break;
        }
    }
    assert_eq!(inventory.ammo(magnum_rounds), 0);
    wait(&mut inventory, &arsenal, 1.0);
    // The SMG (weight 3) outweighs the pistol (2), the grenade (1) and the
    // melee weapons (0); the Gravity Gun is never chosen.
    assert_eq!(inventory.active(), Some(id(&arsenal, "smg")));
}

#[test]
fn secondary_fire_can_take_from_carried_ammo() {
    let arsenal = arsenal();
    let mut inventory = with_weapon(&arsenal, "smg", 0);
    let grenades = ammo(&arsenal, "smg_grenade");
    let none = inventory.tick(&arsenal, &holding(Buttons::ATTACK2), DT);
    assert!(
        none.fired.is_none() && none.noise == Some(Noise::Dry),
        "no grenades: it clicks"
    );
    wait(&mut inventory, &arsenal, 0.6);
    inventory.give_ammo(&arsenal, grenades, 10);
    assert_eq!(inventory.ammo(grenades), 3, "capped at the type's max");
    let fired = inventory.tick(&arsenal, &holding(Buttons::ATTACK2), DT);
    assert_eq!(fired.fired, Some((id(&arsenal, "smg"), Mode::Secondary)));
    assert_eq!(inventory.ammo(grenades), 2);
    // HL2: the grenade holds the gun back for half a second and itself for
    // a second.
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 30),
        0
    );
    assert!(run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 10) >= 1);
    let mut again = 0;
    for _ in 0..20 {
        again += usize::from(
            inventory
                .tick(&arsenal, &holding(Buttons::ATTACK2), DT)
                .fired
                .is_some(),
        );
    }
    assert_eq!(again, 0, "a second has not passed");
}

#[test]
fn switching_deploys_before_firing() {
    let arsenal = arsenal();
    let mut inventory = with_weapon(&arsenal, "smg", 0);
    let sidearm = id(&arsenal, "sidearm");
    assert!(!inventory.select(&arsenal, sidearm), "not carried");
    inventory.give_weapon(&arsenal, sidearm);
    let switch = UserCmd {
        select: Some(sidearm),
        buttons: Buttons::ATTACK,
        ..UserCmd::default()
    };
    let tick = inventory.tick(&arsenal, &switch, DT);
    assert!(tick.switched && tick.fired.is_none());
    assert_eq!(inventory.last(), Some(id(&arsenal, "smg")));
    // The pistol takes 0.8 s to come up.
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 50),
        0
    );
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 6),
        1
    );
}

#[test]
fn the_manipulator_is_a_tool_once_deployed() {
    let arsenal = arsenal();
    let mut inventory = with_weapon(&arsenal, "smg", 0);
    let manipulator = id(&arsenal, "manipulator");
    inventory.give_weapon(&arsenal, manipulator);
    assert_eq!(inventory.tool(&arsenal), None, "a gun is in hand");
    inventory.select(&arsenal, manipulator);
    assert_eq!(inventory.tool(&arsenal), None, "still deploying");
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 66),
        0,
        "a tool never fires through the inventory"
    );
    let intervals = inventory.tool(&arsenal).expect("ready");
    assert_eq!((intervals.primary, intervals.secondary), (0.5, 0.5));
}

#[test]
fn new_weapons_come_loaded_but_only_once() {
    let arsenal = arsenal();
    let mut inventory = Inventory::empty(&arsenal);
    let rifle = id(&arsenal, "rifle");
    assert!(inventory.give_weapon(&arsenal, rifle));
    assert!(!inventory.give_weapon(&arsenal, rifle));
    assert_eq!(inventory.active(), Some(rifle), "taken in hand");
    assert_eq!(inventory.clip(rifle), Some(30));
}

#[test]
fn shotguns_are_pumped_after_every_shot() {
    let arsenal = arsenal();
    let scattergun = id(&arsenal, "scattergun");
    let mut inventory = with_weapon(&arsenal, "scattergun", 12);
    let shot = inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
    assert!(shot.fired.is_some());
    // 0.333 s of firing, then the pump.
    let mut pumped_at = None;
    for tick in 1..100 {
        if inventory
            .tick(&arsenal, &holding(Buttons::ATTACK), DT)
            .noise
            == Some(Noise::Pump)
        {
            pumped_at = Some(tick);
            break;
        }
    }
    let pumped_at = pumped_at.expect("pumped");
    assert!((21..=23).contains(&pumped_at), "{pumped_at}");
    assert_eq!(inventory.pump_count(), 1);
    // The pump takes 0.533 s, in which nothing fires; then it does.
    let mut ready_after = None;
    for tick in 1..100 {
        if inventory
            .tick(&arsenal, &holding(Buttons::ATTACK), DT)
            .fired
            .is_some()
        {
            ready_after = Some(tick);
            break;
        }
    }
    let ready_after = ready_after.expect("fires again");
    assert!((34..=37).contains(&ready_after), "{ready_after}");
    assert_eq!(inventory.clip(scattergun), Some(4));
}

#[test]
fn shells_load_one_at_a_time_and_firing_interrupts() {
    let arsenal = arsenal();
    let scattergun = id(&arsenal, "scattergun");
    let mut inventory = with_weapon(&arsenal, "scattergun", 12);
    for _ in 0..2 {
        click(&mut inventory, &arsenal, Buttons::ATTACK);
        wait(&mut inventory, &arsenal, 1.0);
    }
    assert_eq!(inventory.clip(scattergun), Some(4));
    let started = inventory.tick(&arsenal, &holding(Buttons::RELOAD), DT);
    assert!(started.reloading);
    assert_eq!(inventory.reload_step(), ReloadStep::Start);
    // The reload takes 0.5 s to begin; the first shell goes in then.
    wait(&mut inventory, &arsenal, 0.45);
    assert_eq!(inventory.clip(scattergun), Some(4));
    wait(&mut inventory, &arsenal, 0.1);
    assert_eq!(inventory.clip(scattergun), Some(5), "one shell in");
    assert_eq!(inventory.reload_step(), ReloadStep::Round);
    assert!(matches!(inventory.phase(), Phase::Reloading(_)));
    // Firing during a shell's 0.4 s ends the reload, but the shot waits for
    // the shell to be in.
    let mut fired_at = None;
    for tick in 0..60 {
        if inventory
            .tick(&arsenal, &holding(Buttons::ATTACK), DT)
            .fired
            .is_some()
        {
            fired_at = Some(tick);
            break;
        }
    }
    let fired_at = fired_at.expect("interrupted");
    assert!(fired_at < 27, "within the shell's time: {fired_at}");
    assert_eq!(inventory.clip(scattergun), Some(4));
    assert_eq!(inventory.reload_step(), ReloadStep::Whole);
}

#[test]
fn a_full_shotgun_reload_takes_its_start_its_shells_and_its_finish() {
    let arsenal = arsenal();
    let scattergun = id(&arsenal, "scattergun");
    let mut inventory = with_weapon(&arsenal, "scattergun", 12);
    for _ in 0..2 {
        click(&mut inventory, &arsenal, Buttons::ATTACK);
        wait(&mut inventory, &arsenal, 1.0);
    }
    inventory.tick(&arsenal, &holding(Buttons::RELOAD), DT);
    // Two shells: in at 0.5 s and 0.9 s, then (the last one's 0.4 s over)
    // 0.433 s to close.
    wait(&mut inventory, &arsenal, 0.95);
    assert_eq!(inventory.clip(scattergun), Some(6));
    assert_eq!(inventory.reload_step(), ReloadStep::Round);
    wait(&mut inventory, &arsenal, 0.4);
    assert_eq!(inventory.reload_step(), ReloadStep::Finish);
    assert_eq!(inventory.phase(), Phase::Ready);
    assert!(inventory.until_ready(Mode::Primary) > 0.3, "still closing");
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 20),
        0
    );
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 10),
        1,
        "ready 1.733 s after the reload began"
    );
    assert_eq!(inventory.ammo(ammo(&arsenal, "buckshot")), 10);
}

#[test]
fn the_double_barrel_takes_two_shells_or_fires_the_last_one_alone() {
    let arsenal = arsenal();
    let scattergun = id(&arsenal, "scattergun");
    let mut inventory = with_weapon(&arsenal, "scattergun", 0);
    let double = inventory.tick(&arsenal, &holding(Buttons::ATTACK2), DT);
    assert_eq!(double.fired, Some((scattergun, Mode::Secondary)));
    assert_eq!(inventory.clip(scattergun), Some(4));
    wait(&mut inventory, &arsenal, 2.0);
    click(&mut inventory, &arsenal, Buttons::ATTACK2);
    wait(&mut inventory, &arsenal, 2.0);
    click(&mut inventory, &arsenal, Buttons::ATTACK2);
    wait(&mut inventory, &arsenal, 2.0);
    assert_eq!(inventory.clip(scattergun), Some(0));
    let mut single = with_weapon(&arsenal, "scattergun", 0);
    for _ in 0..5 {
        click(&mut single, &arsenal, Buttons::ATTACK);
        wait(&mut single, &arsenal, 1.5);
    }
    assert_eq!(single.clip(scattergun), Some(1));
    let last = single.tick(&arsenal, &holding(Buttons::ATTACK2), DT);
    assert_eq!(last.fired, Some((scattergun, Mode::Primary)), "one shell");
    assert_eq!(single.clip(scattergun), Some(0));
}

#[test]
fn the_crossbow_reloads_itself_once_its_shot_is_over_and_zooms() {
    let arsenal = arsenal();
    let crossbow = id(&arsenal, "boltcaster");
    let mut inventory = with_weapon(&arsenal, "boltcaster", 5);
    let shot = inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
    assert_eq!(shot.fired, Some((crossbow, Mode::Primary)));
    assert_eq!(inventory.clip(crossbow), Some(0));
    // The next bolt: reloading starts 0.667 s after the shot even with the
    // trigger held, takes 1.833 s, so a held trigger fires every 2.5 s.
    let mut reloading_at = None;
    let mut fired_at = None;
    for tick in 1..400 {
        let result = inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
        if result.reloading && reloading_at.is_none() {
            reloading_at = Some(tick);
        }
        if result.fired.is_some() {
            fired_at = Some(tick);
            break;
        }
    }
    let (reloading_at, fired_at) = (reloading_at.expect("reloaded"), fired_at.expect("fired"));
    assert!((43..=46).contains(&reloading_at), "{reloading_at}");
    assert!((163..=167).contains(&fired_at), "{fired_at}");

    // The alternate button toggles the scope; putting the weapon away
    // ends it.
    assert!(!inventory.zoomed());
    let on = click(&mut inventory, &arsenal, Buttons::ATTACK2);
    assert_eq!(on.zoom, Some(true));
    assert!(inventory.zoomed());
    let off = click(&mut inventory, &arsenal, Buttons::ATTACK2);
    assert_eq!(off.zoom, Some(false));
    click(&mut inventory, &arsenal, Buttons::ATTACK2);
    let sidearm = id(&arsenal, "sidearm");
    inventory.give_weapon(&arsenal, sidearm);
    inventory.select(&arsenal, sidearm);
    assert!(!inventory.zoomed());
}

#[test]
fn the_energy_ball_charges_then_spends_its_charge_and_busies_the_rifle() {
    let arsenal = arsenal();
    let charges = ammo(&arsenal, "rifle_charge");
    let mut inventory = with_weapon(&arsenal, "rifle", 0);
    inventory.give_ammo(&arsenal, charges, 2);
    let press = inventory.tick(&arsenal, &holding(Buttons::ATTACK2), DT);
    assert_eq!(press.noise, Some(Noise::Charge));
    assert_eq!(press.fired, None);
    assert_eq!(inventory.ammo(charges), 2, "nothing spent yet");
    let mut fired_at = None;
    for tick in 1..100 {
        let result = inventory.tick(&arsenal, &holding(Buttons::NONE), DT);
        if result.fired.is_some() {
            fired_at = Some(tick);
            break;
        }
    }
    let fired_at = fired_at.expect("the ball is launched");
    assert!((32..=34).contains(&fired_at), "half a second: {fired_at}");
    assert_eq!(inventory.ammo(charges), 1);
    // The rifle is busy for a second after the launch.
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 60),
        0
    );
    assert!(run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 10) >= 1);
}

#[test]
fn a_launcher_fires_one_rocket_at_a_time_and_reloads_when_it_is_gone() {
    let arsenal = arsenal();
    let launcher = id(&arsenal, "launcher");
    let rockets = ammo(&arsenal, "rocket");
    let mut inventory = with_weapon(&arsenal, "launcher", 3);
    let shot = inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
    assert_eq!(shot.fired, Some((launcher, Mode::Primary)));
    assert_eq!(inventory.ammo(rockets), 2);
    // While the rocket flies nothing fires, not even to click.
    let held = (0..200).map(|_| inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT));
    assert!(
        held.into_iter()
            .all(|tick| tick.fired.is_none() && tick.noise.is_none())
    );
    inventory.projectile_ended(&arsenal, launcher);
    assert!(matches!(inventory.phase(), Phase::Reloading(_)));
    assert_eq!(inventory.reload_step(), ReloadStep::Whole);
    // 1.667 s of reloading.
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 100),
        0
    );
    assert!(run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 20) >= 1);
}

#[test]
fn the_launchers_laser_toggles_and_comes_on_with_the_weapon() {
    let arsenal = arsenal();
    let mut inventory = with_weapon(&arsenal, "launcher", 3);
    assert!(inventory.laser());
    click(&mut inventory, &arsenal, Buttons::ATTACK2);
    assert!(!inventory.laser());
    click(&mut inventory, &arsenal, Buttons::ATTACK2);
    assert!(inventory.laser());
}

#[test]
fn number_keys_cycle_through_a_slot() {
    let arsenal = arsenal();
    let data = GameData::builtin().expect("data");
    let mut inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
    let (manipulator, bar, stunstick) = (
        id(&arsenal, "manipulator"),
        id(&arsenal, "bar"),
        id(&arsenal, "stunstick"),
    );
    // Slot 1 holds the Crowbar, the Stun Baton and the Gravity Gun, in that
    // order, with the Gravity Gun in hand.
    assert_eq!(inventory.choose(&arsenal, Choice::Slot(1)), Some(bar));
    inventory.select(&arsenal, bar);
    assert_eq!(inventory.choose(&arsenal, Choice::Slot(1)), Some(stunstick));
    inventory.select(&arsenal, stunstick);
    assert_eq!(
        inventory.choose(&arsenal, Choice::Slot(1)),
        Some(manipulator)
    );
    assert_eq!(
        inventory.choose(&arsenal, Choice::Slot(9)),
        None,
        "empty slot"
    );
    assert_eq!(inventory.choose(&arsenal, Choice::Last), Some(bar));
}

#[test]
fn next_and_previous_skip_weapons_without_ammo() {
    let arsenal = arsenal();
    let data = GameData::builtin().expect("data");
    let inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
    // In order: Crowbar, Stun Baton, Gravity Gun (in hand), pistol (2),
    // SMG (3), grenade (5).
    assert_eq!(
        inventory.choose(&arsenal, Choice::Next),
        Some(id(&arsenal, "sidearm"))
    );
    assert_eq!(
        inventory.choose(&arsenal, Choice::Previous),
        Some(id(&arsenal, "stunstick"))
    );
    // Without frags, the grenade is skipped and next from the SMG wraps
    // round to the Crowbar.
    let mut empty = Inventory::empty(&arsenal);
    for weapon in inventory.weapons() {
        empty.give_weapon(&arsenal, weapon);
    }
    empty.select(&arsenal, id(&arsenal, "smg"));
    assert_eq!(empty.ammo(ammo(&arsenal, "frag")), 0);
    assert_eq!(
        empty.choose(&arsenal, Choice::Next),
        Some(id(&arsenal, "bar"))
    );
    assert!(inventory.usable(&arsenal, id(&arsenal, "frag")));
}

/// A player with the SMG in hand and `frags` grenades, the frag selected.
fn with_frags(arsenal: &Arsenal, frags: u32) -> Inventory {
    let data = GameData::builtin().expect("data");
    let mut inventory = Inventory::loadout(arsenal, &data.rules.loadout);
    inventory.select(arsenal, id(arsenal, "smg"));
    wait(&mut inventory, arsenal, 1.0);
    inventory.give_ammo(arsenal, ammo(arsenal, "frag"), frags.saturating_sub(1));
    let frag = id(arsenal, "frag");
    inventory.select(arsenal, frag);
    // Let it come up (1.267 s).
    wait(&mut inventory, arsenal, 1.4);
    inventory
}

#[test]
fn a_grenade_is_primed_while_held_and_thrown_a_moment_after_release() {
    let arsenal = arsenal();
    let mut inventory = with_frags(&arsenal, 2);
    let frag = id(&arsenal, "frag");
    let frags = ammo(&arsenal, "frag");

    let pull = inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
    assert_eq!(pull.primed, Some((frag, Mode::Primary)));
    assert_eq!(pull.fired, None);
    assert_eq!(inventory.phase(), Phase::Priming(Mode::Primary));
    // Held for as long as the player likes: nothing is thrown or spent.
    for _ in 0..200 {
        let held = inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
        assert_eq!((held.fired, held.primed), (None, None));
    }
    assert_eq!(inventory.ammo(frags), 2);

    // Let go: the grenade leaves the hand 0.05 s into the throw.
    inventory.tick(&arsenal, &holding(Buttons::NONE), DT);
    assert!(matches!(
        inventory.phase(),
        Phase::Charging(Mode::Primary, _)
    ));
    let mut thrown = None;
    for tick in 1..10 {
        if let Some(fired) = inventory.tick(&arsenal, &holding(Buttons::NONE), DT).fired {
            thrown = Some((tick, fired));
            break;
        }
    }
    let (tick, fired) = thrown.expect("thrown");
    assert_eq!(fired, (frag, Mode::Primary));
    assert!((2..=4).contains(&tick), "{tick}");
    assert_eq!(inventory.ammo(frags), 1);
    assert_eq!(inventory.phase(), Phase::Ready);
    // The next one is ready once the throw and the draw are over.
    assert!(inventory.until_ready(Mode::Primary) > 1.5);
}

#[test]
fn secondary_fire_primes_a_lob() {
    let arsenal = arsenal();
    let mut inventory = with_frags(&arsenal, 2);
    let frag = id(&arsenal, "frag");
    let pull = inventory.tick(&arsenal, &holding(Buttons::ATTACK2), DT);
    assert_eq!(pull.primed, Some((frag, Mode::Secondary)));
    // The other button does not change what is primed.
    let both = Buttons::from_bits(Buttons::ATTACK.bits() | Buttons::ATTACK2.bits());
    let held = inventory.tick(&arsenal, &holding(both), DT);
    assert_eq!(held.fired, None);
    inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
    let thrown = (0..12)
        .find_map(|_| {
            inventory
                .tick(&arsenal, &holding(Buttons::ATTACK), DT)
                .fired
        })
        .expect("thrown");
    assert_eq!(thrown, (frag, Mode::Secondary));
}

#[test]
fn switching_away_from_a_primed_grenade_cancels_the_throw() {
    let arsenal = arsenal();
    let mut inventory = with_frags(&arsenal, 2);
    let frags = ammo(&arsenal, "frag");
    inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
    let swap = UserCmd {
        buttons: Buttons::ATTACK,
        select: Some(id(&arsenal, "smg")),
        ..UserCmd::default()
    };
    assert!(inventory.tick(&arsenal, &swap, DT).switched);
    assert_eq!(inventory.active(), Some(id(&arsenal, "smg")));
    assert!(!matches!(inventory.phase(), Phase::Priming(_)));
    // Letting go throws nothing, and nothing was spent.
    for _ in 0..60 {
        let tick = inventory.tick(&arsenal, &holding(Buttons::NONE), DT);
        assert_eq!(tick.fired, None);
    }
    assert_eq!(inventory.ammo(frags), 2);
}

#[test]
fn grenades_are_thrown_one_at_a_time() {
    let arsenal = arsenal();
    let mut inventory = with_frags(&arsenal, 3);
    let frag = id(&arsenal, "frag");
    let mut thrown = 0;
    // The button is held and let go over and over.
    for tick in 0..800 {
        let button = if tick % 40 < 20 {
            Buttons::ATTACK
        } else {
            Buttons::NONE
        };
        // (With the last one gone the SMG is back in hand, and fires.)
        if let Some((weapon, _)) = inventory.tick(&arsenal, &holding(button), DT).fired
            && weapon == frag
        {
            thrown += 1;
        }
    }
    assert_eq!(thrown, 3);
}

#[test]
fn the_last_grenade_leaves_no_weapon_in_hand() {
    let arsenal = arsenal();
    let mut inventory = with_frags(&arsenal, 1);
    let frag = id(&arsenal, "frag");
    inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
    inventory.tick(&arsenal, &holding(Buttons::NONE), DT);
    let thrown = (0..12)
        .find_map(|_| inventory.tick(&arsenal, &holding(Buttons::NONE), DT).fired)
        .expect("thrown");
    assert_eq!(thrown, (frag, Mode::Primary));
    // The throw plays out with the grenade still in hand...
    assert_eq!(inventory.active(), Some(frag));
    assert!(inventory.has(frag));
    // ...and then it is gone, and the weapon before it is back.
    let mut retired = None;
    for _ in 0..240 {
        retired = retired.or(inventory
            .tick(&arsenal, &holding(Buttons::NONE), DT)
            .retired);
    }
    assert_eq!(retired, Some(frag));
    assert!(!inventory.has(frag));
    assert_eq!(inventory.active(), Some(id(&arsenal, "smg")));
}

#[test]
fn picking_a_grenade_up_brings_the_weapon_back() {
    let arsenal = arsenal();
    let mut inventory = with_frags(&arsenal, 1);
    let frag = id(&arsenal, "frag");
    inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
    inventory.tick(&arsenal, &holding(Buttons::NONE), DT);
    run(&mut inventory, &arsenal, &UserCmd::default(), 240);
    assert!(!inventory.has(frag));
    assert!(inventory.give_weapon(&arsenal, frag));
    inventory.give_ammo(&arsenal, ammo(&arsenal, "frag"), 1);
    assert!(inventory.usable(&arsenal, frag));
    assert!(inventory.select(&arsenal, frag));
}

#[test]
fn a_chosen_weapon_must_be_carried_and_differ_from_the_one_in_hand() {
    let arsenal = arsenal();
    let data = GameData::builtin().expect("data");
    let inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
    let (frag, manipulator) = (id(&arsenal, "frag"), id(&arsenal, "manipulator"));
    assert_eq!(inventory.choose(&arsenal, Choice::Weapon(frag)), Some(frag));
    assert_eq!(
        inventory.choose(&arsenal, Choice::Weapon(manipulator)),
        None
    );
    let empty = Inventory::empty(&arsenal);
    assert_eq!(empty.choose(&arsenal, Choice::Weapon(frag)), None);
}

#[test]
fn cycling_from_a_weapon_wraps_through_the_usable_ones() {
    let arsenal = arsenal();
    let data = GameData::builtin().expect("data");
    let inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
    let (bar, frag, smg) = (
        id(&arsenal, "bar"),
        id(&arsenal, "frag"),
        id(&arsenal, "smg"),
    );
    assert_eq!(inventory.cycle(&arsenal, Some(smg), true), Some(frag));
    assert_eq!(inventory.cycle(&arsenal, Some(frag), true), Some(bar));
    assert_eq!(inventory.cycle(&arsenal, Some(bar), false), Some(frag));
    assert_eq!(inventory.cycle(&arsenal, None, true), Some(bar));
}

#[test]
fn stocking_gives_every_weapon_full_clips_and_full_ammo() {
    let arsenal = arsenal();
    let mut inventory = with_weapon(&arsenal, "sidearm", 0);
    click(&mut inventory, &arsenal, Buttons::ATTACK);
    inventory.stock(&arsenal);
    assert_eq!(inventory.weapons().count(), arsenal.weapons().count());
    assert_eq!(inventory.clip(id(&arsenal, "sidearm")), Some(18));
    assert_eq!(inventory.clip(id(&arsenal, "scattergun")), Some(6));
    assert_eq!(inventory.ammo(ammo(&arsenal, "smg")), 225);
    assert_eq!(inventory.ammo(ammo(&arsenal, "rocket")), 3);
}

#[test]
fn a_heavier_weapon_picked_up_is_brought_up_unless_the_gravity_gun_is_in_hand() {
    let arsenal = arsenal();
    let mut inventory = with_weapon(&arsenal, "sidearm", 0);
    let (magnum, bar, tool) = (
        id(&arsenal, "magnum"),
        id(&arsenal, "bar"),
        id(&arsenal, "manipulator"),
    );
    inventory.give_weapon(&arsenal, bar);
    assert!(!inventory.switch_for_pickup(&arsenal, bar), "lighter");
    inventory.give_weapon(&arsenal, magnum);
    assert!(inventory.switch_for_pickup(&arsenal, magnum));
    assert_eq!(inventory.active(), Some(magnum));
    inventory.give_weapon(&arsenal, tool);
    inventory.select(&arsenal, tool);
    let smg = id(&arsenal, "smg");
    inventory.give_weapon(&arsenal, smg);
    assert!(
        !inventory.switch_for_pickup(&arsenal, smg),
        "the tool stays"
    );
}
