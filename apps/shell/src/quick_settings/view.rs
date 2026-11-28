// SPDX-License-Identifier: MIT
//! Quick Settings popover, transient volume/brightness OSD pill, and media key
//! handler.

use gpui::{layer_shell::*, prelude::*, *};
use std::time::Instant;

use super::{AudioQuickState, OSD_DURATION, OsdKind, OsdState, PowerConfirm};
use crate::{
    chrome,
    events::{Event, MediaKey},
    launcher_client::LauncherClient,
    ui::{
        ActivePopover, BAR_HEIGHT, Close, DOCK_HEIGHT, Model, clear_active_popover_if,
        close_active_popover, icon, layer, layer_with_margin,
    },
};

pub struct QuickSettingsPopover {
    model: Entity<Model>,
    focus: FocusHandle,
    confirm_power: Option<PowerConfirm>,
    busy: bool,
    error: Option<String>,
}

impl QuickSettingsPopover {
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.read(cx).is_quick_settings_open() {
            log::info!("SHELL quick-settings open=false");
        }
        clear_active_popover_if(
            &self.model,
            |p| matches!(p, ActivePopover::QuickSettings(_)),
            cx,
        );
        window.remove_window();
        cx.refresh_windows();
    }

    fn run_audio_action(
        &mut self,
        op: impl FnOnce(LauncherClient) -> Result<AudioQuickState, String> + Send + 'static,
        kind: OsdKind,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let (tx, launcher) = {
            let m = self.model.read(cx);
            (m.tx.clone(), m.launcher_client.clone())
        };
        let task = cx.background_executor().spawn(async move { op(launcher) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(audio) => {
                        let (percent, muted) = match kind {
                            OsdKind::Microphone => (audio.volume, audio.mic_muted),
                            _ => (audio.volume, audio.muted),
                        };
                        this.model.update(cx, |m, _| m.quick_status.audio = audio);
                        let _ = tx.send(Event::Osd {
                            kind,
                            percent,
                            muted,
                        });
                    }
                    Err(err) => {
                        this.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn run_brightness_action(&mut self, percent: u32, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let (tx, launcher) = {
            let m = self.model.read(cx);
            (m.tx.clone(), m.launcher_client.clone())
        };
        let task = cx
            .background_executor()
            .spawn(async move { launcher.set_brightness(percent) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(brightness) => {
                        let pct = brightness.percent;
                        this.model
                            .update(cx, |m, _| m.quick_status.brightness = brightness);
                        let _ = tx.send(Event::Osd {
                            kind: OsdKind::Brightness,
                            percent: pct,
                            muted: false,
                        });
                    }
                    Err(err) => {
                        this.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for QuickSettingsPopover {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let qs = m.quick_status.clone();
        let audio = &qs.audio;
        let wifi = &qs.wifi;
        let bt = &qs.bluetooth;
        let brightness = &qs.brightness;
        let confirm_power = self.confirm_power;
        let error = self.error.clone();

        let wifi_label = if !wifi.available {
            "Unavailable".to_string()
        } else if let Some(ssid) = &wifi.ssid {
            ssid.clone()
        } else if wifi.powered {
            "On".to_string()
        } else {
            "Off".to_string()
        };
        let bt_label = if !bt.available {
            "Unavailable"
        } else if bt.powered {
            "On"
        } else {
            "Off"
        };

        let vol_pct = audio.volume;
        let vol_muted = audio.muted;
        let vol_steps = ((vol_pct.min(100) as f32) / 5.0).round() as usize;
        let mut vol_track = div()
            .flex_1()
            .h(px(10.))
            .rounded_full()
            .overflow_hidden()
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(p.line))
            .flex()
            .items_center();
        for step in 1..=20usize {
            let target = (step * 5) as u32;
            let active = audio.available && !vol_muted && step <= vol_steps;
            vol_track = vol_track.child(
                div()
                    .id(("qs-vol-step", step))
                    .flex_1()
                    .h_full()
                    .bg(rgb(if active { p.accent } else { p.surface }))
                    .when(audio.available, |d| {
                        d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                            this.run_audio_action(
                                move |launcher| launcher.set_volume(target),
                                OsdKind::Volume,
                                cx,
                            );
                        }))
                    }),
            );
        }

        let mut panel = div()
            .id("quick-settings-panel")
            .w(px(344.))
            .p(px(14.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .rounded(px(12.))
            .bg(rgb(p.canvas))
            .border_1()
            .border_color(rgb(p.line))
            .text_color(rgb(p.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::BOLD)
                            .child("Quick Settings"),
                    )
                    .child(
                        div()
                            .id("qs-open-studio-settings")
                            .h(px(26.))
                            .px(px(8.))
                            .rounded(px(6.))
                            .bg(rgb(p.surface))
                            .border_1()
                            .border_color(rgb(p.line))
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.86))
                            .text_size(px(11.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(icon("settings", p.muted, 12.5))
                            .child("Studio Settings")
                            .on_click(cx.listener(|this, _, w, cx| {
                                let model = this.model.clone();
                                this.close(w, cx);
                                chrome::activate_pin(
                                    &model,
                                    "com.crowbaros.StudioSettings.desktop",
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child({
                        let wifi_avail = wifi.available;
                        let wifi_next = !wifi.powered;
                        let wifi_active = wifi_avail && wifi.powered;
                        div()
                            .id("qs-wifi-tile")
                            .flex_1()
                            .min_w_0()
                            .p(px(10.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(rgb(if wifi_active { p.accent } else { p.line }))
                            .bg(rgb(if wifi_active { p.accent } else { p.surface }))
                            .text_color(rgb(if wifi_active { p.on_accent } else { p.text }))
                            .when(!wifi_avail, |d| d.opacity(0.65))
                            .when(wifi_avail, |d| {
                                d.cursor_pointer().hover(|s| s.opacity(0.88))
                            })
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Wi-Fi"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(if wifi_active {
                                        p.on_accent
                                    } else {
                                        p.muted
                                    }))
                                    .text_ellipsis()
                                    .child(wifi_label),
                            )
                            .when(wifi_avail, |d| {
                                d.on_click(cx.listener(move |this, _, _, cx| {
                                    if this.busy {
                                        return;
                                    }
                                    this.busy = true;
                                    this.error = None;
                                    cx.notify();
                                    let launcher = this.model.read(cx).launcher_client.clone();
                                    let task = cx
                                        .background_executor()
                                        .spawn(async move { launcher.set_wifi_power(wifi_next) });
                                    cx.spawn(async move |this, cx| {
                                        let result = task.await;
                                        let _ = this.update(cx, |this, cx| {
                                            this.busy = false;
                                            match result {
                                                Ok(w) => this
                                                    .model
                                                    .update(cx, |m, _| m.quick_status.wifi = w),
                                                Err(e) => this.error = Some(e),
                                            }
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }))
                            })
                    })
                    .child({
                        let bt_avail = bt.available;
                        let bt_next = !bt.powered;
                        let bt_active = bt_avail && bt.powered;
                        div()
                            .id("qs-bt-tile")
                            .flex_1()
                            .min_w_0()
                            .p(px(10.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(rgb(if bt_active { p.accent } else { p.line }))
                            .bg(rgb(if bt_active { p.accent } else { p.surface }))
                            .text_color(rgb(if bt_active { p.on_accent } else { p.text }))
                            .when(!bt_avail, |d| d.opacity(0.65))
                            .when(bt_avail, |d| {
                                d.cursor_pointer().hover(|s| s.opacity(0.88))
                            })
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Bluetooth"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(if bt_active {
                                        p.on_accent
                                    } else {
                                        p.muted
                                    }))
                                    .child(bt_label),
                            )
                            .when(bt_avail, |d| {
                                d.on_click(cx.listener(move |this, _, _, cx| {
                                    if this.busy {
                                        return;
                                    }
                                    this.busy = true;
                                    this.error = None;
                                    cx.notify();
                                    let launcher = this.model.read(cx).launcher_client.clone();
                                    let task = cx.background_executor().spawn(async move {
                                        launcher.set_bluetooth_power(bt_next)
                                    });
                                    cx.spawn(async move |this, cx| {
                                        let result = task.await;
                                        let _ = this.update(cx, |this, cx| {
                                            this.busy = false;
                                            match result {
                                                Ok(b) => this.model.update(cx, |m, _| {
                                                    m.quick_status.bluetooth = b
                                                }),
                                                Err(e) => this.error = Some(e),
                                            }
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }))
                            })
                    }),
            )
            .child(
                div()
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if audio.available {
                                        audio.sink_name.clone()
                                    } else {
                                        "Audio unavailable".into()
                                    }),
                            )
                            .child(
                                div()
                                    .font_family("IBM Plex Mono")
                                    .text_size(px(11.))
                                    .text_color(rgb(p.muted))
                                    .child(if !audio.available {
                                        "—".into()
                                    } else if vol_muted {
                                        "Muted".into()
                                    } else {
                                        format!("{vol_pct}%")
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .id("qs-vol-down")
                                    .w(px(26.))
                                    .h(px(24.))
                                    .rounded(px(6.))
                                    .bg(rgb(p.canvas))
                                    .border_1()
                                    .border_color(rgb(p.line))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(audio.available, |d| {
                                        d.cursor_pointer().hover(|s| s.opacity(0.85)).on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.run_audio_action(
                                                    |launcher| launcher.step_volume(-5),
                                                    OsdKind::Volume,
                                                    cx,
                                                );
                                            }),
                                        )
                                    })
                                    .child("−"),
                            )
                            .child(vol_track)
                            .child(
                                div()
                                    .id("qs-vol-up")
                                    .w(px(26.))
                                    .h(px(24.))
                                    .rounded(px(6.))
                                    .bg(rgb(p.canvas))
                                    .border_1()
                                    .border_color(rgb(p.line))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(audio.available, |d| {
                                        d.cursor_pointer().hover(|s| s.opacity(0.85)).on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.run_audio_action(
                                                    |launcher| launcher.step_volume(5),
                                                    OsdKind::Volume,
                                                    cx,
                                                );
                                            }),
                                        )
                                    })
                                    .child("+"),
                            )
                            .child(
                                div()
                                    .id("qs-vol-mute")
                                    .h(px(24.))
                                    .px(px(8.))
                                    .rounded(px(6.))
                                    .bg(rgb(if vol_muted { p.accent } else { p.canvas }))
                                    .border_1()
                                    .border_color(rgb(if vol_muted { p.accent } else { p.line }))
                                    .text_color(rgb(if vol_muted { p.on_accent } else { p.text }))
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .flex()
                                    .items_center()
                                    .when(audio.available, |d| {
                                        d.cursor_pointer().hover(|s| s.opacity(0.85)).on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.run_audio_action(
                                                    |launcher| launcher.toggle_mute(),
                                                    OsdKind::Volume,
                                                    cx,
                                                );
                                            }),
                                        )
                                    })
                                    .child(if vol_muted { "Unmute" } else { "Mute" }),
                            ),
                    )
                    .child(
                        div()
                            .pt(px(4.))
                            .border_t_1()
                            .border_color(rgb(p.line))
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(rgb(p.muted))
                                    .child(if audio.mic_available {
                                        "Microphone"
                                    } else {
                                        "No microphone connected"
                                    }),
                            )
                            .when(audio.mic_available, |d| {
                                let mic_muted = audio.mic_muted;
                                d.child(
                                    div()
                                        .id("qs-mic-mute")
                                        .h(px(22.))
                                        .px(px(8.))
                                        .rounded(px(6.))
                                        .bg(rgb(if mic_muted { p.accent } else { p.canvas }))
                                        .border_1()
                                        .border_color(rgb(if mic_muted {
                                            p.accent
                                        } else {
                                            p.line
                                        }))
                                        .text_color(rgb(if mic_muted {
                                            p.on_accent
                                        } else {
                                            p.text
                                        }))
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .hover(|s| s.opacity(0.85))
                                        .child(if mic_muted { "Mic muted" } else { "Mute mic" })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.run_audio_action(
                                                |launcher| launcher.toggle_mic_mute(),
                                                OsdKind::Microphone,
                                                cx,
                                            );
                                        })),
                                )
                            }),
                    ),
            );

        if brightness.available {
            let b_pct = brightness.percent;
            let b_steps = ((b_pct.min(100) as f32) / 5.0).round() as usize;
            let mut b_track = div()
                .flex_1()
                .h(px(10.))
                .rounded_full()
                .overflow_hidden()
                .bg(rgb(p.canvas))
                .border_1()
                .border_color(rgb(p.line))
                .flex()
                .items_center();
            for step in 1..=20usize {
                let target = ((step * 5) as u32).max(5);
                let active = step <= b_steps;
                b_track = b_track.child(
                    div()
                        .id(("qs-bright-step", step))
                        .flex_1()
                        .h_full()
                        .bg(rgb(if active { p.accent } else { p.canvas }))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.run_brightness_action(target, cx);
                        })),
                );
            }
            panel = panel.child(
                div()
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Display brightness"),
                            )
                            .child(
                                div()
                                    .font_family("IBM Plex Mono")
                                    .text_size(px(11.))
                                    .text_color(rgb(p.muted))
                                    .child(format!("{b_pct}%")),
                            ),
                    )
                    .child(div().flex().items_center().gap(px(6.)).child(b_track)),
            );
        }

        if let Some(err) = error {
            panel = panel.child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(p.muted))
                    .child(err),
            );
        }

        if let Some(confirm) = confirm_power {
            panel = panel.child(
                div()
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.focus))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(confirm.prompt()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(
                                div()
                                    .id("qs-power-cancel")
                                    .h(px(26.))
                                    .px(px(8.))
                                    .rounded(px(6.))
                                    .bg(rgb(p.canvas))
                                    .border_1()
                                    .border_color(rgb(p.line))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_size(px(11.5))
                                    .child("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_power = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .id("qs-power-confirm")
                                    .h(px(26.))
                                    .px(px(10.))
                                    .rounded(px(6.))
                                    .bg(rgb(p.accent))
                                    .text_color(rgb(p.on_accent))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_size(px(11.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(confirm.confirm_label())
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        let action = confirm.action_name();
                                        let launcher = this.model.read(cx).launcher_client.clone();
                                        this.confirm_power = None;
                                        this.close(w, cx);
                                        std::thread::spawn(move || {
                                            if let Err(e) = launcher.power_action(action) {
                                                log::warn!(
                                                    "SHELL power action={action} failed: {e}"
                                                );
                                            }
                                        });
                                    })),
                            ),
                    ),
            );
        } else {
            let power_btn = |id: &'static str, label: &'static str| {
                div()
                    .id(id)
                    .flex_1()
                    .h(px(30.))
                    .rounded(px(7.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.85))
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(label)
            };
            panel = panel.child(
                div()
                    .flex()
                    .gap(px(6.))
                    .child(power_btn("qs-lock", "Lock").on_click(cx.listener(
                        |this, _, w, cx| {
                            let compositor = this.model.read(cx).compositor.clone();
                            this.close(w, cx);
                            compositor.lock(true);
                        },
                    )))
                    .child(power_btn("qs-suspend", "Suspend").on_click(cx.listener(
                        |this, _, w, cx| {
                            let launcher = this.model.read(cx).launcher_client.clone();
                            this.close(w, cx);
                            std::thread::spawn(move || {
                                if let Err(e) = launcher.power_action("suspend") {
                                    log::warn!("SHELL power action=suspend failed: {e}");
                                }
                            });
                        },
                    )))
                    .child(power_btn("qs-reboot", "Reboot").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.confirm_power = Some(PowerConfirm::Reboot);
                            cx.notify();
                        },
                    )))
                    .child(power_btn("qs-poweroff", "Power Off").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.confirm_power = Some(PowerConfirm::PowerOff);
                            cx.notify();
                        },
                    ))),
            );
        }

        div()
            .id("quick-settings-backdrop")
            .key_context("QuickSettings")
            .track_focus(&self.focus)
            .size_full()
            .pt(px(BAR_HEIGHT + 8.))
            .pr(px(12.))
            .flex()
            .items_start()
            .justify_end()
            .font_family("Manrope")
            .on_action(cx.listener(|this, _: &Close, w, cx| this.close(w, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, w, cx| this.close(w, cx)),
            )
            .child(panel)
    }
}

