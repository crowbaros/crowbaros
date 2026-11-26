// SPDX-License-Identifier: MIT
mod bluetooth;
mod default_apps;
mod displays;
mod input_config;
mod network;
mod power_config;
mod session;
mod settings;
mod sound;
mod system_info;
mod theme;

use bluetooth::{BluetoothAdapter, BluetoothDevice, BluetoothError, BluetoothState};
use crowbar_theme::{ActiveTheme, ThemeStore};
use crowbar_ui::{
    ButtonVariant, FONT_MONO, FONT_UI, SIDEBAR_WIDTH, TEXT_BODY, TEXT_LABEL, TEXT_SECTION,
    TEXT_SELECTION_LABEL, TEXT_SUPPORTING, TEXT_TECHNICAL, TEXT_TITLE, action_chip, badge,
    button as ui_button, card, divider, load_fonts, section_header, sidebar, spawn_theme_watcher,
    status_footer, titlebar,
};
use default_apps::{AppCategory, CategoryState};
use displays::{
    DisplayError, DisplayMode, OutputDraft, OutputInfo, RelativeSide, SCALES, arrange_relative,
    dedup_sorted_modes, format_refresh, format_scale, logical_size, project_diagram,
};
use gpui::{
    App, Application, Bounds, Context, Div, FocusHandle, FontWeight, KeyBinding, KeyDownEvent,
    ObjectFit, Stateful, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, img,
    prelude::*, px, rgb, size,
};
use input_config::{InputSettings, LAYOUTS, POINTER_SPEEDS, REPEAT_DELAYS, REPEAT_RATES};
use network::{NetworkError, NetworkSnapshot, WifiNetwork, WifiState, WifiStation};
use power_config::{LOCK_TIMEOUTS, PowerSettings, SUSPEND_TIMEOUTS, format_timeout};
use settings::{Preferences, SaveOutcome, Store, Theme, Wallpaper};
use sound::{AudioEndpoint, SoundError, SoundSnapshot};
use std::path::PathBuf;
use std::sync::mpsc;
use system_info::SystemSummary;
use theme::Palette;

actions!(
    studio,
    [Next, Previous, Save, Undo, Reload, Quit, CancelClose]
);

const CONTROL_APPEARANCE: usize = 0;
const CONTROL_ABOUT: usize = 1;
const CONTROL_THEME_FIRST: usize = 2;
const CONTROL_WALLPAPER_FIRST: usize = 5;
const CONTROL_STUDIO: usize = 8;
const CONTROL_REDUCED_MOTION: usize = 9;
const CONTROL_UNDO: usize = 10;
const CONTROL_SAVE: usize = 11;
const CONTROL_RELOAD: usize = 12;
const CONTROL_CLOSE: usize = 13;
const CONTROL_KEEP_EDITING: usize = 14;
const CONTROL_DISCARD_CLOSE: usize = 15;
const CONTROL_DISPLAYS: usize = 16;
const CONTROL_DISPLAY_REFRESH: usize = 17;
const CONTROL_NETWORK: usize = 18;
const CONTROL_SOUND: usize = 19;
const CONTROL_BLUETOOTH: usize = 20;
const CONTROL_NETWORK_REFRESH: usize = 21;
const CONTROL_WIFI_SCAN: usize = 22;
const CONTROL_WIFI_PASSPHRASE: usize = 23;
const CONTROL_SOUND_REFRESH: usize = 24;
const CONTROL_BLUETOOTH_REFRESH: usize = 25;
const CONTROL_BLUETOOTH_POWER: usize = 26;
const CONTROL_BLUETOOTH_SCAN: usize = 27;
const CONTROL_FOOTER_REFRESH: usize = 28;
const CONTROL_INPUT: usize = 29;
const CONTROL_POWER: usize = 30;
const CONTROL_DEFAULT_APPS: usize = 31;
const CONTROL_INPUT_REFRESH: usize = 32;
const CONTROL_POWER_REFRESH: usize = 33;
const CONTROL_DEFAULTS_REFRESH: usize = 34;
const CONTROL_SYSTEM_REFRESH: usize = 35;
const CONTROL_NATURAL_SCROLL: usize = 36;
const CONTROL_COUNT: usize = 37;

const MAX_WIFI_PASSPHRASE: usize = 63;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Appearance,
    Displays,
    Network,
    Sound,
    Bluetooth,
    Input,
    Power,
    DefaultApps,
    About,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WifiPrompt {
    network_path: String,
    ssid: String,
}

