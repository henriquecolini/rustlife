//! Trigger volumes and the filters that decide who sets them off.
//!
//! A trigger is a brush entity players walk into: `trigger_multiple` and
//! `trigger_once` fire outputs, `trigger_teleport` sends players to a
//! destination and `trigger_push` throws them. Like hazards, a trigger's
//! volume is its brush model's bounding box.
//!
//! Spawnflags say which kinds of thing a trigger notices; rustlife's only
//! activators are players, so a trigger that does not notice clients (or
//! "everything") never fires. A filter entity (`filtername`) narrows it
//! further.

use glam::Vec3;
use source_formats::entities::MapEntity;

use crate::collision::rotation_from_angles;

/// Trigger spawnflags: clients set it off, and everything does.
const NOTICES_CLIENTS: u32 = 1;
const NOTICES_EVERYTHING: u32 = 64;
/// `trigger_push` spawnflag: push once, then stop.
const PUSH_ONCE: u32 = 128;
/// Shortest wait between a `trigger_multiple`'s triggers, seconds.
const MIN_WAIT: f32 = 0.2;

/// What a trigger does to the players inside.
#[derive(Debug, Clone, PartialEq)]
pub enum TriggerKind {
    /// Fires `OnTrigger` on touch, then again after `wait` seconds while
    /// touched (`None`: only once, as `trigger_once` does).
    Multiple {
        /// Seconds between triggers; `None` fires once ever.
        wait: Option<f32>,
        /// Tick before which it does not fire again.
        ready_at: u64,
        /// Fired its once.
        spent: bool,
    },
    /// Sends players to the named destination.
    Teleport {
        /// The destination entity's name, lower case.
        target: String,
    },
    /// Throws players along a direction.
    Push {
        /// Push velocity, units/s, Source space.
        velocity: Vec3,
        /// Pushes once, then stops.
        once: bool,
    },
}

/// A trigger volume.
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    /// Minimum corner, Source space.
    pub mins: Vec3,
    /// Maximum corner, Source space.
    pub maxs: Vec3,
    /// Whether it is on.
    pub enabled: bool,
    /// Whether players set it off at all.
    pub notices_players: bool,
    /// The filter entity's name, lower case, if any.
    pub filter: Option<String>,
    /// What it does.
    pub kind: TriggerKind,
}

impl Trigger {
    /// The trigger `entity` makes with bounds `mins..maxs`, or `None` if
    /// its class is no trigger rustlife runs.
    #[must_use]
    pub fn from_entity(entity: &MapEntity, mins: Vec3, maxs: Vec3) -> Option<Self> {
        let flags = entity.number("spawnflags").map_or(0, |flags| flags as u32);
        let kind = match entity.classname.as_str() {
            "trigger_multiple" => TriggerKind::Multiple {
                // As in Source, a wait of zero means a fifth of a second.
                wait: Some(match entity.number("wait").unwrap_or(1.0) {
                    wait if wait > 0.0 => wait,
                    _ => MIN_WAIT,
                }),
                ready_at: 0,
                spent: false,
            },
            "trigger_once" => TriggerKind::Multiple {
                wait: None,
                ready_at: 0,
                spent: false,
            },
            "trigger_teleport" => TriggerKind::Teleport {
                target: entity.property("target")?.to_ascii_lowercase(),
            },
            "trigger_push" => {
                // The push direction is in the entity's own frame.
                let direction =
                    entity
                        .property("pushdir")
                        .and_then(parse_vector)
                        .map_or(Vec3::X, |angles| {
                            rotation_from_angles(entity.angles)
                                * (rotation_from_angles(angles) * Vec3::X)
                        });
                TriggerKind::Push {
                    velocity: direction * entity.number("speed").unwrap_or(40.0),
                    once: flags & PUSH_ONCE != 0,
                }
            }
            _ => return None,
        };
        Some(Self {
            mins,
            maxs,
            enabled: entity.property("StartDisabled") != Some("1"),
            notices_players: flags & (NOTICES_CLIENTS | NOTICES_EVERYTHING) != 0,
            filter: entity
                .property("filtername")
                .filter(|name| !name.is_empty())
                .map(str::to_ascii_lowercase),
            kind,
        })
    }

    /// Whether a box `mins..maxs` is (partly) inside.
    #[must_use]
    pub fn touches(&self, mins: Vec3, maxs: Vec3) -> bool {
        mins.cmplt(self.maxs).all() && self.mins.cmplt(maxs).all()
    }
}

