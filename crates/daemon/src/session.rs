//! Session state machine (design D9). Pure: no I/O, effects are returned.

use std::time::{Duration, Instant};

use woints_core::proto::State;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    Idle,
    Showing {
        snapshot_id: u64,
        typed: String,
        since: Instant,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A new hint session was rendered for `snapshot_id`.
    Shown {
        snapshot_id: u64,
    },
    /// `hints` arrived; any current session must end first.
    HintsRequested,
    Cancel,
    Escape,
    Quit,
    /// Watchdog tick.
    Tick {
        max_showing: Duration,
    },
    MonitorsChanged,
    /// A printable key from the overlay (echoed; filtering arrives in M2).
    Typed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Unmap the overlay.
    Hide,
}

impl Session {
    pub fn state(&self) -> State {
        match self {
            Session::Idle => State::Idle,
            Session::Showing { .. } => State::Showing,
        }
    }

    /// Apply `input` at time `now`, returning the next state and the effect
    /// the caller must perform.
    pub fn transition(&self, input: Input, now: Instant) -> (Session, Effect) {
        use Session::*;
        match (self, input) {
            (_, Input::Shown { snapshot_id }) => (
                Showing {
                    snapshot_id,
                    typed: String::new(),
                    since: now,
                },
                Effect::None,
            ),
            (Showing { .. }, Input::HintsRequested)
            | (Showing { .. }, Input::Cancel)
            | (Showing { .. }, Input::Quit)
            | (Showing { .. }, Input::MonitorsChanged) => (Idle, Effect::Hide),
            // The overlay already unmapped itself on Escape.
            (Showing { .. }, Input::Escape) => (Idle, Effect::None),
            (Showing { since, .. }, Input::Tick { max_showing })
                if now.duration_since(*since) >= max_showing =>
            {
                (Idle, Effect::Hide)
            }
            (
                Showing {
                    snapshot_id,
                    typed,
                    since,
                },
                Input::Typed(s),
            ) => (
                Showing {
                    snapshot_id: *snapshot_id,
                    typed: format!("{typed}{s}"),
                    since: *since,
                },
                Effect::None,
            ),
            (s, _) => (s.clone(), Effect::None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn showing(t: Instant) -> Session {
        Session::Idle
            .transition(Input::Shown { snapshot_id: 1 }, t)
            .0
    }

    #[test]
    fn hints_shows() {
        let t = Instant::now();
        assert_eq!(showing(t).state(), State::Showing);
    }

    #[test]
    fn hints_while_showing_restarts() {
        let t = Instant::now();
        let (s, e) = showing(t).transition(Input::HintsRequested, t);
        assert_eq!((s.clone(), e), (Session::Idle, Effect::Hide));
        let (s, _) = s.transition(Input::Shown { snapshot_id: 2 }, t);
        assert!(matches!(s, Session::Showing { snapshot_id: 2, .. }));
    }

    #[test]
    fn hints_while_idle_needs_no_hide() {
        let t = Instant::now();
        assert_eq!(
            Session::Idle.transition(Input::HintsRequested, t),
            (Session::Idle, Effect::None)
        );
    }

    #[test]
    fn cancel_and_quit_hide() {
        let t = Instant::now();
        assert_eq!(
            showing(t).transition(Input::Cancel, t),
            (Session::Idle, Effect::Hide)
        );
        assert_eq!(
            showing(t).transition(Input::Quit, t),
            (Session::Idle, Effect::Hide)
        );
        assert_eq!(
            showing(t).transition(Input::MonitorsChanged, t),
            (Session::Idle, Effect::Hide)
        );
    }

    #[test]
    fn escape_goes_idle() {
        let t = Instant::now();
        assert_eq!(
            showing(t).transition(Input::Escape, t),
            (Session::Idle, Effect::None)
        );
    }

    #[test]
    fn watchdog() {
        let t = Instant::now();
        let max = Duration::from_millis(15_000);
        let s = showing(t);
        assert_eq!(
            s.transition(Input::Tick { max_showing: max }, t + Duration::from_secs(1))
                .0
                .state(),
            State::Showing
        );
        assert_eq!(
            s.transition(
                Input::Tick { max_showing: max },
                t + Duration::from_secs(15)
            ),
            (Session::Idle, Effect::Hide)
        );
    }

    #[test]
    fn typed_echo() {
        let t = Instant::now();
        let (s, _) = showing(t).transition(Input::Typed("a".into()), t);
        let (s, _) = s.transition(Input::Typed("s".into()), t);
        assert!(matches!(s, Session::Showing { ref typed, .. } if typed == "as"));
    }
}
