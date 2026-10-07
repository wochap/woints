//! Session state machine (design D9, D20). Pure: no I/O, effects are returned.

use std::time::{Duration, Instant};

use woints_core::model::ActionKind;
use woints_core::proto::State;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    Idle,
    Showing(Showing),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Showing {
    pub snapshot_id: u64,
    /// Label per element index; empty for elements without a badge.
    pub labels: Vec<String>,
    pub action: ActionKind,
    pub typed: String,
    pub since: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A new hint session was rendered for `snapshot_id`.
    Shown {
        snapshot_id: u64,
        labels: Vec<String>,
        action: ActionKind,
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
    /// A key that maps to a label glyph under the current layout.
    Glyph(char),
    Backspace,
}

/// What survives the typed buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterState {
    pub typed: String,
    /// Element indices whose label starts with `typed`.
    pub matching: Vec<usize>,
    /// Element indices eliminated by the last key.
    pub eliminated: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Redraw the badges for the new buffer.
    Redraw(FilterState),
    /// Draw element `index`'s badge in the flash style for `flash_ms`.
    Flash(usize),
    /// Unmap the overlay (and wait for the commit when an `Act` follows).
    Hide,
    /// Run the actuator chain on element `index` of `snapshot_id`.
    Act {
        snapshot_id: u64,
        index: usize,
        action: ActionKind,
    },
}

fn matching(labels: &[String], typed: &str) -> Vec<usize> {
    labels
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.is_empty() && l.starts_with(typed))
        .map(|(i, _)| i)
        .collect()
}

impl Session {
    pub fn state(&self) -> State {
        match self {
            Session::Idle => State::Idle,
            Session::Showing(_) => State::Showing,
        }
    }

