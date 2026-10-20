//! Tests of the map's logic on its own, without a simulation: entities
//! wired together, run tick by tick with hand-made senses.

use glam::Vec3;
use rl_data::GameData;
use source_formats::entities::MapEntity;

use super::mover::Phase;
use super::{Effect, MapLogic, Senses};
use crate::collision::{CollisionWorld, WorldBuilder};
use crate::simulation::PlayerId;

const DT: f32 = 0.1;

fn entity(classname: &str, keys: &[(&str, &str)]) -> MapEntity {
    keys.iter().fold(
        MapEntity::new(classname, [0.0; 3]),
        |entity, (key, value)| entity.with(key, value),
    )
}

/// A world whose brush entities are, in order, `models` (classname, name),
/// each a 64-unit box at the origin of its own frame.
fn world(models: &[(&str, Option<&str>)]) -> CollisionWorld {
    models
        .iter()
        .fold(WorldBuilder::new(), |builder, &(classname, name)| {
            builder.brush_entity(classname, name, Vec3::ZERO, Vec3::ZERO, Vec3::splat(64.0))
        })
        .build()
}

struct Run {
    logic: MapLogic,
    tick: u64,
}

impl Run {
    fn new(entities: &[MapEntity], world: &CollisionWorld) -> Self {
        Self::with_hazards(entities, world, &[])
    }

    fn with_hazards(
        entities: &[MapEntity],
        world: &CollisionWorld,
        hazards: &[(usize, usize)],
    ) -> Self {
        let data = GameData::builtin().expect("builtin data");
        Self {
            logic: MapLogic::new(entities, world, hazards, &data.map_logic, DT),
            tick: 0,
        }
    }

    /// Runs one tick with `senses`, moving movers straight to where they
    /// go; returns the effects.
    fn step(&mut self, senses: &Senses) -> Vec<Effect> {
        self.tick += 1;
        let mut effects = Vec::new();
        self.logic.step(self.tick, senses, &mut effects);
        for index in 0..self.logic.movers().len() {
            let mover = self.logic.mover_mut(index).expect("mover");
            if let Some(next) = mover.next_position(DT)
                && let Some(output) = mover.arrive(next, self.tick)
            {
                self.logic.fire_mover(index, output);
            }
        }
        effects
    }

    /// Runs `ticks` idle ticks, gathering the effects.
    fn idle(&mut self, ticks: usize) -> Vec<Effect> {
        (0..ticks)
            .flat_map(|_| self.step(&Senses::default()))
            .collect()
    }
}

fn hazard_switches(effects: &[Effect]) -> Vec<bool> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Hazard { enabled, .. } => Some(*enabled),
            _ => None,
        })
        .collect()
}

/// A `trigger_hurt` (entity 0, hazard 0) and the entities after it.
fn with_hurt(rest: Vec<MapEntity>) -> Vec<MapEntity> {
    let mut entities = vec![entity(
        "trigger_hurt",
        &[("targetname", "pit"), ("StartDisabled", "1")],
    )];
    entities.extend(rest);
    entities
}

#[test]
fn logic_auto_starts_a_delayed_chain() {
    let entities = with_hurt(vec![
        entity("logic_auto", &[("OnMapSpawn", "relay,Trigger,,1,-1")]),
        entity(
            "logic_relay",
            &[("targetname", "relay"), ("OnTrigger", "pit,Enable,,0.5,-1")],
        ),
    ]);
    let mut run = Run::with_hazards(&entities, &world(&[]), &[(0, 0)]);
    let early = run.idle(15);
    assert!(hazard_switches(&early).is_empty(), "1.5 s: not yet");
    let later = run.idle(1);
    assert_eq!(hazard_switches(&later), [true], "1 s + 0.5 s");
}

