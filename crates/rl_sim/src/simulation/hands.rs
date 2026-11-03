//! Carrying props in the hands in the tick: use picks one up (putting the
//! weapon away) or sets it down; carried, it is tossed, set down or steered
//! on; once the hands are empty the weapon comes back out. See
//! `tools::hands`.

use std::sync::Arc;

use glam::Vec3;

use super::{PlayerId, Simulation, TickEvents};
use crate::collision::CollisionQuery;
use crate::collision::contents::MASK_PLAYER_SOLID;
use crate::movement::{Buttons, UserCmd};
use crate::rng::Rng;
use crate::scene::Scene;
use crate::tools::hands::{HandsEvent, UseTarget, find_prop};
use crate::tools::hold::Carrier;
use crate::tools::manipulator::{Aim, ToolEvent};

/// Seeds a toss's spin, apart from the other random draws of a command.
const TOSS_SEED: u64 = 0x4841_4e44;

/// How far below their feet a player looks for what they stand on, units.
const GROUND_PROBE: f32 = 2.0;

impl Simulation {
    /// Runs the hands of a living player for the tick, after their move: a
    /// press of use (`use_pressed`) sets the carried prop down or picks up the
    /// prop use finds; a carried prop is otherwise tossed, set down or steered
    /// on. Returns whether the press was the hands', so that it opens no door.
    pub(super) fn run_hands(
        &mut self,
        id: PlayerId,
        cmd: &UserCmd,
        use_pressed: bool,
        events: &mut TickEvents,
    ) -> bool {
        let Some(carrier) = self.carrier(id, cmd) else {
            return false;
        };
        let taken = if use_pressed {
            self.use_hands(id, &carrier, events)
        } else {
            self.carry(id, &carrier, cmd, events);
            false
        };
        let arsenal = Arc::clone(&self.arsenal);
        if let Some(player) = self.players.get_mut(id.index()).and_then(Option::as_mut)
            && player.hands.held().is_none()
            && let Some(weapon) = player.inventory.unholster(&arsenal)
        {
            events.deploys.push((id, weapon));
        }
        taken
    }

    /// Where player `id` carries from, looking where `cmd` does.
    fn carrier(&self, id: PlayerId, cmd: &UserCmd) -> Option<Carrier> {
        let player = self.player(id)?;
        Some(Carrier {
            origin: player.state.origin,
            hull: player.state.hull(&self.data.movement),
            aim: Aim {
                eye: self.eye_position(id)?,
                pitch: cmd.pitch,
                yaw: cmd.yaw,
            },
        })
    }

    /// Use pressed: sets the carried prop down, or picks up the prop use finds
    /// if the weapon in hand can be put away. Returns whether the press was
    /// the hands'.
    fn use_hands(&mut self, id: PlayerId, carrier: &Carrier, events: &mut TickEvents) -> bool {
        let release = &self.data.hands.release;
        let Some(player) = self.players.get_mut(id.index()).and_then(Option::as_mut) else {
            return false;
        };
        if let Some(body) = player.hands.drop_held(&mut self.physics, release) {
            self.hands_acted(id, HandsEvent::Dropped(body), events);
            return true;
        }
        let reach = self.data.map_logic.use_range;
        let body = match find_prop(
            carrier,
            reach,
            &self.world,
            &self.physics,
            &self.data.hands.pickup,
        ) {
            UseTarget::Prop(body) => body,
            UseTarget::Mover | UseTarget::Nothing => return false,
        };
        // Found, the prop takes the press even if it is not picked up.
        if self.stands_on(carrier, body) {
            return true;
        }
        let arsenal = Arc::clone(&self.arsenal);
        let Some(player) = self.players.get_mut(id.index()).and_then(Option::as_mut) else {
            return true;
        };
        if !player.inventory.can_holster(&arsenal) {
            return true;
        }
        // The Gravity Gun lets go of what it holds as it is put away.
        if let Some(held) = player.manipulator.drop_held(&mut self.physics) {
            events.tools.push((id, ToolEvent::Dropped(held)));
            self.tool_acted(id, ToolEvent::Dropped(held));
        }
        let Some(player) = self.players.get_mut(id.index()).and_then(Option::as_mut) else {
            return true;
        };
        player.inventory.holster();
        let player_body = player.body;
        player.hands.pick_up(
            body,
            player_body,
            carrier,
            &mut self.physics,
            &self.data.hands,
        );
        self.hands_acted(id, HandsEvent::PickedUp(body), events);
        true
    }

    /// Carries the prop in the hands of player `id` on for the tick.
    fn carry(&mut self, id: PlayerId, carrier: &Carrier, cmd: &UserCmd, events: &mut TickEvents) {
        let Some(player) = self.players.get_mut(id.index()).and_then(Option::as_mut) else {
            return;
        };
        if player.hands.held().is_none() {
            return;
        }
        let mut rng = Rng::seeded(&[u64::from(cmd.number), id.index() as u64, TOSS_SEED]);
        let event = player.hands.tick(
            player.body,
            carrier,
            cmd.buttons,
            self.dt,
            &mut rng,
            &self.world,
            &mut self.physics,
            &self.data.hands,
        );
        if let Some(event) = event {
            self.hands_acted(id, event, events);
        }
    }

    /// Whether `carrier` stands on `body` (`CHL2_Player::PickupObject` will not
    /// pick up the ground entity).
    fn stands_on(&self, carrier: &Carrier, body: crate::physics::BodyId) -> bool {
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
            others: &[],
        };
        let below = carrier.origin - Vec3::Z * GROUND_PROBE;
        let trace = scene.trace_hull(carrier.origin, below, &carrier.hull, MASK_PLAYER_SOLID);
        trace.body == Some(body)
    }

    /// What the hands of `id` having done `event` means for the rest of the
    /// simulation: a prop let go of is theirs to be credited with for a while,
    /// as one the Manipulator let go of is.
    fn hands_acted(&mut self, id: PlayerId, event: HandsEvent, events: &mut TickEvents) {
        match event {
            HandsEvent::PickedUp(body) => self.breakables.arm(body, 0.0),
            HandsEvent::Dropped(body) | HandsEvent::Tossed(body) => {
                self.thrown.retain(|&(thrown, ..)| thrown != body);
                self.thrown.push((body, id, self.tick));
                self.breakables.arm(body, 0.0);
            }
        }
        events.hands.push((id, event));
    }
}

/// The buttons a player's move sees: no sprinting while the hands carry a
/// prop (`EnableSprint(false)`).
#[must_use]
pub(super) fn movement_buttons(buttons: Buttons, carrying: bool) -> Buttons {
    if carrying {
        buttons.with(Buttons::SPRINT, false)
    } else {
        buttons
    }
}
