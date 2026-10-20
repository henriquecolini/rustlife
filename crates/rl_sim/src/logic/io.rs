//! Entity I/O: outputs wired to inputs, and the queue of events in flight.
//!
//! A map wires its entities together in their key-values. An *output* key
//! (`OnPressed`, `OnTrigger`, ...) holds a *connection*: the entity to
//! reach, the input to send it, a parameter, a delay and how many times the
//! connection may fire. Compilers separate the five fields with commas, or
//! with the escape character (0x1B) in newer maps:
//!
//! ```text
//! "OnPressed" "alarm_sequence,Trigger,,0,-1"
//! ```
//!
//! Firing an output queues an [`Event`] per connection, due after its
//! delay; the queue hands them out in due order, first queued first among
//! equals, which is the order Source's event queue keeps.

use crate::simulation::PlayerId;

/// One connection of an output: what it sends where.
#[derive(Debug, Clone, PartialEq)]
pub struct Connection {
    /// The output it belongs to, lower case (`onpressed`).
    pub output: String,
    /// The entities it reaches: a name, lower case, possibly ending in `*`
    /// to match every name with that prefix.
    pub target: String,
    /// The input sent, lower case (`trigger`).
    pub input: String,
    /// The input's parameter; empty passes on the output's own value.
    pub parameter: String,
    /// Seconds before the input arrives.
    pub delay: f32,
    /// How many more times it may fire; `None` for ever.
    pub times: Option<u32>,
}

impl Connection {
    /// Reads output `key` holding `value`, or `None` if the value is not a
    /// connection.
    #[must_use]
    pub fn parse(key: &str, value: &str) -> Option<Self> {
        let separator = if value.contains('\u{1b}') {
            '\u{1b}'
        } else {
            ','
        };
        let mut fields = value.split(separator).map(str::trim);
        let target = fields.next().filter(|target| !target.is_empty())?;
        let input = fields.next().filter(|input| !input.is_empty())?;
        let parameter = fields.next().unwrap_or_default();
        let delay = fields
            .next()
            .and_then(|delay| delay.parse::<f32>().ok())
            .filter(|delay| delay.is_finite())
            .unwrap_or(0.0)
            .max(0.0);
        let times = fields
            .next()
            .and_then(|times| times.parse::<i64>().ok())
            .and_then(|times| u32::try_from(times).ok());
        Some(Self {
            output: key.to_ascii_lowercase(),
            target: target.to_ascii_lowercase(),
            input: input.to_ascii_lowercase(),
            parameter: parameter.to_owned(),
            delay,
            times,
        })
    }

    /// Whether this connection reaches an entity named `name` (lower case).
    #[must_use]
    pub fn reaches(&self, name: &str) -> bool {
        match self.target.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => self.target == name,
        }
    }
}

/// Whether an entity key is an output: by convention outputs, and only
/// outputs, are named `On…` or `Out…`.
#[must_use]
pub fn is_output(key: &str) -> bool {
    let key = key.as_bytes();
    let starts = |prefix: &[u8]| {
        key.len() > prefix.len() && key[..prefix.len()].eq_ignore_ascii_case(prefix)
    };
    starts(b"on") || starts(b"out")
}

/// An input on its way to its targets.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// The tick it arrives on.
    pub due: u64,
    /// Who it reaches (as [`Connection::target`]).
    pub target: String,
    /// The input, lower case.
    pub input: String,
    /// Its parameter.
    pub parameter: String,
    /// The player who set the chain off, if one did.
    pub activator: Option<PlayerId>,
    /// The entity whose output sent it, by index.
    pub caller: usize,
}

/// Events in flight, handed out in due order.
#[derive(Debug, Clone, Default)]
pub struct Queue {
    /// Kept sorted by due tick; equal ticks in the order queued.
    events: Vec<Event>,
}

impl Queue {
    /// Queues `event` after every event due no later.
    pub fn push(&mut self, event: Event) {
        let at = self
            .events
            .partition_point(|queued| queued.due <= event.due);
        self.events.insert(at, event);
    }

    /// Takes the next event due by `tick`, if any.
    pub fn pop_due(&mut self, tick: u64) -> Option<Event> {
        (self.events.first()?.due <= tick).then(|| self.events.remove(0))
    }

    /// Drops every event sent by entity `caller` (`CancelPending`).
    pub fn cancel_from(&mut self, caller: usize) {
        self.events.retain(|event| event.caller != caller);
    }

    /// Drops everything.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// How many events are in flight.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether nothing is in flight.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_are_read_with_either_separator() {
        let comma = Connection::parse("OnPressed", "Alarm_Sequence,Trigger,,0.5,-1").unwrap();
        assert_eq!(comma.output, "onpressed");
        assert_eq!(comma.target, "alarm_sequence");
        assert_eq!(comma.input, "trigger");
        assert_eq!(comma.parameter, "");
        assert_eq!(comma.delay, 0.5);
        assert_eq!(comma.times, None);

        let escape =
            Connection::parse("OnTrigger", "door1\u{1b}Open\u{1b}fast\u{1b}2\u{1b}1").unwrap();
        assert_eq!(
            (escape.target.as_str(), escape.input.as_str()),
            ("door1", "open")
        );
        assert_eq!(escape.parameter, "fast");
        assert_eq!((escape.delay, escape.times), (2.0, Some(1)));
    }

    #[test]
    fn missing_fields_default_and_garbage_is_refused() {
        let short = Connection::parse("OnTrigger", "relay,Trigger").unwrap();
        assert_eq!((short.delay, short.times), (0.0, None));
        let negative = Connection::parse("OnTrigger", "relay,Trigger,,-3,0").unwrap();
        assert_eq!((negative.delay, negative.times), (0.0, Some(0)));
        assert!(Connection::parse("OnTrigger", "").is_none());
        assert!(Connection::parse("OnTrigger", "relay").is_none());
        assert!(Connection::parse("OnTrigger", ",Trigger").is_none());
    }

    #[test]
    fn targets_match_names_and_prefixes() {
        let exact = Connection::parse("OnTrigger", "door,Open").unwrap();
        assert!(exact.reaches("door"));
        assert!(!exact.reaches("door2"));
        let prefix = Connection::parse("OnTrigger", "door*,Open").unwrap();
        assert!(prefix.reaches("door2") && prefix.reaches("door"));
        assert!(!prefix.reaches("valve"));
    }

    #[test]
    fn outputs_are_the_on_and_out_keys() {
        assert!(is_output("OnPressed") && is_output("OutValue") && is_output("onhitmax"));
        assert!(!is_output("origin") && !is_output("On") && !is_output("Out"));
        assert!(!is_output("targetname"));
    }

    fn event(due: u64, input: &str, caller: usize) -> Event {
        Event {
            due,
            target: "x".to_owned(),
            input: input.to_owned(),
            parameter: String::new(),
            activator: None,
            caller,
        }
    }

    #[test]
    fn events_come_out_in_due_order_first_queued_first() {
        let mut queue = Queue::default();
        queue.push(event(5, "late", 0));
        queue.push(event(2, "first", 0));
        queue.push(event(2, "second", 1));
        assert!(queue.pop_due(1).is_none());
        assert_eq!(queue.pop_due(3).unwrap().input, "first");
        assert_eq!(queue.pop_due(3).unwrap().input, "second");
        assert!(queue.pop_due(3).is_none());
        queue.cancel_from(0);
        assert!(queue.is_empty(), "the late event was the canceled caller's");
    }
}
