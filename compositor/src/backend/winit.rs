//! The nested backend: the compositor draws into one window on a host
//! Wayland or X11 session. It is for development and for the checks that
//! need a display. Base runs the compositor on a TTY.

use std::{cell::RefCell, rc::Rc};

use smithay::{
    backend::{
        egl::EGLDevice,
        renderer::{ImportDma, damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::{layer_map_for_output, space::space_render_elements},
    output::{Mode, Output},
    reexports::calloop::EventLoop,
    utils::Transform,
    wayland::dmabuf::DmabufFeedbackBuilder,
};

use crate::{
    State,
    backend::{BACKGROUND, Backend, Drawn},
};

/// The host does not say how fast it shows the window. Assume 60 Hz.
const REFRESH: i32 = 60_000;

struct Nested {
    window: Rc<RefCell<WinitGraphicsBackend<GlesRenderer>>>,
    damage: OutputDamageTracker,
}

pub fn init(event_loop: &mut EventLoop<'static, State>, state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    let attributes = smithay::reexports::winit::window::WindowAttributes::default()
        .with_title("CrowbarOS compositor")
        .with_surface_size(smithay::reexports::winit::dpi::LogicalSize::new(1280.0, 800.0));
    let (mut window, events) = winit::init_from_attributes::<GlesRenderer>(attributes)?;

    let size = window.window_size();
    let output = state.add_output("winit", (size.w, size.h), REFRESH);
    // The winit backend draws upside down relative to the output.
    output.change_current_state(None, Some(Transform::Flipped180), None, None);

    // linux-dmabuf, for clients that draw with the GPU. Without the host's
    // render node there is nothing to tell them, so it is not offered.
    let render_node = EGLDevice::device_for_display(window.renderer().egl_context().display())
        .and_then(|device| device.try_get_render_node());
    match render_node {
        Ok(Some(node)) => {
            let formats = window.renderer().dmabuf_formats();
            state.offer_dmabuf(&DmabufFeedbackBuilder::new(node.dev_id(), formats).build()?);
        }
        other => tracing::warn!(?other, "winit: no render node, so linux-dmabuf is not offered"),
    }

    let window = Rc::new(RefCell::new(window));
    state.set_backend(Nested {
        window: window.clone(),
        damage: OutputDamageTracker::from_output(&output),
    });

    event_loop
        .handle()
        .insert_source(events, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                output.change_current_state(Some(Mode { size, refresh: REFRESH }), None, None, None);
                layer_map_for_output(&output).arrange();
                state.queue_redraw(&output);
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            // The host wants the window drawn again, for example after it was
            // covered.
            WinitEvent::Redraw => state.queue_redraw(&output),
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            _ => {}
        })?;
    Ok(())
}

impl Backend for Nested {
    fn draw(&mut self, state: &State, output: &Output) -> Drawn {
        let mut window = self.window.borrow_mut();
        let age = window.buffer_age().unwrap_or(0);
        let damage = {
            let (renderer, mut framebuffer) = match window.bind() {
                Ok(bound) => bound,
                Err(error) => {
                    tracing::warn!(%error, "winit: could not bind the window");
                    return Drawn::Nothing;
                }
            };
            let elements = match space_render_elements(renderer, [&state.space], output, 1.0) {
                Ok(elements) => elements,
                Err(_) => return Drawn::Nothing,
            };
            match self
                .damage
                .render_output(renderer, &mut framebuffer, age, &elements, BACKGROUND)
            {
                Ok(result) => result.damage.cloned(),
                Err(error) => {
                    tracing::warn!(%error, "winit: render failed");
                    return Drawn::Nothing;
                }
            }
        };
        let Some(damage) = damage else {
            return Drawn::Nothing;
        };
        match window.submit(Some(&damage)) {
            Ok(()) => Drawn::Shown,
            Err(error) => {
                tracing::warn!(%error, "winit: could not show the frame");
                Drawn::Nothing
            }
        }
    }

    fn import_dmabuf(&mut self, dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf) -> bool {
        self.window.borrow_mut().renderer().import_dmabuf(dmabuf, None).is_ok()
    }
}