#[test]
fn relays_that_fire_once_or_are_disabled_stay_quiet() {
    let entities = with_hurt(vec![
        entity(
            "logic_auto",
            &[
                ("OnMapSpawn", "once,Trigger,,0,-1"),
                ("OnMapSpawn", "once,Trigger,,0.2,-1"),
                ("OnMapSpawn", "off,Trigger,,0,-1"),
            ],
        ),
        entity(
            "logic_relay",
            &[
                ("targetname", "once"),
                ("spawnflags", "1"),
                ("OnTrigger", "pit,Toggle,,0,-1"),
            ],
        ),
        entity(
            "logic_relay",
            &[
                ("targetname", "off"),
                ("StartDisabled", "1"),
                ("OnTrigger", "pit,Toggle,,0,-1"),
            ],
        ),
    ]);
    let mut run = Run::with_hazards(&entities, &world(&[]), &[(0, 0)]);
    assert_eq!(hazard_switches(&run.idle(10)), [true]);
}

#[test]
fn connections_fire_only_their_times() {
    let entities = with_hurt(vec![entity(
        "logic_timer",
        &[("RefireTime", "0.5"), ("OnTimer", "pit,Toggle,,0,2")],
    )]);
    let mut run = Run::with_hazards(&entities, &world(&[]), &[(0, 0)]);
    assert_eq!(
        hazard_switches(&run.idle(40)),
        [true, false],
        "twice, then spent"
    );
}

#[test]
fn timers_fire_on_their_interval_and_obey_enable() {
    let entities = with_hurt(vec![
        entity(
            "logic_timer",
            &[
                ("targetname", "clock"),
                ("RefireTime", "1"),
                ("OnTimer", "pit,Toggle,,0,-1"),
            ],
        ),
        entity("logic_auto", &[("OnMapSpawn", "clock,Disable,,3.05,-1")]),
    ]);
    let mut run = Run::with_hazards(&entities, &world(&[]), &[(0, 0)]);
    let switches = hazard_switches(&run.idle(100));
    assert_eq!(switches, [true, false, true], "at 1, 2 and 3 s, then off");
}

#[test]
fn random_timers_fire_within_their_bounds_repeatably() {
    let entities = with_hurt(vec![entity(
        "logic_timer",
        &[
            ("UseRandomTime", "1"),
            ("LowerRandomBound", "1"),
            ("UpperRandomBound", "2"),
            ("OnTimer", "pit,Toggle,,0,-1"),
        ],
    )]);
    let fire_ticks = || {
        let mut run = Run::with_hazards(&entities, &world(&[]), &[(0, 0)]);
        (0..200)
            .filter_map(|_| (!run.step(&Senses::default()).is_empty()).then_some(run.tick))
            .collect::<Vec<_>>()
    };
    let ticks = fire_ticks();
    assert!(ticks.len() >= 9 && ticks.len() <= 21, "{ticks:?}");
    for pair in ticks.windows(2) {
        assert!((10..=20).contains(&(pair[1] - pair[0])), "{ticks:?}");
    }
    assert_eq!(ticks, fire_ticks(), "seeded by tick and timer");
}

#[test]
fn counters_pass_their_value_on_and_hit_their_maximum_once() {
    let entities = with_hurt(vec![
        entity(
            "logic_auto",
            &[
                ("OnMapSpawn", "count,Add,1,0,-1"),
                ("OnMapSpawn", "count,Add,1,0.1,-1"),
                ("OnMapSpawn", "count,Add,1,0.2,-1"),
                ("OnMapSpawn", "count,Add,5,0.3,-1"),
            ],
        ),
        entity(
            "math_counter",
            &[
                ("targetname", "count"),
                ("max", "3"),
                ("OnHitMax", "pit,Toggle,,0,-1"),
                ("OutValue", "copy,SetValue,,0,-1"),
            ],
        ),
        entity(
            "math_counter",
            &[
                ("targetname", "copy"),
                ("max", "100"),
                ("OnHitMax", "pit,Toggle,,0,-1"),
            ],
        ),
    ]);
    let mut run = Run::with_hazards(&entities, &world(&[]), &[(0, 0)]);
    assert_eq!(
        hazard_switches(&run.idle(10)),
        [true],
        "3 reached once; the copy stays below 100"
    );
}

