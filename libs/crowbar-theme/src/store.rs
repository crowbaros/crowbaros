// SPDX-License-Identifier: MIT
//! Theme discovery, persistence, atomic activation, and multi-background management.

use crate::{
    ActiveTheme, Appearance, SCHEMA_VERSION, Theme, ThemeColors,
    builtin::{builtin_order, builtin_themes, is_builtin_id, nullkeel_dark},
    generators::{
        LegacyAppearance, build_legacy_appearance, legacy_appearance_to_theme_id, legacy_wallpaper_to_background,
        render_foot_ini, render_gtk_css,
    },
    slugify, validate_filename, validate_slug,
};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_JSON_BYTES: u64 = 256 * 1024;
const SUPPORTED_IMAGE_EXTS: [&str; 6] = ["png", "jpg", "jpeg", "webp", "bmp", "gif"];
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Manages built-in, system, and user themes plus live activation on disk.
#[derive(Clone, Debug)]
pub struct ThemeStore {
    config_dir: PathBuf,
    system_dir: PathBuf,
    xdg_config_dir: PathBuf,
}

impl ThemeStore {
    /// Create a store with explicit `config_dir` (e.g. `<xdg>/crowbaros`) and `system_dir`
    /// (e.g. `/usr/share/crowbaros/themes`).
    ///
    /// If `config_dir` ends with `"crowbaros"`, per-app config files (`foot/theme.ini`,
    /// `gtk-3.0/colors.css`, `gtk-4.0/colors.css`) are written under its parent directory;
    /// otherwise they are written inside `config_dir` so tests stay fully isolated.
    pub fn new(config_dir: PathBuf, system_dir: PathBuf) -> Self {
        let xdg_config_dir = if config_dir.file_name() == Some(OsStr::new("crowbaros")) {
            config_dir
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| config_dir.clone())
        } else {
            config_dir.clone()
        };
        Self {
            config_dir,
            system_dir,
            xdg_config_dir,
        }
    }

    /// Override the XDG config root where per-app theme files (`foot/`, `gtk-3.0/`, `gtk-4.0/`)
    /// are generated.
    pub fn with_xdg_config_dir(mut self, xdg_config_dir: PathBuf) -> Self {
        self.xdg_config_dir = xdg_config_dir;
        self
    }

    /// Create a store for the current person's account using `$XDG_CONFIG_HOME/crowbaros`
    /// (falling back to `$HOME/.config/crowbaros`) and `/usr/share/crowbaros/themes`.
    pub fn for_user() -> Result<Self, String> {
        let xdg_config_dir = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|p| PathBuf::from(p).is_absolute())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .ok_or_else(|| "Cannot determine configuration directory (HOME/XDG_CONFIG_HOME unset).".to_string())?;
        Ok(Self {
            config_dir: xdg_config_dir.join("crowbaros"),
            system_dir: PathBuf::from("/usr/share/crowbaros/themes"),
            xdg_config_dir,
        })
    }

    /// Directory holding `current-theme.json`, `appearance.json`, and `themes/`.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// System themes directory (default `/usr/share/crowbaros/themes`).
    pub fn system_dir(&self) -> &Path {
        &self.system_dir
    }

    /// XDG config directory holding `foot/`, `gtk-3.0/`, and `gtk-4.0/`.
    pub fn xdg_config_dir(&self) -> &Path {
        &self.xdg_config_dir
    }

    /// Path to `<config_dir>/current-theme.json`.
    pub fn current_theme_path(&self) -> PathBuf {
        self.config_dir.join("current-theme.json")
    }

    /// Path to `<config_dir>/appearance.json`.
    pub fn appearance_path(&self) -> PathBuf {
        self.config_dir.join("appearance.json")
    }

    /// Path to `<config_dir>/themes`.
    pub fn user_themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }

    /// List all available themes: embedded built-ins merged with `system_dir` and user
    /// overrides/custom themes in `<config_dir>/themes/<slug>/theme.json`.
    ///
    /// Built-in themes appear first in canonical order, followed by extra system themes
    /// and custom themes sorted alphabetically by name.
    pub fn list_themes(&self) -> Vec<Theme> {
        let mut by_id: BTreeMap<String, Theme> = BTreeMap::new();

        // 1. Embedded built-ins.
        for theme in builtin_themes() {
            by_id.insert(theme.id.clone(), theme);
        }

        // 2. System-installed themes (may supply root_dir or additional system themes).
        for mut theme in scan_theme_dir(&self.system_dir) {
            theme.builtin = true;
            by_id.insert(theme.id.clone(), theme);
        }

        // 3. User custom themes / overrides.
        for mut theme in scan_theme_dir(&self.user_themes_dir()) {
            theme.builtin = is_builtin_id(&theme.id);
            by_id.insert(theme.id.clone(), theme);
        }

        let mut themes: Vec<Theme> = by_id.into_values().collect();
        themes.sort_by(|a, b| {
            let ao = builtin_order(&a.id);
            let bo = builtin_order(&b.id);
            match (ao, bo) {
                (Some(ai), Some(bi)) => ai.cmp(&bi),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => b
                    .builtin
                    .cmp(&a.builtin)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                    .then_with(|| a.id.cmp(&b.id)),
            }
        });
        themes
    }

    /// Look up a theme by validated slug ID.
    pub fn get_theme(&self, theme_id: &str) -> Option<Theme> {
        validate_slug(theme_id).ok()?;
        self.list_themes().into_iter().find(|t| t.id == theme_id)
    }

    /// Resolve a background filename for `theme` to an existing file on disk, if available.
    ///
    /// Search order:
    /// 1. `theme.root_dir/backgrounds/<bg>`
    /// 2. `<system_dir>/<theme.id>/backgrounds/<bg>`
    /// 3. `/usr/share/crowbaros/themes/<theme.id>/backgrounds/<bg>`
    /// 4. `/usr/share/crowbaros-shell/<bg>` (mapping `signal-feather.png` -> `wallpaper.png`)
    /// 5. Repo fallback `apps/shell/assets/<bg>` (and brand-kit wallpapers)
    pub fn resolve_background_path(&self, theme: &Theme, bg: Option<&str>) -> Option<PathBuf> {
        let filename = match bg {
            Some(name) => {
                validate_filename(name).ok()?;
                name
            }
            None => theme.backgrounds.first().map(String::as_str)?,
        };
        if validate_filename(filename).is_err() {
            return None;
        }

        // 1. Theme's own directory.
        if let Some(root) = &theme.root_dir {
            let candidate = root.join("backgrounds").join(filename);
            if candidate.is_file() {
                return Some(candidate);
            }
        }

        // 2. Configured system_dir.
        let sys_candidate = self.system_dir.join(&theme.id).join("backgrounds").join(filename);
        if sys_candidate.is_file() {
            return Some(sys_candidate);
        }

        // 3. Standard installed system themes directory.
        let installed_theme_bg = PathBuf::from("/usr/share/crowbaros/themes")
            .join(&theme.id)
            .join("backgrounds")
            .join(filename);
        if installed_theme_bg.is_file() {
            return Some(installed_theme_bg);
        }

        // 4. Installed shell assets directory (mapping signal-feather.png -> wallpaper.png).
        let shell_dir = Path::new("/usr/share/crowbaros-shell");
        if let Some(found) = check_shell_asset_dir(shell_dir, filename) {
            return Some(found);
        }

        // 5. Repo fallback: apps/shell/assets and docs/branding/crowbaros-brand-kit/wallpapers.
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_shell_assets = manifest_dir.join("../../apps/shell/assets");
        if let Some(found) = check_shell_asset_dir(&repo_shell_assets, filename) {
            return Some(found);
        }

        let brand_kit_dir = manifest_dir.join("../../docs/branding/crowbaros-brand-kit/wallpapers");
        let brand_name = match filename {
            "quiet-signal.png" => "quiet-signal-4k.png",
            other => other,
        };
        let brand_candidate = brand_kit_dir.join(brand_name);
        if brand_candidate.is_file() {
            return Some(brand_candidate);
        }

        None
    }

    /// Return all resolvable `(filename, path)` background entries for `theme`.
    pub fn list_background_paths(&self, theme: &Theme) -> Vec<(String, PathBuf)> {
        let mut seen = HashSet::new();
        let mut names: Vec<String> = Vec::new();

        for bg in &theme.backgrounds {
            if validate_filename(bg).is_ok() && seen.insert(bg.clone()) {
                names.push(bg.clone());
            }
        }

        if let Some(root) = &theme.root_dir {
            let bg_dir = root.join("backgrounds");
            if let Ok(entries) = fs::read_dir(bg_dir) {
                let mut extra: Vec<String> = entries
                    .flatten()
                    .filter(|e| e.path().is_file())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| validate_filename(n).is_ok() && has_supported_image_ext(Path::new(n)))
                    .collect();
                extra.sort();
                for name in extra {
                    if seen.insert(name.clone()) {
                        names.push(name);
                    }
                }
            }
        }

        names
            .into_iter()
            .filter_map(|name| {
                self.resolve_background_path(theme, Some(&name))
                    .map(|path| (name, path))
            })
            .collect()
    }

    /// Load the active theme state from `<config_dir>/current-theme.json`, falling back to
    /// `<config_dir>/appearance.json` (or preferring `appearance.json` if it was updated more
    /// recently than `current-theme.json`) and finally to `nullkeel-dark`.
    pub fn load_active(&self) -> ActiveTheme {
        let legacy_is_newer = match (
            file_mtime(&self.current_theme_path()),
            file_mtime(&self.appearance_path()),
        ) {
            (Some(cur), Some(leg)) => leg > cur,
            _ => false,
        };

        if !legacy_is_newer && let Some(mut active) = self.read_current_theme() {
            // Re-resolve background path if missing or stale on disk.
            let needs_resolve = active.background_path.as_ref().map(|p| !p.is_file()).unwrap_or(true);
            if needs_resolve
                && let Some(bg) = active.background.as_deref()
                && let Some(theme) = self.get_theme(&active.theme_id)
            {
                active.background_path = self.resolve_background_path(&theme, Some(bg));
            }
            return active;
        }

        if let Some(legacy) = self.read_legacy_appearance() {
            let theme_id = legacy_appearance_to_theme_id(legacy.theme);
            let theme = self.get_theme(theme_id).unwrap_or_else(nullkeel_dark);
            let bg = legacy_wallpaper_to_background(&legacy.wallpaper).to_string();
            let background_path = self.resolve_background_path(&theme, Some(&bg));
            let colors = if legacy.studio {
                ThemeColors::studio_neutral()
            } else {
                theme.colors.clone()
            };
            return ActiveTheme {
                schema_version: SCHEMA_VERSION,
                theme_id: theme.id,
                theme_name: theme.name,
                appearance: theme.appearance,
                studio: legacy.studio,
                background: Some(bg),
                background_path,
                colors,
                generation: 0,
            };
        }

        if let Some(mut active) = self.read_current_theme() {
            let needs_resolve = active.background_path.as_ref().map(|p| !p.is_file()).unwrap_or(true);
            if needs_resolve
                && let Some(bg) = active.background.as_deref()
                && let Some(theme) = self.get_theme(&active.theme_id)
            {
                active.background_path = self.resolve_background_path(&theme, Some(bg));
            }
            return active;
        }

        let fallback = self.get_theme("nullkeel-dark").unwrap_or_else(nullkeel_dark);
        let bg = fallback.backgrounds.first().cloned();
        let background_path = bg
            .as_deref()
            .and_then(|name| self.resolve_background_path(&fallback, Some(name)));
        ActiveTheme {
            schema_version: SCHEMA_VERSION,
            theme_id: fallback.id,
            theme_name: fallback.name,
            appearance: fallback.appearance,
            studio: false,
            background: bg,
            background_path,
            colors: fallback.colors,
            generation: 0,
        }
    }

    /// Return the modification time of the active theme file (`current-theme.json`,
    /// falling back to `appearance.json`), for lightweight change polling.
    pub fn active_mtime(&self) -> Option<SystemTime> {
        let path = self.active_watch_path()?;
        file_mtime(&path)
    }

    /// Return a fast `(mtime_nanos, byte_len)` fingerprint of the active theme file
    /// for live change detection without parsing JSON.
    pub fn active_fingerprint(&self) -> Option<(u64, u64)> {
        let path = self.active_watch_path()?;
        let meta = fs::metadata(path).ok()?;
        let modified = meta.modified().ok()?;
        let dur = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
        let nanos = dur
            .as_secs()
            .saturating_mul(1_000_000_000)
            .saturating_add(dur.subsec_nanos() as u64);
        Some((nanos, meta.len()))
    }

    fn active_watch_path(&self) -> Option<PathBuf> {
        let current = self.current_theme_path();
        let legacy = self.appearance_path();
        match (file_mtime(&current), file_mtime(&legacy)) {
            (Some(c), Some(l)) => Some(if l > c { legacy } else { current }),
            (Some(_), None) => Some(current),
            (None, Some(_)) => Some(legacy),
            (None, None) => None,
        }
    }

    /// Activate `theme_id` with optional `background` filename and `studio` override.
    ///
    /// Atomically updates `<config_dir>/current-theme.json`, syncs backwards-compatible
    /// `<config_dir>/appearance.json`, and generates per-app theme files for Foot and GTK 3/4.
    pub fn apply_theme(
        &self,
        theme_id: &str,
        background: Option<&str>,
        studio: Option<bool>,
    ) -> Result<ActiveTheme, String> {
        validate_slug(theme_id)?;
        if let Some(bg) = background {
            validate_filename(bg)?;
        }

        let theme = self
            .get_theme(theme_id)
            .ok_or_else(|| format!("Theme '{theme_id}' not found."))?;

        let prev_current = self.read_current_theme();
        let prev_legacy = self.read_legacy_appearance();
        let prev_active = self.load_active();

        let studio_enabled = studio.unwrap_or(prev_active.studio);

        let selected_bg: Option<String> = match background {
            Some(bg) => Some(bg.to_string()),
            None => {
                if prev_active.theme_id == theme.id
                    && let Some(curr_bg) = prev_active.background.as_ref()
                    && theme.backgrounds.iter().any(|b| b == curr_bg)
                {
                    Some(curr_bg.clone())
                } else {
                    theme.backgrounds.first().cloned()
                }
            }
        };

        let background_path = selected_bg
            .as_deref()
            .and_then(|bg| self.resolve_background_path(&theme, Some(bg)));

        let colors = if studio_enabled {
            ThemeColors::studio_neutral()
        } else {
            theme.colors.clone()
        };

        let generation = prev_current
            .as_ref()
            .map(|p| p.generation.saturating_add(1))
            .unwrap_or(1)
            .max(1);

        let active = ActiveTheme {
            schema_version: SCHEMA_VERSION,
            theme_id: theme.id.clone(),
            theme_name: theme.name.clone(),
            appearance: theme.appearance,
            studio: studio_enabled,
            background: selected_bg,
            background_path,
            colors,
            generation,
        };

        // 1. Sync strict schema-1 appearance.json first (exact 5 fields for studio-settings compatibility).
        let legacy = build_legacy_appearance(&active, prev_legacy.as_ref());
        let mut legacy_bytes =
            serde_json::to_vec_pretty(&legacy).map_err(|e| format!("Failed to serialize appearance.json: {e}"))?;
        legacy_bytes.push(b'\n');
        atomic_write(&self.appearance_path(), &legacy_bytes)?;

        // 2. Atomically write current-theme.json last so its mtime is always >= appearance.json.
        let mut active_bytes =
            serde_json::to_vec_pretty(&active).map_err(|e| format!("Failed to serialize active theme: {e}"))?;
        active_bytes.push(b'\n');
        atomic_write(&self.current_theme_path(), &active_bytes)?;

        // 3. Generate Omarchy-style per-app config files.
        let foot_ini = render_foot_ini(&active);
        atomic_write(&self.xdg_config_dir.join("foot/theme.ini"), foot_ini.as_bytes())?;

        let gtk_css = render_gtk_css(&active);
        atomic_write(&self.xdg_config_dir.join("gtk-3.0/colors.css"), gtk_css.as_bytes())?;
        atomic_write(&self.xdg_config_dir.join("gtk-4.0/colors.css"), gtk_css.as_bytes())?;

        Ok(active)
    }

    /// Advance to the next background in the active theme's `backgrounds` list and reapply.
    pub fn cycle_background(&self) -> Result<ActiveTheme, String> {
        let active = self.load_active();
        let theme = self
            .get_theme(&active.theme_id)
            .ok_or_else(|| format!("Active theme '{}' no longer exists.", active.theme_id))?;

        if theme.backgrounds.is_empty() {
            return Err(format!("Theme '{}' has no backgrounds to cycle.", theme.id));
        }

        let next_idx = active
            .background
            .as_deref()
            .and_then(|curr| theme.backgrounds.iter().position(|b| b == curr))
            .map(|i| (i + 1) % theme.backgrounds.len())
            .unwrap_or(0);
        let next_bg = &theme.backgrounds[next_idx];

        self.apply_theme(&theme.id, Some(next_bg), Some(active.studio))
    }

    /// Save a custom theme under `<config_dir>/themes/<slug>/`, copying valid background images
    /// into `<config_dir>/themes/<slug>/backgrounds/`.
    pub fn save_custom_theme(
        &self,
        name: &str,
        colors: ThemeColors,
        appearance: Appearance,
        background_sources: &[PathBuf],
    ) -> Result<Theme, String> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("Theme name must not be empty.".into());
        }
        if trimmed.contains("..") || trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0') {
            return Err(format!("Invalid theme name '{name}': path characters are not allowed."));
        }

        let slug = slugify(trimmed);
        validate_slug(&slug)?;

        let theme_dir = self.user_themes_dir().join(&slug);
        let bg_dir = theme_dir.join("backgrounds");
        fs::create_dir_all(&bg_dir)
            .map_err(|e| format!("Failed to create theme directory '{}': {e}", bg_dir.display()))?;

        let mut backgrounds = Vec::with_capacity(background_sources.len());
        for src in background_sources {
            let copied_name = copy_background_image(src, &bg_dir, &backgrounds)?;
            backgrounds.push(copied_name);
        }

        let theme = Theme {
            schema_version: SCHEMA_VERSION,
            id: slug.clone(),
            name: trimmed.to_string(),
            author: "Custom".to_string(),
            appearance,
            colors,
            backgrounds,
            root_dir: Some(theme_dir.clone()),
            builtin: is_builtin_id(&slug),
        };

        write_theme_json(&theme_dir, &theme)?;
        Ok(theme)
    }

    /// Add a background image to an existing theme (creating a user override directory if needed).
    pub fn add_background(&self, theme_id: &str, image_path: &Path) -> Result<Theme, String> {
        validate_slug(theme_id)?;
        let mut theme = self
            .get_theme(theme_id)
            .ok_or_else(|| format!("Theme '{theme_id}' not found."))?;

        let theme_dir = self.user_themes_dir().join(theme_id);
        let bg_dir = theme_dir.join("backgrounds");
        fs::create_dir_all(&bg_dir)
            .map_err(|e| format!("Failed to create backgrounds directory '{}': {e}", bg_dir.display()))?;

        let filename = copy_background_image(image_path, &bg_dir, &theme.backgrounds)?;
        theme.backgrounds.push(filename);
        theme.root_dir = Some(theme_dir.clone());
        write_theme_json(&theme_dir, &theme)?;
        Ok(theme)
    }

    /// Remove a background filename from a theme and delete its copied file from the user theme
    /// directory if present.
    pub fn remove_background(&self, theme_id: &str, filename: &str) -> Result<Theme, String> {
        validate_slug(theme_id)?;
        validate_filename(filename)?;

        let mut theme = self
            .get_theme(theme_id)
            .ok_or_else(|| format!("Theme '{theme_id}' not found."))?;

        let pos = theme
            .backgrounds
            .iter()
            .position(|b| b == filename)
            .ok_or_else(|| format!("Background '{filename}' not found in theme '{theme_id}'."))?;
        theme.backgrounds.remove(pos);

        let theme_dir = self.user_themes_dir().join(theme_id);
        let bg_file = theme_dir.join("backgrounds").join(filename);
        if bg_file.is_file() {
            let _ = fs::remove_file(&bg_file);
        }

        theme.root_dir = Some(theme_dir.clone());
        write_theme_json(&theme_dir, &theme)?;
        Ok(theme)
    }

    /// Delete a user-created custom theme. Refuses to delete built-in themes.
    pub fn delete_custom_theme(&self, theme_id: &str) -> Result<(), String> {
        validate_slug(theme_id)?;
        if is_builtin_id(theme_id) {
            return Err(format!("Cannot delete built-in theme '{theme_id}'."));
        }

        let theme_dir = self.user_themes_dir().join(theme_id);
        if !theme_dir.exists() {
            return Err(format!("Custom theme '{theme_id}' not found."));
        }

        fs::remove_dir_all(&theme_dir).map_err(|e| format!("Failed to delete theme '{theme_id}': {e}"))?;

        if let Some(active) = self.read_current_theme()
            && active.theme_id == theme_id
        {
            let _ = self.apply_theme("nullkeel-dark", None, Some(active.studio));
        }

        Ok(())
    }

    fn read_current_theme(&self) -> Option<ActiveTheme> {
        let bytes = read_bounded(&self.current_theme_path(), MAX_JSON_BYTES).ok()?;
        let active: ActiveTheme = serde_json::from_slice(&bytes).ok()?;
        (active.schema_version == SCHEMA_VERSION).then_some(active)
    }

    fn read_legacy_appearance(&self) -> Option<LegacyAppearance> {
        let bytes = read_bounded(&self.appearance_path(), MAX_JSON_BYTES).ok()?;
        let legacy: LegacyAppearance = serde_json::from_slice(&bytes).ok()?;
        (legacy.schema_version == SCHEMA_VERSION).then_some(legacy)
    }
}

