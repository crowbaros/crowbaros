// SPDX-License-Identifier: MIT
//! Theme metadata crosses the launcher seam; no personal image path is opened.

use crowbar_theme::{ActiveTheme, Theme};
use serde::Deserialize;

use crate::compositor::CompositorClient;
use crate::events::{Event, Sender};
use crate::launcher_client::LauncherClient;

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ThemeCatalog {
    pub active: ActiveTheme,
    pub themes: Vec<Theme>,
}

pub fn watch(sender: Sender, compositor: CompositorClient, launcher: LauncherClient) {
    std::thread::spawn(move || {
        let mut previous = None;
        loop {
            if let Ok(catalog) = launcher.themes() {
                if previous.as_ref() != Some(&catalog) {
                    if let Err(error) = compositor.set_theme(&catalog.active.colors) {
                        log::warn!("Applying compositor theme: {error}");
                    } else {
                        previous = Some(catalog.clone());
                    }
                    if sender.send(Event::Themes(catalog)).is_err() {
                        break;
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}
