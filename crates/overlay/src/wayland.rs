//! Layer-shell overlay thread (design D5): one OVERLAY surface per output,
//! fractional scale via `wp_fractional_scale_v1` + `wp_viewporter`.

use std::time::Instant;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopSignal,
            channel::{Channel, Event as ChannelEvent},
        },
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers as SctkModifiers},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use tokio::sync::{mpsc, oneshot};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::{GlobalList, registry_queue_init},
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
};
use wayland_protocols::wp::{
    fractional_scale::v1::client::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1},
    viewporter::client::{wp_viewport, wp_viewporter},
};

use woints_core::geom::{OutputGeom, OutputId, Point, Size};
use woints_core::ports::{CommitTimings, KeyPress, Modifiers, OverlayError, OverlayEvent, Scene};

use crate::render::{OutputSurface, Renderer, dump_if_requested, to_argb8888};

pub(crate) enum Command {
    Show {
        scene: Box<Scene>,
        reply: oneshot::Sender<Result<CommitTimings, OverlayError>>,
    },
    Hide,
    Shutdown,
}

struct OutSurface {
    output_id: OutputId,
    wl_output: wl_output::WlOutput,
    layer: LayerSurface,
    viewport: Option<wp_viewport::WpViewport>,
    fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    /// `preferred_scale` in 120ths, once the compositor sent it.
    scale120: Option<u32>,
    /// Logical size from the last configure.
    size: Option<(u32, u32)>,
    drawn: bool,
}

struct PendingReply {
    reply: oneshot::Sender<Result<CommitTimings, OverlayError>>,
    timings: CommitTimings,
}

pub(crate) struct State {
    registry: RegistryState,
    outputs: OutputState,
    seats: SeatState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    fractional: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    mods: Modifiers,

    renderer: Renderer,
    scene: Option<Scene>,
    surfaces: Vec<OutSurface>,
    pending: Option<PendingReply>,
    events: mpsc::UnboundedSender<OverlayEvent>,
    warned_mm: Vec<OutputId>,
    signal: LoopSignal,
    qh: QueueHandle<State>,
}