pub fn toggle(model: Entity<Model>, cx: &mut App) {
    if model.read(cx).is_quick_settings_open() {
        close_active_popover(&model, cx);
        return;
    }
    close_active_popover(&model, cx);
    log::info!("SHELL quick-settings open=true");
    let (tx, launcher) = {
        let m = model.read(cx);
        (m.tx.clone(), m.launcher_client.clone())
    };
    std::thread::spawn(move || {
        let status = launcher.quick_status();
        let _ = tx.send(Event::QuickStatus(status));
    });
    let model2 = model.clone();
    let result = cx.open_window(
        layer(
            "quick-settings",
            Layer::Overlay,
            Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            0.,
            0.,
            Some(-1.),
            KeyboardInteractivity::Exclusive,
        ),
        move |w, cx| {
            let focus = cx.focus_handle();
            w.focus(&focus);
            cx.new(|_| QuickSettingsPopover {
                model: model2,
                focus,
                confirm_power: None,
                busy: false,
                error: None,
            })
        },
    );
    match result {
        Ok(w) => {
            model.update(cx, |m, _| {
                m.active_popover = Some(ActivePopover::QuickSettings(w));
            });
            cx.refresh_windows();
        }
        Err(e) => log::error!("Opening quick settings: {e}"),
    }
}

