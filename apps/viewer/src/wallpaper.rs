// SPDX-License-Identifier: MIT
//! Save a normalized image that the shell can read through Base's launcher.
use crate::image_document::LoadedImage;
use crowbar_theme::wallpaper::{self, MAX_EDGE, MAX_PIXELS};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

fn config_dir() -> Result<PathBuf, String> {
    Ok(crowbar_theme::ThemeStore::for_user()?.config_dir().to_owned())
}

/// Reduce before rotating so even a large source needs only one bounded output.
fn normalized(image: &LoadedImage, turns: u8) -> Result<image::RgbaImage, String> {
    let source = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(image.width, image.height, image.rgba.as_slice())
        .ok_or("The image has invalid dimensions")?;
    if image.width == 0 || image.height == 0 {
        return Err("The image is empty".into());
    }
    let scale = (f64::from(MAX_EDGE) / f64::from(image.width.max(image.height)))
        .min((MAX_PIXELS as f64 / (f64::from(image.width) * f64::from(image.height))).sqrt())
        .min(1.);
    let width = ((f64::from(image.width) * scale).floor() as u32).max(1);
    let height = ((f64::from(image.height) * scale).floor() as u32).max(1);
    let resized = image::imageops::resize(&source, width, height, image::imageops::FilterType::Triangle);
    Ok(match turns % 4 {
        1 => image::imageops::rotate90(&resized),
        2 => image::imageops::rotate180(&resized),
        3 => image::imageops::rotate270(&resized),
        _ => resized,
    })
}

fn save_in(dir: &Path, image: &LoadedImage, turns: u8) -> Result<(), String> {
    let pixels = normalized(image, turns)?;
    let mut revision = [0; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut revision))
        .map_err(|e| format!("Could not create a wallpaper revision: {e}"))?;
    let bytes = wallpaper::encode(pixels.width(), pixels.height(), revision, pixels.as_raw())?;
    fs::create_dir_all(dir).map_err(|e| format!("Could not create the appearance folder: {e}"))?;
    // The launcher also refuses symlinked configuration directories. Do not save
    // a preference there that the shell would be unable to read.
    for path in [Some(dir), dir.parent()].into_iter().flatten() {
        if !fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
            return Err("The appearance folder must be a real directory".into());
        }
    }
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| format!("Could not create the wallpaper: {e}"))?;
    file.write_all(&bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| format!("Could not save the wallpaper: {e}"))?;
    file.persist(dir.join(wallpaper::FILE_NAME))
        .map_err(|e| format!("Could not activate the wallpaper: {}", e.error))?;
    sync_directory(dir)?;
    Ok(())
}

fn sync_directory(dir: &Path) -> Result<(), String> {
    fs::File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("The wallpaper changed, but saving the folder to disk failed: {e}"))
}

pub fn save(image: &LoadedImage, turns: u8) -> Result<(), String> {
    save_in(&config_dir()?, image, turns)
}

pub fn restore() -> Result<(), String> {
    let dir = config_dir()?;
    match fs::remove_file(dir.join(wallpaper::FILE_NAME)) {
        Ok(()) => sync_directory(&dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not restore the built-in wallpaper: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn example() -> LoadedImage {
        LoadedImage {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
            metadata: vec![],
        }
    }
    #[test]
    fn saves_rotation_and_changes_revision_on_replacement() {
        let tmp = tempfile::tempdir().unwrap();
        save_in(tmp.path(), &example(), 1).unwrap();
        let first = fs::read(tmp.path().join(wallpaper::FILE_NAME)).unwrap();
        let (info, rgba) = wallpaper::decode(&first).unwrap();
        assert_eq!((info.width, info.height), (1, 2));
        assert_eq!(rgba, example().rgba);
        save_in(tmp.path(), &example(), 0).unwrap();
        let second = fs::read(tmp.path().join(wallpaper::FILE_NAME)).unwrap();
        assert_ne!(info.revision, wallpaper::decode(&second).unwrap().0.revision);
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }
    #[test]
    fn wide_images_fit_the_transfer_budget() {
        let source = LoadedImage {
            width: 8192,
            height: 600,
            rgba: vec![128; 8192 * 600 * 4],
            metadata: vec![],
        };
        let output = normalized(&source, 1).unwrap();
        assert!(output.width().max(output.height()) <= MAX_EDGE);
        assert!(u64::from(output.width()) * u64::from(output.height()) <= MAX_PIXELS);
    }
    #[test]
    fn failure_preserves_existing_wallpaper() {
        let tmp = tempfile::tempdir().unwrap();
        save_in(tmp.path(), &example(), 0).unwrap();
        let path = tmp.path().join(wallpaper::FILE_NAME);
        let before = fs::read(&path).unwrap();
        let mut invalid = example();
        invalid.rgba.clear();
        assert!(save_in(tmp.path(), &invalid, 0).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}
