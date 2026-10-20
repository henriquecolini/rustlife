//! Delivering inputs: what each kind of entity does when an event reaches
//! it.

use super::io::Event;
use super::mover::{MoverClass, Phase};
use super::{Effect, Kind, MapLogic};
use crate::simulation::PlayerId;

impl MapLogic {
    /// Delivers `event` to every entity it reaches.
    pub(super) fn deliver(&mut self, event: &Event, effects: &mut Vec<Effect>) {
        let targets: Vec<usize> = match event.target.as_str() {
            "!self" | "!caller" => vec![event.caller],
            target if target.starts_with('!') => {
                // `!activator`, `!player`: inputs to players (health,
                // speed...) are not part of rustlife's rules.
                self.report_once(format!("input {} to {target} ignored", event.input));
                return;
            }
            _ => self
                .parts
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| {
                    node.name
                        .as_deref()
                        .is_some_and(|name| match event.target.strip_suffix('*') {
                            Some(prefix) => name.starts_with(prefix),
                            None => name == event.target,
                        })
                })
                .map(|(index, _)| index)
                .collect(),
        };
        if targets.is_empty() {
            self.report_once(format!("no entity named {}", event.target));
        }
        for node in targets {
            self.input(node, event, effects);
        }
    }

    /// Delivers `event`'s input to node `node`.
    fn input(&mut self, node: usize, event: &Event, effects: &mut Vec<Effect>) {
        let input = event.input.as_str();
        let activator = event.activator;
        if let Some(user) = input.strip_prefix("fireuser")
            && matches!(user, "1" | "2" | "3" | "4")
        {
            let output = format!("onuser{user}");
            self.fire(node, &output, activator, Some(&event.parameter));
            return;
        }
        if input == "cancelpending" {
            self.queue.cancel_from(node);
            return;
        }
        let handled = match self.parts.nodes[node].kind.clone() {
            Kind::Relay { .. } => self.relay_input(node, input, activator),
            Kind::Timer(_) => self.timer_input(node, input, &event.parameter),
            Kind::Counter(_) => self.counter_input(node, input, &event.parameter, activator),
            Kind::Mover(index) => self.mover_input(index, input, event, effects),
            Kind::Trigger(index) => self.trigger_input(index, input),
            Kind::Hurt { index, enabled } => {
                let now = match input {
                    "enable" => Some(true),
                    "disable" | "kill" => Some(false),
                    "toggle" => Some(!enabled),
                    _ => None,
                };
                if let Some(now) = now {
                    self.parts.nodes[node].kind = Kind::Hurt {
                        index,
                        enabled: now,
                    };
                    effects.push(Effect::Hazard {
                        index,
                        enabled: now,
                    });
                }
                now.is_some()
            }
            Kind::Explosion {
                position,
                damage,
                radius,
                repeatable,
                spent,
            } => match input {
                "explode" => {
                    if !spent {
                        effects.push(Effect::Explosion {
                            position,
                            damage,
                            radius,
                        });
                        if !repeatable {
                            self.parts.nodes[node].kind = Kind::Explosion {
                                position,
                                damage,
                                radius,
                                repeatable,
                                spent: true,
                            };
                        }
                    }
                    true
                }
                "kill" => true,
                _ => false,
            },
            Kind::Sound(index) => self.sound_input(index, input, &event.parameter),
            Kind::Auto | Kind::Filter(_) => input == "kill",
            Kind::Other => false,
        };
        if !handled {
            let classname = self.parts.nodes[node].classname.clone();
            self.report_once(format!("input {input} to {classname} not supported"));
        }
    }

    fn relay_input(&mut self, node: usize, input: &str, activator: Option<PlayerId>) -> bool {
        let Kind::Relay { enabled, once } = &mut self.parts.nodes[node].kind else {
            return false;
        };
        match input {
            "trigger" => {
                if *enabled {
                    if *once {
                        *enabled = false;
                    }
                    self.fire(node, "ontrigger", activator, None);
                }
            }
            "enable" => *enabled = true,
            "disable" | "kill" => *enabled = false,
            "toggle" => *enabled = !*enabled,
            "enablerefire" => {}
            _ => return false,
        }
        true
    }

    /// A rescheduled timer (`next` cleared) counts its interval from the
    /// tick it is next looked at.
    fn timer_input(&mut self, node: usize, input: &str, parameter: &str) -> bool {
        let Kind::Timer(timer) = &mut self.parts.nodes[node].kind else {
            return false;
        };
        let number = parameter
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|n| n.is_finite());
        match input {
            "enable" => {
                if !timer.enabled {
                    timer.enabled = true;
                    timer.next = None;
                }
            }
            "disable" | "kill" => timer.enabled = false,
            "toggle" => {
                timer.enabled = !timer.enabled;
                timer.next = None;
            }
            "resettimer" => timer.next = None,
            "refiretime" => {
                if let Some(seconds) = number {
                    timer.refire = seconds.max(0.0);
                    timer.next = None;
                }
            }
            "lowerrandombound" | "upperrandombound" => {
                if let (Some(seconds), Some((lower, upper))) = (number, timer.random.as_mut()) {
                    if input == "lowerrandombound" {
                        *lower = seconds.max(0.0);
                    } else {
                        *upper = seconds.max(0.0);
                    }
                    *upper = upper.max(*lower);
                }
            }
            "firetimer" => {
                self.fire(node, "ontimer", None, None);
                return true;
            }
            _ => return false,
        }
        true
    }

    fn counter_input(
        &mut self,
        node: usize,
        input: &str,
        parameter: &str,
        activator: Option<PlayerId>,
    ) -> bool {
        let Kind::Counter(counter) = &mut self.parts.nodes[node].kind else {
            return false;
        };
        let number = parameter
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|n| n.is_finite());
        let value = counter.value;
        let new = match (input, number) {
            ("enable", _) => {
                counter.enabled = true;
                return true;
            }
            ("disable" | "kill", _) => {
                counter.enabled = false;
                return true;
            }
            ("getvalue", _) => {
                let text = format_value(value);
                self.fire(node, "ongetvalue", activator, Some(&text));
                return true;
            }
            ("setvaluenofire", Some(number)) => {
                counter.value = number;
                return true;
            }
            ("add", Some(number)) => value + number,
            ("subtract", Some(number)) => value - number,
            ("multiply", Some(number)) => value * number,
            ("divide", Some(number)) if number != 0.0 => value / number,
            ("setvalue", Some(number)) => number,
            ("add" | "subtract" | "multiply" | "divide" | "setvalue" | "setvaluenofire", _) => {
                return true;
            }
            _ => return false,
        };
        if !counter.enabled {
            return true;
        }
        let mut outputs = Vec::new();
        let mut new = new;
        // Bounds apply only when they differ, as in Source.
        if counter.min < counter.max {
            if new >= counter.max {
                new = counter.max;
                if !counter.at_max {
                    outputs.push("onhitmax");
                }
                counter.at_max = true;
            } else {
                counter.at_max = false;
            }
            if new <= counter.min {
                new = counter.min;
                if !counter.at_min {
                    outputs.push("onhitmin");
                }
                counter.at_min = true;
            } else {
                counter.at_min = false;
            }
        }
        counter.value = new;
        let text = format_value(new);
        self.fire(node, "outvalue", activator, Some(&text));
        for output in outputs {
            self.fire(node, output, activator, None);
        }
        true
    }

    fn mover_input(
        &mut self,
        index: usize,
        input: &str,
        event: &Event,
        effects: &mut Vec<Effect>,
    ) -> bool {
        let tick = self.tick;
        let activator = event.activator;
        let Some(mover) = self.parts.movers.get_mut(index) else {
            return false;
        };
        let number = event
            .parameter
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|n| n.is_finite());
        let output = match (mover.class, input) {
            (_, "kill" | "killhierarchy") => {
                mover.kill();
                None
            }
            (_, "lock") => {
                mover.locked = true;
                None
            }
            (_, "unlock") => {
                mover.locked = false;
                None
            }
            (MoverClass::Door, "open") | (MoverClass::Button, "pressin") => {
                mover.open(activator, tick)
            }
            (MoverClass::Door, "close") | (MoverClass::Button, "pressout") => {
                mover.close(activator)
            }
            (MoverClass::Door, "toggle") => mover.toggle(activator, tick),
            (MoverClass::Door | MoverClass::Button, "setspeed") => {
                if let Some(speed) = number {
                    mover.set_speed(speed);
                }
                None
            }
            (MoverClass::Button, "press") => {
                if mover.phase != Phase::Closed {
                    return true;
                }
                let model = mover.model;
                let output = mover.open(activator, tick);
                if output.is_some() {
                    effects.push(Effect::Pressed { model });
                }
                output
            }
            (MoverClass::Brush, "enable" | "turnon") => {
                mover.enable();
                None
            }
            (MoverClass::Brush, "disable" | "turnoff") => {
                mover.disable();
                None
            }
            (MoverClass::Brush, "toggle") => {
                if mover.enabled() {
                    mover.disable();
                } else {
                    mover.enable();
                }
                None
            }
            (MoverClass::Breakable, "break") => {
                if mover.gone() {
                    None
                } else {
                    let model = mover.model;
                    mover.kill();
                    effects.push(Effect::Broke { model });
                    Some("onbreak")
                }
            }
            (MoverClass::Breakable, "sethealth" | "addhealth" | "removehealth") => {
                if let (Some(number), Some(health)) = (number, mover.health.as_mut()) {
                    match input {
                        "sethealth" => *health = number,
                        "addhealth" => *health += number,
                        _ => *health -= number,
                    }
                }
                None
            }
            _ => return false,
        };
        if let Some(output) = output {
            self.fire_mover_as(index, output, activator);
        }
        true
    }

    fn trigger_input(&mut self, index: usize, input: &str) -> bool {
        let Some(trigger) = self.parts.triggers.get_mut(index) else {
            return false;
        };
        match input {
            "enable" => trigger.enabled = true,
            "disable" | "kill" => trigger.enabled = false,
            "toggle" => trigger.enabled = !trigger.enabled,
            _ => return false,
        }
        true
    }

    fn sound_input(&mut self, index: usize, input: &str, parameter: &str) -> bool {
        let tick = self.tick;
        let Some(sound) = self.parts.sounds.get_mut(index) else {
            return false;
        };
        let number = parameter
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|n| n.is_finite());
        let state = &mut sound.state;
        match input {
            "playsound" | "fadein" => state.started = Some(tick),
            "stopsound" | "fadeout" | "kill" => state.started = None,
            "togglesound" => {
                state.started = if state.started.is_some() {
                    None
                } else {
                    Some(tick)
                };
            }
            "volume" => {
                if let Some(volume) = number {
                    state.volume = (volume / 10.0).clamp(0.0, 1.0);
                }
            }
            "pitch" => {
                if let Some(pitch) = number {
                    state.pitch = (pitch / 100.0).clamp(0.01, 2.55);
                }
            }
            _ => return false,
        }
        true
    }
}

/// A counter's value as an output parameter: whole numbers without a
/// fraction, as Source prints them.
fn format_value(value: f32) -> String {
    if value.fract() == 0.0 && value.abs() < 1e9 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}
