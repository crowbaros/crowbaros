// SPDX-License-Identifier: MIT
//! Lock screen view, Nightwatch layout geometry, person label discovery, and
//! lock-screen lifecycle.

use gpui::{layer_shell::*, prelude::*, *};

use crate::{
    compositor, quick_settings,
    ui::{Model, Unlock, asset, close_active_popover, icon, layer},
};

pub const LOCK_GROUND: u32 = 0x030b13;
pub const LOCK_VOID: u32 = 0x080d12;
const LOCK_ABYSS: u32 = 0x17212b;
const LOCK_STEEL: u32 = 0x6d8191;
const LOCK_LINE: u32 = 0x364957;
const LOCK_FROST: u32 = 0xd8e5ec;
const LOCK_MUTED: u32 = 0xa8bac7;
const LOCK_ERROR: u32 = 0xff8a80;
const NIGHTWATCH_RATIO: f32 = 1672.0 / 941.0;
const LOGO_RATIO: f32 = 180.0 / 850.0;
const MAX_VISIBLE_DOTS: usize = 22;

#[derive(Clone, Copy, Debug, PartialEq)]
struct LockLayout {
    left_pad: f32,
    top_pad: f32,
    bottom_pad: f32,
    clock_size: f32,
    date_size: f32,
    login_gap: f32,
    field_width: f32,
    logo_width: f32,
    logo_height: f32,
}

impl LockLayout {
    fn for_viewport(width: f32, height: f32) -> Self {
        let w = if width > 0. { width } else { 1600. };
        let h = if height > 0. { height } else { 900. };
        let left_pad = (w * 0.082).clamp(48., 156.);
        let top_pad = (h * 0.145).clamp(40., 160.);
        let bottom_pad = (h * 0.042).clamp(24., 48.);
        let clock_size = (h * 0.138).clamp(64., 140.);
        let date_size = (h * 0.025).clamp(16., 25.);
        let login_gap = (h * 0.115).clamp(36., 120.);
        let field_width = (w - left_pad * 2.0).min(370.0).max(260.0);
        let logo_width = (w * 0.145).clamp(186., 236.);
        Self {
            left_pad,
            top_pad,
            bottom_pad,
            clock_size,
            date_size,
            login_gap,
            field_width,
            logo_width,
            logo_height: logo_width * LOGO_RATIO,
        }
    }
}

/// Keeps Nightwatch at full viewport height anchored to the right edge so the
/// crow and slate perch stay unclipped on 4:3, 16:9, and ultrawide displays.
pub fn nightwatch_size(viewport_w: f32, viewport_h: f32) -> Option<(f32, f32)> {
    if viewport_w <= 0. || viewport_h <= 0. {
        return None;
    }
    Some((viewport_h * NIGHTWATCH_RATIO, viewport_h))
}

fn visible_password_dots(password: &str) -> usize {
    password.chars().count().min(MAX_VISIBLE_DOTS)
}

fn is_service_account(name: &str) -> bool {
    name.is_empty() || name == "root" || name == "nobody" || name.starts_with("crowbar-")
}

fn gecos_display_name(gecos: &str) -> Option<String> {
    let first = gecos.split(',').next()?.trim();
    (!first.is_empty()).then(|| first.to_string())
}

fn person_label_from(
    env_person: Option<&str>,
    env_user: Option<&str>,
    passwd: Option<&str>,
) -> String {
    if let Some(name) = env_person.map(str::trim).filter(|s| !s.is_empty()) {
        return name.to_string();
    }
    let entries: Vec<(&str, u32, &str)> = passwd
        .unwrap_or("")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let _pw = fields.next()?;
            let uid = fields.next()?.parse::<u32>().ok()?;
            let _gid = fields.next()?;
            let gecos = fields.next().unwrap_or("");
            Some((name, uid, gecos))
        })
        .collect();
    if let Some(user) = env_user.map(str::trim).filter(|u| !is_service_account(u)) {
        if let Some((_, _, gecos)) = entries.iter().find(|(n, _, _)| *n == user) {
            if let Some(display) = gecos_display_name(gecos) {
                return display;
            }
        }
        return user.to_string();
    }
    if let Some((name, _, gecos)) = entries
        .iter()
        .find(|(n, uid, _)| *uid >= 1000 && *uid < 65534 && !is_service_account(n))
    {
        return gecos_display_name(gecos).unwrap_or_else(|| (*name).to_string());
    }
    "Person".into()
}

pub fn person_label() -> String {
    let env_person = std::env::var("CROWBAR_PERSON").ok();
    let env_user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok();
    let passwd = std::fs::read_to_string("/etc/passwd").ok();
    person_label_from(
        env_person.as_deref(),
        env_user.as_deref(),
        passwd.as_deref(),
    )
}