#[test]
fn triggers_fire_for_players_inside_and_wait_between() {
    let entities = with_hurt(vec![
        entity(
            "trigger_multiple",
            &[
                ("model", "*1"),
                ("spawnflags", "1"),
                ("wait", "1"),
                ("OnTrigger", "pit,Toggle,,0,-1"),
            ],
        ),
        entity(
            "trigger_multiple",
            &[
                ("model", "*2"),
                ("spawnflags", "2"),
                ("OnTrigger", "pit,Toggle,,0,-1"),
            ],
        ),
    ]);
    let world = world(&[("trigger_multiple", None), ("trigger_multiple", None)]);
    let mut run = Run::with_hazards(&entities, &world, &[(0, 0)]);
    let inside = [(
        PlayerId::from_index(0),
        Vec3::splat(10.0),
        Vec3::splat(20.0),
    )];
    let senses = Senses {
        players: &inside,
        ..Senses::default()
    };
    let mut switches = Vec::new();
    for _ in 0..25 {
        switches.extend(hazard_switches(&run.step(&senses)));
    }
    // Ticks 1, 11 and 21; the NPC-only trigger never fires.
    assert_eq!(switches, [true, false, true]);
}

#[test]
fn touch_outputs_follow_players_in_and_out() {
    let entities = with_hurt(vec![entity(
        "trigger_once",
        &[
            ("model", "*1"),
            ("spawnflags", "1"),
            ("OnStartTouch", "pit,Enable,,0,-1"),
            ("OnEndTouch", "pit,Disable,,0,-1"),
            ("OnTrigger", "pit,Toggle,,0,-1"),
        ],
    )]);
    let world = world(&[("trigger_once", None)]);
    let mut run = Run::with_hazards(&entities, &world, &[(0, 0)]);
    let inside = [(
        PlayerId::from_index(0),
        Vec3::splat(10.0),
        Vec3::splat(20.0),
    )];
    let entering = run.step(&Senses {
        players: &inside,
        ..Senses::default()
    });
    assert_eq!(
        hazard_switches(&entering),
        [true, false],
        "start touch, then the trigger's toggle"
    );
    // A trigger_once is gone once spent: leaving and coming back does
    // nothing more.
    let left = run.step(&Senses::default());
    assert!(hazard_switches(&left).is_empty(), "no end touch");
    let back = run.step(&Senses {
        players: &inside,
        ..Senses::default()
    });
    assert!(hazard_switches(&back).is_empty(), "no start touch");
}

#[test]
fn filters_keep_players_out_of_npc_triggers() {
    let trigger = |filter: &str| {
        entity(
            "trigger_multiple",
            &[
                ("model", "*1"),
                ("spawnflags", "1"),
                ("filtername", filter),
                ("OnTrigger", "pit,Toggle,,0,-1"),
            ],
        )
    };
    let world = world(&[("trigger_multiple", None)]);
    let inside = [(
        PlayerId::from_index(0),
        Vec3::splat(10.0),
        Vec3::splat(20.0),
    )];
    let fires = |filters: Vec<MapEntity>, name: &str| {
        let mut entities = with_hurt(vec![trigger(name)]);
        entities.extend(filters);
        let mut run = Run::with_hazards(&entities, &world, &[(0, 0)]);
        !hazard_switches(&run.step(&Senses {
            players: &inside,
            ..Senses::default()
        }))
        .is_empty()
    };
    let guard = entity(
        "filter_activator_name",
        &[("targetname", "guards"), ("filtername", "guard")],
    );
    assert!(!fires(vec![guard.clone()], "guards"));
    let not_guard = guard.clone().with("Negated", "1");
    assert!(fires(vec![not_guard], "guards"));
    let players = entity(
        "filter_activator_class",
        &[("targetname", "people"), ("filterclass", "player")],
    );
    assert!(fires(vec![players], "people"));
    assert!(
        fires(vec![], "nobody_by_that_name"),
        "a missing filter passes everyone"
    );
    let team = entity("filter_activator_team", &[("targetname", "team")]);
    assert!(!fires(vec![team], "team"));
}