pub struct OsdOverlay {
    model: Entity<Model>,
}

impl Render for OsdOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let Some(osd) = m.osd_state.clone() else {
            return div().size_full();
        };
        let fill_pct = if osd.muted { 0 } else { osd.percent.min(100) };
        let bar_w = 104.0f32;
        let fill_w = (bar_w * (fill_pct as f32 / 100.0)).round();

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .font_family("Manrope")
            .child(
                div()
                    .w(px(244.))
                    .h(px(40.))
                    .px(px(12.))
                    .rounded(px(10.))
                    .bg(rgb(p.canvas))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .w(px(66.))
                            .text_size(px(11.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(p.text))
                            .child(osd.kind.label(osd.muted)),
                    )
                    .child(
                        div()
                            .w(px(bar_w))
                            .h(px(7.))
                            .rounded_full()
                            .overflow_hidden()
                            .bg(rgb(p.surface))
                            .border_1()
                            .border_color(rgb(p.line))
                            .child(div().h_full().w(px(fill_w)).bg(rgb(p.accent))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .justify_end()
                            .font_family("IBM Plex Mono")
                            .text_size(px(11.))
                            .text_color(rgb(p.muted))
                            .child(osd.value_text()),
                    ),
            )
    }
}

pub fn close_osd(model: &Entity<Model>, cx: &mut App) {
    if let Some(w) = model.update(cx, |m, _| {
        m.osd_state = None;
        m.osd.take()
    }) {
        log::info!("SHELL osd show=false");
        let _ = w.update(cx, |_, w, _| w.remove_window());
    }
}

