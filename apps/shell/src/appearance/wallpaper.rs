// SPDX-License-Identifier: MIT
//! A custom wallpaper is already-decoded, bounded pixels, never a file path to
//! open or an image format to decode in the shell's account.

use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use crowbar_theme::wallpaper::{self, Info};
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    time::Duration,
};

use crate::events::{Event, Sender};
use crate::launcher_client::LauncherClient;

#[derive(Clone, Debug)]
pub struct CustomWallpaper {
    pub info: Info,
    pub rgba: Vec<u8>,
}

impl CustomWallpaper {
    /// Converts validated RGBA pixels into a GPUI `RenderImage` (BGRA byte order).
    pub fn into_render_image(mut self) -> std::sync::Arc<gpui::RenderImage> {
        for pixel in self.rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let pixels = image::RgbaImage::from_raw(self.info.width, self.info.height, self.rgba)
            .expect("validated wallpaper dimensions");
        std::sync::Arc::new(gpui::RenderImage::new([image::Frame::new(pixels)]))
    }
}

enum Update {
    Unchanged,
    Changed(Option<CustomWallpaper>),
}

fn fetch(
    current: Option<&str>,
    mut get_info: impl FnMut() -> Result<Option<Info>>,
    mut get_data: impl FnMut(&str) -> Result<String>,
) -> Result<Update> {
    let Some(info) = get_info()? else {
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
    let encoded = get_data(&info.revision)?;
    if encoded.len() > wallpaper::MAX_BASE64_BYTES {
        bail!("Wallpaper exceeds the transfer limit");
    }
    let bytes = STANDARD.decode(encoded)?;
    let (actual, pixels) = wallpaper::decode(&bytes).map_err(anyhow::Error::msg)?;
    if actual != info {
        bail!("Wallpaper changed during transfer");
    }
    Ok(Update::Changed(Some(CustomWallpaper {
        info: actual,
        rgba: pixels.to_owned(),
    })))
}

fn read_local_wallpaper(expected_revision: Option<&str>) -> Result<Option<(Info, Option<String>)>> {
    // Development sessions use the same preference in their own account.
    let store = crowbar_theme::ThemeStore::for_user().map_err(anyhow::Error::msg)?;
    let path = store.config_dir().join(wallpaper::FILE_NAME);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() }
    {
        bail!("Invalid wallpaper file");
    }
    let mut file = file.take(wallpaper::MAX_FILE_BYTES as u64 + 1);
    let mut header = [0; wallpaper::HEADER_LEN];
    file.read_exact(&mut header)?;
    let info = wallpaper::read_header(&header, metadata.len()).map_err(anyhow::Error::msg)?;
    let Some(revision) = expected_revision else {
        return Ok(Some((info, None)));
    };
    if revision != info.revision {
        bail!("Wallpaper changed during transfer");
    }
    let mut bytes = header.to_vec();
    file.read_to_end(&mut bytes)?;
    wallpaper::decode(&bytes).map_err(anyhow::Error::msg)?;
    Ok(Some((info, Some(STANDARD.encode(bytes)))))
}

fn fetch_info(launcher: &LauncherClient) -> Result<Option<Info>> {
    if launcher.is_configured() {
        return launcher.wallpaper_info();
    }
    Ok(read_local_wallpaper(None)?.map(|(info, _)| info))
}

fn fetch_data(launcher: &LauncherClient, revision: &str) -> Result<String> {
    if launcher.is_configured() {
        return launcher.wallpaper_data(revision);
    }
    match read_local_wallpaper(Some(revision))? {
        Some((_, Some(data))) => Ok(data),
        _ => bail!("Wallpaper changed during transfer"),
    }
}

pub fn watch(sender: Sender, launcher: LauncherClient) {
    std::thread::spawn(move || {
        let mut revision = None;
        let mut failed = false;
        loop {
            match fetch(
                revision.as_deref(),
                || fetch_info(&launcher),
                |rev| fetch_data(&launcher, rev),
            ) {
                Ok(Update::Changed(wallpaper)) => {
                    revision = wallpaper.as_ref().map(|w| w.info.revision.clone());
                    if sender.send(Event::Wallpaper(wallpaper)).is_err() {
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
        let mut info_calls = 0;
        let mut data_calls = 0;
        let loaded = fetch(
            None,
            || {
                info_calls += 1;
                Ok(Some(info.clone()))
            },
            |_| {
                data_calls += 1;
                Ok(STANDARD.encode(&bytes))
            },
        )
        .unwrap();
        assert_eq!((info_calls, data_calls), (1, 1));
        let Update::Changed(Some(loaded)) = loaded else {
            panic!("Expected pixels")
        };
        assert_eq!(loaded.rgba, [12, 34, 56, 255]);
        let mut info_calls = 0;
        let mut data_calls = 0;
        assert!(matches!(
            fetch(
                Some(&info.revision),
                || {
                    info_calls += 1;
                    Ok(Some(info.clone()))
                },
                |_| {
                    data_calls += 1;
                    Ok(String::new())
                },
            )
            .unwrap(),
            Update::Unchanged
        ));
        assert_eq!((info_calls, data_calls), (1, 0));
    }

    #[test]
    fn rejects_stale_or_malformed_transfers() {
        let (info, _) = fixture();
        let changed = wallpaper::encode(1, 1, [8; 16], &[0; 4]).unwrap();
        for encoded in [STANDARD.encode(changed), STANDARD.encode(b"not pixels")] {
            assert!(
                fetch(
                    None,
                    || Ok(Some(info.clone())),
                    |_| Ok(encoded.clone()),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn deleting_override_restores_builtin_once() {
        assert!(matches!(
            fetch(Some("old"), || Ok(None), |_| unreachable!()).unwrap(),
            Update::Changed(None)
        ));
        assert!(matches!(
            fetch(None, || Ok(None), |_| unreachable!()).unwrap(),
            Update::Unchanged
        ));
    }
}
