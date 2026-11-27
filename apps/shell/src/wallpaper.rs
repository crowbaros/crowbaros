// SPDX-License-Identifier: MIT
//! A custom wallpaper is already-decoded, bounded pixels, never a file path to
//! open or an image format to decode in the shell's account.
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use crowbar_theme::wallpaper::{self, Info};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Wallpaper {
    pub info: Info,
    pub rgba: Vec<u8>,
}

enum Update {
    Unchanged,
    Changed(Option<Wallpaper>),
}

fn fetch(current: Option<&str>, mut request: impl FnMut(Value) -> Result<Value>) -> Result<Update> {
    let reply = request(json!({"op":"wallpaper_info"}))?;
    let info: Option<Info> =
        serde_json::from_value(reply.get("wallpaper").context("Missing wallpaper preference")?.clone())?;
    let Some(info) = info else {
        return Ok(if current.is_none() {
            Update::Unchanged
        } else {
            Update::Changed(None)
        });
    };
    if !wallpaper::valid_revision(&info.revision) {
        bail!("Invalid wallpaper revision");
    }
    if current == Some(info.revision.as_str()) {
        return Ok(Update::Unchanged);
    }
    let reply = request(json!({"op":"wallpaper_data","revision":info.revision}))?;
    let encoded = reply["data"].as_str().context("Missing wallpaper pixels")?;
    if encoded.len() > wallpaper::MAX_BASE64_BYTES {
        bail!("Wallpaper exceeds the transfer limit");
    }
    let bytes = STANDARD.decode(encoded)?;
    let (actual, pixels) = wallpaper::decode(&bytes).map_err(anyhow::Error::msg)?;
    if actual != info {
        bail!("Wallpaper changed during transfer");
    }
    Ok(Update::Changed(Some(Wallpaper {
        info: actual,
        rgba: pixels.to_owned(),
    })))
}

fn request(value: Value) -> Result<Value> {
    if std::env::var_os("CROWBAR_LAUNCH_SOCKET").is_some() {
        let limit = if value["op"] == "wallpaper_data" {
            wallpaper::MAX_DATA_REPLY_BYTES
        } else {
            65537
        };
        return crate::session::launcher_request_bounded(value, limit);
    }
    // Development sessions use the same preference in their own account.
    let store = crowbar_theme::ThemeStore::for_user().map_err(anyhow::Error::msg)?;
    let path = store.config_dir().join(wallpaper::FILE_NAME);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({"wallpaper":null})),
        Err(e) => return Err(e.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() } {
        bail!("Invalid wallpaper file");
    }
    let mut file = file.take(wallpaper::MAX_FILE_BYTES as u64 + 1);
    let mut header = [0; wallpaper::HEADER_LEN];
    file.read_exact(&mut header)?;
    let info = wallpaper::read_header(&header, metadata.len()).map_err(anyhow::Error::msg)?;
    if value["op"] == "wallpaper_info" {
        return Ok(json!({"wallpaper":info}));
    }
    if value["revision"] != info.revision {
        bail!("Wallpaper changed during transfer");
    }
    let mut bytes = header.to_vec();
    file.read_to_end(&mut bytes)?;
    wallpaper::decode(&bytes).map_err(anyhow::Error::msg)?;
    Ok(json!({"wallpaper":info,"data":STANDARD.encode(bytes)}))
}

pub fn watch(sender: crate::session::Sender) {
    std::thread::spawn(move || {
        let mut revision = None;
        let mut failed = false;
        loop {
            match fetch(revision.as_deref(), request) {
                Ok(Update::Changed(wallpaper)) => {
                    revision = wallpaper.as_ref().map(|w| w.info.revision.clone());
                    if sender.send(crate::session::Event::Wallpaper(wallpaper)).is_err() {
                        break;
                    }
                    failed = false;
                }
                Ok(Update::Unchanged) => failed = false,
                Err(error) => {
                    if !failed {
                        log::warn!("Wallpaper could not be loaded: {error}");
                    }
                    failed = true;
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Info, Vec<u8>) {
        let bytes = wallpaper::encode(1, 1, [7; 16], &[12, 34, 56, 255]).unwrap();
        (wallpaper::decode(&bytes).unwrap().0, bytes)
    }
    #[test]
    fn only_fetches_pixels_when_revision_changes() {
        let (info, bytes) = fixture();
        let mut calls = 0;
        let loaded = fetch(None, |_| {
            calls += 1;
            Ok(if calls == 1 {
                json!({"wallpaper":info})
            } else {
                json!({"data":STANDARD.encode(&bytes)})
            })
        })
        .unwrap();
        assert_eq!(calls, 2);
        let Update::Changed(Some(loaded)) = loaded else {
            panic!("Expected pixels")
        };
        assert_eq!(loaded.rgba, [12, 34, 56, 255]);
        let mut calls = 0;
        assert!(matches!(
            fetch(Some(&info.revision), |_| {
                calls += 1;
                Ok(json!({"wallpaper":info}))
            })
            .unwrap(),
            Update::Unchanged
        ));
        assert_eq!(calls, 1);
    }
    #[test]
    fn rejects_stale_or_malformed_transfers() {
        let (info, _) = fixture();
        let changed = wallpaper::encode(1, 1, [8; 16], &[0; 4]).unwrap();
        for encoded in [STANDARD.encode(changed), STANDARD.encode(b"not pixels")] {
            assert!(
                fetch(None, |r| Ok(if r["op"] == "wallpaper_info" {
                    json!({"wallpaper":info})
                } else {
                    json!({"data":encoded})
                }))
                .is_err()
            );
        }
    }
    #[test]
    fn deleting_override_restores_builtin_once() {
        assert!(matches!(
            fetch(Some("old"), |_| Ok(json!({"wallpaper":null}))).unwrap(),
            Update::Changed(None)
        ));
        assert!(matches!(
            fetch(None, |_| Ok(json!({"wallpaper":null}))).unwrap(),
            Update::Unchanged
        ));
    }
}