pub fn show_osd(model: &Entity<Model>, kind: OsdKind, percent: u32, muted: bool, cx: &mut App) {
    let seq = model.update(cx, |m, _| {
        m.osd_seq = m.osd_seq.wrapping_add(1);
        m.osd_state = Some(OsdState::new(kind, percent, muted, Instant::now(), m.osd_seq));
        m.osd_seq
    });
    log::info!(
        "SHELL osd show=true kind={} percent={} muted={}",
        kind.log_name(),
        percent,
        muted
    );
    if let Some(w) = model.read(cx).osd {
        let _ = w.update(cx, |_, _, cx| cx.notify());
    } else {
        let model2 = model.clone();
        match cx.open_window(
            layer_with_margin(
                "osd",
                Layer::Overlay,
                Anchor::BOTTOM,
                248.,
                44.,
                Some(-1.),
                Some((px(0.), px(0.), px(DOCK_HEIGHT + 20.), px(0.))),
                KeyboardInteractivity::None,
            ),
            move |_, cx| cx.new(|_| OsdOverlay { model: model2 }),
        ) {
            Ok(w) => model.update(cx, |m, _| m.osd = Some(w)),
            Err(e) => log::error!("Opening OSD overlay: {e}"),
        }
    }
    let dismiss_model = model.clone();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(OSD_DURATION).await;
        let _ = cx.update(|cx| {
            let should_close = dismiss_model
                .read(cx)
                .osd_state
                .as_ref()
                .is_some_and(|osd| osd.sequence == seq && osd.is_expired(Instant::now()));
            if should_close || dismiss_model.read(cx).osd_seq == seq {
                close_osd(&dismiss_model, cx);
            }
        });
    })
    .detach();
}

