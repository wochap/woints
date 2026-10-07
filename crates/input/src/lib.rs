//! Pointer actuation (design D13, D17, D18): cursor placement through
//! Hyprland `movecursor` (or output-local `motion_absolute`) and button,
//! motion and axis events through `zwlr_virtual_pointer_v1`.

use std::future::Future;
use std::time::{Duration, Instant};

use woints_core::act::{
    Axis, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, ButtonState, InputConfig, LINE_PX, PointerBackend,
    VirtualPointer,
};
use woints_core::geom::{Global, OutputGeom, Point, Rect, pick_output};
use woints_core::model::{Action, Timings};
use woints_core::ports::ActError;
use woints_hypr::Hypr;

/// Hyprland IPC used to place and read the cursor.
pub trait CursorIpc {
    /// `dispatch <cmd>`; `Err` carries the non-`ok` reply.
    fn dispatch(&self, cmd: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Place the cursor at a global logical point using whichever dispatcher
    /// syntax the running Hyprland accepts (Lua `hl.dsp.cursor.move` or
    /// legacy `movecursor`).
    fn move_cursor(&self, p: Point<Global>) -> impl Future<Output = Result<(), String>> + Send;
    fn cursor_pos(&self) -> impl Future<Output = Result<Point<Global>, String>> + Send;
}

fn dispatch_reply(e: woints_hypr::Error) -> String {
    match e {
        woints_hypr::Error::Dispatch(reply) => reply,
        e => e.to_string(),
    }
}

impl CursorIpc for Hypr {
    async fn dispatch(&self, cmd: &str) -> Result<(), String> {
        Hypr::dispatch(self, cmd).await.map_err(dispatch_reply)
    }

    async fn move_cursor(&self, p: Point<Global>) -> Result<(), String> {
        Hypr::move_cursor(self, p).await.map_err(dispatch_reply)
    }

