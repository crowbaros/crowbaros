// SPDX-License-Identifier: MIT
use std::borrow::Cow;

use gpui::{
    App, Application, Bounds, Context, Div, FocusHandle, FontWeight, KeyBinding, SharedString,
    Stateful, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px,
    rgb, size,
};

actions!(
    probe,
    [Increment, TogglePalette, Reset, Next, Previous, Quit]
);

struct Probe {
    root: FocusHandle,
    controls: [FocusHandle; 3],
    count: u32,
    studio: bool,
    last_event: SharedString,
}

impl Probe {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        window.set_window_title("CrowbarOS · GPUI Probe");
        let controls = [
            cx.focus_handle().tab_index(1).tab_stop(true),
            cx.focus_handle().tab_index(2).tab_stop(true),
            cx.focus_handle().tab_index(3).tab_stop(true),
        ];
        window.focus(&controls[0]);
        log::info!("GPUI_PROBE event=opened gpui=0.2.2 backend=wayland");
        Self {
            root: cx.focus_handle(),
            controls,
            count: 0,
            studio: false,
            last_event: "Ready. Try the controls below.".into(),
        }
    }

    fn change(&mut self, event: &str, cx: &mut Context<Self>) {
        self.last_event = format!(
            "{} · count {} · {}",
            event,
            self.count,
            if self.studio { "Studio" } else { "Crowbar" }
        )
        .into();
        log::info!(
            "GPUI_PROBE event={event} count={} studio={}",
            self.count,
            self.studio
        );
        cx.notify();
    }

    fn increment(&mut self, _: &Increment, _: &mut Window, cx: &mut Context<Self>) {
        self.count = self.count.saturating_add(1);
        self.change("increment", cx);
    }

    fn toggle(&mut self, _: &TogglePalette, _: &mut Window, cx: &mut Context<Self>) {
        self.studio = !self.studio;
        self.change("palette", cx);
    }

    fn reset(&mut self, _: &Reset, _: &mut Window, cx: &mut Context<Self>) {
        self.count = 0;
        self.change("reset", cx);
    }

    fn button(&self, index: usize, label: &'static str, window: &Window) -> Stateful<Div> {
        let accent = if self.studio { 0xe5e5e5 } else { 0x49d7e8 };
        div()
            .id(("control", index))
            .track_focus(&self.controls[index])
            .flex()
            .items_center()
            .justify_center()
            .h(px(46.))
            .px_4()
            .rounded(px(10.))
            .border_2()
            .border_color(rgb(if self.controls[index].is_focused(window) {
                accent
            } else {
                if self.studio { 0x858585 } else { 0x6d8191 }
            }))
            .bg(rgb(if index == 0 {
                accent
            } else if self.studio {
                0x212121
            } else {
                0x17212b
            }))
            .text_color(rgb(if index == 0 {
                0x080d12
            } else if self.studio {
                0xe5e5e5
            } else {
                0xd8e5ec
            }))
            .text_size(px(14.))
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .hover(|s| s.opacity(0.82))
            .child(label)
    }
}