    /// Apply `input` at time `now`, returning the next state and the effects
    /// the caller must perform in order. `flash_ms = 0` skips the flash.
    pub fn transition(&self, input: Input, now: Instant, flash_ms: u32) -> (Session, Vec<Effect>) {
        use Session::*;
        match (self, input) {
            (
                _,
                Input::Shown {
                    snapshot_id,
                    labels,
                    action,
                },
            ) => (
                Showing(self::Showing {
                    snapshot_id,
                    labels,
                    action,
                    typed: String::new(),
                    since: now,
                }),
                vec![],
            ),
            (Showing(_), Input::HintsRequested)
            | (Showing(_), Input::Cancel)
            | (Showing(_), Input::Quit)
            | (Showing(_), Input::MonitorsChanged) => (Idle, vec![Effect::Hide]),
            // The overlay already unmapped itself on Escape.
            (Showing(_), Input::Escape) => (Idle, vec![]),
            (Showing(s), Input::Tick { max_showing })
                if now.duration_since(s.since) >= max_showing =>
            {
                (Idle, vec![Effect::Hide])
            }
            (Showing(s), Input::Glyph(g)) => {
                let typed = format!("{}{g}", s.typed);
                let now_matching = matching(&s.labels, &typed);
                // A glyph no label continues with is ignored.
                if now_matching.is_empty() {
                    return (self.clone(), vec![]);
                }
                if let [i] = now_matching[..] {
                    if s.labels[i] == typed {
                        let mut fx = Vec::new();
                        if flash_ms > 0 {
                            fx.push(Effect::Flash(i));
                        }
                        fx.push(Effect::Hide);
                        fx.push(Effect::Act {
                            snapshot_id: s.snapshot_id,
                            index: i,
                            action: s.action,
                        });
                        return (Idle, fx);
                    }
                }
                let before = matching(&s.labels, &s.typed);
                let eliminated = before
                    .into_iter()
                    .filter(|i| !now_matching.contains(i))
                    .collect();
                let next = self::Showing {
                    typed: typed.clone(),
                    ..s.clone()
                };
                (
                    Showing(next),
                    vec![Effect::Redraw(FilterState {
                        typed,
                        matching: now_matching,
                        eliminated,
                    })],
                )
            }
            (Showing(s), Input::Backspace) => {
                if s.typed.is_empty() {
                    return (self.clone(), vec![]);
                }
                let mut typed = s.typed.clone();
                typed.pop();
                let next = self::Showing {
                    typed: typed.clone(),
                    ..s.clone()
                };
                (
                    Showing(next),
                    vec![Effect::Redraw(FilterState {
                        matching: matching(&s.labels, &typed),
                        typed,
                        eliminated: vec![],
                    })],
                )
            }
            (s, _) => (s.clone(), vec![]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLASH: u32 = 80;

    fn labels() -> Vec<String> {
        ["a", "s", "d", "fa", "fs"].map(String::from).to_vec()
    }

    fn showing(t: Instant) -> Session {
        Session::Idle
            .transition(
                Input::Shown {
                    snapshot_id: 1,
                    labels: labels(),
                    action: ActionKind::Activate,
                },
                t,
                FLASH,
            )
            .0
    }

    fn tr(s: &Session, i: Input) -> (Session, Vec<Effect>) {
        s.transition(i, Instant::now(), FLASH)
    }

    #[test]
    fn hints_shows() {
        let t = Instant::now();
        assert_eq!(showing(t).state(), State::Showing);
    }

    #[test]
    fn hints_while_showing_restarts() {
        let t = Instant::now();
        let (s, e) = tr(&showing(t), Input::HintsRequested);
        assert_eq!((s.clone(), e), (Session::Idle, vec![Effect::Hide]));
        let (s, _) = tr(
            &s,
            Input::Shown {
                snapshot_id: 2,
                labels: vec![],
                action: ActionKind::Activate,
            },
        );
        assert!(matches!(
            s,
            Session::Showing(Showing { snapshot_id: 2, .. })
        ));
    }

    #[test]
    fn hints_while_idle_needs_no_hide() {
        assert_eq!(
            tr(&Session::Idle, Input::HintsRequested),
            (Session::Idle, vec![])
        );
    }

    #[test]
    fn cancel_and_quit_hide() {
        let t = Instant::now();
        for i in [Input::Cancel, Input::Quit, Input::MonitorsChanged] {
            assert_eq!(tr(&showing(t), i), (Session::Idle, vec![Effect::Hide]));
        }
    }

    #[test]
    fn escape_goes_idle() {
        assert_eq!(
            tr(&showing(Instant::now()), Input::Escape),
            (Session::Idle, vec![])
        );
    }

    #[test]
    fn watchdog() {
        let t = Instant::now();
        let max = Duration::from_millis(15_000);
        let s = showing(t);
        assert_eq!(
            s.transition(
                Input::Tick { max_showing: max },
                t + Duration::from_secs(1),
                FLASH
            )
            .0
            .state(),
            State::Showing
        );
        assert_eq!(
            s.transition(
                Input::Tick { max_showing: max },
                t + Duration::from_secs(15),
                FLASH
            ),
            (Session::Idle, vec![Effect::Hide])
        );
    }

    #[test]
    fn one_glyph_filters() {
        let (s, fx) = tr(&showing(Instant::now()), Input::Glyph('f'));
        assert_eq!(
            fx,
            [Effect::Redraw(FilterState {
                typed: "f".into(),
                matching: vec![3, 4],
                eliminated: vec![0, 1, 2],
            })]
        );
        assert!(matches!(s, Session::Showing(ref sh) if sh.typed == "f"));
    }

    #[test]
    fn non_label_glyph_ignored() {
        let s0 = showing(Instant::now());
        let (s, fx) = tr(&s0, Input::Glyph('q'));
        assert_eq!((s, fx), (s0, vec![]));
    }

    #[test]
    fn backspace_restores() {
        let (s, _) = tr(&showing(Instant::now()), Input::Glyph('f'));
        let (s, fx) = tr(&s, Input::Backspace);
        assert_eq!(
            fx,
            [Effect::Redraw(FilterState {
                typed: String::new(),
                matching: vec![0, 1, 2, 3, 4],
                eliminated: vec![],
            })]
        );
        // Backspace on an empty buffer does nothing.
        let (s2, fx) = tr(&s, Input::Backspace);
        assert_eq!((s2, fx), (s, vec![]));
    }

    #[test]
    fn exact_match_flashes_then_acts() {
        let (s, fx) = tr(&showing(Instant::now()), Input::Glyph('d'));
        assert_eq!(s, Session::Idle);
        assert_eq!(
            fx,
            [
                Effect::Flash(2),
                Effect::Hide,
                Effect::Act {
                    snapshot_id: 1,
                    index: 2,
                    action: ActionKind::Activate
                }
            ]
        );
    }

    #[test]
    fn two_glyph_match() {
        let (s, _) = tr(&showing(Instant::now()), Input::Glyph('f'));
        let (s, fx) = tr(&s, Input::Glyph('s'));
        assert_eq!(s, Session::Idle);
        assert!(matches!(fx.last(), Some(Effect::Act { index: 4, .. })));
    }

    #[test]
    fn flash_zero_skips_flash() {
        let (_, fx) = showing(Instant::now()).transition(Input::Glyph('a'), Instant::now(), 0);
        assert_eq!(fx.len(), 2);
        assert_eq!(fx[0], Effect::Hide);
        assert!(matches!(fx[1], Effect::Act { index: 0, .. }));
    }
}