    async fn cursor_pos(&self) -> Result<Point<Global>, String> {
        Hypr::cursor_pos(self).await.map_err(|e| e.to_string())
    }
}

async fn cursor_pos<I: CursorIpc + Sync>(ipc: &I) -> Result<Point<Global>, ActError> {
    ipc.cursor_pos()
        .await
        .map_err(|e| ActError::Failed(format!("cursorpos failed: {e}")))
}

/// Default backend: `dispatch movecursor x y` in global logical coordinates;
/// no `motion_absolute` (hyprwm/Hyprland#6749).
pub struct MovecursorBackend<I, V> {
    pub ipc: I,
    pub pointer: V,
}

impl<I: CursorIpc + Sync, V: VirtualPointer + Sync> PointerBackend for MovecursorBackend<I, V> {
    async fn move_to(&self, p: Point<Global>) -> Result<(), ActError> {
        self.ipc
            .move_cursor(p)
            .await
            .map_err(|reply| ActError::Failed(format!("movecursor failed: {reply}")))
    }
    async fn cursor_pos(&self) -> Result<Point<Global>, ActError> {
        cursor_pos(&self.ipc).await
    }
    async fn button(&self, code: u32, state: ButtonState) -> Result<(), ActError> {
        self.pointer.button(code, state).await
    }
    async fn motion_rel(&self, dx: f64, dy: f64) -> Result<(), ActError> {
        self.pointer.motion(dx, dy).await
    }
    async fn axis(&self, axis: Axis, px: f64, lines: i32) -> Result<(), ActError> {
        self.pointer.axis(axis, px, lines).await
    }
    async fn roundtrip(&self) -> Result<(), ActError> {
        self.pointer.roundtrip().await
    }
}

/// Alternative backend: a pointer bound to the element's output and
/// output-local `motion_absolute` (wl-kbptr's approach).
pub struct AbsoluteBackend<I, V> {
    pub ipc: I,
    pub pointer: V,
    /// Logical layout; `logical_size` is already transformed.
    pub outputs: Vec<OutputGeom>,
}

impl<I, V> AbsoluteBackend<I, V> {
    /// Output-local position and the output's logical size for `p`.
    pub fn locate(&self, p: Point<Global>) -> Option<(&OutputGeom, u32, u32)> {
        let id = pick_output(Rect::new(p.x, p.y, 1, 1), &self.outputs, None)?;
        let o = self.outputs.iter().find(|o| o.id == id)?;
        let x = (p.x - o.origin.x).max(0) as u32;
        let y = (p.y - o.origin.y).max(0) as u32;
        Some((o, x, y))
    }
}

impl<I: CursorIpc + Sync, V: VirtualPointer + Sync> PointerBackend for AbsoluteBackend<I, V> {
    async fn move_to(&self, p: Point<Global>) -> Result<(), ActError> {
        let (o, x, y) = self
            .locate(p)
            .ok_or_else(|| ActError::Failed("no output for pointer position".into()))?;
        let (w, h) = (o.logical_size.w as u32, o.logical_size.h as u32);
        self.pointer.motion_absolute(&o.id, x, y, w, h).await?;
        self.pointer.roundtrip().await
    }
    async fn cursor_pos(&self) -> Result<Point<Global>, ActError> {
        cursor_pos(&self.ipc).await
    }
    async fn button(&self, code: u32, state: ButtonState) -> Result<(), ActError> {
        self.pointer.button(code, state).await
    }
    async fn motion_rel(&self, dx: f64, dy: f64) -> Result<(), ActError> {
        self.pointer.motion(dx, dy).await
    }
    async fn axis(&self, axis: Axis, px: f64, lines: i32) -> Result<(), ActError> {
        self.pointer.axis(axis, px, lines).await
    }
    async fn roundtrip(&self) -> Result<(), ActError> {
        self.pointer.roundtrip().await
    }
}

/// Either backend, selected by `input.pointer`.
pub enum Backend<I, V> {
    Movecursor(MovecursorBackend<I, V>),
    Absolute(AbsoluteBackend<I, V>),
}

macro_rules! each {
    ($self:ident, $b:ident => $e:expr) => {
        match $self {
            Backend::Movecursor($b) => $e,
            Backend::Absolute($b) => $e,
        }
    };
}

impl<I: CursorIpc + Sync, V: VirtualPointer + Sync> PointerBackend for Backend<I, V> {
    async fn move_to(&self, p: Point<Global>) -> Result<(), ActError> {
        each!(self, b => b.move_to(p).await)
    }
    async fn cursor_pos(&self) -> Result<Point<Global>, ActError> {
        each!(self, b => b.cursor_pos().await)
    }
    async fn button(&self, code: u32, state: ButtonState) -> Result<(), ActError> {
        each!(self, b => b.button(code, state).await)
    }
    async fn motion_rel(&self, dx: f64, dy: f64) -> Result<(), ActError> {
        each!(self, b => b.motion_rel(dx, dy).await)
    }
    async fn axis(&self, axis: Axis, px: f64, lines: i32) -> Result<(), ActError> {
        each!(self, b => b.axis(axis, px, lines).await)
    }
    async fn roundtrip(&self) -> Result<(), ActError> {
        each!(self, b => b.roundtrip().await)
    }
}

/// Runs pointer action sequences on a [`PointerBackend`].
pub struct PointerActuator<B> {
    pub backend: B,
    pub restore_cursor: bool,
    pub double_click_gap_ms: u32,
    pub drag_steps: u32,
    pub scroll_step: u32,
}

/// `n` relative steps summing exactly to `d`.
pub fn split(d: i32, n: u32) -> Vec<f64> {
    let n = n.max(1) as i64;
    let at = |i: i64| (i64::from(d) * i).div_euclid(n);
    (1..=n).map(|i| (at(i) - at(i - 1)) as f64).collect()
}

/// Button code of a click-like action.
pub fn button_of(action: Action) -> Option<u32> {
    match action {
        Action::Click | Action::Double => Some(BTN_LEFT),
        Action::Right => Some(BTN_RIGHT),
        Action::Middle => Some(BTN_MIDDLE),
        _ => None,
    }
}

impl<B: PointerBackend + Sync> PointerActuator<B> {
    pub fn new(backend: B, cfg: &InputConfig) -> Self {
        Self {
            backend,
            restore_cursor: cfg.restore_cursor,
            double_click_gap_ms: cfg.double_click_gap_ms,
            drag_steps: cfg.drag_steps,
            scroll_step: cfg.scroll_step,
        }
    }

    async fn press(&self, code: u32, state: ButtonState) -> Result<(), ActError> {
        self.backend.button(code, state).await?;
        self.backend.roundtrip().await
    }

