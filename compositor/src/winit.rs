//! The nested backend: the compositor draws into one window on a host
//! Wayland or X11 session.

use std::{cell::RefCell, rc::Rc};

use smithay::{
    backend::{
        egl::EGLDevice,
        renderer::{
            ImportDma, damage::OutputDamageTracker, element::surface::WaylandSurfaceRenderElement, gles::GlesRenderer,
        },
        winit::{self, WinitEvent},
    },
    desktop::layer_map_for_output,
    output::Mode,
    reexports::calloop::EventLoop,
    utils::Rectangle,
    wayland::dmabuf::DmabufFeedbackBuilder,
};

use crate::State;

pub fn init_winit(
    event_loop: &mut EventLoop<'static, State>,
    state: &mut State,
    scale: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    let attributes = smithay::reexports::winit::window::WindowAttributes::default()
        .with_title("CrowbarOS compositor spike")
        .with_surface_size(smithay::reexports::winit::dpi::LogicalSize::new(1280.0, 800.0));
    let (backend, winit) = winit::init_from_attributes::<GlesRenderer>(attributes)?;
    let backend = Rc::new(RefCell::new(backend));

    let size = backend.borrow().window_size();
    let output = state.add_output("winit", (size.w, size.h), scale);
    // The winit backend draws upside down relative to the output.
    output.change_current_state(None, Some(smithay::utils::Transform::Flipped180), None, None);

    // Offer linux-dmabuf so GPU clients can hand over their buffers.
    {
        let mut backend = backend.borrow_mut();
        let formats = backend.renderer().dmabuf_formats();
        let render_node = EGLDevice::device_for_display(backend.renderer().egl_context().display())
            .and_then(|device| device.try_get_render_node());
        let global = match render_node {
            Ok(Some(node)) => {
                tracing::info!(?node, "dmabuf: feedback with the host render node");
                let feedback = DmabufFeedbackBuilder::new(node.dev_id(), formats).build()?;
                state
                    .dmabuf_state
                    .create_global_with_default_feedback::<State>(&state.display_handle, &feedback)
            }
            other => {
                tracing::warn!(?other, "dmabuf: no render node, offering version 3");
                state
                    .dmabuf_state
                    .create_global::<State>(&state.display_handle, formats)
            }
        };
        state.dmabuf_global = Some(global);
    }
    let importer = backend.clone();
    state.dmabuf_import = Some(Box::new(move |dmabuf| {
        importer.borrow_mut().renderer().import_dmabuf(dmabuf, None).is_ok()
    }));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    event_loop.handle().insert_source(winit, move |event, _, state| {
        match event {
            WinitEvent::Resized { size, .. } => {
                output.change_current_state(Some(Mode { size, refresh: 60_000 }), None, None, None);
                layer_map_for_output(&output).arrange();
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => {
                let mut backend = backend.borrow_mut();
                let size = backend.window_size();
                let damage = Rectangle::from_size(size);

                {
                    let (renderer, mut framebuffer) = backend.bind().unwrap();
                    smithay::desktop::space::render_output::<_, WaylandSurfaceRenderElement<GlesRenderer>, _, _>(
                        &output,
                        renderer,
                        &mut framebuffer,
                        1.0,
                        0,
                        [&state.space],
                        &[],
                        &mut damage_tracker,
                        [0.1, 0.1, 0.1, 1.0],
                    )
                    .unwrap();
                }
                backend.submit(Some(&[damage])).unwrap();

                state.send_frames(&output);
                state.after_frame();

                // Ask for redraw to schedule new frame.
                backend.window().request_redraw();
            }
            WinitEvent::CloseRequested => {
                state.loop_signal.stop();
            }
            _ => (),
        };
    })?;

    Ok(())
}