/// Connect, bind globals and run the event loop until `Shutdown` or a
/// Wayland error. `ready` reports whether startup succeeded.
pub(crate) fn run(
    commands: Channel<Command>,
    events: mpsc::UnboundedSender<OverlayEvent>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    let setup = || -> Result<(EventLoop<'static, State>, State), String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {e}"))?;
        let (globals, queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().map_err(|e| e.to_string())?;
        WaylandSource::new(conn.clone(), queue)
            .insert(event_loop.handle())
            .map_err(|e| e.to_string())?;
        let state = State::new(&globals, &qh, events.clone(), event_loop.get_signal())?;
        event_loop
            .handle()
            .insert_source(commands, |ev, _, state: &mut State| match ev {
                ChannelEvent::Msg(cmd) => state.handle(cmd),
                ChannelEvent::Closed => state.signal.stop(),
            })
            .map_err(|e| e.to_string())?;
        Ok((event_loop, state))
    };
    let (mut event_loop, mut state) = match setup() {
        Ok(v) => {
            let _ = ready.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = ready.send(Err(e.clone()));
            return Err(e);
        }
    };
    event_loop
        .run(None, &mut state, |_| {})
        .map_err(|e| format!("overlay event loop: {e}"))?;
    state.hide();
    Ok(())
}

impl State {
    fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        events: mpsc::UnboundedSender<OverlayEvent>,
        signal: LoopSignal,
    ) -> Result<Self, String> {
        let compositor =
            CompositorState::bind(globals, qh).map_err(|e| format!("wl_compositor: {e}"))?;
        let layer_shell =
            LayerShell::bind(globals, qh).map_err(|e| format!("zwlr_layer_shell_v1: {e}"))?;
        let shm = Shm::bind(globals, qh).map_err(|e| format!("wl_shm: {e}"))?;
        let pool = SlotPool::new(1 << 20, &shm).map_err(|e| format!("shm pool: {e}"))?;
        let fractional = globals.bind(qh, 1..=1, ()).ok();
        let viewporter = globals.bind(qh, 1..=1, ()).ok();
        if fractional.is_none() || viewporter.is_none() {
            tracing::warn!(
                "fractional scale or viewporter missing; falling back to integer wl_output.scale"
            );
        }
        Ok(Self {
            registry: RegistryState::new(globals),
            outputs: OutputState::new(globals, qh),
            seats: SeatState::new(globals, qh),
            compositor,
            layer_shell,
            shm,
            pool,
            fractional,
            viewporter,
            keyboard: None,
            mods: Modifiers::default(),
            renderer: Renderer::new(),
            scene: None,
            surfaces: Vec::new(),
            pending: None,
            events,
            warned_mm: Vec::new(),
            signal,
            qh: qh.clone(),
        })
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Show { scene, reply } => self.show(*scene, reply),
            Command::Hide => self.hide(),
            Command::Shutdown => {
                self.hide();
                self.signal.stop();
            }
        }
    }

    fn find_output(&self, id: &OutputId) -> Option<wl_output::WlOutput> {
        self.outputs
            .outputs()
            .find(|o| self.outputs.info(o).and_then(|i| i.name).as_deref() == Some(id.0.as_str()))
    }

    fn show(&mut self, scene: Scene, reply: oneshot::Sender<Result<CommitTimings, OverlayError>>) {
        if let Some(p) = self.pending.take() {
            let _ = p.reply.send(Err(OverlayError::Other("superseded".into())));
        }
        let mut wanted: Vec<OutputId> = scene.badges.iter().map(|b| b.output.clone()).collect();
        if let Some(c) = &scene.chip {
            wanted.push(c.output.clone());
        }
        wanted.sort();
        wanted.dedup();

        // Drop surfaces on outputs that no longer have badges.
        self.surfaces.retain(|s| wanted.contains(&s.output_id));
        for id in &wanted {
            if self.surfaces.iter().any(|s| &s.output_id == id) {
                continue;
            }
            let Some(wl_output) = self.find_output(id) else {
                let _ = reply.send(Err(OverlayError::NoOutput(id.clone())));
                self.hide();
                return;
            };
            self.surfaces
                .push(self.create_surface(id.clone(), wl_output));
        }
        for s in &mut self.surfaces {
            s.drawn = false;
        }
        self.scene = Some(scene);
        self.pending = Some(PendingReply {
            reply,
            timings: CommitTimings::default(),
        });
        // Surfaces that are already configured redraw now; new ones on configure.
        for i in 0..self.surfaces.len() {
            if self.surfaces[i].size.is_some() {
                self.draw(i);
            }
        }
        self.maybe_reply();
    }

    fn create_surface(&self, output_id: OutputId, wl_output: wl_output::WlOutput) -> OutSurface {
        let qh = &self.qh;
        let surface = self.compositor.create_surface(qh);
        let fractional = self
            .fractional
            .as_ref()
            .map(|m| m.get_fractional_scale(&surface, qh, surface.id()));
        let viewport = self
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(&surface, qh, ()));
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("woints"),
            Some(&wl_output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        // Empty input region: pointer events pass through to the windows below.
        match Region::new(&self.compositor) {
            Ok(r) => layer.wl_surface().set_input_region(Some(r.wl_region())),
            Err(e) => tracing::warn!("input region: {e}"),
        }
        layer.commit();
        tracing::debug!(output = %output_id, "layer surface created");
        OutSurface {
            output_id,
            wl_output,
            layer,
            viewport,
            fractional,
            scale120: None,
            size: None,
            drawn: false,
        }
    }

    pub(crate) fn hide(&mut self) {
        for s in self.surfaces.drain(..) {
            if let Some(f) = s.fractional {
                f.destroy();
            }
            if let Some(v) = s.viewport {
                v.destroy();
            }
            drop(s.layer);
        }
        self.scene = None;
        if let Some(p) = self.pending.take() {
            let _ = p.reply.send(Err(OverlayError::Other("hidden".into())));
        }
    }

    /// Effective scale for surface `i`: fractional preferred scale, else
    /// Hyprland's scale from the scene, else integer `wl_output.scale`.
    fn scale_of(&self, i: usize, hypr: Option<&OutputGeom>) -> (f32, bool) {
        let s = &self.surfaces[i];
        if let (Some(v), true) = (s.scale120, self.viewporter.is_some()) {
            return (v as f32 / 120.0, true);
        }
        if let (Some(g), true) = (hypr, self.viewporter.is_some()) {
            return (g.scale, true);
        }
        let int = self
            .outputs
            .info(&s.wl_output)
            .map(|i| i.scale_factor)
            .unwrap_or(1)
            .max(1);
        (int as f32, false)
    }

    fn draw(&mut self, i: usize) {
        let Some(scene) = self.scene.clone() else {
            return;
        };
        let Some((lw, lh)) = self.surfaces[i].size else {
            return;
        };
        let started = Instant::now();
        let id = self.surfaces[i].output_id.clone();
        let hypr = scene.outputs.iter().find(|o| o.id == id);
        let (scale, viewported) = self.scale_of(i, hypr);
        let info = self.outputs.info(&self.surfaces[i].wl_output);
        let origin = hypr
            .map(|g| g.origin)
            .or_else(|| {
                info.as_ref()
                    .and_then(|i| i.logical_position)
                    .map(|(x, y)| Point::new(x, y))
            })
            .unwrap_or(Point::new(0, 0));
        let phys_width_mm = info.as_ref().map(|i| i.physical_size.0).unwrap_or(0);
        if scene.theme.hint.size_unit == woints_core::theme::SizeUnit::Mm
            && phys_width_mm <= 0
            && !self.warned_mm.contains(&id)
        {
            tracing::warn!(output = %id, "output reports 0 mm; using logical badge size");
            self.warned_mm.push(id.clone());
        }
        let out = OutputSurface {
            geom: OutputGeom {
                id: id.clone(),
                origin,
                logical_size: Size {
                    w: lw as i32,
                    h: lh as i32,
                },
                scale,
            },
            phys_width_mm,
        };
        let pm = self.renderer.render(&scene, &out);
        let (bw, bh) = (pm.width() as i32, pm.height() as i32);
        let stride = bw * 4;
        let (buffer, canvas) =
            match self
                .pool
                .create_buffer(bw, bh, stride, wl_shm::Format::Argb8888)
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("shm buffer {bw}x{bh}: {e}");
                    return;
                }
            };
        to_argb8888(&pm, canvas);
        let render_ms = started.elapsed().as_secs_f64() * 1000.0;
        dump_if_requested(&pm, &id.0);

        let committed = Instant::now();
        let s = &self.surfaces[i];
        let surface = s.layer.wl_surface();
        if viewported {
            surface.set_buffer_scale(1);
            if let Some(v) = &s.viewport {
                v.set_destination(lw as i32, lh as i32);
            }
        } else {
            surface.set_buffer_scale(scale as i32);
        }
        surface.damage_buffer(0, 0, bw, bh);
        if let Err(e) = buffer.attach_to(surface) {
            tracing::error!("attach: {e}");
            return;
        }
        s.layer.commit();
        let commit_ms = committed.elapsed().as_secs_f64() * 1000.0;
        tracing::debug!(output = %id, scale, bw, bh, "overlay committed");
        self.surfaces[i].drawn = true;
        if let Some(p) = &mut self.pending {
            p.timings.render_ms += render_ms;
            p.timings.commit_ms += commit_ms;
        }
    }

    fn maybe_reply(&mut self) {
        if self.surfaces.iter().all(|s| s.drawn) {
            if let Some(p) = self.pending.take() {
                let _ = p.reply.send(Ok(p.timings));
            }
        }
    }

    fn surface_index(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.surfaces
            .iter()
            .position(|s| s.layer.wl_surface() == surface)
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: i32,
    ) {
        // Integer fallback path only: redraw at the new wl_output scale.
        if self.fractional.is_none() {
            if let Some(i) = self.surface_index(surface) {
                self.draw(i);
            }
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.surfaces.iter().any(|s| s.wl_output == output) {
            self.hide();
            let _ = self.events.send(OverlayEvent::Escaped);
        }
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if self.surfaces.iter().any(|s| &s.layer == layer) {
            self.hide();
            let _ = self.events.send(OverlayEvent::Escaped);
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(i) = self.surfaces.iter().position(|s| &s.layer == layer) else {
            return;
        };
        let (w, h) = configure.new_size;
        if w == 0 || h == 0 {
            return;
        }
        self.surfaces[i].size = Some((w, h));
        self.draw(i);
        self.maybe_reply();
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            // `get_keyboard` (not `_with_repeat`): key repeat is ignored.
            match self.seats.get_keyboard(qh, &seat, None) {
                Ok(k) => self.keyboard = Some(k),
                Err(e) => tracing::error!("keyboard: {e}"),
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        ev: KeyEvent,
    ) {
        if self.surfaces.is_empty() {
            return;
        }
        if ev.keysym == Keysym::Escape {
            self.hide();
            let _ = self.events.send(OverlayEvent::Escaped);
            return;
        }
        let _ = self.events.send(OverlayEvent::Key(KeyPress {
            // sctk reports the evdev code; xkb keycodes are offset by 8.
            keycode: ev.raw_code + 8,
            keysym: ev.keysym.raw(),
            utf8: ev.utf8,
            mods: self.mods,
        }));
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        m: SctkModifiers,
        _: u32,
    ) {
        self.mods = Modifiers {
            ctrl: m.ctrl,
            alt: m.alt,
            shift: m.shift,
            logo: m.logo,
        };
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        _: wp_fractional_scale_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, wayland_client::backend::ObjectId>
    for State
{
    fn event(
        state: &mut Self,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        surface: &wayland_client::backend::ObjectId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wp_fractional_scale_v1::Event::PreferredScale { scale } = event else {
            return;
        };
        let Some(i) = state
            .surfaces
            .iter()
            .position(|s| &s.layer.wl_surface().id() == surface)
        else {
            return;
        };
        let changed = state.surfaces[i].scale120 != Some(scale);
        state.surfaces[i].scale120 = Some(scale);
        tracing::debug!(output = %state.surfaces[i].output_id, scale = scale as f32 / 120.0, "preferred scale");
        // Re-render at the new scale if we already drew at a different one.
        if changed && state.surfaces[i].drawn {
            state.draw(i);
        }
    }
}

impl Dispatch<wp_viewporter::WpViewporter, ()> for State {
    fn event(
        _: &mut Self,
        _: &wp_viewporter::WpViewporter,
        _: wp_viewporter::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_viewport::WpViewport, ()> for State {
    fn event(
        _: &mut Self,
        _: &wp_viewport::WpViewport,
        _: wp_viewport::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_seat!(State);
delegate_keyboard!(State);
delegate_layer!(State);
delegate_registry!(State);