    async fn click_once(&self, code: u32) -> Result<(), ActError> {
        self.press(code, ButtonState::Pressed).await?;
        self.press(code, ButtonState::Released).await
    }

    async fn saved_cursor(&self) -> Option<Point<Global>> {
        if !self.restore_cursor {
            return None;
        }
        match self.backend.cursor_pos().await {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!("cursor restore disabled for this action: {e}");
                None
            }
        }
    }

    async fn restore(&self, saved: Option<Point<Global>>) {
        if let Some(p) = saved {
            if let Err(e) = self.backend.move_to(p).await {
                tracing::warn!("cursor restore: {e}");
            }
        }
    }

    /// `click`, `right`, `middle`, `double` or `hover` at `rect`'s centre.
    pub async fn act(&self, action: Action, rect: Rect<Global>) -> Result<(), ActError> {
        let at = rect.center();
        if action == Action::Hover {
            // Never restored: the tooltip or menu lives under the pointer.
            return self.backend.move_to(at).await;
        }
        let code = button_of(action).ok_or(ActError::NeedsPointer(action))?;
        let saved = self.saved_cursor().await;
        self.backend.move_to(at).await?;
        self.click_once(code).await?;
        if action == Action::Double {
            tokio::time::sleep(Duration::from_millis(u64::from(self.double_click_gap_ms))).await;
            self.click_once(code).await?;
        }
        self.restore(saved).await;
        Ok(())
    }

    /// Press at `from`, move in `drag_steps` relative steps, place exactly
    /// at `to`, release (design D17).
    pub async fn drag(&self, from: Point<Global>, to: Point<Global>) -> Result<(), ActError> {
        let saved = self.saved_cursor().await;
        self.backend.move_to(from).await?;
        self.press(BTN_LEFT, ButtonState::Pressed).await?;
        let r = async {
            let xs = split(to.x - from.x, self.drag_steps);
            let ys = split(to.y - from.y, self.drag_steps);
            for (dx, dy) in xs.into_iter().zip(ys) {
                self.backend.motion_rel(dx, dy).await?;
                self.backend.roundtrip().await?;
            }
            self.backend.move_to(to).await
        }
        .await;
        // Release even when a step failed, so no button stays held.
        let released = self.press(BTN_LEFT, ButtonState::Released).await;
        r.and(released)?;
        self.restore(saved).await;
        Ok(())
    }

    /// Enter scroll mode: axis events go to the surface under the pointer.
    pub async fn scroll_enter(&self, rect: Rect<Global>) -> Result<(), ActError> {
        self.backend.move_to(rect.center()).await
    }

    /// Scroll `lines` (negative: up/left) at the current position.
    pub async fn scroll(&self, axis: Axis, lines: i32) -> Result<(), ActError> {
        self.backend
            .axis(axis, f64::from(lines) * LINE_PX, lines)
            .await?;
        self.backend.roundtrip().await
    }

    /// One scroll key press: `input.scroll_step` lines in `sign`'s direction.
    pub async fn scroll_key(&self, axis: Axis, sign: i32) -> Result<(), ActError> {
        self.scroll(axis, sign.signum() * self.scroll_step as i32)
            .await
    }
}