#[test]
fn a_used_button_opens_a_door_through_a_relay() {
    let entities = vec![
        entity(
            "func_button",
            &[
                ("model", "*1"),
                ("spawnflags", "1025"),
                ("wait", "1"),
                ("OnPressed", "relay,Trigger,,0,-1"),
            ],
        ),
        entity(
            "logic_relay",
            &[("targetname", "relay"), ("OnTrigger", "door,Open,,0,-1")],
        ),
        entity(
            "func_door",
            &[
                ("model", "*2"),
                ("targetname", "door"),
                ("movedir", "-90 0 0"),
                ("speed", "640"),
                ("wait", "-1"),
            ],
        ),
    ];
    let world = world(&[("func_button", None), ("func_door", Some("door"))]);
    let mut run = Run::new(&entities, &world);
    let player = PlayerId::from_index(3);
    let uses = [(player, 1)];
    run.step(&Senses {
        uses: &uses,
        ..Senses::default()
    });
    run.idle(2);
    let door = &run.logic.movers()[1];
    // Pressed, relayed and opened on the first tick; 64 units at 640
    // units/s take that one tick.
    assert_eq!(door.phase, Phase::Open { since: 1 });
    assert_eq!(
        door.activator,
        Some(player),
        "the button's presser opened it"
    );
    let (origin, _) = run.logic.mover_pose(1).expect("pose");
    assert!(
        (origin - Vec3::new(0.0, 0.0, 64.0)).length() < 1e-3,
        "{origin}"
    );
}

#[test]
fn locked_things_refuse_use_until_unlocked() {
    let entities = with_hurt(vec![
        entity(
            "func_button",
            &[
                ("model", "*1"),
                ("targetname", "button"),
                ("spawnflags", "3073"),
                ("OnUseLocked", "pit,Enable,,0,-1"),
                ("OnPressed", "pit,Disable,,0,-1"),
            ],
        ),
        entity(
            "logic_relay",
            &[
                ("targetname", "unlock"),
                ("OnTrigger", "button,Unlock,,0,-1"),
            ],
        ),
    ]);
    let world = world(&[("func_button", Some("button"))]);
    let mut run = Run::with_hazards(&entities, &world, &[(0, 0)]);
    let uses = [(PlayerId::from_index(0), 1)];
    let using = Senses {
        uses: &uses,
        ..Senses::default()
    };
    assert_eq!(hazard_switches(&run.step(&using)), [true]);
    run.logic.parts.movers[0].locked = false;
    assert_eq!(hazard_switches(&run.step(&using)), [false]);
}

#[test]
fn doors_that_stay_open_close_again_with_use_and_others_ignore_it() {
    let door = |wait: &str| {
        entity(
            "func_door",
            &[
                ("model", "*1"),
                ("spawnflags", "256"),
                ("speed", "6400"),
                ("wait", wait),
            ],
        )
    };
    let world = world(&[("func_door", None)]);
    let uses = [(PlayerId::from_index(0), 1)];
    let using = Senses {
        uses: &uses,
        ..Senses::default()
    };
    let mut stays = Run::new(&[door("-1")], &world);
    stays.step(&using);
    assert!(matches!(stays.logic.movers()[0].phase, Phase::Open { .. }));
    stays.step(&using);
    assert_eq!(stays.logic.movers()[0].phase, Phase::Closed);

    let mut returns = Run::new(&[door("10")], &world);
    returns.step(&using);
    returns.step(&using);
    assert!(matches!(
        returns.logic.movers()[0].phase,
        Phase::Open { .. }
    ));
}

