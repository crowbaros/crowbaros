//! Cursor images from the system's XCursor theme, for the shapes a client
//! names with `wp_cursor_shape_device_v1` and for the compositor's own
//! pointer.
//!
//! A theme is a directory of files named for the cursors, in a directory on
//! the search path (`XCURSOR_PATH`, or the usual places), and may inherit
//! another theme's cursors (its `index.theme`'s `Inherits`). Each file holds
//! the cursor at several nominal sizes; the one nearest the size asked for
//! is taken, and of an animated cursor, its first frame. A cursor not found
//! by its name is looked for by the older X11 names themes also use.
//!
//! The files are the system's, which the compositor trusts as it trusts
//! itself. Even so a file is read only up to a size, and an image only up to
//! a size, so a broken one cannot take the compositor's memory.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use crate::{
    backend::{POINTER_COLOUR, POINTER_SIZE},
    image::Image,
};

/// The largest cursor file read.
const MAX_FILE: u64 = 16 * 1024 * 1024;
/// The widest and tallest image taken.
const MAX_SIDE: u32 = 512;
/// How deep a theme's inheritance is followed.
const MAX_DEPTH: usize = 8;

const MAGIC: &[u8; 4] = b"Xcur";
const IMAGE_TYPE: u32 = 0xfffd_0002;

/// A cursor's image, and the point of it that is the pointer's position.
#[derive(Debug, PartialEq)]
pub(crate) struct CursorImage {
    pub image: Image,
    pub hotspot: (i32, i32),
}

/// The image of the nominal size nearest `size` in an XCursor file, or None
/// if the file is not one.
pub(crate) fn parse(bytes: &[u8], size: u32) -> Option<CursorImage> {
    let word = |at: usize| -> Option<u32> {
        let bytes = bytes.get(at..at.checked_add(4)?)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    };
    if bytes.get(..4)? != MAGIC {
        return None;
    }
    let header = word(4)? as usize;
    let count = word(12)? as usize;
    // Each table entry: its type, its nominal size, and where it is.
    let entries = (0..count.min(4096)).filter_map(|index| {
        let at = header.checked_add(index.checked_mul(12)?)?;
        Some((word(at)?, word(at + 4)?, word(at + 8)? as usize))
    });
    let (_, _, position) = entries
        .filter(|(kind, _, _)| *kind == IMAGE_TYPE)
        .min_by_key(|(_, nominal, _)| nominal.abs_diff(size))?;
    // The image's own header: its size, type, nominal size, version,
    // width, height, hotspot, and delay.
    let chunk = word(position)? as usize;
    if word(position + 4)? != IMAGE_TYPE {
        return None;
    }
    let (width, height) = (word(position + 16)?, word(position + 20)?);
    let hotspot = (word(position + 24)?, word(position + 28)?);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let start = position.checked_add(chunk)?;
    let length = (width as usize) * (height as usize) * 4;
    let pixels: Vec<u32> = bytes
        .get(start..start.checked_add(length)?)?
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| u32::from_le_bytes(*pixel))
        .collect();
    Some(CursorImage {
        image: Image {
            width: width as i32,
            height: height as i32,
            pixels,
        },
        hotspot: (hotspot.0.min(width - 1) as i32, hotspot.1.min(height - 1) as i32),
    })
}

/// The names a theme may give a shape, the CSS name first. The shapes are
/// `wp_cursor_shape_device_v1.shape`'s, numbered from 1.
fn names_of(shape: u32) -> &'static [&'static str] {
    const NAMES: [&[&str]; 36] = [
        &["default", "left_ptr"],
        &["context-menu"],
        &["help", "question_arrow", "whats_this"],
        &["pointer", "hand2", "hand1"],
        &["progress", "left_ptr_watch"],
        &["wait", "watch"],
        &["cell", "plus"],
        &["crosshair", "cross"],
        &["text", "xterm"],
        &["vertical-text"],
        &["alias", "dnd-link"],
        &["copy", "dnd-copy"],
        &["move", "fleur"],
        &["no-drop", "dnd-no-drop"],
        &["not-allowed", "crossed_circle"],
        &["grab", "openhand"],
        &["grabbing", "closedhand"],
        &["e-resize", "right_side"],
        &["n-resize", "top_side"],
        &["ne-resize", "top_right_corner"],
        &["nw-resize", "top_left_corner"],
        &["s-resize", "bottom_side"],
        &["se-resize", "bottom_right_corner"],
        &["sw-resize", "bottom_left_corner"],
        &["w-resize", "left_side"],
        &["ew-resize", "sb_h_double_arrow", "h_double_arrow"],
        &["ns-resize", "sb_v_double_arrow", "v_double_arrow"],
        &["nesw-resize", "fd_double_arrow"],
        &["nwse-resize", "bd_double_arrow"],
        &["col-resize", "sb_h_double_arrow"],
        &["row-resize", "sb_v_double_arrow"],
        &["all-scroll", "fleur"],
        &["zoom-in"],
        &["zoom-out"],
        &["dnd-ask", "dnd-copy"],
        &["all-resize", "fleur"],
    ];
    shape
        .checked_sub(1)
        .and_then(|index| NAMES.get(index as usize))
        .copied()
        .unwrap_or(&[])
}