/// A filter entity: who passes.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// `filter_activator_name`: activators of this name (lower case).
    Name {
        /// The name.
        name: String,
        /// Passes everyone else instead.
        negated: bool,
    },
    /// `filter_activator_class`: activators of this class (lower case).
    Class {
        /// The class.
        class: String,
        /// Passes everyone else instead.
        negated: bool,
    },
    /// `filter_multi`: a combination of other filters, by name.
    Multi {
        /// The filters' names, lower case.
        filters: Vec<String>,
        /// All must pass (`false`: any).
        all: bool,
        /// Passes whoever the combination does not.
        negated: bool,
    },
    /// Any other filter: rustlife cannot judge it (it filters NPCs,
    /// teams, damage...), so nobody passes.
    Unknown,
}

impl Filter {
    /// The filter `entity` is, or `None` if it is not a filter.
    #[must_use]
    pub fn from_entity(entity: &MapEntity) -> Option<Self> {
        if !entity.classname.starts_with("filter_") {
            return None;
        }
        // Source's `Negated` reads "Filter mode": 0 allows matches, 1 allows
        // everything else.
        let negated = entity.property("Negated") == Some("1");
        let lower = |key| {
            entity
                .property(key)
                .unwrap_or_default()
                .to_ascii_lowercase()
        };
        Some(match entity.classname.as_str() {
            "filter_activator_name" => Self::Name {
                name: lower("filtername"),
                negated,
            },
            "filter_activator_class" => Self::Class {
                class: lower("filterclass"),
                negated,
            },
            "filter_multi" => Self::Multi {
                filters: (1..=5)
                    .filter_map(|index| entity.property(&format!("Filter{index:02}")))
                    .filter(|name| !name.is_empty())
                    .map(str::to_ascii_lowercase)
                    .collect(),
                all: entity.property("FilterType") != Some("1"),
                negated,
            },
            _ => Self::Unknown,
        })
    }
}

/// `"x y z"` as three numbers.
fn parse_vector(text: &str) -> Option<[f32; 3]> {
    let mut parts = text.split_whitespace().map(str::parse::<f32>);
    let vector = [
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    ];
    vector.iter().all(|part| part.is_finite()).then_some(vector)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(classname: &str, keys: &[(&str, &str)]) -> Option<Trigger> {
        let mut entity = MapEntity::new(classname, [0.0; 3]);
        for (key, value) in keys {
            entity = entity.with(key, value);
        }
        Trigger::from_entity(&entity, Vec3::ZERO, Vec3::splat(64.0))
    }

    #[test]
    fn triggers_notice_players_only_when_flagged() {
        assert!(
            trigger("trigger_multiple", &[("spawnflags", "1")])
                .unwrap()
                .notices_players
        );
        assert!(
            trigger("trigger_multiple", &[("spawnflags", "64")])
                .unwrap()
                .notices_players
        );
        let npcs_only = trigger("trigger_multiple", &[("spawnflags", "2")]).unwrap();
        assert!(!npcs_only.notices_players);
        assert!(
            trigger("trigger_hurt", &[]).is_none(),
            "hazards are not triggers"
        );
        assert!(
            trigger("trigger_teleport", &[]).is_none(),
            "a teleport needs a target"
        );
    }

    #[test]
    fn pushes_point_along_their_direction() {
        let push = trigger("trigger_push", &[("pushdir", "-90 0 0"), ("speed", "300")]).unwrap();
        let TriggerKind::Push { velocity, once } = push.kind else {
            panic!("{push:?}");
        };
        assert!(
            (velocity - Vec3::new(0.0, 0.0, 300.0)).length() < 1e-3,
            "{velocity}"
        );
        assert!(!once);
    }

    #[test]
    fn boxes_touch_triggers_they_overlap() {
        let volume = trigger("trigger_once", &[]).unwrap();
        assert!(volume.touches(Vec3::splat(60.0), Vec3::splat(80.0)));
        assert!(
            !volume.touches(Vec3::splat(64.0), Vec3::splat(80.0)),
            "edge to edge"
        );
        assert!(volume.enabled);
    }

    #[test]
    fn filters_read_their_mode() {
        let name = MapEntity::new("filter_activator_name", [0.0; 3])
            .with("filtername", "Guard")
            .with("Negated", "1");
        assert_eq!(
            Filter::from_entity(&name),
            Some(Filter::Name {
                name: "guard".to_owned(),
                negated: true
            })
        );
        let multi = MapEntity::new("filter_multi", [0.0; 3])
            .with("Filter01", "a")
            .with("Filter02", "B")
            .with("FilterType", "1");
        assert_eq!(
            Filter::from_entity(&multi),
            Some(Filter::Multi {
                filters: vec!["a".to_owned(), "b".to_owned()],
                all: false,
                negated: false
            })
        );
        let team = MapEntity::new("filter_activator_team", [0.0; 3]);
        assert_eq!(Filter::from_entity(&team), Some(Filter::Unknown));
        assert_eq!(
            Filter::from_entity(&MapEntity::new("info_target", [0.0; 3])),
            None
        );
    }
}