#[test]
fn damage_breaks_breakables_and_presses_damage_buttons() {
    let entities = with_hurt(vec![
        entity(
            "func_breakable_surf",
            &[
                ("model", "*1"),
                ("health", "5"),
                ("OnBreak", "pit,Enable,,0,-1"),
            ],
        ),
        entity(
            "func_button",
            &[
                ("model", "*2"),
                ("spawnflags", "513"),
                ("OnDamaged", "pit,Toggle,,0,-1"),
            ],
        ),
    ]);
    let world = world(&[("func_breakable_surf", None), ("func_button", None)]);
    let mut run = Run::with_hazards(&entities, &world, &[(0, 0)]);
    let shooter = Some(PlayerId::from_index(0));
    let damage = [(shooter, 1, 3.0), (shooter, 1, 3.0)];
    let effects = run.step(&Senses {
        damage: &damage,
        ..Senses::default()
    });
    assert!(effects.contains(&Effect::Broke { model: 1 }));
    assert!(!run.logic.movers()[0].solid());
    let damage = [(shooter, 2, 1.0)];
    let effects = run.step(&Senses {
        damage: &damage,
        ..Senses::default()
    });
    assert!(effects.contains(&Effect::Pressed { model: 2 }));
    assert_eq!(
        hazard_switches(&effects),
        [false],
        "toggled back off by OnDamaged"
    );
}

#[test]
fn explosions_go_off_once_unless_repeatable() {
    let entities = vec![
        entity(
            "logic_timer",
            &[("RefireTime", "0.5"), ("OnTimer", "boom*,Explode,,0,-1")],
        ),
        entity(
            "env_explosion",
            &[("targetname", "boom1"), ("iMagnitude", "100")],
        ),
        entity(
            "env_explosion",
            &[
                ("targetname", "boom2"),
                ("iMagnitude", "50"),
                ("spawnflags", "2"),
            ],
        ),
    ];
    let mut run = Run::new(&entities, &world(&[]));
    // The timer counts from its first tick: it fires on ticks 6, 11, 16
    // and 21.
    let blasts: Vec<_> = run
        .idle(21)
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::Explosion { damage, radius, .. } => Some((damage, radius)),
            _ => None,
        })
        .collect();
    assert_eq!(
        blasts,
        [
            (100.0, 250.0),
            (50.0, 125.0),
            (50.0, 125.0),
            (50.0, 125.0),
            (50.0, 125.0)
        ]
    );
}

#[test]
fn teleports_send_players_to_their_destination() {
    let entities = vec![
        entity(
            "trigger_teleport",
            &[("model", "*1"), ("spawnflags", "1"), ("target", "exit")],
        ),
        MapEntity::new("info_teleport_destination", [500.0, 0.0, 10.0])
            .with("targetname", "Exit")
            .with("angles", "0 90 0"),
    ];
    let mut entities = entities;
    entities[1].angles = [0.0, 90.0, 0.0];
    let world = world(&[("trigger_teleport", None)]);
    let mut run = Run::new(&entities, &world);
    let player = PlayerId::from_index(1);
    let inside = [(player, Vec3::splat(10.0), Vec3::splat(20.0))];
    let effects = run.step(&Senses {
        players: &inside,
        ..Senses::default()
    });
    assert_eq!(
        effects,
        [Effect::Teleport {
            player,
            origin: Vec3::new(500.0, 0.0, 10.0),
            yaw: 90.0
        }]
    );
}

#[test]
fn sounds_play_stop_and_change() {
    let entities = vec![
        entity(
            "ambient_generic",
            &[
                ("targetname", "alarm"),
                ("message", "Alarm.Loop"),
                ("spawnflags", "16"),
            ],
        ),
        entity(
            "ambient_generic",
            &[("message", "Hum.Loop"), ("health", "5")],
        ),
        entity(
            "logic_auto",
            &[
                ("OnMapSpawn", "alarm,PlaySound,,0.5,-1"),
                ("OnMapSpawn", "alarm,Pitch,150,0.5,-1"),
                ("OnMapSpawn", "alarm,StopSound,,1,-1"),
            ],
        ),
    ];
    let mut run = Run::new(&entities, &world(&[]));
    let sounds = run.logic.sounds();
    assert_eq!(sounds[0].state.started, None, "starts silent");
    assert_eq!(
        (sounds[1].state.started, sounds[1].state.volume),
        (Some(0), 0.5)
    );
    run.idle(6);
    let alarm = run.logic.sounds()[0].state;
    assert_eq!((alarm.started, alarm.pitch), (Some(6), 1.5));
    run.idle(5);
    assert_eq!(run.logic.sounds()[0].state.started, None);
}