fn check_shell_asset_dir(dir: &Path, filename: &str) -> Option<PathBuf> {
    let direct = dir.join(filename);
    if direct.is_file() {
        return Some(direct);
    }
    if filename == "signal-feather.png" {
        let mapped = dir.join("wallpaper.png");
        if mapped.is_file() {
            return Some(mapped);
        }
    }
    None
}

fn scan_theme_dir(dir: &Path) -> Vec<Theme> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(folder_name) = entry.file_name().into_string().ok() else {
            continue;
        };
        if validate_slug(&folder_name).is_err() {
            continue;
        }
        let manifest = path.join("theme.json");
        let Ok(bytes) = read_bounded(&manifest, MAX_JSON_BYTES) else {
            continue;
        };
        let Ok(mut theme) = serde_json::from_slice::<Theme>(&bytes) else {
            continue;
        };
        if theme.schema_version != SCHEMA_VERSION || validate_slug(&theme.id).is_err() {
            continue;
        }
        theme.backgrounds.retain(|bg| validate_filename(bg).is_ok());
        theme.root_dir = Some(path);
        out.push(theme);
    }
    out
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    if !path.is_file() {
        return None;
    }
    fs::metadata(path).ok()?.modified().ok()
}

fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open '{}': {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read '{}': {e}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "File '{}' exceeds the {} byte limit.",
            path.display(),
            max_bytes
        ));
    }
    Ok(bytes)
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Invalid path '{}'.", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory '{}': {e}", parent.display()))?;

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    let temp_path = parent.join(format!(".{file_name}.{}.{stamp}.{seq}.tmp", std::process::id()));

    let write_res = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|e| format!("Failed to create temp file '{}': {e}", temp_path.display()))?;
        file.write_all(data)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("Failed to write '{}': {e}", temp_path.display()))?;
        fs::rename(&temp_path, path).map_err(|e| format!("Failed to commit '{}': {e}", path.display()))?;
        Ok(())
    })();

    if write_res.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    write_res
}