impl Render for Probe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = if self.studio { 0x111111 } else { 0x080d12 };
        let surface = if self.studio { 0x212121 } else { 0x17212b };
        let accent = if self.studio { 0xe5e5e5 } else { 0x49d7e8 };
        let secondary = if self.studio { 0xb8b8b8 } else { 0xa8bac7 };
        div().id("probe").key_context("GpuiProbe").track_focus(&self.root)
            .on_action(cx.listener(Self::increment))
            .on_action(cx.listener(Self::toggle))
            .on_action(cx.listener(Self::reset))
            .on_action(cx.listener(|this, _: &Next, window, cx| {
                window.focus_next(); this.change("focus-next", cx);
            }))
            .on_action(cx.listener(|this, _: &Previous, window, cx| {
                window.focus_prev(); this.change("focus-previous", cx);
            }))
            .on_action(|_: &Quit, _, cx| cx.quit())
            .size_full().flex().flex_col().p_8().gap_5()
            .bg(rgb(bg)).text_color(rgb(if self.studio { 0xe5e5e5 } else { 0xd8e5ec })).font_family("Manrope")
            .child(div().flex().justify_between().items_center()
                .child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child("CrowbarOS"))
                .child(div().font_family("IBM Plex Mono").text_size(px(11.))
                    .text_color(rgb(accent)).child("GPUI / WAYLAND / 0.2.2")))
            .child(div().flex().flex_col().gap_2()
                .child(div().text_size(px(34.)).font_weight(FontWeight::BOLD).child("A small window. A real start."))
                .child(div().text_size(px(15.)).text_color(rgb(secondary))
                    .child("Native Rust, packaged for Fedora. Explore the controls, then resize this window.")))
            .child(div().flex().items_center().gap_6().p_5().rounded(px(16.)).bg(rgb(surface))
                .child(div().font_family("IBM Plex Mono").text_size(px(48.))
                    .text_color(rgb(accent)).child(format!("{:02}", self.count)))
                .child(div().flex().flex_col().gap_1()
                    .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child("Interactions counted"))
                    .child(div().text_size(px(13.)).text_color(rgb(secondary))
                        .child("Click Increment, or use the keyboard."))))
            .child(div().flex().flex_wrap().gap_3()
                .child(self.button(0, "Increment", window).on_click(cx.listener(|this, _, window, cx| {
                    window.focus(&this.controls[0]); this.increment(&Increment, window, cx);
                })))
                .child(self.button(1, if self.studio { "Use Crowbar palette" } else { "Use Studio palette" }, window)
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.focus(&this.controls[1]); this.toggle(&TogglePalette, window, cx);
                    })))
                .child(self.button(2, "Reset count", window).on_click(cx.listener(|this, _, window, cx| {
                    window.focus(&this.controls[2]); this.reset(&Reset, window, cx);
                }))))
            .child(div().font_family("IBM Plex Mono").text_size(px(11.))
                .text_color(rgb(secondary)).child("Tab: focus   Enter / Space: activate   Ctrl+I: increment   Ctrl+Q: quit"))
            .child(div().flex().flex_col().flex_1().min_h(px(80.)).gap_2()
                .child(div().flex().justify_between().text_size(px(13.))
                    .child("Scroll & text rendering")
                    .child(div().text_color(rgb(secondary)).child("40 rows")))
                .child(div().id("scroll-list").flex_1().min_h(px(0.)).overflow_y_scroll()
                    .rounded(px(10.)).bg(rgb(surface))
                    .on_scroll_wheel(|_, _, _| log::info!("GPUI_PROBE event=scroll"))
                    .children((1usize..=40).map(|index| div().id(("row", index)).h(px(38.)).px_4()
                        .flex().items_center().justify_between().border_b_1().border_color(rgb(bg))
                        .child(div().text_size(px(13.)).child(format!("{:02}  Quiet surface. Sharp mind.", index)))
                        .child(div().font_family("IBM Plex Mono").text_size(px(11.))
                            .text_color(rgb(secondary)).child("Aa 0123 · café →"))))))
            .child(div().text_size(px(12.)).text_color(rgb(accent)).child(self.last_event.clone()))
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!(
            "crowbaros-gpui-probe {} (GPUI 0.2.2; Wayland)",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    Application::new().run(|cx: &mut App| {
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../fonts/Manrope-Regular.ttf")),
                Cow::Borrowed(include_bytes!("../fonts/Manrope-SemiBold.ttf")),
                Cow::Borrowed(include_bytes!("../fonts/Manrope-Bold.ttf")),
                Cow::Borrowed(include_bytes!("../fonts/IBMPlexMono-Regular.ttf")),
            ])
            .expect("bundled fonts must load");
        cx.bind_keys([
            KeyBinding::new("ctrl-i", Increment, Some("GpuiProbe")),
            KeyBinding::new("ctrl-t", TogglePalette, Some("GpuiProbe")),
            KeyBinding::new("ctrl-r", Reset, Some("GpuiProbe")),
            KeyBinding::new("tab", Next, Some("GpuiProbe")),
            KeyBinding::new("shift-tab", Previous, Some("GpuiProbe")),
            KeyBinding::new("ctrl-q", Quit, Some("GpuiProbe")),
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(820.), px(700.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(640.), px(600.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("CrowbarOS · GPUI Probe".into()),
                    ..Default::default()
                }),
                app_id: Some("com.crowbaros.GpuiProbe".into()),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Probe::new(window, cx)),
        )
        .expect("Wayland window must open");
        cx.activate(true);
    });
}