pub fn handle_media_key(model: &Entity<Model>, key: MediaKey, cx: &mut App) {
    let (tx, launcher) = {
        let m = model.read(cx);
        (m.tx.clone(), m.launcher_client.clone())
    };
    std::thread::spawn(move || match key {
        MediaKey::VolumeUp => {
            if let Ok(audio) = launcher.step_volume(5) {
                let _ = tx.send(Event::Osd {
                    kind: OsdKind::Volume,
                    percent: audio.volume,
                    muted: audio.muted,
                });
                let _ = tx.send(Event::QuickStatus(launcher.quick_status()));
            }
        }
        MediaKey::VolumeDown => {
            if let Ok(audio) = launcher.step_volume(-5) {
                let _ = tx.send(Event::Osd {
                    kind: OsdKind::Volume,
                    percent: audio.volume,
                    muted: audio.muted,
                });
                let _ = tx.send(Event::QuickStatus(launcher.quick_status()));
            }
        }
        MediaKey::VolumeMute => {
            if let Ok(audio) = launcher.toggle_mute() {
                let _ = tx.send(Event::Osd {
                    kind: OsdKind::Volume,
                    percent: audio.volume,
                    muted: audio.muted,
                });
                let _ = tx.send(Event::QuickStatus(launcher.quick_status()));
            }
        }
        MediaKey::MicMute => {
            if let Ok(audio) = launcher.toggle_mic_mute() {
                let _ = tx.send(Event::Osd {
                    kind: OsdKind::Microphone,
                    percent: audio.volume,
                    muted: audio.mic_muted,
                });
                let _ = tx.send(Event::QuickStatus(launcher.quick_status()));
            }
        }
        MediaKey::BrightnessUp => {
            if let Ok(b) = launcher.step_brightness(10) {
                let _ = tx.send(Event::Osd {
                    kind: OsdKind::Brightness,
                    percent: b.percent,
                    muted: false,
                });
                let _ = tx.send(Event::QuickStatus(launcher.quick_status()));
            }
        }
        MediaKey::BrightnessDown => {
            if let Ok(b) = launcher.step_brightness(-10) {
                let _ = tx.send(Event::Osd {
                    kind: OsdKind::Brightness,
                    percent: b.percent,
                    muted: false,
                });
                let _ = tx.send(Event::QuickStatus(launcher.quick_status()));
            }
        }
    });
}