fn write_theme_json(theme_dir: &Path, theme: &Theme) -> Result<(), String> {
    let mut bytes =
        serde_json::to_vec_pretty(theme).map_err(|e| format!("Failed to serialize theme '{}': {e}", theme.id))?;
    bytes.push(b'\n');
    atomic_write(&theme_dir.join("theme.json"), &bytes)
}

fn has_supported_image_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SUPPORTED_IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn copy_background_image(src: &Path, dest_dir: &Path, existing: &[String]) -> Result<String, String> {
    if !src.is_file() {
        return Err(format!(
            "Background image '{}' does not exist or is not a file.",
            src.display()
        ));
    }
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| SUPPORTED_IMAGE_EXTS.contains(&e.as_str()))
        .ok_or_else(|| {
            format!(
                "Unsupported background image format '{}': expected png, jpg, jpeg, webp, bmp, or gif.",
                src.display()
            )
        })?;

    let meta = fs::metadata(src).map_err(|e| format!("Cannot inspect background image '{}': {e}", src.display()))?;
    if meta.len() == 0 {
        return Err(format!("Background image '{}' is empty.", src.display()));
    }

    let raw_stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("background");
    let clean_stem = {
        let s = slugify(raw_stem);
        if s.is_empty() { "background".to_string() } else { s }
    };

    let mut candidate = format!("{clean_stem}.{ext}");
    let mut suffix = 2u32;
    while existing.iter().any(|n| n == &candidate) || dest_dir.join(&candidate).exists() {
        candidate = format!("{clean_stem}-{suffix}.{ext}");
        suffix += 1;
    }
    validate_filename(&candidate)?;

    let bytes = fs::read(src).map_err(|e| format!("Failed to read background image '{}': {e}", src.display()))?;
    atomic_write(&dest_dir.join(&candidate), &bytes)?;
    Ok(candidate)
}