#[test]
fn looping_relays_spend_the_tick_budget_instead_of_hanging() {
    let entities = vec![
        entity("logic_auto", &[("OnMapSpawn", "ping,Trigger,,0,-1")]),
        entity(
            "logic_relay",
            &[("targetname", "ping"), ("OnTrigger", "pong,Trigger,,0,-1")],
        ),
        entity(
            "logic_relay",
            &[("targetname", "pong"), ("OnTrigger", "ping,Trigger,,0,-1")],
        ),
    ];
    let mut run = Run::new(&entities, &world(&[]));
    run.idle(3);
    assert_eq!(run.logic.queue.len(), 1, "one event always carried over");
}

#[test]
fn a_new_match_puts_everything_back() {
    let entities = with_hurt(vec![
        entity(
            "logic_auto",
            &[
                ("OnMapSpawn", "pit,Enable,,0,-1"),
                ("OnMapSpawn", "door,Open,,0,-1"),
            ],
        ),
        entity(
            "func_door",
            &[
                ("model", "*1"),
                ("targetname", "door"),
                ("speed", "6400"),
                ("wait", "-1"),
            ],
        ),
    ]);
    let world = world(&[("func_door", Some("door"))]);
    let mut run = Run::with_hazards(&entities, &world, &[(0, 0)]);
    run.idle(2);
    assert!(matches!(run.logic.movers()[0].phase, Phase::Open { .. }));
    assert_eq!(
        run.logic.reset(),
        [Effect::Hazard {
            index: 0,
            enabled: false
        }]
    );
    assert_eq!(run.logic.movers()[0].phase, Phase::Closed);
    assert_eq!(
        hazard_switches(&run.idle(1)),
        [true],
        "the match starts again"
    );
}

#[test]
fn children_ride_their_parent_mover() {
    let entities = vec![
        entity(
            "func_door",
            &[
                ("model", "*1"),
                ("targetname", "lift"),
                ("movedir", "-90 0 0"),
                ("speed", "6400"),
                ("wait", "-1"),
            ],
        ),
        entity(
            "func_button",
            &[("model", "*2"), ("parentname", "lift"), ("spawnflags", "1")],
        ),
        entity("logic_auto", &[("OnMapSpawn", "lift,Open,,0,-1")]),
    ];
    let world = world(&[("func_door", Some("lift")), ("func_button", None)]);
    let mut run = Run::new(&entities, &world);
    run.idle(2);
    let (button, _) = run.logic.mover_pose(1).expect("pose");
    assert!(
        (button - Vec3::new(0.0, 0.0, 64.0)).length() < 1e-3,
        "{button}"
    );
}

#[test]
fn replicas_take_on_the_servers_state() {
    let entities = vec![
        entity(
            "func_door",
            &[
                ("model", "*1"),
                ("targetname", "door"),
                ("speed", "32"),
                ("wait", "-1"),
            ],
        ),
        entity("ambient_generic", &[("message", "Hum.Loop")]),
        entity("logic_auto", &[("OnMapSpawn", "door,Open,,0,-1")]),
    ];
    let world = world(&[("func_door", Some("door"))]);
    let mut server = Run::new(&entities, &world);
    server.idle(5);
    let mut client = Run::new(&entities, &world);
    client.logic.restore(&server.logic.state());
    assert_eq!(client.logic.state(), server.logic.state());
    assert_eq!(client.logic.mover_pose(0), server.logic.mover_pose(0));
}
