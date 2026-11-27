// SPDX-License-Identifier: MIT
//! Theme metadata crosses the launcher boundary; no personal image path is opened.
use crowbar_theme::{ActiveTheme, Theme};
use serde::Deserialize;
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Catalog {
    pub active: ActiveTheme,
    pub themes: Vec<Theme>,
}

pub fn watch(sender: crate::session::Sender) {
    std::thread::spawn(move || {
        let mut previous = None;
        loop {
            let reply = crate::session::launcher_request_bounded(serde_json::json!({"op":"themes"}), 512 * 1024);
            if let Ok(reply) = reply {
                if let Ok(catalog) = serde_json::from_value::<Catalog>(reply) {
                    if previous.as_ref() != Some(&catalog) {
                        if let Err(error) = crate::session::set_theme(&catalog.active.colors) {
                            log::warn!("Applying compositor theme: {error}");
                        } else {
                            previous = Some(catalog.clone());
                        }
                        if sender.send(crate::session::Event::Themes(catalog)).is_err() {
                            break;
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}