/// The shape a cursor not found falls back to: the arrow.
pub(crate) const DEFAULT_SHAPE: u32 = 1;

/// The system's cursor theme, read as cursors are first asked for.
pub(crate) struct CursorTheme {
    /// Where themes are looked for, in order.
    path: Vec<PathBuf>,
    name: String,
    size: u32,
    /// Each shape's image, once looked for: None if the theme has none.
    loaded: HashMap<u32, Option<Rc<CursorImage>>>,
    /// The compositor's own pointer where there is no theme.
    square: Rc<CursorImage>,
}

impl CursorTheme {
    pub(crate) fn new(path: Vec<PathBuf>, name: &str, size: u32) -> CursorTheme {
        CursorTheme {
            path,
            name: name.to_owned(),
            size: size.clamp(8, 256),
            loaded: HashMap::new(),
            square: Rc::new(CursorImage {
                image: Image {
                    width: POINTER_SIZE,
                    height: POINTER_SIZE,
                    pixels: vec![POINTER_COLOUR; (POINTER_SIZE * POINTER_SIZE) as usize],
                },
                hotspot: (0, 0),
            }),
        }
    }

    /// The theme the environment names: `XCURSOR_THEME` and `XCURSOR_SIZE`,
    /// on `XCURSOR_PATH` or the usual places.
    pub(crate) fn from_environment() -> CursorTheme {
        let path = match std::env::var_os("XCURSOR_PATH") {
            Some(path) => std::env::split_paths(&path).collect(),
            None => {
                let mut path = Vec::new();
                if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                    path.push(home.join(".local/share/icons"));
                    path.push(home.join(".icons"));
                }
                path.extend(["/usr/share/icons", "/usr/share/pixmaps"].map(PathBuf::from));
                path
            }
        };
        let name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|size| size.parse().ok())
            .unwrap_or(24);
        CursorTheme::new(path, &name, size)
    }

    /// No theme: every shape is the compositor's own pointer.
    pub(crate) fn none() -> CursorTheme {
        CursorTheme::new(Vec::new(), "default", 24)
    }

    /// A shape's image: the theme's, by any of its names, or else the
    /// arrow's. None if the theme has neither.
    #[cfg(test)]
    pub(crate) fn shape(&mut self, shape: u32) -> Option<Rc<CursorImage>> {
        self.load(shape);
        self.image(shape)
    }

    /// A shape's image as loaded, or else the arrow's, without reading a
    /// file: for drawing, once [`CursorTheme::load`] has read it.
    pub(crate) fn image(&self, shape: u32) -> Option<Rc<CursorImage>> {
        let loaded = |shape| self.loaded.get(&shape).cloned().flatten();
        loaded(shape).or_else(|| loaded(DEFAULT_SHAPE))
    }

    /// A shape's image, or the white square where the theme has neither it
    /// nor the arrow.
    pub(crate) fn image_or_square(&self, shape: u32) -> Rc<CursorImage> {
        self.image(shape).unwrap_or_else(|| self.square.clone())
    }

    /// Reads a shape's image, and the arrow's, if they have not been.
    pub(crate) fn load(&mut self, shape: u32) {
        self.read_shape(DEFAULT_SHAPE);
        self.read_shape(shape);
    }

    fn read_shape(&mut self, shape: u32) -> Option<Rc<CursorImage>> {
        if let Some(loaded) = self.loaded.get(&shape) {
            return loaded.clone();
        }
        let found = names_of(shape)
            .iter()
            .find_map(|name| self.find(&self.name, name, &mut Vec::new()))
            .map(Rc::new);
        self.loaded.insert(shape, found.clone());
        found
    }

    /// A cursor in a theme or a theme it inherits.
    fn find(&self, theme: &str, cursor: &str, seen: &mut Vec<String>) -> Option<CursorImage> {
        if seen.len() >= MAX_DEPTH || seen.iter().any(|seen| seen == theme) || !plain(theme) || !plain(cursor) {
            return None;
        }
        seen.push(theme.to_owned());
        for dir in &self.path {
            if let Some(image) =
                read(&dir.join(theme).join("cursors").join(cursor)).and_then(|bytes| parse(&bytes, self.size))
            {
                return Some(image);
            }
        }
        self.inherits(theme)
            .into_iter()
            .find_map(|parent| self.find(&parent, cursor, seen))
    }

    /// The themes a theme inherits, as the first `index.theme` found says.
    fn inherits(&self, theme: &str) -> Vec<String> {
        let Some(index) = self
            .path
            .iter()
            .find_map(|dir| read(&dir.join(theme).join("index.theme")))
        else {
            return Vec::new();
        };
        String::from_utf8_lossy(&index)
            .lines()
            .find_map(|line| line.trim().strip_prefix("Inherits"))
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .map(|list| {
                list.split([',', ';'])
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A name that is one path component, and no way out of its directory.
fn plain(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

/// A file's bytes, if it is no larger than a cursor file may be.
fn read(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_FILE {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// An XCursor file of one image per nominal size: each image of one colour,
/// `size` square, with its hotspot at the middle. For the tests.
#[doc(hidden)]
pub fn file_for_tests(images: &[(u32, u32)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let header = 16u32;
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&header.to_le_bytes());
    bytes.extend_from_slice(&0x1_0000u32.to_le_bytes());
    bytes.extend_from_slice(&(images.len() as u32).to_le_bytes());
    let mut position = header + 12 * images.len() as u32;
    for (size, _) in images {
        bytes.extend_from_slice(&IMAGE_TYPE.to_le_bytes());
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&position.to_le_bytes());
        position += 36 + size * size * 4;
    }
    for (size, argb) in images {
        for word in [36, IMAGE_TYPE, *size, 1, *size, *size, size / 2, size / 2, 0] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        for _ in 0..size * size {
            bytes.extend_from_slice(&argb.to_le_bytes());
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_image_nearest_the_size_asked_for_is_taken() {
        let file = file_for_tests(&[(16, 0xff00_00ff), (24, 0xff00_ff00), (48, 0xffff_0000)]);
        let cursor = parse(&file, 32).unwrap();
        assert_eq!((cursor.image.width, cursor.image.height), (24, 24));
        assert_eq!(cursor.image.pixels[0], 0xff00_ff00);
        assert_eq!(cursor.hotspot, (12, 12));
        assert_eq!(parse(&file, 40).unwrap().image.width, 48);
    }

    #[test]
    fn what_is_not_a_cursor_file_is_none_and_never_a_panic() {
        let file = file_for_tests(&[(24, 0xffff_ffff)]);
        assert!(parse(b"not a cursor", 24).is_none());
        for length in 0..file.len() {
            assert!(parse(&file[..length], 24).is_none(), "a file cut at {length}");
        }
        let mut huge = file.clone();
        // The image says it is 100000 pixels wide.
        let at = 16 + 12 + 16;
        huge[at..at + 4].copy_from_slice(&100_000u32.to_le_bytes());
        assert!(parse(&huge, 24).is_none());
        let mut seed = 7u32;
        for _ in 0..2000 {
            let mut broken = file.clone();
            for _ in 0..4 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let at = (seed as usize) % broken.len();
                broken[at] = (seed >> 16) as u8;
            }
            let _ = parse(&broken, 24);
        }
    }

    fn theme_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fulcrum-xcursor-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn put(dir: &Path, theme: &str, cursor: &str, bytes: &[u8]) {
        let cursors = dir.join(theme).join("cursors");
        std::fs::create_dir_all(&cursors).unwrap();
        std::fs::write(cursors.join(cursor), bytes).unwrap();
    }

    #[test]
    fn a_shape_is_found_by_an_older_name_and_in_an_inherited_theme() {
        let dir = theme_dir("inherit");
        put(&dir, "Base", "xterm", &file_for_tests(&[(24, 0xff11_1111)]));
        put(&dir, "Base", "left_ptr", &file_for_tests(&[(24, 0xff22_2222)]));
        std::fs::create_dir_all(dir.join("Mine")).unwrap();
        std::fs::write(dir.join("Mine/index.theme"), "[Icon Theme]\nInherits=Missing, Base\n").unwrap();
        let mut theme = CursorTheme::new(vec![dir.clone()], "Mine", 24);

        let text = theme.shape(9).unwrap();
        assert_eq!(text.image.pixels[0], 0xff11_1111, "text is xterm");
        let unknown = theme.shape(33).unwrap();
        assert_eq!(
            unknown.image.pixels[0], 0xff22_2222,
            "a shape the theme lacks is the arrow"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_systems_arrow_loads_where_it_has_a_theme() {
        let Some(theme) = ["Adwaita", "default"]
            .into_iter()
            .find(|theme| Path::new("/usr/share/icons").join(theme).join("cursors").is_dir())
        else {
            eprintln!("no cursor theme: skipped");
            return;
        };
        let mut theme = CursorTheme::new(vec![PathBuf::from("/usr/share/icons")], theme, 24);
        let arrow = theme.shape(DEFAULT_SHAPE).expect("the arrow");
        assert!(arrow.image.width >= 16 && arrow.image.width <= 64, "{:?}", arrow.image);
        assert!(
            arrow.image.pixels.iter().any(|pixel| pixel >> 24 != 0),
            "an empty arrow"
        );
        assert!(theme.shape(9).is_some(), "text");
    }

    #[test]
    fn a_theme_that_inherits_itself_ends_and_a_missing_one_is_none() {
        let dir = theme_dir("loop");
        std::fs::create_dir_all(dir.join("A")).unwrap();
        std::fs::create_dir_all(dir.join("B")).unwrap();
        std::fs::write(dir.join("A/index.theme"), "Inherits=B\n").unwrap();
        std::fs::write(dir.join("B/index.theme"), "Inherits=A\n").unwrap();
        assert!(CursorTheme::new(vec![dir.clone()], "A", 24).shape(1).is_none());
        assert!(CursorTheme::new(vec![dir.clone()], "../etc", 24).shape(1).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
