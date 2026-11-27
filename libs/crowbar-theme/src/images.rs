// SPDX-License-Identifier: MIT
//! Person-side bounded image loading and wallpaper activation. Never used by the shell.
use crate::{ActiveTheme, ThemeStore, wallpaper};
use image::{ImageDecoder, ImageReader, RgbaImage};
use std::{
    fs::{File, OpenOptions},
    io::{Cursor, Read},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
const MAX_FILE: u64 = 64 * 1024 * 1024;

/// PNG, JPEG, WebP, GIF (first frame), BMP, and TIFF, with EXIF orientation.
/// Reject special files and bound compressed bytes, dimensions, and decoded memory.
pub fn load(path: &Path) -> Result<RgbaImage, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("Cannot open image: {e}"))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return Err("Choose a regular image file of at most 64 MiB.".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("Image exceeds 64 MiB.".into());
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(192 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| format!("Cannot decode image: {e}"))?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > 40_000_000 || decoder.total_bytes() > 192 * 1024 * 1024 {
        return Err("Image exceeds the 40 megapixel decode limit.".into());
    }
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    decoded.apply_orientation(orientation);
    let scale = (f64::from(wallpaper::MAX_EDGE) / f64::from(decoded.width().max(decoded.height())))
        .min((wallpaper::MAX_PIXELS as f64 / (f64::from(decoded.width()) * f64::from(decoded.height()))).sqrt())
        .min(1.0);
    Ok(decoded
        .resize_exact(
            (f64::from(decoded.width()) * scale).floor().max(1.0) as u32,
            (f64::from(decoded.height()) * scale).floor().max(1.0) as u32,
            image::imageops::FilterType::Triangle,
        )
        .into_rgba8())
}

/// Apply a theme and its selected background. Decode before changing preferences,
/// then publish bounded pixels through the same fixed file used by Viewer.
pub fn apply(store: &ThemeStore, id: &str, background: Option<&str>) -> Result<ActiveTheme, String> {
    let theme = store.get_theme(id).ok_or("Theme not found.")?;
    let previous = store.load_active();
    let selected = background
        .map(str::to_owned)
        .or_else(|| {
            (previous.theme_id == id)
                .then_some(previous.background)
                .flatten()
                .filter(|bg| theme.backgrounds.contains(bg))
        })
        .or_else(|| theme.backgrounds.first().cloned());
    if selected.as_ref().is_some_and(|bg| !theme.backgrounds.contains(bg)) {
        return Err("This background does not belong to the theme.".into());
    }
    let pixels = if let Some(bg) = selected.as_deref() {
        let path = store
            .resolve_background_path(&theme, Some(bg))
            .ok_or("Theme background is missing.")?;
        Some(load(&path)?)
    } else {
        None
    };
    let payload = pixels
        .map(|pixels| {
            let mut revision = [0; 16];
            File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut revision))
                .map_err(|e| e.to_string())?;
            wallpaper::encode(pixels.width(), pixels.height(), revision, pixels.as_raw())
        })
        .transpose()?;
    let active = store.apply_theme(id, selected.as_deref(), None)?;
    let path = store.config_dir().join(wallpaper::FILE_NAME);
    if let Some(bytes) = payload {
        crate::store::atomic_write(&path, &bytes)?;
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(active)
}

pub fn cycle(store: &ThemeStore) -> Result<ActiveTheme, String> {
    let active = store.load_active();
    let theme = store.get_theme(&active.theme_id).ok_or("Active theme is missing.")?;
    if theme.backgrounds.is_empty() {
        return Err("This theme has no backgrounds.".into());
    }
    let next = active
        .background
        .as_ref()
        .and_then(|bg| theme.backgrounds.iter().position(|b| b == bg))
        .map(|i| (i + 1) % theme.backgrounds.len())
        .unwrap_or(0);
    apply(store, &theme.id, Some(&theme.backgrounds[next]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Appearance, builtin, extract};
    #[test]
    fn images_extract_apply_and_cycle_through_bounded_wallpaper() {
        let root = std::env::temp_dir().join(format!(
            "theme-images-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let first = root.join("first.png");
        let second = root.join("second.png");
        RgbaImage::from_pixel(4, 3, image::Rgba([10, 100, 200, 255]))
            .save(&first)
            .unwrap();
        RgbaImage::from_pixel(3, 4, image::Rgba([200, 100, 10, 255]))
            .save(&second)
            .unwrap();
        let store = ThemeStore::new(root.join("config/crowbaros"), root.join("system"));
        let pixels = load(&first).unwrap();
        let colors =
            extract::palette_from_accent(extract::dominant_colors(pixels.as_raw()).unwrap()[0], Appearance::Dark);
        store
            .save_custom_theme("Fixture", colors, Appearance::Dark, &[first, second])
            .unwrap();
        let a = apply(&store, "fixture", None).unwrap();
        assert_eq!(a.background.as_deref(), Some("first.png"));
        let bytes = std::fs::read(store.config_dir().join(wallpaper::FILE_NAME)).unwrap();
        assert_eq!(&wallpaper::decode(&bytes).unwrap().1[..4], &[10, 100, 200, 255]);
        let b = cycle(&store).unwrap();
        assert_eq!(b.background.as_deref(), Some("second.png"));
        assert!(apply(&store, "fixture", Some("../first.png")).is_err());
        let bytes = std::fs::read(store.config_dir().join(wallpaper::FILE_NAME)).unwrap();
        assert_eq!(&wallpaper::decode(&bytes).unwrap().1[..4], &[200, 100, 10, 255]);
        let broken = root.join("broken.png");
        std::fs::write(&broken, b"not an image").unwrap();
        assert!(load(&broken).is_err());
        assert!(load(Path::new("/dev/null")).is_err());
        store
            .save_custom_theme("Broken", builtin::nord().colors, Appearance::Dark, &[broken])
            .unwrap();
        assert!(apply(&store, "broken", None).is_err());
        assert_eq!(store.load_active().theme_id, "fixture");
        std::fs::remove_dir_all(root).unwrap();
    }
}