fn wallpaper_path(wallpaper: Wallpaper) -> Option<PathBuf> {
    let installed = PathBuf::from("/usr/share/crowbaros-shell").join(wallpaper.asset_name());
    if installed.is_file() {
        return Some(installed);
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let shell_asset = manifest_dir
        .join("../shell/assets")
        .join(wallpaper.asset_name());
    if shell_asset.is_file() {
        return Some(shell_asset);
    }
    let brand_kit_name = match wallpaper {
        Wallpaper::SignalFeather => "signal-feather.png",
        Wallpaper::Nightwatch => "nightwatch.png",
        Wallpaper::QuietSignal => "quiet-signal-4k.png",
    };
    let brand_kit = manifest_dir
        .join("../../docs/branding/crowbaros-brand-kit/wallpapers")
        .join(brand_kit_name);
    if brand_kit.is_file() {
        return Some(brand_kit);
    }
    None
}

struct Settings {
    root: FocusHandle,
    controls: Vec<FocusHandle>,
    wallpaper_paths: [Option<PathBuf>; 3],
    store: Store,
    saved: Option<Preferences>,
    draft: Preferences,
    builtin_wallpaper_selected: bool,
    live_theme: ActiveTheme,
    previewing_appearance: bool,
    load_error: bool,
    error: Option<String>,
    busy: bool,
    page: Page,
    status: String,
    confirming_close: bool,
    outputs: Vec<OutputInfo>,
    display_drafts: Vec<OutputDraft>,
    selected_display: usize,
    displays_loaded: bool,
    displays_loading: bool,
    displays_unavailable: Option<String>,
    display_error: Option<String>,
    display_status: String,
    network: Option<NetworkSnapshot>,
    network_loaded: bool,
    network_loading: bool,
    network_busy: bool,
    network_error: Option<String>,
    network_status: String,
    wifi_prompt: Option<WifiPrompt>,
    wifi_passphrase: String,
    wifi_show_passphrase: bool,
    sound: Option<SoundSnapshot>,
    sound_loaded: bool,
    sound_loading: bool,
    sound_busy: bool,
    sound_unavailable: Option<String>,
    sound_error: Option<String>,
    sound_status: String,
    sound_watcher_started: bool,
    bluetooth: Option<BluetoothState>,
    bluetooth_loaded: bool,
    bluetooth_loading: bool,
    bluetooth_busy: bool,
    bluetooth_error: Option<String>,
    bluetooth_status: String,
    input: InputSettings,
    input_live: bool,
    input_loaded: bool,
    input_loading: bool,
    input_busy: bool,
    input_error: Option<String>,
    input_status: String,
    power: PowerSettings,
    power_live: bool,
    power_loaded: bool,
    power_loading: bool,
    power_busy: bool,
    power_error: Option<String>,
    power_status: String,
    default_apps: Vec<CategoryState>,
    defaults_loaded: bool,
    defaults_loading: bool,
    defaults_busy: bool,
    defaults_error: Option<String>,
    defaults_status: String,
    system_info: Option<SystemSummary>,
    system_loaded: bool,
    system_loading: bool,
}

impl Settings {
    fn new(store: Store, window: &mut Window, cx: &mut Context<Self>) -> Self {
        window.set_window_title("Studio Settings");
        let controls: Vec<_> = (0..CONTROL_COUNT as isize)
            .map(|i| cx.focus_handle().tab_index(i).tab_stop(true))
            .collect();
        window.focus(&controls[CONTROL_THEME_FIRST]);
        let loaded = store.load();
        let error = loaded.as_ref().err().cloned();
        let saved = loaded.unwrap_or_default();
        let draft = saved.clone().unwrap_or_default();
        let live_theme = ThemeStore::for_user()
            .map(|theme_store| theme_store.load_active())
            .unwrap_or_else(|_| theme::fallback_active_theme(&draft));
        spawn_theme_watcher(cx, |this, active, cx| {
            this.live_theme = active;
            if !this.previewing_appearance
                && !this.has_unsaved_edits()
                && !this.busy
                && let Ok(reloaded) = this.store.load()
            {
                this.saved = reloaded;
                this.draft = this.saved.clone().unwrap_or_default();
                this.load_error = false;
                this.error = None;
            }
            cx.notify();
        });
        log::info!(
            "STUDIO event=opened theme={:?} studio={} reduced_motion={} wallpaper={:?}",
            draft.theme,
            draft.studio,
            draft.reduced_motion,
            draft.wallpaper
        );
        Self {
            root: cx.focus_handle(),
            controls,
            wallpaper_paths: Wallpaper::ALL.map(wallpaper_path),
            store,
            saved,
            draft,
            builtin_wallpaper_selected: false,
            live_theme,
            previewing_appearance: false,
            load_error: error.is_some(),
            error,
            busy: false,
            page: Page::Appearance,
            status: "".into(),
            confirming_close: false,
            outputs: Vec::new(),
            display_drafts: Vec::new(),
            selected_display: 0,
            displays_loaded: false,
            displays_loading: false,
            displays_unavailable: None,
            display_error: None,
            display_status: "".into(),
            network: None,
            network_loaded: false,
            network_loading: false,
            network_busy: false,
            network_error: None,
            network_status: "".into(),
            wifi_prompt: None,
            wifi_passphrase: String::new(),
            wifi_show_passphrase: false,
            sound: None,
            sound_loaded: false,
            sound_loading: false,
            sound_busy: false,
            sound_unavailable: None,
            sound_error: None,
            sound_status: "".into(),
            sound_watcher_started: false,
            bluetooth: None,
            bluetooth_loaded: false,
            bluetooth_loading: false,
            bluetooth_busy: false,
            bluetooth_error: None,
            bluetooth_status: "".into(),
            input: InputSettings::default(),
            input_live: false,
            input_loaded: false,
            input_loading: false,
            input_busy: false,
            input_error: None,
            input_status: "".into(),
            power: PowerSettings::default(),
            power_live: false,
            power_loaded: false,
            power_loading: false,
            power_busy: false,
            power_error: None,
            power_status: "".into(),
            default_apps: Vec::new(),
            defaults_loaded: false,
            defaults_loading: false,
            defaults_busy: false,
            defaults_error: None,
            defaults_status: "".into(),
            system_info: None,
            system_loaded: false,
            system_loading: false,
        }
    }
    fn active_palette(&self) -> Palette {
        theme::resolve_palette(&self.live_theme, &self.draft, self.previewing_appearance)
    }
    fn appearance_dirty(&self) -> bool {
        self.builtin_wallpaper_selected || self.saved.as_ref() != Some(&self.draft)
    }
    fn displays_dirty(&self) -> bool {
        self.outputs.len() == self.display_drafts.len()
            && self
                .outputs
                .iter()
                .zip(&self.display_drafts)
                .any(|(live, draft)| !draft.matches_output(live))
    }
    fn has_unsaved_edits(&self) -> bool {
        self.builtin_wallpaper_selected
            || (self.appearance_dirty() && self.draft != self.saved.clone().unwrap_or_default())
            || self.displays_dirty()
    }
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.previewing_appearance = true;
        self.status.clear();
        self.confirming_close = false;
        if !self.load_error {
            self.error = None;
        }
        log::info!(
            "STUDIO event=preview theme={:?} studio={} reduced_motion={} wallpaper={:?}",
            self.draft.theme,
            self.draft.studio,
            self.draft.reduced_motion,
            self.draft.wallpaper
        );
        cx.notify();
    }
    fn display_changed(&mut self, cx: &mut Context<Self>) {
        self.display_status.clear();
        self.display_error = None;
        self.confirming_close = false;
        cx.notify();
    }
    fn refresh_displays(&mut self, cx: &mut Context<Self>) {
        if self.displays_loading || self.busy {
            return;
        }
        self.displays_loading = true;
        self.display_error = None;
        cx.notify();
        let previous_name = self
            .outputs
            .get(self.selected_display)
            .map(|o| o.name.clone());
        let task = cx.background_executor().spawn(async move { displays::fetch_outputs() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.displays_loading = false;
                this.displays_loaded = true;
                match result {
                    Ok(outputs) => {
                        let next_selected = previous_name
                            .as_deref()
                            .and_then(|name| outputs.iter().position(|o| o.name == name))
                            .unwrap_or(0);
                        this.display_drafts = outputs.iter().map(OutputDraft::from_output).collect();
                        this.outputs = outputs;
                        this.selected_display = next_selected;
                        this.displays_unavailable = None;
                        this.display_error = None;
                        log::info!(
                            "STUDIO event=displays-loaded count={}",
                            this.outputs.len()
                        );
                    }
                    Err(DisplayError::Unavailable(message)) => {
                        this.outputs.clear();
                        this.display_drafts.clear();
                        this.selected_display = 0;
                        this.displays_unavailable = Some(message);
                        this.display_error = None;
                    }
                    Err(DisplayError::Failed(message)) => {
                        log::warn!("STUDIO event=displays-load-failed {message}");
                        this.displays_unavailable = None;
                        this.display_error = Some(message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn apply_displays(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.displays_loading || !self.displays_dirty() {
            return;
        }
        self.busy = true;
        self.display_error = None;
        self.display_status = "Applying…".into();
        cx.notify();
        let live = self.outputs.clone();
        let drafts = self.display_drafts.clone();
        let previous_name = self
            .outputs
            .get(self.selected_display)
            .map(|o| o.name.clone());
        let task = cx.background_executor().spawn(async move {
            let applied = displays::apply_changes(&live, &drafts)?;
            let refreshed = displays::fetch_outputs()?;
            Ok::<_, DisplayError>((applied, refreshed))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok((applied, refreshed)) => {
                        let next_selected = previous_name
                            .as_deref()
                            .and_then(|name| refreshed.iter().position(|o| o.name == name))
                            .unwrap_or(0);
                        this.display_drafts =
                            refreshed.iter().map(OutputDraft::from_output).collect();
                        this.outputs = refreshed;
                        this.selected_display = next_selected;
                        this.displays_unavailable = None;
                        this.display_error = None;
                        this.display_status = "Display settings applied.".into();
                        this.confirming_close = false;
                        log::info!("STUDIO event=display-applied outputs={}", applied.join(","));
                    }
                    Err(error) => {
                        log::warn!("STUDIO event=display-apply-failed {}", error.message());
                        this.display_error = Some(error.message().to_string());
                        this.display_status = "Display changes not applied.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn clear_wifi_prompt(&mut self) {
        self.wifi_prompt = None;
        self.wifi_passphrase.clear();
        self.wifi_show_passphrase = false;
    }

    fn refresh_network(&mut self, cx: &mut Context<Self>) {
        if self.network_loading || self.network_busy {
            return;
        }
        self.network_loading = true;
        self.network_error = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move { network::fetch_network() });
        cx.spawn(async move |this, cx| {
            let snapshot = task.await;
            let _ = this.update(cx, |this, cx| {
                this.network_loading = false;
                this.network_loaded = true;
                let wifi_label = match &snapshot.wifi {
                    WifiState::Available(_) => "available",
                    WifiState::Unavailable(_) => "unavailable",
                };
                log::info!(
                    "STUDIO event=network-loaded ethernet={} wifi={wifi_label}",
                    snapshot.ethernet.len()
                );
                this.network = Some(snapshot);
                this.network_error = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn trigger_wifi_scan(&mut self, device_path: String, cx: &mut Context<Self>) {
        if self.network_busy {
            return;
        }
        self.network_busy = true;
        self.network_error = None;
        self.network_status = "Scanning for Wi-Fi networks…".into();
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            network::scan_wifi(&device_path)?;
            std::thread::sleep(std::time::Duration::from_millis(600));
            Ok::<_, NetworkError>(network::fetch_network())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.network_busy = false;
                match result {
                    Ok(snapshot) => {
                        this.network = Some(snapshot);
                        this.network_status = "Wi-Fi scan updated.".into();
                        this.network_error = None;
                        log::info!("STUDIO event=wifi-scanned");
                    }
                    Err(err) => {
                        log::warn!("STUDIO event=wifi-scan-failed {}", err.message());
                        this.network_error = Some(err.message().to_string());
                        this.network_status = "Wi-Fi scan failed.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn connect_wifi_network(
        &mut self,
        network_path: String,
        ssid: String,
        passphrase: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.network_busy {
            return;
        }
        self.network_busy = true;
        self.network_error = None;
        self.network_status = format!("Connecting to {ssid}…");
        self.clear_wifi_prompt();
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            network::connect_wifi(&network_path, passphrase)?;
            Ok::<_, NetworkError>(network::fetch_network())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.network_busy = false;
                match result {
                    Ok(snapshot) => {
                        this.network = Some(snapshot);
                        this.network_status = format!("Connected to {ssid}.");
                        this.network_error = None;
                        log::info!("STUDIO event=wifi-connected");
                    }
                    Err(err) => {
                        log::warn!("STUDIO event=wifi-connect-failed {}", err.message());
                        this.network_error = Some(err.message().to_string());
                        this.network_status = "Wi-Fi connection failed.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn disconnect_wifi_station(&mut self, device_path: String, cx: &mut Context<Self>) {
        if self.network_busy {
            return;
        }
        self.network_busy = true;
        self.network_error = None;
        self.network_status = "Disconnecting Wi-Fi…".into();
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            network::disconnect_wifi(&device_path)?;
            Ok::<_, NetworkError>(network::fetch_network())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.network_busy = false;
                match result {
                    Ok(snapshot) => {
                        this.network = Some(snapshot);
                        this.network_status = "Wi-Fi disconnected.".into();
                        this.network_error = None;
                        log::info!("STUDIO event=wifi-disconnected");
                    }
                    Err(err) => {
                        log::warn!("STUDIO event=wifi-disconnect-failed {}", err.message());
                        this.network_error = Some(err.message().to_string());
                        this.network_status = "Could not disconnect Wi-Fi.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn forget_wifi_network(&mut self, known_path: String, ssid: String, cx: &mut Context<Self>) {
        if self.network_busy {
            return;
        }
        self.network_busy = true;
        self.network_error = None;
        self.network_status = format!("Forgetting {ssid}…");
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            network::forget_wifi(&known_path)?;
            Ok::<_, NetworkError>(network::fetch_network())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.network_busy = false;
                match result {
                    Ok(snapshot) => {
                        this.network = Some(snapshot);
                        this.network_status = format!("Forgot {ssid}.");
                        this.network_error = None;
                        log::info!("STUDIO event=wifi-forgotten");
                    }
                    Err(err) => {
                        log::warn!("STUDIO event=wifi-forget-failed {}", err.message());
                        this.network_error = Some(err.message().to_string());
                        this.network_status = "Could not forget network.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn wifi_passphrase_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.network_busy || self.wifi_prompt.is_none() {
            return;
        }
        let k = &event.keystroke;
        if k.modifiers.control || k.modifiers.alt || k.modifiers.platform || k.modifiers.function {
            return;
        }
        match k.key.as_str() {
            "backspace" => {
                self.wifi_passphrase.pop();
                cx.notify();
            }
            "escape" => {
                self.clear_wifi_prompt();
                cx.notify();
            }
            "enter" => {
                if !self.wifi_passphrase.is_empty()
                    && let Some(prompt) = self.wifi_prompt.clone()
                {
                    let secret = std::mem::take(&mut self.wifi_passphrase);
                    self.connect_wifi_network(prompt.network_path, prompt.ssid, Some(secret), cx);
                }
            }
            _ => {
                if let Some(ch) = &k.key_char
                    && !ch.chars().any(char::is_control)
                    && self.wifi_passphrase.len() + ch.len() <= MAX_WIFI_PASSPHRASE
                {
                    self.wifi_passphrase.push_str(ch);
                    cx.notify();
                }
            }
        }
    }

    fn start_sound_watcher(&mut self, cx: &mut Context<Self>) {
        if self.sound_watcher_started {
            return;
        }
        self.sound_watcher_started = true;
        let (tx, rx) = mpsc::channel();
        sound::spawn_sound_watcher(tx);
        cx.spawn(async move |this, cx| {
            let mut receiver = rx;
            loop {
                let next_task = cx.background_executor().spawn(async move {
                    let received = receiver.recv().ok();
                    (received, receiver)
                });
                let (received, back) = next_task.await;
                receiver = back;
                let Some(snapshot) = received else {
                    break;
                };
                let updated = this.update(cx, |this, cx| {
                    if this.sound.as_ref() != Some(&snapshot) {
                        log::info!(
                            "STUDIO event=sound-synced sinks={} sources={}",
                            snapshot.sinks.len(),
                            snapshot.sources.len()
                        );
                        this.sound = Some(snapshot);
                        this.sound_unavailable = None;
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn refresh_sound(&mut self, cx: &mut Context<Self>) {
        if self.sound_loading || self.sound_busy {
            return;
        }
        self.sound_loading = true;
        self.sound_error = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move { sound::fetch_sound() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.sound_loading = false;
                this.sound_loaded = true;
                match result {
                    Ok(snapshot) => {
                        log::info!(
                            "STUDIO event=sound-loaded sinks={} sources={}",
                            snapshot.sinks.len(),
                            snapshot.sources.len()
                        );
                        this.sound = Some(snapshot);
                        this.sound_unavailable = None;
                        this.sound_error = None;
                    }
                    Err(SoundError::Unavailable(reason)) => {
                        this.sound = None;
                        this.sound_unavailable = Some(reason);
                        this.sound_error = None;
                        log::info!("STUDIO event=sound-unavailable");
                    }
                    Err(SoundError::Failed(reason)) => {
                        log::warn!("STUDIO event=sound-load-failed {reason}");
                        this.sound_unavailable = None;
                        this.sound_error = Some(reason);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn mutate_sound(
        &mut self,
        id: u32,
        op: &'static str,
        action: impl FnOnce() -> Result<(), SoundError> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.sound_busy {
            return;
        }
        self.sound_busy = true;
        self.sound_error = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            action()?;
            sound::fetch_sound()
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.sound_busy = false;
                match result {
                    Ok(snapshot) => {
                        log::info!("STUDIO event=sound-updated id={id} op={op}");
                        this.sound = Some(snapshot);
                        this.sound_unavailable = None;
                        this.sound_error = None;
                        this.sound_status = match op {
                            "default" => "Default audio device updated.".into(),
                            "mute" => "Mute state updated.".into(),
                            _ => "Volume updated.".into(),
                        };
                    }
                    Err(err) => {
                        log::warn!("STUDIO event=sound-update-failed id={id} op={op} {}", err.message());
                        this.sound_error = Some(err.message().to_string());
                        this.sound_status = "Audio change failed.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_bluetooth(&mut self, cx: &mut Context<Self>) {
        if self.bluetooth_loading || self.bluetooth_busy {
            return;
        }
        self.bluetooth_loading = true;
        self.bluetooth_error = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move { bluetooth::fetch_bluetooth() });
        cx.spawn(async move |this, cx| {
            let state = task.await;
            let _ = this.update(cx, |this, cx| {
                this.bluetooth_loading = false;
                this.bluetooth_loaded = true;
                match &state {
                    BluetoothState::Available { devices, .. } => {
                        log::info!(
                            "STUDIO event=bluetooth-loaded state=available devices={}",
                            devices.len()
                        );
                    }
                    BluetoothState::Unavailable(_) => {
                        log::info!("STUDIO event=bluetooth-loaded state=unavailable devices=0");
                    }
                }
                this.bluetooth = Some(state);
                this.bluetooth_error = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn mutate_bluetooth(
        &mut self,
        op: &'static str,
        pending_status: impl Into<String>,
        done_status: impl Into<String>,
        action: impl FnOnce() -> Result<(), BluetoothError> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.bluetooth_busy {
            return;
        }
        self.bluetooth_busy = true;
        self.bluetooth_error = None;
        self.bluetooth_status = pending_status.into();
        let done_status = done_status.into();
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            action()?;
            Ok::<_, BluetoothError>(bluetooth::fetch_bluetooth())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.bluetooth_busy = false;
                match result {
                    Ok(state) => {
                        log::info!("STUDIO event=bluetooth-updated op={op}");
                        this.bluetooth = Some(state);
                        this.bluetooth_error = None;
                        this.bluetooth_status = done_status;
                    }
                    Err(err) => {
                        log::warn!("STUDIO event=bluetooth-update-failed op={op} {}", err.message());
                        this.bluetooth_error = Some(err.message().to_string());
                        this.bluetooth_status = "Bluetooth request failed.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_input(&mut self, cx: &mut Context<Self>) {
        if self.input_loading || self.input_busy {
            return;
        }
        self.input_loading = true;
        self.input_error = None;
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { input_config::load_live_or_saved() });
        cx.spawn(async move |this, cx| {
            let (settings, live) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.input_loading = false;
                this.input_loaded = true;
                this.input_live = live;
                log::info!(
                    "STUDIO event=input-loaded live={} layout={} rate={} delay={} speed={} natural={}",
                    live,
                    settings.layout,
                    settings.repeat_rate,
                    settings.repeat_delay,
                    settings.pointer_speed,
                    settings.natural_scroll
                );
                this.input = settings;
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_input_settings(&mut self, next: InputSettings, cx: &mut Context<Self>) {
        if self.input_busy {
            return;
        }
        let clean = next.clamped();
        self.input = clean.clone();
        self.input_busy = true;
        self.input_error = None;
        self.input_status = "Applying input settings…".into();
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { input_config::apply_and_save(&clean).map(|live| (clean, live)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.input_busy = false;
                match result {
                    Ok((applied, live)) => {
                        this.input = applied.clone();
                        this.input_live = live;
                        this.input_error = None;
                        this.input_status = if live {
                            "Input settings applied live.".into()
                        } else {
                            "Input settings saved.".into()
                        };
                        log::info!(
                            "STUDIO event=input-applied live={} layout={} rate={} delay={} speed={} natural={}",
                            live,
                            applied.layout,
                            applied.repeat_rate,
                            applied.repeat_delay,
                            applied.pointer_speed,
                            applied.natural_scroll
                        );
                    }
                    Err(err) => {
                        log::warn!("STUDIO event=input-apply-failed {err}");
                        this.input_error = Some(err);
                        this.input_status = "Input change failed.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_power(&mut self, cx: &mut Context<Self>) {
        if self.power_loading || self.power_busy {
            return;
        }
        self.power_loading = true;
        self.power_error = None;
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { power_config::load_live_or_saved() });
        cx.spawn(async move |this, cx| {
            let (settings, live) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.power_loading = false;
                this.power_loaded = true;
                this.power_live = live;
                log::info!(
                    "STUDIO event=power-loaded live={} lock={} suspend={}",
                    live,
                    settings.lock_after_secs,
                    settings.suspend_after_secs
                );
                this.power = settings;
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_power_settings(&mut self, next: PowerSettings, cx: &mut Context<Self>) {
        if self.power_busy {
            return;
        }
        let clean = next.clamped();
        self.power = clean.clone();
        self.power_busy = true;
        self.power_error = None;
        self.power_status = "Updating power timers…".into();
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { power_config::apply_and_save(&clean).map(|live| (clean, live)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.power_busy = false;
                match result {
                    Ok((applied, live)) => {
                        this.power = applied.clone();
                        this.power_live = live;
                        this.power_error = None;
                        this.power_status = if live {
                            "Power & lock timers updated live.".into()
                        } else {
                            "Power & lock timers saved.".into()
                        };
                        log::info!(
                            "STUDIO event=power-applied live={} lock={} suspend={}",
                            live,
                            applied.lock_after_secs,
                            applied.suspend_after_secs
                        );
                    }
                    Err(err) => {
                        log::warn!("STUDIO event=power-apply-failed {err}");
                        this.power_error = Some(err);
                        this.power_status = "Power update failed.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_default_apps(&mut self, cx: &mut Context<Self>) {
        if self.defaults_loading || self.defaults_busy {
            return;
        }
        self.defaults_loading = true;
        self.defaults_error = None;
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { default_apps::load_default_apps() });
        cx.spawn(async move |this, cx| {
            let categories = task.await;
            let _ = this.update(cx, |this, cx| {
                this.defaults_loading = false;
                this.defaults_loaded = true;
                log::info!("STUDIO event=defaults-loaded count={}", categories.len());
                this.default_apps = categories;
                cx.notify();
            });
        })
        .detach();
    }

    fn update_default_app(
        &mut self,
        category: AppCategory,
        desktop_id: String,
        app_name: String,
        cx: &mut Context<Self>,
    ) {
        if self.defaults_busy {
            return;
        }
        self.defaults_busy = true;
        self.defaults_error = None;
        self.defaults_status = format!("Setting {} to {app_name}…", category.label());
        cx.notify();
        let id_clone = desktop_id.clone();
        let task = cx.background_executor().spawn(async move {
            default_apps::set_category_default(category, &id_clone)?;
            Ok::<_, String>(default_apps::load_default_apps())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.defaults_busy = false;
                match result {
                    Ok(categories) => {
                        this.default_apps = categories;
                        this.defaults_error = None;
                        this.defaults_status = format!("Default {} set to {app_name}.", category.label());
                        log::info!(
                            "STUDIO event=default-app-saved category={category:?} desktop={desktop_id}"
                        );
                    }
                    Err(err) => {
                        log::warn!("STUDIO event=default-app-failed category={category:?} {err}");
                        this.defaults_error = Some(err);
                        this.defaults_status = "Could not update default app.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_system_info(&mut self, cx: &mut Context<Self>) {
        if self.system_loading {
            return;
        }
        self.system_loading = true;
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { system_info::collect() });
        cx.spawn(async move |this, cx| {
            let summary = task.await;
            let _ = this.update(cx, |this, cx| {
                this.system_loading = false;
                this.system_loaded = true;
                log::info!("STUDIO event=system-loaded hostname={}", summary.hostname);
                this.system_info = Some(summary);
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        if self.page == Page::Displays {
            self.apply_displays(cx);
            return;
        }
        if self.busy || self.load_error || !self.appearance_dirty() {
            return;
        }
        self.busy = true;
        self.error = None;
        self.status = "Saving…".into();
        cx.notify();
        let store = self.store.clone();
        let expected = self.saved.clone();
        let draft = self.draft.clone();
        let builtin_wallpaper_selected = self.builtin_wallpaper_selected;
        let task = cx.background_executor().spawn(async move {
            let result = session::Session::connect().and_then(|motion| {
                store.save_with_wallpaper_choice(
                    &draft, &expected, &motion, builtin_wallpaper_selected,
                )
            });
            let synced_theme = if result.is_ok() {
                match ThemeStore::for_user()
                    .and_then(|theme_store| theme::sync_theme_store(&theme_store, &draft))
                {
                    Ok(active) => Some(active),
                    Err(err) => {
                        log::warn!("STUDIO event=theme-sync-failed {err}");
                        None
                    }
                }
            } else {
                None
            };
            (draft, result, synced_theme)
        });
        cx.spawn(async move |this, cx| {
            let (draft, result, synced_theme) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(outcome) => {
                        log::info!(
                            "STUDIO event=saved theme={:?} studio={} reduced_motion={} wallpaper={:?}",
                            draft.theme,
                            draft.studio,
                            draft.reduced_motion,
                            draft.wallpaper
                        );
                        this.live_theme = synced_theme
                            .unwrap_or_else(|| theme::fallback_active_theme(&draft));
                        this.previewing_appearance = false;
                        this.saved = Some(draft);
                        match outcome {
                            SaveOutcome::Complete => {
                                this.builtin_wallpaper_selected = false;
                                this.status = "Saved.".into();
                            }
                            SaveOutcome::CustomWallpaperKept(error) => {
                                this.error = Some(error);
                                this.status = "Appearance saved; wallpaper unchanged.".into();
                            }
                        }
                        this.confirming_close = false;
                    }
                    Err(error) => {
                        log::warn!("STUDIO event=save-failed {error}");
                        this.error = Some(error);
                        this.status = "Changes not saved.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.page == Page::Displays {
            self.display_drafts = self.outputs.iter().map(OutputDraft::from_output).collect();
            self.display_status = "Display changes reset.".into();
            self.display_error = None;
            self.confirming_close = false;
            log::info!("STUDIO event=display-undo");
            cx.notify();
            return;
        }
        self.draft = self.saved.clone().unwrap_or_default();
        self.builtin_wallpaper_selected = false;
        self.previewing_appearance = false;
        self.status = "Preview reset.".into();
        self.confirming_close = false;
        if !self.load_error {
            self.error = None;
        }
        log::info!("STUDIO event=undo");
        cx.notify();
    }
    fn reload(&mut self, _: &Reload, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match self.page {
            Page::Displays => {
                self.refresh_displays(cx);
                return;
            }
            Page::Network => {
                self.refresh_network(cx);
                return;
            }
            Page::Sound => {
                self.refresh_sound(cx);
                return;
            }
            Page::Bluetooth => {
                self.refresh_bluetooth(cx);
                return;
            }
            Page::Input => {
                self.refresh_input(cx);
                return;
            }
            Page::Power => {
                self.refresh_power(cx);
                return;
            }
            Page::DefaultApps => {
                self.refresh_default_apps(cx);
                return;
            }
            Page::About => {
                self.refresh_system_info(cx);
                return;
            }
            Page::Appearance => {}
        }
        match self.store.load() {
            Ok(saved) => {
                self.saved = saved;
                self.draft = self.saved.clone().unwrap_or_default();
                self.builtin_wallpaper_selected = false;
                self.live_theme = ThemeStore::for_user()
                    .map(|theme_store| theme_store.load_active())
                    .unwrap_or_else(|_| theme::fallback_active_theme(&self.draft));
                self.previewing_appearance = false;
                self.error = None;
                self.load_error = false;
                self.status = "Reloaded saved preferences.".into();
                self.confirming_close = false;
            }
            Err(e) => {
                self.error = Some(e);
                self.load_error = true;
            }
        }
        cx.notify();
    }
    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.has_unsaved_edits() {
            self.confirming_close = true;
            window.focus(&self.controls[CONTROL_KEEP_EDITING]);
            cx.notify();
        } else {
            cx.quit();
        }
    }
    fn button(
        &self,
        index: usize,
        label: impl Into<gpui::SharedString>,
        p: Palette,
        window: &Window,
        primary: bool,
        enabled: bool,
    ) -> Stateful<Div> {
        ui_button(
            ("button", index),
            &self.controls[index],
            label,
            p,
            window,
            if primary {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Secondary
            },
            enabled,
        )
    }
    fn theme_choice(
        &self,
        index: usize,
        theme: Theme,
        p: Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.draft.theme == theme;
        let preview = theme::palette_for_preferences(&Preferences {
            theme,
            studio: self.draft.studio,
            ..Preferences::default()
        });
        let control = CONTROL_THEME_FIRST + index;
        let focus = self.controls[control].is_focused(window);
        div()
            .id(("theme", index))
            .track_focus(&self.controls[control])
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .rounded(px(12.))
            .border_2()
            .border_color(rgb(if focus || selected { p.focus } else { p.line }))
            .when(focus, |d| d.border_dashed())
            .bg(rgb(p.surface))
            .cursor_pointer()
            .hover(|s| s.opacity(0.86))
            .child(
                div()
                    .h(px(72.))
                    .rounded(px(6.))
                    .bg(rgb(preview.canvas))
                    .p_3()
                    .flex()
                    .gap_2()
                    .child(div().w(px(18.)).rounded(px(3.)).bg(rgb(preview.surface)))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .gap_2()
                            .child(
                                div()
                                    .h(px(5.))
                                    .w(px(42.))
                                    .rounded(px(2.))
                                    .bg(rgb(preview.text)),
                            )
                            .child(
                                div()
                                    .h(px(4.))
                                    .w_full()
                                    .rounded(px(2.))
                                    .bg(rgb(preview.line)),
                            )
                            .child(
                                div()
                                    .h(px(12.))
                                    .w(px(32.))
                                    .rounded(px(3.))
                                    .bg(rgb(preview.accent)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_1()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(theme.label())
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(p.muted))
                            .child(if selected { "Selected" } else { "" }),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.busy {
                    return;
                }
                window.focus(&this.controls[control]);
                this.draft.theme = theme;
                this.changed(cx);
            }))
    }
    fn wallpaper_choice(
        &self,
        index: usize,
        wallpaper: Wallpaper,
        p: Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.draft.wallpaper == wallpaper;
        let control = CONTROL_WALLPAPER_FIRST + index;
        let focus = self.controls[control].is_focused(window);
        let preview = if self.draft.studio {
            div()
                .h(px(72.))
                .rounded(px(6.))
                .bg(rgb(p.canvas))
                .p_2()
                .flex()
                .flex_col()
                .justify_between()
                .items_center()
                .child(div().w_full().h(px(5.)).rounded(px(2.)).bg(rgb(p.surface)))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(p.muted))
                        .child("Neutral canvas"),
                )
                .child(div().w(px(28.)).h(px(4.)).rounded_full().bg(rgb(p.line)))
        } else {
            let mut box_div = div()
                .h(px(72.))
                .rounded(px(6.))
                .overflow_hidden()
                .bg(rgb(0x080d12));
            if let Some(path) = self.wallpaper_paths[index].clone() {
                box_div = box_div.child(
                    img(path)
                        .size_full()
                        .rounded(px(6.))
                        .object_fit(ObjectFit::Cover),
                );
            }
            box_div
        };
        div()
            .id(("wallpaper", index))
            .track_focus(&self.controls[control])
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .rounded(px(12.))
            .border_2()
            .border_color(rgb(if focus || selected { p.focus } else { p.line }))
            .when(focus, |d| d.border_dashed())
            .bg(rgb(p.surface))
            .cursor_pointer()
            .hover(|s| s.opacity(0.86))
            .child(preview)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_1()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(wallpaper.label())
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(p.muted))
                            .child(if selected { "Selected" } else { "" }),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.busy {
                    return;
                }
                window.focus(&this.controls[control]);
                this.draft.wallpaper = wallpaper;
                this.builtin_wallpaper_selected = true;
                this.changed(cx);
            }))
    }
    #[allow(clippy::too_many_arguments)]
    fn toggle(
        &self,
        index: usize,
        title: &'static str,
        description: &'static str,
        value: bool,
        p: Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(("toggle", index))
            .track_focus(&self.controls[index])
            .flex()
            .items_center()
            .gap_5()
            .py_4()
            .px_3()
            .rounded(px(10.))
            .border_2()
            .border_color(rgb(if self.controls[index].is_focused(window) {
                p.focus
            } else {
                p.canvas
            }))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(p.surface)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(p.muted))
                            .child(description),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(p.muted))
                    .child(if value { "On" } else { "Off" }),
            )
            .child(
                div()
                    .w(px(44.))
                    .h(px(26.))
                    .px(px(3.))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .bg(rgb(if value { p.accent } else { p.line }))
                    .when(value, |d| d.justify_end())
                    .child(div().size(px(20.)).rounded_full().bg(rgb(if value {
                        p.on_accent
                    } else {
                        p.text
                    }))),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.busy {
                    return;
                }
                window.focus(&this.controls[index]);
                if index == CONTROL_STUDIO {
                    this.draft.studio = !this.draft.studio;
                } else {
                    this.draft.reduced_motion = !this.draft.reduced_motion;
                }
                this.changed(cx);
            }))
    }
    fn appearance(&self, p: Palette, window: &Window, cx: &Context<Self>) -> Div {
        div().flex().flex_col().gap_5()
            .child(div().flex().flex_col().gap_2()
                .child(div().text_size(px(28.)).font_weight(FontWeight::BOLD).child("Make it yours."))
                .child(div().text_size(px(13.)).text_color(rgb(p.muted)).child("A comfortable surface. A calmer workspace.")))
            .child(div().flex().flex_col().gap_3()
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child("Appearance"))
                .child(div().flex().gap_3().children(Theme::ALL.into_iter().enumerate().map(|(i,t)|self.theme_choice(i,t,p,window,cx))))
                .child(div().text_size(px(12.)).text_color(rgb(p.muted)).child(if self.draft.studio{
                    "Studio Mode is showing neutral grays. Your selected mode is kept for later."
                }else{"Previewed here immediately. Save to remember your choice."})))
            .child(div().flex().flex_col().gap_3()
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child("Wallpaper"))
                .child(div().flex().gap_3().children(Wallpaper::ALL.into_iter().enumerate().map(|(i,w)|self.wallpaper_choice(i,w,p,window,cx))))
                .when(self.draft.studio, |d| {
                    d.child(div().text_size(px(12.)).text_color(rgb(p.muted)).child(
                        "Studio Mode uses a neutral canvas. Your selected wallpaper is kept for later.",
                    ))
                }))
            .child(div().flex().flex_col().border_t_1().border_b_1().border_color(rgb(p.line))
                .child(self.toggle(CONTROL_STUDIO,"Studio Mode","Use neutral grays for visual work. This does not calibrate your display.",self.draft.studio,p,window,cx))
                .child(divider(p))
                .child(self.toggle(CONTROL_REDUCED_MOTION,if session::on_crowbaros() { "Reduced motion preference" } else { "Reduced motion" },if session::on_crowbaros() { "Saved for compatible desktops. This desktop has no animations." } else { "Turn off desktop window and workspace animations when you save." },self.draft.reduced_motion,p,window,cx)))
            .child(div().text_size(px(12.)).text_color(rgb(p.muted))
                .child(if session::on_crowbaros() { "Saved appearance and wallpaper also apply to the shell. Other applications keep their own themes." } else { "Appearance currently styles Studio Settings. Reduced motion applies to your Hyprland desktop and is restored at login." }))
    }
    fn displays(&self, p: Palette, window: &Window, cx: &Context<Self>) -> Div {
        let mut page = div().flex().flex_col().gap_4().child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(28.))
                                .font_weight(FontWeight::BOLD)
                                .child("Displays"),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(p.muted))
                                .child("Resolution, scale, and layout for connected monitors."),
                        ),
                )
                .child(
                    self.button(
                        CONTROL_DISPLAY_REFRESH,
                        if self.displays_loading {
                            "Refreshing…"
                        } else {
                            "Refresh"
                        },
                        p,
                        window,
                        false,
                        !self.displays_loading && !self.busy,
                    )
                    .h(px(34.))
                    .px_3()
                    .on_click(cx.listener(|this, _, w, cx| {
                        w.focus(&this.controls[CONTROL_DISPLAY_REFRESH]);
                        this.refresh_displays(cx);
                    })),
                ),
        );

        if self.displays_loading && !self.displays_loaded {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("Reading connected displays…"),
            );
        }

        if let Some(reason) = &self.displays_unavailable {
            return page.child(
                card(p)
                    .p_5()
                    .gap_2()
                    .child(section_header("Display controls unavailable", None, p))
                    .child(
                        div()
                            .text_size(px(TEXT_BODY))
                            .text_color(rgb(p.muted))
                            .child(reason.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_SUPPORTING))
                            .text_color(rgb(p.muted))
                            .child("Sign into a Fulcrum session to adjust resolution, scale, and monitor layout."),
                    ),
            );
        }

        if self.outputs.is_empty() {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("No displays reported by Fulcrum."),
            );
        }

        let selected = self.selected_display.min(self.outputs.len() - 1);
        let stage_w = 540.0f32;
        let stage_h = 114.0f32;
        let rects = project_diagram(&self.display_drafts, stage_w, stage_h, 10.0);

        // Arrangement canvas
        let mut stage = div().w(px(stage_w)).h(px(stage_h)).relative();
        for (i, ((output, draft), rect)) in self
            .outputs
            .iter()
            .zip(&self.display_drafts)
            .zip(rects)
            .enumerate()
        {
            let is_selected = i == selected;
            let (lw, lh) = logical_size(draft.width, draft.height, draft.scale);
            let modified = !draft.matches_output(output);
            stage = stage.child(
                div()
                    .id(("display-node", i))
                    .absolute()
                    .left(px(rect.x))
                    .top(px(rect.y))
                    .w(px(rect.width))
                    .h(px(rect.height))
                    .rounded(px(8.))
                    .border_2()
                    .border_color(rgb(if is_selected { p.focus } else { p.line }))
                    .bg(rgb(p.canvas))
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(2.))
                    .overflow_hidden()
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.9))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(if is_selected { p.text } else { p.muted }))
                            .child(output.name.clone()),
                    )
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(TEXT_SELECTION_LABEL))
                            .text_color(rgb(p.muted))
                            .child(format!("{lw}×{lh}")),
                    )
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(TEXT_SELECTION_LABEL))
                            .text_color(rgb(if modified { p.accent } else { p.muted }))
                            .child(format_scale(draft.scale)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_display = i;
                        cx.notify();
                    })),
            );
        }

        let mut arrangement = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Arrangement"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(p.muted))
                            .child(if self.outputs.len() == 1 {
                                "1 display connected".to_string()
                            } else {
                                format!("{} displays connected · click a display to configure", self.outputs.len())
                            }),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .h(px(126.))
                    .rounded(px(12.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .child(stage),
            );

        if self.outputs.len() >= 2 {
            let anchor_idx = if selected == 0 { 1 } else { 0 };
            let anchor_name = self.outputs[anchor_idx].name.clone();
            let selected_name = self.outputs[selected].name.clone();
            arrangement = arrangement.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(p.muted))
                            .child(format!("Place {selected_name} relative to {anchor_name}")),
                    )
                    .child(div().flex().gap_2().children(
                        RelativeSide::ALL.into_iter().enumerate().map(|(si, side)| {
                            div()
                                .id(("display-side", si))
                                .h(px(30.))
                                .px_3()
                                .rounded(px(8.))
                                .border_1()
                                .border_color(rgb(p.line))
                                .bg(rgb(p.surface))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(p.text))
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.85))
                                .child(side.label())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.busy {
                                        return;
                                    }
                                    let idx = this
                                        .selected_display
                                        .min(this.display_drafts.len().saturating_sub(1));
                                    arrange_relative(&mut this.display_drafts, idx, side);
                                    this.display_changed(cx);
                                }))
                        }),
                    )),
            );
        }

        page = page.child(arrangement);

        // Selected display summary
        let output = &self.outputs[selected];
        let draft = &self.display_drafts[selected];
        let (lw, lh) = logical_size(draft.width, draft.height, draft.scale);
        let modified = !draft.matches_output(output);
        let identity_text = if output.identity.trim().is_empty() {
            "Connected display".to_string()
        } else {
            output.identity.clone()
        };

        page = page.child(
            card(p)
                .px_4()
                .py_3()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .text_size(px(TEXT_SECTION))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(output.name.clone()),
                                )
                                .child(
                                    div()
                                        .font_family(FONT_MONO)
                                        .text_size(px(TEXT_TECHNICAL))
                                        .text_color(rgb(p.muted))
                                        .child(identity_text),
                                ),
                        )
                        .child(badge(
                            if modified { "Pending" } else { "Active" },
                            p,
                            modified,
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_4()
                        .font_family(FONT_MONO)
                        .text_size(px(TEXT_TECHNICAL))
                        .text_color(rgb(p.text))
                        .child(format!(
                            "Mode: {}×{} @ {}",
                            draft.width,
                            draft.height,
                            format_refresh(draft.refresh)
                        ))
                        .child(format!("Logical: {lw}×{lh}"))
                        .child(format!("Position: ({}, {})", draft.x, draft.y)),
                ),
        );

        // Scale picker
        let current_scale = draft.scale;
        page = page.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(TEXT_SECTION))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Scale"),
                        )
                        .child(
                            div()
                                .text_size(px(TEXT_TECHNICAL))
                                .text_color(rgb(p.muted))
                                .child("Interface size in 25% steps"),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .children(SCALES.into_iter().enumerate().map(|(si, scale)| {
                            let is_active = current_scale == scale;
                            action_chip(("display-scale", si), format_scale(scale), p, is_active)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.busy {
                                        return;
                                    }
                                    if let Some(d) = this.display_drafts.get_mut(selected) {
                                        d.scale = scale;
                                        this.display_changed(cx);
                                    }
                                }))
                        })),
                ),
        );

        // Resolution & refresh picker
        let current_mode = draft.mode();
        let live_mode = DisplayMode {
            width: output.width,
            height: output.height,
            refresh: output.refresh,
        };
        let modes = dedup_sorted_modes(&output.modes, current_mode);
        let mut mode_list = div()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(p.line))
            .bg(rgb(p.surface))
            .overflow_hidden()
            .flex()
            .flex_col();

        for (mi, mode) in modes.into_iter().enumerate() {
            let is_selected = mode == current_mode;
            let is_live = mode == live_mode;
            mode_list = mode_list.child(
                div()
                    .id(("display-mode", mi))
                    .px_4()
                    .py_3()
                    .when(mi > 0, |d| d.border_t_1().border_color(rgb(p.line)))
                    .when(is_selected, |d| d.bg(rgb(p.canvas)))
                    .flex()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.88))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(if is_selected {
                                        FontWeight::BOLD
                                    } else {
                                        FontWeight::SEMIBOLD
                                    })
                                    .child(format!("{} × {}", mode.width, mode.height)),
                            )
                            .child(
                                div()
                                    .font_family(FONT_MONO)
                                    .text_size(px(TEXT_SUPPORTING))
                                    .text_color(rgb(p.muted))
                                    .child(format_refresh(mode.refresh)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(is_selected, |d| {
                                d.child(div().size(px(7.)).rounded_full().bg(rgb(p.accent)))
                            })
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(if is_selected { p.text } else { p.muted }))
                                    .child(if is_selected {
                                        "Selected"
                                    } else if is_live {
                                        "Current"
                                    } else {
                                        ""
                                    }),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.busy {
                            return;
                        }
                        if let Some(d) = this.display_drafts.get_mut(selected) {
                            d.width = mode.width;
                            d.height = mode.height;
                            d.refresh = mode.refresh;
                            this.display_changed(cx);
                        }
                    })),
            );
        }

        page.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Resolution & refresh rate"),
                )
                .child(mode_list),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(rgb(p.muted))
                .child("Applied display settings take effect immediately and are remembered for this monitor across sessions."),
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn page_header(
        &self,
        title: &'static str,
        subtitle: &'static str,
        refresh_control: usize,
        loading: bool,
        disabled: bool,
        p: Palette,
        window: &Window,
        on_refresh: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &Context<Self>,
    ) -> Div {
        div()
            .flex()
            .items_start()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(TEXT_TITLE))
                            .font_weight(FontWeight::BOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_LABEL))
                            .text_color(rgb(p.muted))
                            .child(subtitle),
                    ),
            )
            .child(
                self.button(
                    refresh_control,
                    if loading { "Refreshing…" } else { "Refresh" },
                    p,
                    window,
                    false,
                    !loading && !disabled,
                )
                .h(px(34.))
                .px_3()
                .on_click(cx.listener(move |this, _, w, cx| {
                    w.focus(&this.controls[refresh_control]);
                    on_refresh(this, w, cx);
                })),
            )
    }

    fn network_page(&self, p: Palette, window: &Window, cx: &Context<Self>) -> Div {
        let mut page = div().flex().flex_col().gap_5().child(self.page_header(
            "Network",
            "Wired Ethernet and Wi-Fi connections through iwd.",
            CONTROL_NETWORK_REFRESH,
            self.network_loading,
            self.network_busy,
            p,
            window,
            |this, _, cx| this.refresh_network(cx),
            cx,
        ));

        if self.network_loading && !self.network_loaded {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("Reading network interfaces…"),
            );
        }

        let Some(snapshot) = &self.network else {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("Network state has not loaded yet."),
            );
        };

        // Ethernet section
        let mut eth_card = card(p).child(section_header(
            "Ethernet",
            Some("Wired network adapters and active IP addresses."),
            p,
        ));
        if snapshot.ethernet.is_empty() {
            eth_card = eth_card.child(
                div()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("No wired Ethernet adapter detected."),
            );
        } else {
            for (i, iface) in snapshot.ethernet.iter().enumerate() {
                let connected = iface.state == network::LinkState::Up;
                let addr_text = if iface.addresses.is_empty() {
                    if connected {
                        "Waiting for IP address".to_string()
                    } else {
                        "Cable unplugged".to_string()
                    }
                } else {
                    iface.addresses.join("  ·  ")
                };
                if i > 0 {
                    eth_card = eth_card.child(divider(p));
                }
                eth_card = eth_card.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .py_1()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_size(px(TEXT_SECTION))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(iface.name.clone()),
                                        )
                                        .when_some(iface.mac.clone(), |d, mac| {
                                            d.child(
                                                div()
                                                    .font_family(FONT_MONO)
                                                    .text_size(px(TEXT_TECHNICAL))
                                                    .text_color(rgb(p.muted))
                                                    .child(mac),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .font_family(FONT_MONO)
                                        .text_size(px(TEXT_SUPPORTING))
                                        .text_color(rgb(if connected { p.text } else { p.muted }))
                                        .child(addr_text),
                                ),
                        )
                        .child(badge(iface.state.label(), p, connected)),
                );
            }
        }
        page = page.child(eth_card);

        // Wi-Fi section
        match &snapshot.wifi {
            WifiState::Unavailable(reason) => {
                page = page.child(
                    card(p)
                        .p_5()
                        .gap_2()
                        .child(section_header("Wi-Fi unavailable", Some(reason), p))
                        .child(
                            div()
                                .text_size(px(TEXT_SUPPORTING))
                                .text_color(rgb(p.muted))
                                .child("Wired Ethernet remains available. Connect a supported Wi-Fi adapter to manage wireless networks here."),
                        ),
                );
            }
            WifiState::Available(station) => {
                page = page.child(self.wifi_station_card(station, p, window, cx));
            }
        }

        page
    }

    fn wifi_station_card(
        &self,
        station: &WifiStation,
        p: Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let dev_path_scan = station.device_path.clone();
        let dev_path_disc = station.device_path.clone();
        let is_connected = station.connected_ssid.is_some();
        let state_badge = if station.scanning {
            "Scanning"
        } else if is_connected {
            "Connected"
        } else if station.powered {
            "Disconnected"
        } else {
            "Off"
        };

        let mut wifi_card = card(p).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(TEXT_SECTION))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("Wi-Fi"),
                                )
                                .child(
                                    div()
                                        .font_family(FONT_MONO)
                                        .text_size(px(TEXT_TECHNICAL))
                                        .text_color(rgb(p.muted))
                                        .child(station.device_name.clone()),
                                )
                                .child(badge(state_badge, p, is_connected)),
                        )
                        .child(
                            div()
                                .text_size(px(TEXT_SUPPORTING))
                                .text_color(rgb(p.muted))
                                .child(if let Some(ssid) = &station.connected_ssid {
                                    if station.addresses.is_empty() {
                                        format!("Connected to {ssid}")
                                    } else {
                                        format!(
                                            "Connected to {ssid} · {}",
                                            station.addresses.join(", ")
                                        )
                                    }
                                } else {
                                    "Choose a network below or scan to discover nearby access points."
                                        .into()
                                }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(is_connected, |d| {
                            d.child(
                                action_chip("wifi-disconnect-top", "Disconnect", p, false)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.disconnect_wifi_station(dev_path_disc.clone(), cx);
                                    })),
                            )
                        })
                        .child(
                            self.button(
                                CONTROL_WIFI_SCAN,
                                if station.scanning || self.network_busy {
                                    "Scanning…"
                                } else {
                                    "Scan"
                                },
                                p,
                                window,
                                false,
                                !self.network_busy && station.powered,
                            )
                            .h(px(32.))
                            .px_3()
                            .on_click(cx.listener(move |this, _, w, cx| {
                                w.focus(&this.controls[CONTROL_WIFI_SCAN]);
                                this.trigger_wifi_scan(dev_path_scan.clone(), cx);
                            })),
                        ),
                ),
        );

        // Inline passphrase prompt for WPA2/WPA3 networks
        if let Some(prompt) = &self.wifi_prompt {
            let prompt_clone = prompt.clone();
            let focus = self.controls[CONTROL_WIFI_PASSPHRASE].is_focused(window);
            let display_value =
                network::mask_passphrase(&self.wifi_passphrase, self.wifi_show_passphrase);
            let empty = self.wifi_passphrase.is_empty();
            wifi_card = wifi_card.child(
                div()
                    .p_4()
                    .rounded(px(10.))
                    .bg(rgb(p.canvas))
                    .border_1()
                    .border_color(rgb(p.accent))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(section_header(
                        format!("Connect to {}", prompt.ssid),
                        Some("Enter the WPA2/WPA3 passphrase. Passphrases are never logged."),
                        p,
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("wifi-passphrase-input")
                                    .track_focus(&self.controls[CONTROL_WIFI_PASSPHRASE])
                                    .on_key_down(cx.listener(Self::wifi_passphrase_key_down))
                                    .flex_1()
                                    .h(px(38.))
                                    .px_3()
                                    .rounded(px(8.))
                                    .bg(rgb(p.surface))
                                    .border_2()
                                    .border_color(rgb(if focus { p.focus } else { p.line }))
                                    .when(focus, |d| d.border_dashed())
                                    .flex()
                                    .items_center()
                                    .font_family(FONT_MONO)
                                    .text_size(px(TEXT_LABEL))
                                    .text_color(rgb(if empty { p.muted } else { p.text }))
                                    .child(if empty {
                                        "Type passphrase…".to_string()
                                    } else {
                                        display_value
                                    }),
                            )
                            .child(
                                action_chip(
                                    "wifi-passphrase-reveal",
                                    if self.wifi_show_passphrase {
                                        "Hide"
                                    } else {
                                        "Show"
                                    },
                                    p,
                                    self.wifi_show_passphrase,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.wifi_show_passphrase = !this.wifi_show_passphrase;
                                    w.focus(&this.controls[CONTROL_WIFI_PASSPHRASE]);
                                    cx.notify();
                                })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                action_chip("wifi-prompt-cancel", "Cancel", p, false).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.clear_wifi_prompt();
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(
                                action_chip(
                                    "wifi-prompt-submit",
                                    "Connect",
                                    p,
                                    !empty && !self.network_busy,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.wifi_passphrase.is_empty() || this.network_busy {
                                        return;
                                    }
                                    let secret = std::mem::take(&mut this.wifi_passphrase);
                                    this.connect_wifi_network(
                                        prompt_clone.network_path.clone(),
                                        prompt_clone.ssid.clone(),
                                        Some(secret),
                                        cx,
                                    );
                                })),
                            ),
                    ),
            );
        }

        if station.networks.is_empty() {
            wifi_card = wifi_card.child(divider(p)).child(
                div()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("No nearby Wi-Fi networks reported yet. Click Scan to search."),
            );
        } else {
            let mut list = div().flex().flex_col();
            for (i, net) in station.networks.iter().enumerate() {
                list = list.child(divider(p)).child(self.wifi_network_row(
                    i,
                    net,
                    &station.device_path,
                    p,
                    cx,
                ));
            }
            wifi_card = wifi_card.child(list);
        }

        wifi_card
    }

    fn wifi_network_row(
        &self,
        index: usize,
        net: &WifiNetwork,
        device_path: &str,
        p: Palette,
        cx: &Context<Self>,
    ) -> Div {
        let net_path = net.path.clone();
        let ssid = net.ssid.clone();
        let needs_pass = net.security.needs_passphrase() && !net.known;
        let known_path = net.known_network_path.clone();
        let ssid_forget = net.ssid.clone();
        let dev_path = device_path.to_string();

        div()
            .py_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(TEXT_LABEL))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(net.ssid.clone()),
                            )
                            .child(badge(net.security.label(), p, false))
                            .when(net.connected, |d| d.child(badge("Connected", p, true)))
                            .when(!net.connected && net.known, |d| {
                                d.child(badge("Saved", p, false))
                            }),
                    )
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(TEXT_TECHNICAL))
                            .text_color(rgb(p.muted))
                            .child(net.signal_label()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when_some(known_path, |d, kp| {
                        d.child(
                            action_chip(("wifi-forget", index), "Forget", p, false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.forget_wifi_network(kp.clone(), ssid_forget.clone(), cx);
                                }),
                            ),
                        )
                    })
                    .child(if net.connected {
                        action_chip(("wifi-disconnect", index), "Disconnect", p, false).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.disconnect_wifi_station(dev_path.clone(), cx);
                            }),
                        )
                    } else {
                        action_chip(("wifi-connect", index), "Connect", p, true).on_click(
                            cx.listener(move |this, _, window, cx| {
                                if this.network_busy {
                                    return;
                                }
                                if needs_pass {
                                    this.wifi_prompt = Some(WifiPrompt {
                                        network_path: net_path.clone(),
                                        ssid: ssid.clone(),
                                    });
                                    this.wifi_passphrase.clear();
                                    this.wifi_show_passphrase = false;
                                    this.network_error = None;
                                    window.focus(&this.controls[CONTROL_WIFI_PASSPHRASE]);
                                    cx.notify();
                                } else {
                                    this.connect_wifi_network(
                                        net_path.clone(),
                                        ssid.clone(),
                                        None,
                                        cx,
                                    );
                                }
                            }),
                        )
                    }),
            )
    }

    fn sound_page(&self, p: Palette, window: &Window, cx: &Context<Self>) -> Div {
        let mut page = div().flex().flex_col().gap_5().child(self.page_header(
            "Sound",
            "PipeWire output devices, microphones, and volume levels.",
            CONTROL_SOUND_REFRESH,
            self.sound_loading,
            self.sound_busy,
            p,
            window,
            |this, _, cx| this.refresh_sound(cx),
            cx,
        ));

        if self.sound_loading && !self.sound_loaded {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("Reading PipeWire audio devices…"),
            );
        }

        if let Some(reason) = &self.sound_unavailable {
            return page.child(
                card(p)
                    .p_5()
                    .gap_2()
                    .child(section_header("Audio unavailable", Some(reason), p))
                    .child(
                        div()
                            .text_size(px(TEXT_SUPPORTING))
                            .text_color(rgb(p.muted))
                            .child("Start PipeWire and WirePlumber to manage output volume, mute, and audio devices."),
                    ),
            );
        }

        let Some(snapshot) = &self.sound else {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("No audio state available yet."),
            );
        };

        // Output sinks
        let mut out_card = card(p).child(section_header(
            "Output",
            Some("Choose the default speaker or headphones and adjust output level."),
            p,
        ));
        if snapshot.sinks.is_empty() {
            out_card = out_card.child(
                div()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("No output devices reported by PipeWire."),
            );
        } else {
            for (i, sink) in snapshot.sinks.iter().enumerate() {
                if i > 0 {
                    out_card = out_card.child(divider(p));
                }
                out_card = out_card.child(self.audio_endpoint_row("sink", i, sink, p, cx));
            }
        }
        page = page.child(out_card);

        // Input sources
        let mut in_card = card(p).child(section_header(
            "Input",
            Some("Choose the default microphone and adjust capture level."),
            p,
        ));
        if snapshot.sources.is_empty() {
            in_card = in_card.child(
                div()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("No input devices connected."),
            );
        } else {
            for (i, source) in snapshot.sources.iter().enumerate() {
                if i > 0 {
                    in_card = in_card.child(divider(p));
                }
                in_card = in_card.child(self.audio_endpoint_row("source", i, source, p, cx));
            }
        }
        page = page.child(in_card).child(
            div()
                .text_size(px(TEXT_SUPPORTING))
                .text_color(rgb(p.muted))
                .child("Volume and mute changes apply immediately and stay in sync with keyboard media keys and the shell."),
        );

        page
    }

    fn audio_endpoint_row(
        &self,
        kind: &'static str,
        index: usize,
        endpoint: &AudioEndpoint,
        p: Palette,
        cx: &Context<Self>,
    ) -> Div {
        let id = endpoint.id;
        let pct = endpoint.volume_percent();
        let muted = endpoint.muted;
        let current_vol = endpoint.volume;
        let filled_steps = ((pct.min(100) as f32) / 5.0).round() as usize;

        let mut track = div()
            .flex_1()
            .h(px(14.))
            .rounded_full()
            .overflow_hidden()
            .bg(rgb(p.canvas))
            .border_1()
            .border_color(rgb(p.line))
            .flex()
            .items_center();

        for step in 1..=20usize {
            let step_vol = (step as f32) * 0.05;
            let active = !muted && step <= filled_steps;
            let seg_id = (
                if kind == "sink" {
                    "sound-sink-step"
                } else {
                    "sound-source-step"
                },
                index * 32 + step,
            );
            track = track.child(
                div()
                    .id(seg_id)
                    .flex_1()
                    .h_full()
                    .bg(rgb(if active { p.accent } else { p.canvas }))
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.85))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mutate_sound(
                            id,
                            "volume",
                            move || sound::set_volume(id, step_vol),
                            cx,
                        );
                    })),
            );
        }

        div()
            .py_2()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .size(px(8.))
                                    .flex_shrink_0()
                                    .rounded_full()
                                    .bg(rgb(if endpoint.is_default { p.accent } else { p.line })),
                            )
                            .child(
                                div()
                                    .text_size(px(TEXT_LABEL))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(endpoint.name.clone()),
                            ),
                    )
                    .child(if endpoint.is_default {
                        badge("Default", p, true).into_any_element()
                    } else {
                        action_chip(
                            (
                                if kind == "sink" {
                                    "sound-sink-default"
                                } else {
                                    "sound-source-default"
                                },
                                index,
                            ),
                            "Set default",
                            p,
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mutate_sound(id, "default", move || sound::set_default(id), cx);
                        }))
                        .into_any_element()
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        action_chip(
                            (
                                if kind == "sink" {
                                    "sound-sink-minus"
                                } else {
                                    "sound-source-minus"
                                },
                                index,
                            ),
                            "−",
                            p,
                            false,
                        )
                        .w(px(34.))
                        .px_0()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let target = (current_vol - 0.05).max(0.0);
                            this.mutate_sound(
                                id,
                                "volume",
                                move || sound::set_volume(id, target),
                                cx,
                            );
                        })),
                    )
                    .child(track)
                    .child(
                        action_chip(
                            (
                                if kind == "sink" {
                                    "sound-sink-plus"
                                } else {
                                    "sound-source-plus"
                                },
                                index,
                            ),
                            "+",
                            p,
                            false,
                        )
                        .w(px(34.))
                        .px_0()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let target = (current_vol + 0.05).min(1.0);
                            this.mutate_sound(
                                id,
                                "volume",
                                move || sound::set_volume(id, target),
                                cx,
                            );
                        })),
                    )
                    .child(
                        div()
                            .w(px(52.))
                            .flex()
                            .justify_end()
                            .font_family(FONT_MONO)
                            .text_size(px(TEXT_SUPPORTING))
                            .text_color(rgb(if muted { p.muted } else { p.text }))
                            .child(if muted {
                                "Muted".to_string()
                            } else {
                                format!("{pct}%")
                            }),
                    )
                    .child(
                        action_chip(
                            (
                                if kind == "sink" {
                                    "sound-sink-mute"
                                } else {
                                    "sound-source-mute"
                                },
                                index,
                            ),
                            if muted { "Unmute" } else { "Mute" },
                            p,
                            muted,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mutate_sound(
                                id,
                                "mute",
                                move || sound::set_mute(id, !muted),
                                cx,
                            );
                        })),
                    ),
            )
    }

    fn bluetooth_page(&self, p: Palette, window: &Window, cx: &Context<Self>) -> Div {
        let page = div().flex().flex_col().gap_5().child(self.page_header(
            "Bluetooth",
            "Pair and connect wireless headphones, keyboards, and accessories through BlueZ.",
            CONTROL_BLUETOOTH_REFRESH,
            self.bluetooth_loading,
            self.bluetooth_busy,
            p,
            window,
            |this, _, cx| this.refresh_bluetooth(cx),
            cx,
        ));

        if self.bluetooth_loading && !self.bluetooth_loaded {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("Checking Bluetooth adapters…"),
            );
        }

        let Some(state) = &self.bluetooth else {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("Bluetooth state has not loaded yet."),
            );
        };

        match state {
            BluetoothState::Unavailable(reason) => page.child(
                card(p)
                    .p_5()
                    .gap_2()
                    .child(section_header("Bluetooth unavailable", Some(reason), p))
                    .child(
                        div()
                            .text_size(px(TEXT_SUPPORTING))
                            .text_color(rgb(p.muted))
                            .child("BlueZ is running and ready. Connect a Bluetooth adapter to pair and manage wireless devices here."),
                    ),
            ),
            BluetoothState::Available { adapter, devices } => page
                .child(self.bluetooth_adapter_card(adapter, p, window, cx))
                .child(self.bluetooth_devices_card(adapter, devices, p, cx)),
        }
    }

    fn bluetooth_adapter_card(
        &self,
        adapter: &BluetoothAdapter,
        p: Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> Div {
        let adapter_path_power = adapter.path.clone();
        let next_power = !adapter.powered;
        let adapter_path_scan = adapter.path.clone();
        let next_discover = !adapter.discovering;
        let power_focused = self.controls[CONTROL_BLUETOOTH_POWER].is_focused(window);

        card(p).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(TEXT_SECTION))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(adapter.alias.clone()),
                                )
                                .child(badge(
                                    if adapter.powered { "On" } else { "Off" },
                                    p,
                                    adapter.powered,
                                )),
                        )
                        .child(
                            div()
                                .font_family(FONT_MONO)
                                .text_size(px(TEXT_TECHNICAL))
                                .text_color(rgb(p.muted))
                                .child(adapter.address.clone()),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            self.button(
                                CONTROL_BLUETOOTH_SCAN,
                                if adapter.discovering {
                                    "Stop scan"
                                } else {
                                    "Scan"
                                },
                                p,
                                window,
                                adapter.discovering,
                                adapter.powered && !self.bluetooth_busy,
                            )
                            .h(px(34.))
                            .px_3()
                            .on_click(cx.listener(move |this, _, w, cx| {
                                w.focus(&this.controls[CONTROL_BLUETOOTH_SCAN]);
                                let path = adapter_path_scan.clone();
                                this.mutate_bluetooth(
                                    if next_discover {
                                        "start-discovery"
                                    } else {
                                        "stop-discovery"
                                    },
                                    if next_discover {
                                        "Scanning for Bluetooth devices…"
                                    } else {
                                        "Stopping Bluetooth scan…"
                                    },
                                    if next_discover {
                                        "Scanning for nearby devices."
                                    } else {
                                        "Bluetooth scan stopped."
                                    },
                                    move || bluetooth::set_discovery(&path, next_discover),
                                    cx,
                                );
                            })),
                        )
                        .child(
                            div()
                                .id("bluetooth-power-toggle")
                                .track_focus(&self.controls[CONTROL_BLUETOOTH_POWER])
                                .w(px(44.))
                                .h(px(26.))
                                .px(px(3.))
                                .flex()
                                .items_center()
                                .rounded_full()
                                .border_2()
                                .border_color(rgb(if power_focused {
                                    p.focus
                                } else if adapter.powered {
                                    p.accent
                                } else {
                                    p.line
                                }))
                                .bg(rgb(if adapter.powered { p.accent } else { p.line }))
                                .when(adapter.powered, |d| d.justify_end())
                                .cursor_pointer()
                                .child(div().size(px(18.)).rounded_full().bg(rgb(
                                    if adapter.powered {
                                        p.on_accent
                                    } else {
                                        p.text
                                    },
                                )))
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    w.focus(&this.controls[CONTROL_BLUETOOTH_POWER]);
                                    let path = adapter_path_power.clone();
                                    this.mutate_bluetooth(
                                        if next_power { "power-on" } else { "power-off" },
                                        if next_power {
                                            "Turning Bluetooth on…"
                                        } else {
                                            "Turning Bluetooth off…"
                                        },
                                        if next_power {
                                            "Bluetooth turned on."
                                        } else {
                                            "Bluetooth turned off."
                                        },
                                        move || bluetooth::set_powered(&path, next_power),
                                        cx,
                                    );
                                })),
                        ),
                ),
        )
    }

    fn bluetooth_devices_card(
        &self,
        adapter: &BluetoothAdapter,
        devices: &[BluetoothDevice],
        p: Palette,
        cx: &Context<Self>,
    ) -> Div {
        let mut dev_card = card(p).child(section_header(
            "Devices",
            Some(if adapter.discovering {
                "Scanning for nearby Bluetooth devices…"
            } else {
                "Paired and recently discovered devices."
            }),
            p,
        ));

        if devices.is_empty() {
            return dev_card.child(
                div()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child(if adapter.powered {
                        "No Bluetooth devices found yet. Click Scan to discover nearby accessories."
                    } else {
                        "Turn on Bluetooth to see paired devices and scan for new ones."
                    }),
            );
        }

        for (i, device) in devices.iter().enumerate() {
            let dev_path = device.path.clone();
            let adapter_path = adapter.path.clone();
            let dev_name = device.name.clone();
            let dev_name_forget = device.name.clone();
            let dev_path_forget = device.path.clone();

            dev_card = dev_card.child(divider(p)).child(
                div()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(TEXT_LABEL))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(device.name.clone()),
                                    )
                                    .child(badge(device.status_label(), p, device.connected))
                                    .when_some(device.battery_percentage, |d, pct| {
                                        d.child(badge(format!("{pct}%"), p, false))
                                    }),
                            )
                            .child(
                                div()
                                    .font_family(FONT_MONO)
                                    .text_size(px(TEXT_TECHNICAL))
                                    .text_color(rgb(p.muted))
                                    .child(device.address.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(device.paired, |d| {
                                d.child(
                                    action_chip(("bt-forget", i), "Forget", p, false).on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            let ap = adapter_path.clone();
                                            let dp = dev_path_forget.clone();
                                            let name = dev_name_forget.clone();
                                            this.mutate_bluetooth(
                                                "forget",
                                                format!("Forgetting {name}…"),
                                                format!("Forgot {name}."),
                                                move || bluetooth::forget_device(&ap, &dp),
                                                cx,
                                            );
                                        }),
                                    ),
                                )
                            })
                            .child(if device.connected {
                                action_chip(("bt-disconnect", i), "Disconnect", p, false).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        let dp = dev_path.clone();
                                        let name = dev_name.clone();
                                        this.mutate_bluetooth(
                                            "disconnect",
                                            format!("Disconnecting {name}…"),
                                            format!("Disconnected {name}."),
                                            move || bluetooth::disconnect_device(&dp),
                                            cx,
                                        );
                                    }),
                                )
                            } else if device.paired {
                                action_chip(("bt-connect", i), "Connect", p, true).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        let dp = dev_path.clone();
                                        let name = dev_name.clone();
                                        this.mutate_bluetooth(
                                            "connect",
                                            format!("Connecting to {name}…"),
                                            format!("Connected to {name}."),
                                            move || bluetooth::connect_device(&dp),
                                            cx,
                                        );
                                    }),
                                )
                            } else {
                                action_chip(("bt-pair", i), "Pair", p, true).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let dp = dev_path.clone();
                                        let name = dev_name.clone();
                                        this.mutate_bluetooth(
                                            "pair",
                                            format!("Pairing with {name}…"),
                                            format!("Paired with {name}."),
                                            move || bluetooth::pair_device(&dp),
                                            cx,
                                        );
                                    },
                                ))
                            }),
                    ),
            );
        }

        dev_card
    }

    fn input_page(&self, p: Palette, window: &Window, cx: &Context<Self>) -> Div {
        let mut page = div().flex().flex_col().gap_4().child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(TEXT_TITLE))
                                .font_weight(FontWeight::BOLD)
                                .child("Input"),
                        )
                        .child(
                            div()
                                .text_size(px(TEXT_BODY))
                                .text_color(rgb(p.muted))
                                .child("Keyboard layout, key repeat, and pointer feel."),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(badge(
                            if self.input_live {
                                "Live via Fulcrum"
                            } else {
                                "Saved locally"
                            },
                            p,
                            self.input_live,
                        ))
                        .child(
                            self.button(
                                CONTROL_INPUT_REFRESH,
                                if self.input_loading {
                                    "Refreshing…"
                                } else {
                                    "Refresh"
                                },
                                p,
                                window,
                                false,
                                !self.input_loading && !self.input_busy,
                            )
                            .h(px(34.))
                            .px_3()
                            .on_click(cx.listener(|this, _, w, cx| {
                                w.focus(&this.controls[CONTROL_INPUT_REFRESH]);
                                this.refresh_input(cx);
                            })),
                        ),
                ),
        );

        if self.input_loading && !self.input_loaded {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("Reading keyboard and pointer settings…"),
            );
        }

        let current = self.input.clone();

        // Keyboard card
        let keyboard_card = card(p)
            .p_4()
            .gap_3()
            .child(section_header(
                "Keyboard",
                Some("Applied immediately to active Wayland apps through Fulcrum."),
                p,
            ))
            .child(divider(p))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(TEXT_LABEL))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Keyboard layout"),
                            )
                            .child(
                                div()
                                    .font_family(FONT_MONO)
                                    .text_size(px(TEXT_TECHNICAL))
                                    .text_color(rgb(p.muted))
                                    .child(format!("xkb: {}", current.layout)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .children(LAYOUTS.into_iter().enumerate().map(|(i, (code, label))| {
                                let selected = current.layout == code;
                                let next_cfg = InputSettings {
                                    layout: code.to_string(),
                                    ..current.clone()
                                };
                                action_chip(("input-layout", i), label, p, selected).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.apply_input_settings(next_cfg.clone(), cx);
                                    }),
                                )
                            })),
                    ),
            )
            .child(divider(p))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(TEXT_LABEL))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Repeat rate"),
                            )
                            .child(
                                div()
                                    .font_family(FONT_MONO)
                                    .text_size(px(TEXT_TECHNICAL))
                                    .text_color(rgb(p.muted))
                                    .child(format!("{} keys / sec", current.repeat_rate)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .children(REPEAT_RATES.into_iter().enumerate().map(|(i, rate)| {
                                let selected = current.repeat_rate == rate;
                                let next_cfg = InputSettings {
                                    repeat_rate: rate,
                                    ..current.clone()
                                };
                                action_chip(("input-rate", i), format!("{rate}/s"), p, selected)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.apply_input_settings(next_cfg.clone(), cx);
                                    }))
                            })),
                    ),
            )
            .child(divider(p))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(TEXT_LABEL))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Repeat delay"),
                            )
                            .child(
                                div()
                                    .font_family(FONT_MONO)
                                    .text_size(px(TEXT_TECHNICAL))
                                    .text_color(rgb(p.muted))
                                    .child(format!("{} ms before repeating", current.repeat_delay)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .children(REPEAT_DELAYS.into_iter().enumerate().map(|(i, delay)| {
                                let selected = current.repeat_delay == delay;
                                let next_cfg = InputSettings {
                                    repeat_delay: delay,
                                    ..current.clone()
                                };
                                action_chip(("input-delay", i), format!("{delay} ms"), p, selected)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.apply_input_settings(next_cfg.clone(), cx);
                                    }))
                            })),
                    ),
            );

        // Pointer & Scroll card
        let nat = current.natural_scroll;
        let next_nat = InputSettings {
            natural_scroll: !nat,
            ..current.clone()
        };
        let nat_focus = self.controls[CONTROL_NATURAL_SCROLL].is_focused(window);
        let pointer_card = card(p)
            .p_4()
            .gap_3()
            .child(section_header(
                "Pointer & Scrolling",
                Some("Mouse and touchpad sensitivity and scroll direction."),
                p,
            ))
            .child(divider(p))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(TEXT_LABEL))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Pointer speed"),
                            )
                            .child(
                                div()
                                    .font_family(FONT_MONO)
                                    .text_size(px(TEXT_TECHNICAL))
                                    .text_color(rgb(p.muted))
                                    .child(format!("Offset {:+}", current.pointer_speed)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .children(POINTER_SPEEDS.into_iter().enumerate().map(
                                |(i, (speed, label))| {
                                    let selected = current.pointer_speed == speed;
                                    let next_cfg = InputSettings {
                                        pointer_speed: speed,
                                        ..current.clone()
                                    };
                                    action_chip(("input-speed", i), label, p, selected).on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.apply_input_settings(next_cfg.clone(), cx);
                                        }),
                                    )
                                },
                            )),
                    ),
            )
            .child(divider(p))
            .child(
                div()
                    .id("input-natural-scroll")
                    .track_focus(&self.controls[CONTROL_NATURAL_SCROLL])
                    .flex()
                    .items_center()
                    .gap_4()
                    .py_2()
                    .px_2()
                    .rounded(px(8.))
                    .border_2()
                    .border_color(rgb(if nat_focus { p.focus } else { p.surface }))
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.9))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(TEXT_LABEL))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Natural scrolling"),
                            )
                            .child(
                                div()
                                    .text_size(px(TEXT_SUPPORTING))
                                    .text_color(rgb(p.muted))
                                    .child("Content tracks finger movement instead of scrollbar direction."),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_SUPPORTING))
                            .text_color(rgb(p.muted))
                            .child(if nat { "On" } else { "Off" }),
                    )
                    .child(
                        div()
                            .w(px(44.))
                            .h(px(26.))
                            .px(px(3.))
                            .flex()
                            .items_center()
                            .rounded_full()
                            .bg(rgb(if nat { p.accent } else { p.line }))
                            .when(nat, |d| d.justify_end())
                            .child(div().size(px(20.)).rounded_full().bg(rgb(if nat {
                                p.on_accent
                            } else {
                                p.text
                            }))),
                    )
                    .on_click(cx.listener(move |this, _, w, cx| {
                        w.focus(&this.controls[CONTROL_NATURAL_SCROLL]);
                        this.apply_input_settings(next_nat.clone(), cx);
                    })),
            );

        page = page.child(keyboard_card).child(pointer_card);
        page
    }

    fn power_page(&self, p: Palette, window: &Window, cx: &Context<Self>) -> Div {
        let mut page = div().flex().flex_col().gap_4().child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(TEXT_TITLE))
                                .font_weight(FontWeight::BOLD)
                                .child("Power & Lock"),
                        )
                        .child(
                            div()
                                .text_size(px(TEXT_BODY))
                                .text_color(rgb(p.muted))
                                .child("Automatic screen lock and sleep timers."),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(badge(
                            if self.power_live {
                                "Live via Fulcrum"
                            } else {
                                "Saved locally"
                            },
                            p,
                            self.power_live,
                        ))
                        .child(
                            self.button(
                                CONTROL_POWER_REFRESH,
                                if self.power_loading {
                                    "Refreshing…"
                                } else {
                                    "Refresh"
                                },
                                p,
                                window,
                                false,
                                !self.power_loading && !self.power_busy,
                            )
                            .h(px(34.))
                            .px_3()
                            .on_click(cx.listener(|this, _, w, cx| {
                                w.focus(&this.controls[CONTROL_POWER_REFRESH]);
                                this.refresh_power(cx);
                            })),
                        ),
                ),
        );

        if self.power_loading && !self.power_loaded {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("Reading power and idle lock timers…"),
            );
        }

        let current = self.power.clone();

        let lock_card = card(p)
            .p_4()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(section_header(
                        "Automatic screen lock",
                        Some("Locks the session through Fulcrum when idle. Media playback can inhibit automatic locking."),
                        p,
                    ))
                    .child(badge(
                        format_timeout(current.lock_after_secs),
                        p,
                        current.lock_after_secs > 0,
                    )),
            )
            .child(divider(p))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(LOCK_TIMEOUTS.into_iter().enumerate().map(|(i, (secs, label))| {
                        let selected = current.lock_after_secs == secs;
                        let next_cfg = PowerSettings {
                            lock_after_secs: secs,
                            ..current.clone()
                        };
                        action_chip(("power-lock", i), label, p, selected).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.apply_power_settings(next_cfg.clone(), cx);
                            },
                        ))
                    })),
            );

        let suspend_card = card(p)
            .p_4()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(section_header(
                        "Automatic suspend",
                        Some("Preferred inactivity timeout before suspending the machine."),
                        p,
                    ))
                    .child(badge(
                        format_timeout(current.suspend_after_secs),
                        p,
                        current.suspend_after_secs > 0,
                    )),
            )
            .child(divider(p))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(SUSPEND_TIMEOUTS.into_iter().enumerate().map(
                        |(i, (secs, label))| {
                            let selected = current.suspend_after_secs == secs;
                            let next_cfg = PowerSettings {
                                suspend_after_secs: secs,
                                ..current.clone()
                            };
                            action_chip(("power-suspend", i), label, p, selected).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.apply_power_settings(next_cfg.clone(), cx);
                                }),
                            )
                        },
                    )),
            );

        page = page.child(lock_card).child(suspend_card);
        page
    }

    fn default_apps_page(&self, p: Palette, window: &Window, cx: &Context<Self>) -> Div {
        let mut page = div().flex().flex_col().gap_4().child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(TEXT_TITLE))
                                .font_weight(FontWeight::BOLD)
                                .child("Default Apps"),
                        )
                        .child(
                            div()
                                .text_size(px(TEXT_BODY))
                                .text_color(rgb(p.muted))
                                .child("Choose which applications open links, files, and media."),
                        ),
                )
                .child(
                    self.button(
                        CONTROL_DEFAULTS_REFRESH,
                        if self.defaults_loading {
                            "Refreshing…"
                        } else {
                            "Refresh"
                        },
                        p,
                        window,
                        false,
                        !self.defaults_loading && !self.defaults_busy,
                    )
                    .h(px(34.))
                    .px_3()
                    .on_click(cx.listener(|this, _, w, cx| {
                        w.focus(&this.controls[CONTROL_DEFAULTS_REFRESH]);
                        this.refresh_default_apps(cx);
                    })),
                ),
        );

        if self.defaults_loading && !self.defaults_loaded {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("Scanning installed applications and MIME associations…"),
            );
        }

        let mut list_card = card(p).p_4().gap_3().child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(section_header(
                    "Application handlers",
                    Some("Changes write atomically to your user mimeapps.list for xdg-open."),
                    p,
                ))
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(TEXT_TECHNICAL))
                        .text_color(rgb(p.muted))
                        .child("mimeapps.list"),
                ),
        );

        for (ci, cat) in self.default_apps.iter().enumerate() {
            let current_badge = cat
                .current
                .clone()
                .unwrap_or_else(|| "Not set".to_string());
            let mut chips = div().flex().flex_wrap().gap_2();
            if cat.candidates.is_empty() {
                chips = chips.child(
                    div()
                        .text_size(px(TEXT_SUPPORTING))
                        .text_color(rgb(p.muted))
                        .child("No registered applications found."),
                );
            } else {
                for (ai, candidate) in cat.candidates.iter().enumerate() {
                    let selected = cat.current.as_deref() == Some(candidate.desktop_id.as_str());
                    let category = cat.category;
                    let id = candidate.desktop_id.clone();
                    let name = candidate.name.clone();
                    chips = chips.child(
                        action_chip(
                            ("default-app", ci * 100 + ai),
                            candidate.name.clone(),
                            p,
                            selected,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_default_app(category, id.clone(), name.clone(), cx);
                        })),
                    );
                }
            }

            list_card = list_card.child(divider(p)).child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .py_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .text_size(px(TEXT_LABEL))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(cat.category.label()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(TEXT_SUPPORTING))
                                            .text_color(rgb(p.muted))
                                            .child(cat.category.description()),
                                    ),
                            )
                            .child(badge(current_badge, p, cat.current.is_some())),
                    )
                    .child(chips),
            );
        }

        page = page.child(list_card);
        page
    }

    fn about(&self, p: Palette, window: &Window, cx: &Context<Self>) -> Div {
        let mut page = div().flex().flex_col().gap_4().child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(TEXT_TITLE))
                                .font_weight(FontWeight::BOLD)
                                .child("About CrowbarOS"),
                        )
                        .child(
                            div()
                                .text_size(px(TEXT_BODY))
                                .text_color(rgb(p.muted))
                                .child("System summary and Studio Settings details."),
                        ),
                )
                .child(
                    self.button(
                        CONTROL_SYSTEM_REFRESH,
                        if self.system_loading {
                            "Refreshing…"
                        } else {
                            "Refresh"
                        },
                        p,
                        window,
                        false,
                        !self.system_loading,
                    )
                    .h(px(34.))
                    .px_3()
                    .on_click(cx.listener(|this, _, w, cx| {
                        w.focus(&this.controls[CONTROL_SYSTEM_REFRESH]);
                        this.refresh_system_info(cx);
                    })),
                ),
        );

        let sys_card = if let Some(info) = &self.system_info {
            let rows: [(&str, String); 7] = [
                ("Hostname", info.hostname.clone()),
                ("Operating System", info.os_name.clone()),
                ("Kernel", info.kernel.clone()),
                ("Processor", info.cpu.clone()),
                ("Memory", info.memory.clone()),
                ("Graphics", info.gpu.clone()),
                ("Root Storage", info.disk.clone()),
            ];
            let mut c = card(p)
                .p_4()
                .gap_2()
                .child(section_header("This Machine", None, p));
            for (label, value) in rows {
                c = c.child(divider(p)).child(
                    div()
                        .py_1()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .child(
                            div()
                                .text_size(px(TEXT_LABEL))
                                .text_color(rgb(p.muted))
                                .child(label),
                        )
                        .child(
                            div()
                                .font_family(FONT_MONO)
                                .text_size(px(TEXT_TECHNICAL))
                                .text_color(rgb(p.text))
                                .child(value),
                        ),
                );
            }
            c
        } else {
            card(p)
                .p_5()
                .text_size(px(TEXT_BODY))
                .text_color(rgb(p.muted))
                .child("Reading hardware and OS summary…")
        };

        let app_card = card(p)
            .p_4()
            .gap_3()
            .child(section_header(
                "Studio Settings",
                Some("Your workspace, with room to make it your own."),
                p,
            ))
            .child(divider(p))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("Configure Appearance, Displays, Network, Sound, Bluetooth, Input, Power, and Default Apps in one native Wayland surface.")
                    .child("Keyboard: Tab and Shift+Tab move focus. Enter or Space activates a control. Ctrl+S saves or applies; Ctrl+Z undoes; Ctrl+Q closes."),
            )
            .child(divider(p))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(TEXT_LABEL))
                            .child(format!("CrowbarOS · Studio Settings {}", env!("CARGO_PKG_VERSION"))),
                    )
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(TEXT_TECHNICAL))
                            .text_color(rgb(p.muted))
                            .child("Rust / GPUI 0.2.2 / Wayland"),
                    ),
            );

        page = page.child(sys_card).child(app_card);
        page
    }
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.active_palette();
        let on_displays = self.page == Page::Displays;
        let is_live_hardware = matches!(
            self.page,
            Page::Network
                | Page::Sound
                | Page::Bluetooth
                | Page::Input
                | Page::Power
                | Page::DefaultApps
                | Page::About
        );
        let active_dirty = if on_displays {
            self.displays_dirty()
        } else if self.page == Page::Appearance {
            self.appearance_dirty()
        } else {
            false
        };
        let can_primary = if on_displays {
            self.displays_dirty() && !self.busy && !self.displays_loading
        } else if self.page == Page::Appearance {
            self.appearance_dirty() && !self.load_error && !self.busy
        } else {
            false
        };
        let active_error = match self.page {
            Page::Displays => self.display_error.clone(),
            Page::Network => self.network_error.clone(),
            Page::Sound => self.sound_error.clone(),
            Page::Bluetooth => self.bluetooth_error.clone(),
            Page::Input => self.input_error.clone(),
            Page::Power => self.power_error.clone(),
            Page::DefaultApps => self.defaults_error.clone(),
            Page::Appearance | Page::About => self.error.clone(),
        };
        let status = match self.page {
            Page::Displays => {
                if !self.display_status.is_empty() {
                    self.display_status.clone()
                } else if self.displays_unavailable.is_some() {
                    "Display controls unavailable".into()
                } else if self.displays_dirty() {
                    "Pending display changes · not applied".into()
                } else if self.displays_loading {
                    "Reading displays…".into()
                } else {
                    "Display settings up to date".into()
                }
            }
            Page::Network => {
                if !self.network_status.is_empty() {
                    self.network_status.clone()
                } else if self.network_loading {
                    "Reading network interfaces…".into()
                } else if let Some(net) = &self.network {
                    match &net.wifi {
                        WifiState::Available(st) if st.connected_ssid.is_some() => format!(
                            "Wi-Fi connected · {}",
                            st.connected_ssid.as_deref().unwrap_or("")
                        ),
                        WifiState::Available(_) => {
                            if net.ethernet.iter().any(|e| e.state == network::LinkState::Up) {
                                "Ethernet connected · Wi-Fi ready".into()
                            } else {
                                "Wi-Fi ready".into()
                            }
                        }
                        WifiState::Unavailable(_) => {
                            if net.ethernet.iter().any(|e| e.state == network::LinkState::Up) {
                                "Ethernet connected · No Wi-Fi adapter".into()
                            } else {
                                "No active network connection".into()
                            }
                        }
                    }
                } else {
                    "Network ready".into()
                }
            }
            Page::Sound => {
                if !self.sound_status.is_empty() {
                    self.sound_status.clone()
                } else if self.sound_loading {
                    "Reading PipeWire audio devices…".into()
                } else if self.sound_unavailable.is_some() {
                    "PipeWire audio unavailable".into()
                } else if let Some(snd) = &self.sound {
                    if let Some(sink) = snd.default_sink() {
                        if sink.muted {
                            format!("Output: {} · Muted", sink.name)
                        } else {
                            format!("Output: {} · {}%", sink.name, sink.volume_percent())
                        }
                    } else {
                        "No output devices".into()
                    }
                } else {
                    "PipeWire ready".into()
                }
            }
            Page::Bluetooth => {
                if !self.bluetooth_status.is_empty() {
                    self.bluetooth_status.clone()
                } else if self.bluetooth_loading {
                    "Checking Bluetooth adapters…".into()
                } else if let Some(bt) = &self.bluetooth {
                    match bt {
                        BluetoothState::Available { adapter, devices } => {
                            let connected = devices.iter().filter(|d| d.connected).count();
                            if !adapter.powered {
                                "Bluetooth is off".into()
                            } else if connected > 0 {
                                format!("Bluetooth on · {connected} connected")
                            } else {
                                "Bluetooth on · Ready".into()
                            }
                        }
                        BluetoothState::Unavailable(_) => "No Bluetooth adapter detected".into(),
                    }
                } else {
                    "Bluetooth ready".into()
                }
            }
            Page::Input => {
                if !self.input_status.is_empty() {
                    self.input_status.clone()
                } else if self.input_loading {
                    "Reading input configuration…".into()
                } else {
                    format!(
                        "Layout {} · {} keys/s · Natural scroll {}",
                        self.input.layout,
                        self.input.repeat_rate,
                        if self.input.natural_scroll { "on" } else { "off" }
                    )
                }
            }
            Page::Power => {
                if !self.power_status.is_empty() {
                    self.power_status.clone()
                } else if self.power_loading {
                    "Reading power timers…".into()
                } else {
                    format!(
                        "Lock: {} · Suspend: {}",
                        format_timeout(self.power.lock_after_secs),
                        format_timeout(self.power.suspend_after_secs)
                    )
                }
            }
            Page::DefaultApps => {
                if !self.defaults_status.is_empty() {
                    self.defaults_status.clone()
                } else if self.defaults_loading {
                    "Scanning default applications…".into()
                } else {
                    format!("{} default app categories", self.default_apps.len())
                }
            }
            Page::About => {
                if self.system_loading {
                    "Reading system summary…".into()
                } else if let Some(info) = &self.system_info {
                    format!("{} · Linux {}", info.hostname, info.kernel)
                } else {
                    "CrowbarOS system summary".into()
                }
            }
            Page::Appearance => {
                if !self.status.is_empty() {
                    self.status.clone()
                } else if self.appearance_dirty() {
                    "Preview · not saved".into()
                } else {
                    "All changes saved".into()
                }
            }
        };
        let primary_label = if on_displays {
            if self.busy {
                "Applying…"
            } else {
                "Apply changes"
            }
        } else if self.busy {
            "Saving…"
        } else {
            "Save changes"
        };
        div()
            .id("studio-settings")
            .key_context("StudioSettings")
            .track_focus(&self.root)
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::reload))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(|this, _: &Next, w, cx| {
                w.focus_next();
                cx.notify();
                log::info!("STUDIO event=focus-next");
                this.status.clear();
            }))
            .on_action(cx.listener(|_, _: &Previous, w, cx| {
                w.focus_prev();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CancelClose, window, cx| {
                if this.wifi_prompt.is_some() {
                    this.clear_wifi_prompt();
                    cx.notify();
                    return;
                }
                this.confirming_close = false;
                window.focus(&this.controls[CONTROL_SAVE]);
                cx.notify();
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(p.canvas))
            .text_color(rgb(p.text))
            .font_family(FONT_UI)
            .child(titlebar(
                "Studio Settings",
                self.button(CONTROL_CLOSE, "Close", p, window, false, !self.busy)
                    .h(px(30.))
                    .on_click(cx.listener(|this, _, w, cx| this.quit(&Quit, w, cx))),
                p,
            ))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        sidebar(SIDEBAR_WIDTH, p)
                            .child(
                                self.button(
                                    CONTROL_APPEARANCE,
                                    "Appearance",
                                    p,
                                    window,
                                    self.page == Page::Appearance,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.clear_wifi_prompt();
                                    this.page = Page::Appearance;
                                    w.focus(&this.controls[CONTROL_APPEARANCE]);
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.button(
                                    CONTROL_DISPLAYS,
                                    "Displays",
                                    p,
                                    window,
                                    self.page == Page::Displays,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.clear_wifi_prompt();
                                    this.page = Page::Displays;
                                    w.focus(&this.controls[CONTROL_DISPLAYS]);
                                    if !this.displays_loaded && !this.displays_loading {
                                        this.refresh_displays(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.button(
                                    CONTROL_NETWORK,
                                    "Network",
                                    p,
                                    window,
                                    self.page == Page::Network,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.page = Page::Network;
                                    w.focus(&this.controls[CONTROL_NETWORK]);
                                    if !this.network_loaded && !this.network_loading {
                                        this.refresh_network(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.button(
                                    CONTROL_SOUND,
                                    "Sound",
                                    p,
                                    window,
                                    self.page == Page::Sound,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.clear_wifi_prompt();
                                    this.page = Page::Sound;
                                    w.focus(&this.controls[CONTROL_SOUND]);
                                    this.start_sound_watcher(cx);
                                    if !this.sound_loaded && !this.sound_loading {
                                        this.refresh_sound(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.button(
                                    CONTROL_BLUETOOTH,
                                    "Bluetooth",
                                    p,
                                    window,
                                    self.page == Page::Bluetooth,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.clear_wifi_prompt();
                                    this.page = Page::Bluetooth;
                                    w.focus(&this.controls[CONTROL_BLUETOOTH]);
                                    if !this.bluetooth_loaded && !this.bluetooth_loading {
                                        this.refresh_bluetooth(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.button(
                                    CONTROL_INPUT,
                                    "Input",
                                    p,
                                    window,
                                    self.page == Page::Input,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.clear_wifi_prompt();
                                    this.page = Page::Input;
                                    w.focus(&this.controls[CONTROL_INPUT]);
                                    if !this.input_loaded && !this.input_loading {
                                        this.refresh_input(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.button(
                                    CONTROL_POWER,
                                    "Power",
                                    p,
                                    window,
                                    self.page == Page::Power,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.clear_wifi_prompt();
                                    this.page = Page::Power;
                                    w.focus(&this.controls[CONTROL_POWER]);
                                    if !this.power_loaded && !this.power_loading {
                                        this.refresh_power(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.button(
                                    CONTROL_DEFAULT_APPS,
                                    "Default Apps",
                                    p,
                                    window,
                                    self.page == Page::DefaultApps,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.clear_wifi_prompt();
                                    this.page = Page::DefaultApps;
                                    w.focus(&this.controls[CONTROL_DEFAULT_APPS]);
                                    if !this.defaults_loaded && !this.defaults_loading {
                                        this.refresh_default_apps(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.button(
                                    CONTROL_ABOUT,
                                    "About",
                                    p,
                                    window,
                                    self.page == Page::About,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.clear_wifi_prompt();
                                    this.page = Page::About;
                                    w.focus(&this.controls[CONTROL_ABOUT]);
                                    if !this.system_loaded && !this.system_loading {
                                        this.refresh_system_info(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(p.muted))
                                    .child("CrowbarOS"),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .id("settings-content")
                                    .flex_1()
                                    .min_h(px(0.))
                                    .overflow_y_scroll()
                                    .p_6()
                                    .child(match self.page {
                                        Page::Appearance => self.appearance(p, window, cx),
                                        Page::Displays => self.displays(p, window, cx),
                                        Page::Network => self.network_page(p, window, cx),
                                        Page::Sound => self.sound_page(p, window, cx),
                                        Page::Bluetooth => self.bluetooth_page(p, window, cx),
                                        Page::Input => self.input_page(p, window, cx),
                                        Page::Power => self.power_page(p, window, cx),
                                        Page::DefaultApps => self.default_apps_page(p, window, cx),
                                        Page::About => self.about(p, window, cx),
                                    }),
                            )
                            .when(active_error.is_some(), |d| {
                                d.child(
                                    div()
                                        .px_6()
                                        .py_3()
                                        .border_t_1()
                                        .border_color(rgb(p.line))
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .flex_shrink_0()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .text_size(px(12.))
                                                .child(active_error.clone().unwrap_or_default()),
                                        )
                                        .child(
                                            self.button(
                                                CONTROL_RELOAD,
                                                if on_displays || is_live_hardware {
                                                    "Refresh"
                                                } else {
                                                    "Reload saved"
                                                },
                                                p,
                                                window,
                                                false,
                                                !self.busy
                                                    && !self.displays_loading
                                                    && !self.network_loading
                                                    && !self.sound_loading
                                                    && !self.bluetooth_loading,
                                            )
                                            .flex_shrink_0()
                                            .on_click(
                                                cx.listener(|this, _, w, cx| {
                                                    this.reload(&Reload, w, cx)
                                                }),
                                            ),
                                        ),
                                )
                            })
                            .child(
                                status_footer(
                                    if self.confirming_close {
                                        "Discard unsaved changes?".into()
                                    } else {
                                        status
                                    },
                                    p,
                                )
                                .when(self.confirming_close, |d| {
                                    d.child(
                                        self.button(
                                            CONTROL_KEEP_EDITING,
                                            "Keep editing",
                                            p,
                                            window,
                                            false,
                                            true,
                                        )
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirming_close = false;
                                            window.focus(&this.controls[CONTROL_SAVE]);
                                            cx.notify();
                                        })),
                                    )
                                    .child(
                                        self.button(
                                            CONTROL_DISCARD_CLOSE,
                                            "Discard & close",
                                            p,
                                            window,
                                            true,
                                            true,
                                        )
                                        .on_click(|_, _, cx| cx.quit()),
                                    )
                                })
                                .when(!self.confirming_close && is_live_hardware, |d| {
                                    d.child(
                                        self.button(
                                            CONTROL_FOOTER_REFRESH,
                                            "Refresh",
                                            p,
                                            window,
                                            false,
                                            !self.network_loading
                                                && !self.sound_loading
                                                && !self.bluetooth_loading
                                                && !self.input_loading
                                                && !self.power_loading
                                                && !self.defaults_loading
                                                && !self.system_loading
                                                && !self.network_busy
                                                && !self.sound_busy
                                                && !self.bluetooth_busy
                                                && !self.input_busy
                                                && !self.power_busy
                                                && !self.defaults_busy,
                                        )
                                        .on_click(cx.listener(|this, _, w, cx| {
                                            this.reload(&Reload, w, cx)
                                        })),
                                    )
                                })
                                .when(!self.confirming_close && !is_live_hardware, |d| {
                                    d.child(
                                        self.button(
                                            CONTROL_UNDO,
                                            "Undo",
                                            p,
                                            window,
                                            false,
                                            active_dirty && !self.busy,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, w, cx| {
                                                this.undo(&Undo, w, cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        self.button(
                                            CONTROL_SAVE,
                                            primary_label,
                                            p,
                                            window,
                                            true,
                                            can_primary,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, w, cx| {
                                                this.save(&Save, w, cx)
                                            }),
                                        ),
                                    )
                                }),
                            ),
                    ),
            )
    }
}

fn cli() -> Result<bool, String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return Ok(false);
    }
    match args.as_slice() {
        [arg] if arg == "--version" => println!(
            "crowbaros-studio-settings {} (GPUI 0.2.2; Wayland)",
            env!("CARGO_PKG_VERSION")
        ),
        [arg] if arg == "--print-settings" => println!(
            "{}",
            serde_json::to_string_pretty(&Store::for_user()?.load()?.unwrap_or_default())
                .map_err(|e| e.to_string())?
        ),
        [arg] if arg == "--apply-session" => {
            if let Some(p) = Store::for_user()?.load()? {
                session::restore_motion(p.reduced_motion)?;
            }
        }
        _ => {
            return Err(
                "Usage: crowbaros-studio-settings [--version|--print-settings|--apply-session]"
                    .into(),
            );
        }
    }
    Ok(true)
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    match cli() {
        Ok(true) => return,
        Err(e) => {
            eprintln!("Studio Settings: {e}");
            std::process::exit(1);
        }
        _ => (),
    }
    let store = Store::for_user().unwrap_or_else(|e| {
        eprintln!("Studio Settings: {e}");
        std::process::exit(1)
    });
    Application::new().run(move |cx: &mut App| {
        load_fonts(cx);
        cx.bind_keys([
            KeyBinding::new("tab", Next, Some("StudioSettings")),
            KeyBinding::new("shift-tab", Previous, Some("StudioSettings")),
            KeyBinding::new("ctrl-s", Save, Some("StudioSettings")),
            KeyBinding::new("ctrl-z", Undo, Some("StudioSettings")),
            KeyBinding::new("ctrl-q", Quit, Some("StudioSettings")),
            KeyBinding::new("escape", CancelClose, Some("StudioSettings")),
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(980.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(800.), px(640.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Studio Settings".into()),
                    ..Default::default()
                }),
                app_id: Some("com.crowbaros.StudioSettings".into()),
                ..Default::default()
            },
            |w, cx| {
                let entity = cx.new(|cx| Settings::new(store, w, cx));
                let weak = entity.downgrade();
                w.on_window_should_close(cx, move |window, cx| {
                    weak.update(cx, |this, cx| {
                        if this.busy {
                            return false;
                        }
                        if this.has_unsaved_edits() {
                            this.confirming_close = true;
                            window.focus(&this.controls[CONTROL_KEEP_EDITING]);
                            cx.notify();
                            false
                        } else {
                            true
                        }
                    })
                    .unwrap_or(true)
                });
                entity
            },
        )
        .expect("Wayland window must open");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_tab_order_preserves_theme_indices_and_shifts_remaining() {
        assert_eq!(CONTROL_APPEARANCE, 0);
        assert_eq!(CONTROL_ABOUT, 1);
        assert_eq!(CONTROL_THEME_FIRST, 2);
        assert_eq!(CONTROL_THEME_FIRST + Theme::ALL.len(), CONTROL_WALLPAPER_FIRST);
        assert_eq!(CONTROL_WALLPAPER_FIRST, 5);
        assert_eq!(
            CONTROL_WALLPAPER_FIRST + Wallpaper::ALL.len(),
            CONTROL_STUDIO
        );
        assert_eq!(CONTROL_STUDIO, 8);
        assert_eq!(CONTROL_REDUCED_MOTION, 9);
        assert_eq!(CONTROL_UNDO, 10);
        assert_eq!(CONTROL_SAVE, 11);
        assert_eq!(CONTROL_RELOAD, 12);
        assert_eq!(CONTROL_CLOSE, 13);
        assert_eq!(CONTROL_KEEP_EDITING, 14);
        assert_eq!(CONTROL_DISCARD_CLOSE, 15);
        assert_eq!(CONTROL_DISPLAYS, 16);
        assert_eq!(CONTROL_DISPLAY_REFRESH, 17);
        assert_eq!(CONTROL_NETWORK, 18);
        assert_eq!(CONTROL_SOUND, 19);
        assert_eq!(CONTROL_BLUETOOTH, 20);
        assert_eq!(CONTROL_INPUT, 29);
        assert_eq!(CONTROL_POWER, 30);
        assert_eq!(CONTROL_DEFAULT_APPS, 31);
        assert_eq!(CONTROL_COUNT, 37);
    }

    #[test]
    fn wallpaper_paths_resolve_in_checkout() {
        for wallpaper in Wallpaper::ALL {
            let path = wallpaper_path(wallpaper)
                .unwrap_or_else(|| panic!("expected preview asset for {wallpaper:?}"));
            assert!(path.is_file(), "resolved path must exist: {}", path.display());
        }
    }
}