/// Run `f`, recording its duration as stage `act`.
pub async fn timed<T>(timings: &mut Timings, f: impl Future<Output = T>) -> T {
    let t = Instant::now();
    let r = f.await;
    timings.record(Timings::ACT, t.elapsed());
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use woints_core::geom::{OutputId, Size};

    /// Records every backend call as a string.
    #[derive(Default)]
    struct Rec {
        log: Mutex<Vec<String>>,
        fail_move: Option<String>,
        cursor: Option<Point<Global>>,
    }

    impl Rec {
        fn push(&self, s: String) {
            self.log.lock().unwrap().push(s);
        }
        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl CursorIpc for Rec {
        async fn move_cursor(&self, p: Point<Global>) -> Result<(), String> {
            CursorIpc::dispatch(self, &format!("movecursor {} {}", p.x, p.y)).await
        }
        async fn dispatch(&self, cmd: &str) -> Result<(), String> {
            self.push(cmd.to_owned());
            match &self.fail_move {
                Some(r) => Err(r.clone()),
                None => Ok(()),
            }
        }
        async fn cursor_pos(&self) -> Result<Point<Global>, String> {
            self.push("cursorpos".into());
            self.cursor.ok_or_else(|| "no cursor".into())
        }
    }

    impl VirtualPointer for &Rec {
        async fn button(&self, code: u32, state: ButtonState) -> Result<(), ActError> {
            let s = match state {
                ButtonState::Pressed => "pressed",
                ButtonState::Released => "released",
            };
            self.push(format!("button {code} {s}"));
            self.push("frame".into());
            Ok(())
        }
        async fn motion(&self, dx: f64, dy: f64) -> Result<(), ActError> {
            self.push(format!("motion {dx} {dy}"));
            self.push("frame".into());
            Ok(())
        }
        async fn axis(&self, axis: Axis, px: f64, lines: i32) -> Result<(), ActError> {
            self.push(format!("axis {axis:?} {px}"));
            self.push(format!("axis_discrete {axis:?} {lines}"));
            self.push("frame".into());
            Ok(())
        }
        async fn motion_absolute(
            &self,
            output: &OutputId,
            x: u32,
            y: u32,
            w: u32,
            h: u32,
        ) -> Result<(), ActError> {
            self.push(format!("motion_absolute@{} {x} {y} {w} {h}", output.0));
            self.push("frame".into());
            Ok(())
        }
        async fn roundtrip(&self) -> Result<(), ActError> {
            self.push("roundtrip".into());
            Ok(())
        }
    }

    fn actuator<'a>(
        ipc: &'a Rec,
        vp: &'a Rec,
        cfg: &InputConfig,
    ) -> PointerActuator<Backend<&'a Rec, &'a Rec>> {
        PointerActuator::new(
            Backend::Movecursor(MovecursorBackend { ipc, pointer: vp }),
            cfg,
        )
    }

    impl CursorIpc for &Rec {
        async fn dispatch(&self, cmd: &str) -> Result<(), String> {
            (**self).dispatch(cmd).await
        }
        async fn move_cursor(&self, p: Point<Global>) -> Result<(), String> {
            CursorIpc::move_cursor(*self, p).await
        }
        async fn cursor_pos(&self) -> Result<Point<Global>, String> {
            CursorIpc::cursor_pos(*self).await
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn left_click_sequence() {
        let rec = Rec::default();
        let a = actuator(&rec, &rec, &InputConfig::default());
        a.act(Action::Click, Rect::new(80, 90, 40, 20))
            .await
            .unwrap();
        assert_eq!(
            rec.log(),
            s(&[
                "movecursor 100 100",
                "button 272 pressed",
                "frame",
                "roundtrip",
                "button 272 released",
                "frame",
                "roundtrip",
            ])
        );
    }

    #[tokio::test]
    async fn negative_offset() {
        let rec = Rec::default();
        let a = actuator(&rec, &rec, &InputConfig::default());
        a.act(Action::Right, Rect::new(-1800, 100, 40, 40))
            .await
            .unwrap();
        let log = rec.log();
        assert_eq!(log[0], "movecursor -1780 120");
        assert_eq!(log[1], "button 273 pressed");
    }

    #[tokio::test]
    async fn middle_and_double() {
        let rec = Rec::default();
        let a = actuator(&rec, &rec, &InputConfig::default());
        a.act(Action::Middle, Rect::new(0, 0, 10, 10))
            .await
            .unwrap();
        assert!(rec.log().contains(&"button 274 pressed".to_string()));
        let rec = Rec::default();
        let a = actuator(&rec, &rec, &InputConfig::default());
        a.act(Action::Double, Rect::new(0, 0, 10, 10))
            .await
            .unwrap();
        let presses = rec
            .log()
            .iter()
            .filter(|l| l.starts_with("button 272"))
            .count();
        assert_eq!(presses, 4);
    }

    #[tokio::test]
    async fn dispatch_failure_aborts() {
        let rec = Rec {
            fail_move: Some("invalid args".into()),
            ..Default::default()
        };
        let a = actuator(&rec, &rec, &InputConfig::default());
        let e = a
            .act(Action::Click, Rect::new(0, 0, 10, 10))
            .await
            .unwrap_err();
        assert_eq!(e.to_string(), "movecursor failed: invalid args");
        assert!(!rec.log().iter().any(|l| l.starts_with("button")));
    }

    #[tokio::test]
    async fn hover_moves_only_and_never_restores() {
        let rec = Rec {
            cursor: Some(Point::new(300, 400)),
            ..Default::default()
        };
        let cfg = InputConfig {
            restore_cursor: true,
            ..Default::default()
        };
        let a = actuator(&rec, &rec, &cfg);
        a.act(Action::Hover, Rect::new(0, 0, 10, 10)).await.unwrap();
        assert_eq!(rec.log(), s(&["movecursor 5 5"]));
    }

    #[tokio::test]
    async fn restore_cursor() {
        let rec = Rec {
            cursor: Some(Point::new(300, 400)),
            ..Default::default()
        };
        let cfg = InputConfig {
            restore_cursor: true,
            ..Default::default()
        };
        let a = actuator(&rec, &rec, &cfg);
        a.act(Action::Click, Rect::new(0, 0, 10, 10)).await.unwrap();
        let log = rec.log();
        assert_eq!(log[0], "cursorpos");
        assert_eq!(log.last().unwrap(), "movecursor 300 400");
    }

    #[tokio::test]
    async fn drag_sequence() {
        let rec = Rec::default();
        let a = actuator(&rec, &rec, &InputConfig::default());
        a.drag(Point::new(100, 100), Point::new(500, 300))
            .await
            .unwrap();
        let log = rec.log();
        assert_eq!(log[0], "movecursor 100 100");
        assert_eq!(log[1], "button 272 pressed");
        let motions: Vec<(f64, f64)> = log
            .iter()
            .filter_map(|l| l.strip_prefix("motion "))
            .map(|m| {
                let mut it = m.split(' ').map(|v| v.parse::<f64>().unwrap());
                (it.next().unwrap(), it.next().unwrap())
            })
            .collect();
        assert_eq!(motions.len(), 8);
        let sum = motions
            .iter()
            .fold((0.0, 0.0), |(x, y), (dx, dy)| (x + dx, y + dy));
        assert_eq!(sum, (400.0, 200.0));
        // Each motion is followed by frame + roundtrip.
        let i = log.iter().position(|l| l.starts_with("motion")).unwrap();
        assert_eq!(&log[i + 1..i + 3], &s(&["frame", "roundtrip"])[..]);
        let n = log.len();
        assert_eq!(
            &log[n - 4..],
            &s(&[
                "movecursor 500 300",
                "button 272 released",
                "frame",
                "roundtrip"
            ])[..]
        );
    }

    #[tokio::test]
    async fn scroll_down() {
        let rec = Rec::default();
        let a = actuator(&rec, &rec, &InputConfig::default());
        a.scroll_enter(Rect::new(100, 100, 400, 300)).await.unwrap();
        a.scroll_key(Axis::Vertical, 1).await.unwrap();
        a.scroll(Axis::Vertical, -3).await.unwrap();
        assert_eq!(
            rec.log(),
            s(&[
                "movecursor 300 250",
                "axis Vertical 45",
                "axis_discrete Vertical 3",
                "frame",
                "roundtrip",
                "axis Vertical -45",
                "axis_discrete Vertical -3",
                "frame",
                "roundtrip",
            ])
        );
    }

    #[test]
    fn split_sums() {
        for (d, n) in [(400, 8), (-7, 3), (0, 8), (5, 8), (-1800, 7)] {
            let v = split(d, n);
            assert_eq!(v.len(), n as usize);
            assert_eq!(v.iter().sum::<f64>(), f64::from(d));
        }
    }

    #[tokio::test]
    async fn absolute_backend() {
        let rec = Rec::default();
        let b = AbsoluteBackend {
            ipc: &rec,
            pointer: &rec,
            outputs: vec![OutputGeom {
                id: OutputId("DP-1".into()),
                origin: Point::new(0, 0),
                logical_size: Size { w: 1440, h: 900 },
                scale: 2.0,
            }],
        };
        b.move_to(Point::new(1500, 450)).await.unwrap();
        assert_eq!(
            rec.log(),
            s(&[
                "motion_absolute@DP-1 1500 450 1440 900",
                "frame",
                "roundtrip"
            ])
        );
    }
}
