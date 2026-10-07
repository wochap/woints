//! Session state machine (design D9, D17, D18, D20). Pure: no I/O, effects
//! are returned.

use std::time::{Duration, Instant};

use woints_core::act::Axis;
use woints_core::model::Action;
use woints_core::proto::State;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    Idle,
    Showing(Showing),
    /// Drag phase 2: picking the destination.
    ShowingSecond(Second),
    Scrolling(Scrolling),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Showing {
    pub snapshot_id: u64,
    /// Label per element index; empty for elements without a badge.
    pub labels: Vec<String>,
    pub action: Action,
    /// The action was requested explicitly, so modifiers do not override it.
    pub explicit: bool,
    pub typed: String,
    pub since: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Second {
    pub snapshot_id: u64,
    /// Element index of the drag source.
    pub source: usize,
    /// Phase-2 labels per element index; the source has none.
    pub labels: Vec<String>,
    pub typed: String,
    pub since: Instant,
    /// Phase 1, restored by Escape.
    pub first: Box<Showing>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scrolling {
    pub snapshot_id: u64,
    /// Element index of the scroll container.
    pub target: usize,
    /// Last key, for the watchdog.
    pub since: Instant,
    /// Container picking, restored by Tab.
    pub first: Box<Showing>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A new hint session was rendered for `snapshot_id`.
    Shown {
        snapshot_id: u64,
        labels: Vec<String>,
        action: Action,
        explicit: bool,
    },
    /// Drag phase 2 was rendered with these labels.
    SecondShown {
        labels: Vec<String>,
    },
    /// `scroll` found exactly one container: scroll it without picking.
    ScrollDirect {
        snapshot_id: u64,
        labels: Vec<String>,
        index: usize,
    },
    /// `hints` arrived; any current session must end first.
    HintsRequested,
    Cancel,
    Escape,
    Tab,
    Quit,
    /// Watchdog tick.
    Tick {
        max_showing: Duration,
    },
    MonitorsChanged,
    /// A key that maps to a label glyph under the current layout, with the
    /// action its held modifiers map to.
    Glyph(char, Option<Action>),
    Backspace,
    /// A scroll key in scroll mode: axis and direction (-1 or 1).
    ScrollKey(Axis, i32),
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
    /// Unmap the overlay (and wait for the commit when an action follows).
    Hide,
    /// Run the actuator chain on element `index` of `snapshot_id`.
    Act {
        snapshot_id: u64,
        index: usize,
        action: Action,
    },
    /// Relabel for drag phase 2 and feed back [`Input::SecondShown`].
    BeginSecond { snapshot_id: u64, source: usize },
    Drag {
        snapshot_id: u64,
        from: usize,
        to: usize,
    },
    /// Draw the scroll frame and move the cursor to the container.
    BeginScroll { snapshot_id: u64, index: usize },
    Scroll {
        snapshot_id: u64,
        index: usize,
        axis: Axis,
        sign: i32,
    },
    /// Show phase 1 again (after Escape in drag phase 2, Tab in scroll mode).
    ReShow,
}

fn matching(labels: &[String], typed: &str) -> Vec<usize> {
    labels
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.is_empty() && l.starts_with(typed))
        .map(|(i, _)| i)
        .collect()
}

enum Typed {
    Ignored,
    Filtered(FilterState),
    Complete(usize),
}

fn type_glyph(labels: &[String], typed: &str, g: char) -> Typed {
    let next = format!("{typed}{g}");
    let now_matching = matching(labels, &next);
    // A glyph no label continues with is ignored.
    if now_matching.is_empty() {
        return Typed::Ignored;
    }
    if let [i] = now_matching[..] {
        if labels[i] == next {
            return Typed::Complete(i);
        }
    }
    let eliminated = matching(labels, typed)
        .into_iter()
        .filter(|i| !now_matching.contains(i))
        .collect();
    Typed::Filtered(FilterState {
        typed: next,
        matching: now_matching,
        eliminated,
    })
}

fn backspace(labels: &[String], typed: &str) -> Option<FilterState> {
    if typed.is_empty() {
        return None;
    }
    let mut typed = typed.to_owned();
    typed.pop();
    Some(FilterState {
        matching: matching(labels, &typed),
        typed,
        eliminated: vec![],
    })
}

fn flash_then(flash_ms: u32, i: usize, then: Effect) -> Vec<Effect> {
    let mut fx = Vec::new();
    if flash_ms > 0 {
        fx.push(Effect::Flash(i));
    }
    fx.push(Effect::Hide);
    fx.push(then);
    fx
}

impl Showing {
    fn reset(&self, now: Instant) -> Showing {
        Showing {
            typed: String::new(),
            since: now,
            ..self.clone()
        }
    }
}

impl Session {
    pub fn state(&self) -> State {
        match self {
            Session::Idle => State::Idle,
            Session::Showing(_) => State::Showing,
            Session::ShowingSecond(_) => State::ShowingSecond,
            Session::Scrolling(_) => State::Scrolling,
        }
    }

    fn since(&self) -> Option<Instant> {
        match self {
            Session::Idle => None,
            Session::Showing(s) => Some(s.since),
            Session::ShowingSecond(s) => Some(s.since),
            Session::Scrolling(s) => Some(s.since),
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
                    explicit,
                },
            ) => (
                Showing(self::Showing {
                    snapshot_id,
                    labels,
                    action,
                    explicit,
                    typed: String::new(),
                    since: now,
                }),
                vec![],
            ),
            (
                _,
                Input::ScrollDirect {
                    snapshot_id,
                    labels,
                    index,
                },
            ) => (
                Scrolling(self::Scrolling {
                    snapshot_id,
                    target: index,
                    since: now,
                    first: Box::new(self::Showing {
                        snapshot_id,
                        labels,
                        action: Action::Scroll,
                        explicit: true,
                        typed: String::new(),
                        since: now,
                    }),
                }),
                vec![Effect::BeginScroll { snapshot_id, index }],
            ),
            (ShowingSecond(s), Input::SecondShown { labels }) => (
                ShowingSecond(Second {
                    labels,
                    since: now,
                    ..s.clone()
                }),
                vec![],
            ),
            (Idle, _) => (Idle, vec![]),
            (_, Input::HintsRequested | Input::Cancel | Input::Quit | Input::MonitorsChanged) => {
                (Idle, vec![Effect::Hide])
            }
            (s, Input::Tick { max_showing })
                if s.since()
                    .is_some_and(|t| now.duration_since(t) >= max_showing) =>
            {
                (Idle, vec![Effect::Hide])
            }
            // The overlay already unmapped itself on Escape.
            (Showing(_) | Scrolling(_), Input::Escape) => (Idle, vec![]),
            (ShowingSecond(s), Input::Escape) => {
                (Showing(s.first.reset(now)), vec![Effect::ReShow])
            }
            (Scrolling(s), Input::Tab) => (Showing(s.first.reset(now)), vec![Effect::ReShow]),
            (Scrolling(s), Input::ScrollKey(axis, sign)) => (
                Scrolling(self::Scrolling {
                    since: now,
                    ..s.clone()
                }),
                vec![Effect::Scroll {
                    snapshot_id: s.snapshot_id,
                    index: s.target,
                    axis,
                    sign,
                }],
            ),
            (Showing(s), Input::Glyph(g, mapped)) => match type_glyph(&s.labels, &s.typed, g) {
                Typed::Ignored => (self.clone(), vec![]),
                Typed::Filtered(f) => (
                    Showing(self::Showing {
                        typed: f.typed.clone(),
                        ..s.clone()
                    }),
                    vec![Effect::Redraw(f)],
                ),
                Typed::Complete(i) => {
                    let action = match mapped {
                        Some(a) if !s.explicit => a,
                        Some(a) => {
                            tracing::debug!(session = %s.action, ignored = %a, "explicit action wins over modifier");
                            s.action
                        }
                        None => s.action,
                    };
                    let snapshot_id = s.snapshot_id;
                    match action {
                        Action::Drag => (
                            ShowingSecond(Second {
                                snapshot_id,
                                source: i,
                                labels: vec![],
                                typed: String::new(),
                                since: now,
                                first: Box::new(s.reset(now)),
                            }),
                            vec![Effect::BeginSecond {
                                snapshot_id,
                                source: i,
                            }],
                        ),
                        Action::Scroll => (
                            Scrolling(self::Scrolling {
                                snapshot_id,
                                target: i,
                                since: now,
                                first: Box::new(s.reset(now)),
                            }),
                            vec![Effect::BeginScroll {
                                snapshot_id,
                                index: i,
                            }],
                        ),
                        action => (
                            Idle,
                            flash_then(
                                flash_ms,
                                i,
                                Effect::Act {
                                    snapshot_id,
                                    index: i,
                                    action,
                                },
                            ),
                        ),
                    }
                }
            },
            (ShowingSecond(s), Input::Glyph(g, _)) => match type_glyph(&s.labels, &s.typed, g) {
                Typed::Ignored => (self.clone(), vec![]),
                Typed::Filtered(f) => (
                    ShowingSecond(Second {
                        typed: f.typed.clone(),
                        ..s.clone()
                    }),
                    vec![Effect::Redraw(f)],
                ),
                Typed::Complete(to) => (
                    Idle,
                    flash_then(
                        flash_ms,
                        to,
                        Effect::Drag {
                            snapshot_id: s.snapshot_id,
                            from: s.source,
                            to,
                        },
                    ),
                ),
            },
            (Showing(s), Input::Backspace) => match backspace(&s.labels, &s.typed) {
                None => (self.clone(), vec![]),
                Some(f) => (
                    Showing(self::Showing {
                        typed: f.typed.clone(),
                        ..s.clone()
                    }),
                    vec![Effect::Redraw(f)],
                ),
            },
            (ShowingSecond(s), Input::Backspace) => match backspace(&s.labels, &s.typed) {
                None => (self.clone(), vec![]),
                Some(f) => (
                    ShowingSecond(Second {
                        typed: f.typed.clone(),
                        ..s.clone()
                    }),
                    vec![Effect::Redraw(f)],
                ),
            },
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

    fn shown(t: Instant, action: Action, explicit: bool) -> Session {
        Session::Idle
            .transition(
                Input::Shown {
                    snapshot_id: 1,
                    labels: labels(),
                    action,
                    explicit,
                },
                t,
                FLASH,
            )
            .0
    }

    fn showing(t: Instant) -> Session {
        shown(t, Action::Click, false)
    }

    fn tr(s: &Session, i: Input) -> (Session, Vec<Effect>) {
        s.transition(i, Instant::now(), FLASH)
    }

    fn g(c: char) -> Input {
        Input::Glyph(c, None)
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
                action: Action::Click,
                explicit: false,
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
        let (s, fx) = tr(&showing(Instant::now()), g('f'));
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
        let (s, fx) = tr(&s0, g('q'));
        assert_eq!((s, fx), (s0, vec![]));
    }

    #[test]
    fn backspace_restores() {
        let (s, _) = tr(&showing(Instant::now()), g('f'));
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
        let (s, fx) = tr(&showing(Instant::now()), g('d'));
        assert_eq!(s, Session::Idle);
        assert_eq!(
            fx,
            [
                Effect::Flash(2),
                Effect::Hide,
                Effect::Act {
                    snapshot_id: 1,
                    index: 2,
                    action: Action::Click
                }
            ]
        );
    }

    #[test]
    fn two_glyph_match() {
        let (s, _) = tr(&showing(Instant::now()), g('f'));
        let (s, fx) = tr(&s, g('s'));
        assert_eq!(s, Session::Idle);
        assert!(matches!(fx.last(), Some(Effect::Act { index: 4, .. })));
    }

    #[test]
    fn flash_zero_skips_flash() {
        let (_, fx) = showing(Instant::now()).transition(g('a'), Instant::now(), 0);
        assert_eq!(fx.len(), 2);
        assert_eq!(fx[0], Effect::Hide);
        assert!(matches!(fx[1], Effect::Act { index: 0, .. }));
    }

    fn acted(fx: &[Effect]) -> Option<Action> {
        fx.iter().find_map(|e| match e {
            Effect::Act { action, .. } => Some(*action),
            _ => None,
        })
    }

    #[test]
    fn shift_on_last_glyph() {
        let (s, _) = tr(&showing(Instant::now()), Input::Glyph('f', None));
        let (_, fx) = tr(&s, Input::Glyph('s', Some(Action::Right)));
        assert_eq!(acted(&fx), Some(Action::Right));
    }

    #[test]
    fn shift_on_first_glyph_only() {
        let (s, _) = tr(
            &showing(Instant::now()),
            Input::Glyph('f', Some(Action::Right)),
        );
        let (_, fx) = tr(&s, Input::Glyph('s', None));
        assert_eq!(acted(&fx), Some(Action::Click));
    }

    #[test]
    fn explicit_action_wins() {
        let s = shown(Instant::now(), Action::Drag, true);
        let (s, fx) = tr(&s, Input::Glyph('d', Some(Action::Right)));
        assert_eq!(s.state(), State::ShowingSecond);
        assert_eq!(
            fx,
            [Effect::BeginSecond {
                snapshot_id: 1,
                source: 2
            }]
        );
    }

    fn second() -> Session {
        let s = shown(Instant::now(), Action::Drag, true);
        let (s, _) = tr(&s, g('d'));
        let (s, fx) = tr(
            &s,
            Input::SecondShown {
                labels: ["a", "s", "", "f", "g"].map(String::from).to_vec(),
            },
        );
        assert!(fx.is_empty());
        s
    }

    #[test]
    fn drag_transitions() {
        let s = second();
        // The source has no label in phase 2.
        let (s2, fx) = tr(&s, g('d'));
        assert_eq!((s2.state(), fx), (State::ShowingSecond, vec![]));
        let (s, fx) = tr(&s, g('g'));
        assert_eq!(s, Session::Idle);
        assert_eq!(
            fx,
            [
                Effect::Flash(4),
                Effect::Hide,
                Effect::Drag {
                    snapshot_id: 1,
                    from: 2,
                    to: 4
                }
            ]
        );
    }

    #[test]
    fn escape_in_phase_two_restores_phase_one() {
        let (s, fx) = tr(&second(), Input::Escape);
        assert_eq!(fx, [Effect::ReShow]);
        let Session::Showing(sh) = &s else {
            panic!("{s:?}")
        };
        assert_eq!(sh.labels, labels());
        assert_eq!(sh.typed, "");
        assert_eq!(sh.action, Action::Drag);
        // Escape again cancels.
        assert_eq!(tr(&s, Input::Escape), (Session::Idle, vec![]));
    }

    #[test]
    fn watchdog_in_phase_two() {
        let s = second();
        let max = Duration::from_millis(100);
        let (s, fx) = s.transition(
            Input::Tick { max_showing: max },
            Instant::now() + Duration::from_secs(1),
            FLASH,
        );
        assert_eq!((s, fx), (Session::Idle, vec![Effect::Hide]));
    }

    fn scrolling() -> Session {
        let s = shown(Instant::now(), Action::Scroll, true);
        let (s, fx) = tr(&s, g('s'));
        assert_eq!(
            fx,
            [Effect::BeginScroll {
                snapshot_id: 1,
                index: 1
            }]
        );
        s
    }

    #[test]
    fn scroll_keys_tab_escape_cancel() {
        let s = scrolling();
        assert_eq!(s.state(), State::Scrolling);
        let (s2, fx) = tr(&s, Input::ScrollKey(Axis::Vertical, 1));
        assert_eq!(s2.state(), State::Scrolling);
        assert_eq!(
            fx,
            [Effect::Scroll {
                snapshot_id: 1,
                index: 1,
                axis: Axis::Vertical,
                sign: 1
            }]
        );
        // Glyphs do nothing in scroll mode.
        assert_eq!(tr(&s, g('a')).1, vec![]);
        let (s3, fx) = tr(&s, Input::Tab);
        assert_eq!((s3.state(), fx), (State::Showing, vec![Effect::ReShow]));
        assert_eq!(tr(&s, Input::Escape), (Session::Idle, vec![]));
        assert_eq!(tr(&s, Input::Cancel), (Session::Idle, vec![Effect::Hide]));
    }

    #[test]
    fn single_container_skips_picking() {
        let (s, fx) = tr(
            &Session::Idle,
            Input::ScrollDirect {
                snapshot_id: 3,
                labels: vec!["a".into()],
                index: 0,
            },
        );
        assert_eq!(s.state(), State::Scrolling);
        assert_eq!(
            fx,
            [Effect::BeginScroll {
                snapshot_id: 3,
                index: 0
            }]
        );
    }
}