/// The lock screen: shown while the compositor has the session locked, and
/// all it shows (ADR 0032).
pub struct LockScreen {
    model: Entity<Model>,
    focus: FocusHandle,
    password: String,
    error: Option<String>,
    busy: bool,
}

impl LockScreen {
    const MAX_PASSWORD: usize = 1024;

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let m = self.model.read(cx);
        if !m.unlock_client.unlocks_with_password() {
            m.compositor.lock(false);
            return;
        }
        if self.password.is_empty() {
            return;
        }
        let tx = m.tx.clone();
        let unlock = m.unlock_client.clone();
        self.busy = true;
        self.error = None;
        unlock.unlock_with_password(std::mem::take(&mut self.password), tx);
        cx.notify();
    }

    fn typed(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.busy || !self.model.read(cx).unlock_client.unlocks_with_password() {
            return;
        }
        let k = &event.keystroke;
        if k.modifiers.control || k.modifiers.alt || k.modifiers.platform || k.modifiers.function {
            return;
        }
        match k.key.as_str() {
            "backspace" => {
                self.password.pop();
            }
            "escape" => self.password.clear(),
            _ => match &k.key_char {
                Some(c)
                    if !c.chars().any(char::is_control)
                        && self.password.len() + c.len() <= Self::MAX_PASSWORD =>
                {
                    self.password.push_str(c)
                }
                _ => return,
            },
        }
        self.error = None;
        cx.notify();
    }

    pub fn answered(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = result.err();
        cx.notify();
    }
}

