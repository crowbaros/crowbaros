// SPDX-License-Identifier: MIT
use crowbar_theme::{Appearance, ThemeStore, extract, images};
use std::path::PathBuf;

/// Headless commands use the same store and bounded image pipeline as the UI.
pub fn command(args: &[String], store: &ThemeStore) -> Result<String, String> {
    match args.first().map(String::as_str) {
        Some("--list-themes") if args.len() == 1 => serde_json::to_string_pretty(&store.list_themes()).map_err(|e| e.to_string()),
        Some("--apply-theme") if (2..=3).contains(&args.len()) => {
            let active = images::apply(store, &args[1], args.get(2).map(String::as_str))?;
            Ok(format!("Applied {}", active.theme_name))
        }
        Some("--cycle-background") if args.len() == 1 => {
            Ok(format!("Background: {}", images::cycle(store)?.background.unwrap_or_default()))
        }
        Some("--extract") if args.len() >= 4 => {
            let appearance = match args[3].as_str() { "dark" => Appearance::Dark, "light" => Appearance::Light,
                _ => return Err("Choose dark or light.".into()) };
            let pixels = images::load(&PathBuf::from(&args[1]))?;
            let candidates = extract::dominant_colors(pixels.as_raw())?;
            let backgrounds: Vec<_> = std::iter::once(&args[1]).chain(args[4..].iter()).map(PathBuf::from).collect();
            for path in &backgrounds { images::load(path)?; }
            let theme = store.save_custom_theme(&args[2], extract::palette_from_accent(candidates[0], appearance), appearance, &backgrounds)?;
            Ok(format!("Saved {}", theme.id))
        }
        _ => Err("Usage: --list-themes | --apply-theme ID [BACKGROUND] | --cycle-background | --extract IMAGE NAME dark|light [BACKGROUND ...]".into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_commands_do_not_write_preferences() {
        let store = ThemeStore::new(
            PathBuf::from("/nonexistent/crowbaros-test"),
            PathBuf::from("/nonexistent/themes"),
        );
        for args in [
            vec!["--apply-theme", "../escape"],
            vec!["--extract", "image", "name", "other"],
            vec!["--cycle-background", "extra"],
        ] {
            assert!(command(&args.into_iter().map(str::to_owned).collect::<Vec<_>>(), &store).is_err());
        }
        assert!(
            command(&["--list-themes".into()], &store)
                .unwrap()
                .contains("nullkeel-dark")
        );
    }
}

/// A draft remains unsaved if it changes while persistence is in flight.
#[derive(Default)]
pub struct DraftState {
    revision: u64,
    saved: u64,
    pub confirming_close: bool,
}
impl DraftState {
    pub fn changed(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.confirming_close = false;
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn saved(&mut self, revision: u64) {
        self.saved = revision;
    }
    pub fn request_close(&mut self) -> bool {
        self.confirming_close = self.revision != self.saved;
        !self.confirming_close
    }
    pub fn keep_editing(&mut self) {
        self.confirming_close = false;
    }
}
#[cfg(test)]
mod draft_tests {
    use super::*;
    #[test]
    fn close_preserves_unsaved_work_and_a_late_save_does_not_erase_edits() {
        let mut draft = DraftState::default();
        assert!(draft.request_close());
        draft.changed();
        assert!(!draft.request_close());
        assert!(draft.confirming_close);
        draft.keep_editing();
        assert!(!draft.confirming_close);
        let saving = draft.revision();
        draft.changed();
        draft.saved(saving);
        assert!(!draft.request_close());
        draft.saved(draft.revision());
        assert!(draft.request_close());
    }
}
