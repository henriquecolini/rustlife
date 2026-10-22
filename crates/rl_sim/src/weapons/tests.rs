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

#[test]
fn the_loadout_comes_with_full_clips_and_its_weapon_in_hand() {
    let arsenal = arsenal();
    let data = GameData::builtin().expect("data");
    let inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
    let smg = id(&arsenal, "smg");
    assert_eq!(inventory.active(), Some(smg));
    assert_eq!(inventory.clip(smg), Some(45));
    assert_eq!(inventory.ammo(ammo(&arsenal, "smg")), 90);
    assert_eq!(inventory.phase(), Phase::Ready);
    assert_eq!(
        inventory.weapons().count(),
        data.rules.loadout.weapons.len()
    );
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
fn semi_automatic_weapons_need_a_press_per_shot() {
    let arsenal = arsenal();
    let mut inventory = with_weapon(&arsenal, "sidearm", 0);
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 66),
        1
    );
    let mut shots = 0;
    for _ in 0..5 {
        shots += run(&mut inventory, &arsenal, &holding(Buttons::NONE), 10);
        shots += run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 10);
    }
    assert_eq!(shots, 5);
}

#[test]
fn an_empty_clip_reloads_from_carried_ammo() {
    let arsenal = arsenal();
    let magnum = id(&arsenal, "magnum");
    let mut inventory = with_weapon(&arsenal, "magnum", 4);
    for _ in 0..6 {
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 1);
        run(&mut inventory, &arsenal, &holding(Buttons::NONE), 60);
    }
    // Emptied, it reloaded what was carried: 4 of 6 rounds.
    run(&mut inventory, &arsenal, &holding(Buttons::NONE), 66 * 3);
    assert_eq!(inventory.clip(magnum), Some(4));
    assert_eq!(inventory.ammo(ammo(&arsenal, "magnum")), 0);
    assert_eq!(inventory.phase(), Phase::Ready);
}

#[test]
fn the_reload_button_tops_up_a_partial_clip() {
    let arsenal = arsenal();
    let sidearm = id(&arsenal, "sidearm");
    let mut inventory = with_weapon(&arsenal, "sidearm", 30);
    run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 1);
    run(&mut inventory, &arsenal, &holding(Buttons::NONE), 30);
    let started = inventory.tick(&arsenal, &holding(Buttons::RELOAD), DT);
    assert!(started.reloading);
    // Reloading takes 1.4 s; firing does nothing meanwhile.
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::NONE), 60),
        0
    );
    assert!(matches!(inventory.phase(), Phase::Reloading(_)));
    run(&mut inventory, &arsenal, &holding(Buttons::NONE), 60);
    assert_eq!(inventory.clip(sidearm), Some(15));
    assert_eq!(inventory.ammo(ammo(&arsenal, "pistol")), 29);
}

#[test]
fn shells_load_one_at_a_time_and_firing_interrupts() {
    let arsenal = arsenal();
    let scattergun = id(&arsenal, "scattergun");
    let mut inventory = with_weapon(&arsenal, "scattergun", 12);
    // Fire twice, then reload: 0.4 s to start, 0.45 s per shell.
    for _ in 0..2 {
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 1);
        run(&mut inventory, &arsenal, &holding(Buttons::NONE), 60);
    }
    inventory.tick(&arsenal, &holding(Buttons::RELOAD), DT);
    // 0.6 s later: the first shell is in (at 0.4 s), the second is not
    // (0.85 s).
    run(&mut inventory, &arsenal, &holding(Buttons::NONE), 40);
    assert_eq!(inventory.clip(scattergun), Some(5), "one shell in");
    assert!(matches!(inventory.phase(), Phase::Reloading(_)));
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 1),
        1,
        "firing interrupts the reload"
    );
    assert_eq!(inventory.clip(scattergun), Some(4));
}