impl Render for LockScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let studio = m.preferences.studio;
        let accent = p.accent;
        let lock_time = m.lock_time.clone();
        let lock_date = m.lock_date.clone();
        let person = m.person.clone();
        let with_password = m.unlock_client.unlocks_with_password();
        let has_password = !self.password.is_empty();
        let dot_count = visible_password_dots(&self.password);
        let busy = self.busy;
        let error = self.error.clone();
        let vp = window.viewport_size();
        let vp_w: f32 = vp.width.into();
        let vp_h: f32 = vp.height.into();
        let layout = LockLayout::for_viewport(vp_w, vp_h);
        let background = match nightwatch_size(vp_w, vp_h) {
            Some((bg_w, bg_h)) => img(asset("nightwatch.png"))
                .absolute()
                .top_0()
                .right_0()
                .w(px(bg_w))
                .h(px(bg_h))
                .object_fit(ObjectFit::Fill),
            None => img(asset("nightwatch.png"))
                .size_full()
                .object_fit(ObjectFit::Cover),
        };
        let logo = ImageSource::Resource(Resource::Embedded(
            if studio {
                "logo-horizontal-frost"
            } else {
                "logo-horizontal-primary"
            }
            .into(),
        ));
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            .key_context("Lock")
            .on_action(cx.listener(|this, _: &Unlock, _, cx| this.submit(cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.typed(event, cx)))
            .bg(rgb(LOCK_GROUND))
            .text_color(rgb(LOCK_FROST))
            .font_family("Manrope")
            .child(div().absolute().inset_0().overflow_hidden().child(background))
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .pl(px(layout.left_pad))
                    .pr(px(layout.left_pad))
                    .pt(px(layout.top_pad))
                    .pb(px(layout.bottom_pad))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_size(px(layout.clock_size))
                                            .font_weight(FontWeight::NORMAL)
                                            .line_height(relative(1.0))
                                            .text_color(rgb(LOCK_FROST))
                                            .child(lock_time),
                                    )
                                    .child(
                                        div()
                                            .mt(px(16.))
                                            .text_size(px(layout.date_size))
                                            .text_color(rgb(LOCK_MUTED))
                                            .child(lock_date),
                                    ),
                            )
                            .child(
                                div()
                                    .mt(px(layout.login_gap))
                                    .w(px(layout.field_width))
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(15.))
                                            .mb(px(22.))
                                            .child(
                                                div()
                                                    .size(px(44.))
                                                    .rounded_full()
                                                    .bg(rgb(LOCK_ABYSS))
                                                    .border_1()
                                                    .border_color(rgb(LOCK_LINE))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .child(icon("user", LOCK_FROST, 20.)),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(20.))
                                                    .text_color(rgb(LOCK_FROST))
                                                    .child(person),
                                            ),
                                    )
                                    .when(with_password, |d| {
                                        let border = if error.is_some() {
                                            LOCK_ERROR
                                        } else if busy {
                                            LOCK_LINE
                                        } else {
                                            accent
                                        };
                                        let can_submit = has_password && !busy;
                                        d.child(
                                            div()
                                                .id("password")
                                                .w_full()
                                                .h(px(52.))
                                                .pl(px(17.))
                                                .pr(px(8.))
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap(px(10.))
                                                .rounded(px(10.))
                                                .bg(rgb(LOCK_VOID))
                                                .border_1()
                                                .border_color(rgb(border))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .overflow_hidden()
                                                        .flex()
                                                        .items_center()
                                                        .when(!has_password, |field| {
                                                            field
                                                                .when(!busy, |f| {
                                                                    f.child(
                                                                        div()
                                                                            .w(px(2.))
                                                                            .h(px(19.))
                                                                            .mr(px(6.))
                                                                            .rounded(px(1.))
                                                                            .bg(rgb(accent)),
                                                                    )
                                                                })
                                                                .child(
                                                                    div()
                                                                        .text_size(px(14.))
                                                                        .text_color(rgb(LOCK_MUTED))
                                                                        .child(if busy {
                                                                            "Checking…"
                                                                        } else {
                                                                            "Password"
                                                                        }),
                                                                )
                                                        })
                                                        .when(has_password, |field| {
                                                            field
                                                                .gap(px(6.))
                                                                .children((0..dot_count).map(|_| {
                                                                    div()
                                                                        .size(px(7.))
                                                                        .rounded_full()
                                                                        .bg(rgb(LOCK_FROST))
                                                                }))
                                                                .when(!busy, |f| {
                                                                    f.child(
                                                                        div()
                                                                            .w(px(2.))
                                                                            .h(px(19.))
                                                                            .ml(px(2.))
                                                                            .rounded(px(1.))
                                                                            .bg(rgb(accent)),
                                                                    )
                                                                })
                                                        }),
                                                )
                                                .child(
                                                    div()
                                                        .id("unlock")
                                                        .size(px(36.))
                                                        .rounded(px(8.))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .when(can_submit, |btn| {
                                                            btn.bg(rgb(accent))
                                                                .cursor_pointer()
                                                                .hover(|s| s.opacity(0.85))
                                                                .child(icon(
                                                                    "arrow",
                                                                    p.on_accent,
                                                                    18.,
                                                                ))
                                                        })
                                                        .when(!can_submit, |btn| {
                                                            btn.child(icon(
                                                                "arrow", LOCK_STEEL, 18.,
                                                            ))
                                                        })
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.submit(cx)
                                                        })),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .mt(px(14.))
                                                .min_h(px(20.))
                                                .flex()
                                                .items_center()
                                                .gap(px(8.))
                                                .when_some(error.clone(), |row, message| {
                                                    row.child(
                                                        div()
                                                            .size(px(6.))
                                                            .rounded_full()
                                                            .bg(rgb(LOCK_ERROR)),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(13.))
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .text_color(rgb(LOCK_ERROR))
                                                            .child(message),
                                                    )
                                                })
                                                .when(error.is_none() && busy, |row| {
                                                    row.child(
                                                        div()
                                                            .size(px(6.))
                                                            .rounded_full()
                                                            .bg(rgb(accent)),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(12.))
                                                            .text_color(rgb(LOCK_MUTED))
                                                            .child("Checking password…"),
                                                    )
                                                })
                                                .when(error.is_none() && !busy, |row| {
                                                    row.child(
                                                        div()
                                                            .text_size(px(12.))
                                                            .text_color(rgb(LOCK_MUTED))
                                                            .child("Sign in to your workspace"),
                                                    )
                                                }),
                                        )
                                    })
                                    .when(!with_password, |d| {
                                        d.child(
                                            div()
                                                .id("unlock")
                                                .w_full()
                                                .h(px(48.))
                                                .px(px(18.))
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .rounded(px(10.))
                                                .bg(rgb(accent))
                                                .text_color(rgb(p.on_accent))
                                                .cursor_pointer()
                                                .hover(|s| s.opacity(0.85))
                                                .child(
                                                    div()
                                                        .text_size(px(14.))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child("Unlock"),
                                                )
                                                .child(icon("arrow", p.on_accent, 18.))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.submit(cx)
                                                })),
                                        )
                                        .child(
                                            div()
                                                .mt(px(14.))
                                                .text_size(px(12.))
                                                .text_color(rgb(LOCK_MUTED))
                                                .child("Press Enter to unlock"),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div().child(
                            img(logo)
                                .w(px(layout.logo_width))
                                .h(px(layout.logo_height))
                                .object_fit(ObjectFit::Contain),
                        ),
                    ),
            )
    }
}

/// Whether a snapshot puts the lock screen up or takes it down.
pub fn lock_screen_changes(snapshot: &compositor::Snapshot, showing: bool) -> bool {
    snapshot.connected && snapshot.locked != showing
}

/// Shows the lock screen over every display, or takes it down.
pub fn set_lock_screen(model: &Entity<Model>, locked: bool, cx: &mut App) {
    if !locked {
        if let Some(w) = model.update(cx, |m, _| m.lock.take()) {
            log::info!("SHELL lock open=false");
            let _ = w.update(cx, |_, w, _| w.remove_window());
        }
        return;
    }
    if model.read(cx).lock.is_some() {
        return;
    }
    close_active_popover(model, cx);
    quick_settings::close_osd(model, cx);
    log::info!("SHELL lock open=true");
    let screen = model.clone();
    let result = cx.open_window(
        layer(
            "lock",
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
            cx.new(|_| LockScreen {
                model: screen,
                focus,
                password: String::new(),
                error: None,
                busy: false,
            })
        },
    );
    match result {
        Ok(w) => model.update(cx, |m, _| m.lock = Some(w)),
        Err(e) => log::error!("Opening the lock screen: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LockLayout, MAX_VISIBLE_DOTS, lock_screen_changes, nightwatch_size, person_label_from,
        visible_password_dots,
    };
    use crate::compositor;
    use crate::ui::Assets;
    use gpui::AssetSource;

    #[test]
    fn a_snapshot_that_did_not_reach_the_compositor_leaves_the_lock_screen_alone() {
        let failed = compositor::Snapshot::default();
        assert!(!lock_screen_changes(&failed, true));
        assert!(!lock_screen_changes(&failed, false));

        let unlocked = compositor::Snapshot {
            connected: true,
            ..Default::default()
        };
        assert!(lock_screen_changes(&unlocked, true));
        let locked = compositor::Snapshot {
            connected: true,
            locked: true,
            ..Default::default()
        };
        assert!(lock_screen_changes(&locked, false));
        assert!(!lock_screen_changes(&locked, true));
    }

    #[test]
    fn lock_layout_matches_brand_proportions_at_1600x900() {
        let layout = LockLayout::for_viewport(1600., 900.);
        assert!((layout.left_pad - 131.2).abs() < 1.0);
        assert!((layout.clock_size - 124.2).abs() < 1.0);
        assert_eq!(layout.field_width, 370.0);
        assert!((layout.logo_width - 232.0).abs() < 1.0);
        assert!((layout.logo_height - 232.0 * (180.0 / 850.0)).abs() < 0.1);
    }

    #[test]
    fn lock_layout_keeps_login_clear_of_crow_on_1280x960() {
        let layout = LockLayout::for_viewport(1280., 960.);
        let (bg_w, bg_h) = nightwatch_size(1280., 960.).unwrap();
        assert_eq!(bg_h, 960.0);
        assert!(bg_w > 1280.0);
        let crow_left_x = 1280.0 - bg_w * (709.0 / 1672.0);
        assert!(
            layout.left_pad + layout.field_width + 48.0 < crow_left_x,
            "login right {} too close to crow left {}",
            layout.left_pad + layout.field_width,
            crow_left_x
        );
    }

    #[test]
    fn nightwatch_size_handles_zero_viewport() {
        assert_eq!(nightwatch_size(0., 900.), None);
        assert_eq!(nightwatch_size(1600., 0.), None);
    }

    #[test]
    fn person_label_prefers_explicit_env_then_human_account_then_fallback() {
        let passwd = concat!(
            "root:x:0:0:root:/root:/bin/sh\n",
            "crowbar-compositor:x:1000:1000::/home/crowbar-compositor:/bin/sh\n",
            "crowbar-shell:x:1001:1001::/home/crowbar-shell:/bin/sh\n",
        );
        assert_eq!(
            person_label_from(Some("Alex"), Some("crowbar-shell"), Some(passwd)),
            "Alex"
        );
        assert_eq!(
            person_label_from(None, Some("crowbar-shell"), Some(passwd)),
            "Person"
        );
        let with_lab = format!("{passwd}crowbar:x:1003:1003::/home/crowbar:/bin/bash\n");
        assert_eq!(
            person_label_from(None, Some("crowbar-shell"), Some(&with_lab)),
            "crowbar"
        );
        let with_human =
            format!("{passwd}jason:x:1003:1003:Jason Alexander,,,:/home/jason:/bin/bash\n");
        assert_eq!(
            person_label_from(None, Some("crowbar-shell"), Some(&with_human)),
            "Jason Alexander"
        );
        assert_eq!(
            person_label_from(None, Some("jason"), Some(&with_human)),
            "Jason Alexander"
        );
    }

    #[test]
    fn password_dots_cap_at_field_capacity() {
        assert_eq!(visible_password_dots(""), 0);
        assert_eq!(visible_password_dots("crowbar"), 7);
        assert_eq!(visible_password_dots(&"a".repeat(64)), MAX_VISIBLE_DOTS);
    }

    #[test]
    fn embedded_lock_assets_load() {
        let assets = Assets;
        for name in [
            "user",
            "mark-micro",
            "mark-dark",
            "mark-frost",
            "logo-horizontal-primary",
            "logo-horizontal-frost",
        ] {
            let bytes = assets.load(name).unwrap();
            assert!(bytes.is_some_and(|b| !b.is_empty()), "missing asset {name}");
        }
    }
}