#[test]
fn secondary_fire_can_take_from_carried_ammo() {
    let arsenal = arsenal();
    let mut inventory = with_weapon(&arsenal, "smg", 0);
    let grenades = ammo(&arsenal, "smg_grenade");
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK2), 1),
        0,
        "no grenades"
    );
    inventory.give_ammo(&arsenal, grenades, 10);
    assert_eq!(inventory.ammo(grenades), 3, "capped at the type's max");
    let fired = inventory.tick(&arsenal, &holding(Buttons::NONE), DT);
    assert!(fired.fired.is_none());
    let fired = inventory.tick(&arsenal, &holding(Buttons::ATTACK2), DT);
    assert_eq!(fired.fired, Some((id(&arsenal, "smg"), Mode::Secondary)));
    assert_eq!(inventory.ammo(grenades), 2);
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
    // Deploying takes 0.4 s.
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::NONE), 26),
        0
    );
    assert_eq!(
        run(&mut inventory, &arsenal, &holding(Buttons::ATTACK), 2),
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
    assert_eq!((intervals.primary, intervals.secondary), (0.5, 0.4));
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
fn number_keys_cycle_through_a_slot() {
    let arsenal = arsenal();
    let data = GameData::builtin().expect("data");
    let mut inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
    let (manipulator, bar) = (id(&arsenal, "manipulator"), id(&arsenal, "bar"));
    assert_eq!(
        inventory.choose(&arsenal, Choice::Slot(1)),
        Some(manipulator)
    );
    inventory.select(&arsenal, manipulator);
    assert_eq!(inventory.choose(&arsenal, Choice::Slot(1)), Some(bar));
    inventory.select(&arsenal, bar);
    assert_eq!(
        inventory.choose(&arsenal, Choice::Slot(1)),
        Some(manipulator)
    );
    assert_eq!(
        inventory.choose(&arsenal, Choice::Slot(9)),
        None,
        "empty slot"
    );
    assert_eq!(inventory.choose(&arsenal, Choice::Last), Some(manipulator));
}

#[test]
fn next_and_previous_skip_weapons_without_ammo() {
    let arsenal = arsenal();
    let data = GameData::builtin().expect("data");
    let mut inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
    // The loadout: Manipulator, bar (1), sidearm (2), SMG (3, in hand),
    // frag (5).
    assert_eq!(
        inventory.choose(&arsenal, Choice::Next),
        Some(id(&arsenal, "frag"))
    );
    assert_eq!(
        inventory.choose(&arsenal, Choice::Previous),
        Some(id(&arsenal, "sidearm"))
    );
    // Without frags, next wraps round to the Manipulator.
    let frags = ammo(&arsenal, "frag");
    let mut empty = Inventory::empty(&arsenal);
    for weapon in inventory.weapons() {
        empty.give_weapon(&arsenal, weapon);
    }
    empty.select(&arsenal, id(&arsenal, "smg"));
    assert_eq!(empty.ammo(frags), 0);
    assert_eq!(
        empty.choose(&arsenal, Choice::Next),
        Some(id(&arsenal, "manipulator"))
    );
    inventory.select(&arsenal, id(&arsenal, "frag"));
    assert!(inventory.usable(&arsenal, id(&arsenal, "frag")));
}

/// A player with the SMG in hand and `frags` grenades, the frag selected.
fn with_frags(arsenal: &Arsenal, frags: u32) -> Inventory {
    let data = GameData::builtin().expect("data");
    let mut inventory = Inventory::loadout(arsenal, &data.rules.loadout);
    inventory.give_ammo(arsenal, ammo(arsenal, "frag"), frags.saturating_sub(1));
    let frag = id(arsenal, "frag");
    inventory.select(arsenal, frag);
    // Let it come up.
    run(&mut inventory, arsenal, &UserCmd::default(), 40);
    inventory
}

#[test]
fn a_grenade_is_primed_while_held_and_thrown_on_release() {
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

    let thrown = inventory.tick(&arsenal, &holding(Buttons::NONE), DT);
    assert_eq!(thrown.fired, Some((frag, Mode::Primary)));
    assert_eq!(inventory.ammo(frags), 1);
    assert_eq!(inventory.phase(), Phase::Ready);
}

#[test]
fn secondary_fire_primes_a_roll() {
    let arsenal = arsenal();
    let mut inventory = with_frags(&arsenal, 2);
    let frag = id(&arsenal, "frag");
    let pull = inventory.tick(&arsenal, &holding(Buttons::ATTACK2), DT);
    assert_eq!(pull.primed, Some((frag, Mode::Secondary)));
    // The other button does not change what is primed.
    let both = Buttons::from_bits(Buttons::ATTACK.bits() | Buttons::ATTACK2.bits());
    let held = inventory.tick(&arsenal, &holding(both), DT);
    assert_eq!(held.fired, None);
    let thrown = inventory.tick(&arsenal, &holding(Buttons::ATTACK), DT);
    assert_eq!(thrown.fired, Some((frag, Mode::Secondary)));
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
    for tick in 0..400 {
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
    let thrown = inventory.tick(&arsenal, &holding(Buttons::NONE), DT);
    assert_eq!(thrown.fired, Some((frag, Mode::Primary)));
    // The throw plays out with the grenade still in hand...
    assert_eq!(inventory.active(), Some(frag));
    assert!(inventory.has(frag));
    // ...and then it is gone, and the weapon before it is back.
    let mut retired = None;
    for _ in 0..120 {
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
    run(&mut inventory, &arsenal, &UserCmd::default(), 120);
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
    let (frag, smg) = (id(&arsenal, "frag"), id(&arsenal, "smg"));
    assert_eq!(inventory.choose(&arsenal, Choice::Weapon(frag)), Some(frag));
    assert_eq!(inventory.choose(&arsenal, Choice::Weapon(smg)), None);
    let empty = Inventory::empty(&arsenal);
    assert_eq!(empty.choose(&arsenal, Choice::Weapon(frag)), None);
}

#[test]
fn cycling_from_a_weapon_wraps_through_the_usable_ones() {
    let arsenal = arsenal();
    let data = GameData::builtin().expect("data");
    let inventory = Inventory::loadout(&arsenal, &data.rules.loadout);
    let (manipulator, frag, smg) = (
        id(&arsenal, "manipulator"),
        id(&arsenal, "frag"),
        id(&arsenal, "smg"),
    );
    assert_eq!(inventory.cycle(&arsenal, Some(smg), true), Some(frag));
    assert_eq!(
        inventory.cycle(&arsenal, Some(frag), true),
        Some(manipulator)
    );
    assert_eq!(
        inventory.cycle(&arsenal, Some(manipulator), false),
        Some(frag)
    );
    assert_eq!(inventory.cycle(&arsenal, None, true), Some(manipulator));
}
