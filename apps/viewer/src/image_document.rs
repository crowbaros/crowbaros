// SPDX-License-Identifier: MIT
//! Bounded, window-independent image loading and view state.

use image::{ImageDecoder, ImageFormat, ImageReader};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const MAX_PIXELS: u64 = 40_000_000;
pub const MAX_SIDE: u32 = 16_384;
pub const MAX_FILE_BYTES: u64 = 64 << 20;
const HEADER_BYTES: u64 = 256 << 10;
const MAX_SVG_BYTES: u64 = 4 << 20;
const MAX_DECODE_BYTES: u64 = 160 << 20;
const MAX_EXIF_BYTES: usize = 256 << 10;
const MAX_SIBLINGS: usize = 10_000;
const MAX_SVG_NODES: usize = 20_000;
const MAX_SVG_SEGMENTS: usize = 200_000;
const MAX_SVG_DEPTH: usize = 128;

#[derive(Debug)]
pub struct LoadedImage {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA8 pixels, with the file's EXIF orientation applied.
    pub rgba: Vec<u8>,
    pub metadata: Vec<(String, String)>,
}

/// Open first and then inspect the descriptor, so replacing a regular path with
/// a FIFO cannot block the worker, and a symlink to a device cannot be decoded.
fn open_regular(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("Cannot open image: {e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Viewer opens regular files only.".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err("This image exceeds the 64 MiB file limit.".into());
    }
    Ok(file)
}

pub fn check_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > MAX_SIDE
        || height > MAX_SIDE
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        Err("This image exceeds the 40 megapixel or 16,384-pixel side limit.".into())
    } else {
        Ok(())
    }
}

fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    limits
}

fn reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("Cannot read image header: {e}"))?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Bmp | ImageFormat::Tiff)
    ) {
        return Err("Supported images are PNG, JPEG, WebP, BMP, TIFF, and SVG.".into());
    }
    reader.limits(limits());
    Ok(reader)
}

/// Inspect at most 256 KiB; this function never decodes a pixel buffer.
pub fn header_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    let head = &bytes[..bytes.len().min(HEADER_BYTES as usize)];
    let dimensions = reader(head)?
        .into_dimensions()
        .map_err(|e| format!("Cannot safely read image dimensions from its header: {e}"))?;
    check_dimensions(dimensions.0, dimensions.1)?;
    Ok(dimensions)
}

pub fn load_image(path: &Path) -> Result<LoadedImage, String> {
    let mut file = open_regular(path)?;
    if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("svg")) {
        let mut bytes = Vec::new();
        file.take(MAX_SVG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_SVG_BYTES {
            return Err("This SVG exceeds the 4 MiB vector file limit.".into());
        }
        return load_svg(&bytes);
    }

    let mut head = Vec::new();
    (&mut file)
        .take(HEADER_BYTES)
        .read_to_end(&mut head)
        .map_err(|e| e.to_string())?;
    let dimensions = header_dimensions(&head)?;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("This image exceeds the 64 MiB file limit.".into());
    }
    let reader = reader(&bytes)?;
    let format = reader.format().unwrap(); // reader() permits only known formats.
    let mut decoder = reader.into_decoder().map_err(|e| format!("Cannot decode image: {e}"))?;
    // The file may have changed after reading the bounded header. Validate the
    // exact bytes being decoded as well, including their native bit depth.
    check_dimensions(decoder.dimensions().0, decoder.dimensions().1)?;
    if decoder.dimensions() != dimensions || decoder.total_bytes() > MAX_DECODE_BYTES {
        return Err("The image changed while opening or exceeds the decode memory limit.".into());
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut metadata = vec![
        ("Format".into(), format!("{format:?}")),
        ("File size".into(), format!("{} bytes", bytes.len())),
        ("Color".into(), format!("{:?}", decoder.color_type())),
    ];
    if let Ok(Some(exif_bytes)) = decoder.exif_metadata() {
        append_exif(&exif_bytes, &mut metadata);
    }
    let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(|e| format!("Cannot decode image: {e}"))?;
    decoded.apply_orientation(orientation);
    let rgba = decoded.into_rgba8();
    metadata.insert(
        0,
        ("Dimensions".into(), format!("{} × {}", rgba.width(), rgba.height())),
    );
    Ok(LoadedImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        metadata,
    })
}

fn append_exif(bytes: &[u8], metadata: &mut Vec<(String, String)>) {
    if bytes.len() > MAX_EXIF_BYTES {
        return;
    }
    let Ok(exif) = exif::Reader::new().read_raw(bytes.to_vec()) else {
        return;
    };
    // Keep the inspector bounded and useful, rather than displaying binary
    // maker notes or thousands of arbitrary fields from the input.
    for (tag, label) in [
        (exif::Tag::Make, "Camera make"),
        (exif::Tag::Model, "Camera model"),
        (exif::Tag::DateTimeOriginal, "Taken"),
        (exif::Tag::ExposureTime, "Exposure"),
        (exif::Tag::FNumber, "Aperture"),
        (exif::Tag::PhotographicSensitivity, "ISO"),
        (exif::Tag::FocalLength, "Focal length"),
        (exif::Tag::LensModel, "Lens"),
    ] {
        if let Some(field) = exif.get_field(tag, exif::In::PRIMARY) {
            let value: String = field
                .display_value()
                .with_unit(&exif)
                .to_string()
                .chars()
                .take(256)
                .collect();
            metadata.push((label.into(), value));
        }
    }
}

fn load_svg(bytes: &[u8]) -> Result<LoadedImage, String> {
    // SVG must not read other paths or cause an unbounded embedded raster
    // decode. Refuse image hrefs explicitly rather than silently omitting them.
    let referenced_image = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let data_seen = referenced_image.clone();
    let string_seen = referenced_image.clone();
    let mut options = resvg::usvg::Options {
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(move |_, _, _| {
                data_seen.store(true, std::sync::atomic::Ordering::Relaxed);
                None
            }),
            resolve_string: Box::new(move |_, _| {
                string_seen.store(true, std::sync::atomic::Ordering::Relaxed);
                None
            }),
        },
        ..Default::default()
    };
    options
        .fontdb_mut()
        .load_font_data(include_bytes!("../../../libs/crowbar-ui/fonts/Manrope-Regular.ttf").to_vec());
    options.font_family = "Manrope".into();
    let text = std::str::from_utf8(bytes).map_err(|_| "SVG must be UTF-8 text.")?;
    if text.contains("<!DOCTYPE") {
        return Err("SVG document type declarations are not supported.".into());
    }
    // from_str deliberately excludes SVGZ and its unbounded decompression.
    let tree = resvg::usvg::Tree::from_str(text, &options).map_err(|e| format!("Cannot read SVG: {e}"))?;
    if referenced_image.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("SVGs with linked or embedded images are not supported.".into());
    }
    validate_svg_render_tree(&tree)?;
    let size = tree.size().to_int_size();
    check_dimensions(size.width(), size.height())?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or("Cannot allocate SVG pixels within the image limit.")?;
    resvg::render(&tree, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    // tiny-skia produces premultiplied RGBA; the app uses straight RGBA8.
    for pixel in pixmap.data_mut().as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha > 0 {
            for channel in &mut pixel[..3] {
                *channel = (u32::from(*channel) * 255 + alpha / 2)
                    .checked_div(alpha)
                    .unwrap_or(0)
                    .min(255) as u8;
            }
        }
    }
    Ok(LoadedImage {
        width: size.width(),
        height: size.height(),
        rgba: pixmap.take(),
        metadata: vec![
            ("Dimensions".into(), format!("{} × {}", size.width(), size.height())),
            ("Format".into(), "SVG".into()),
            ("File size".into(), format!("{} bytes", bytes.len())),
        ],
    })
}

/// resvg's intermediate layer/filter/pattern pixmaps do not inherit our final
/// canvas limit. Accept only the rendering paths that draw directly onto that
/// bounded canvas. Examine the normalized tree, including flattened text, so
/// CSS styles and references cannot bypass the check.
fn validate_svg_render_tree(tree: &resvg::usvg::Tree) -> Result<(), String> {
    use resvg::usvg::{Node, Paint};
    let mut groups = vec![(tree.root(), 0usize)];
    let mut nodes = 0usize;
    let mut segments = 0usize;
    while let Some((group, depth)) = groups.pop() {
        if group.should_isolate() {
            return Err(
                "This SVG uses filters, masks, clipping, opacity groups, or blending that exceed Viewer's safe rendering support."
                    .into(),
            );
        }
        if depth > MAX_SVG_DEPTH {
            return Err("This SVG has too many nested groups.".into());
        }
        for node in group.children() {
            nodes += 1;
            if nodes > MAX_SVG_NODES {
                return Err("This SVG exceeds the 20,000 drawing-node limit.".into());
            }
            match node {
                Node::Group(group) => groups.push((group, depth + 1)),
                Node::Text(text) => groups.push((text.flattened(), depth + 1)),
                Node::Image(_) => return Err("SVG image references are not supported.".into()),
                Node::Path(path) => {
                    if path
                        .fill()
                        .is_some_and(|fill| matches!(fill.paint(), Paint::Pattern(_)))
                        || path
                            .stroke()
                            .is_some_and(|stroke| matches!(stroke.paint(), Paint::Pattern(_)))
                    {
                        return Err("SVG pattern fills and strokes are not supported.".into());
                    }
                    segments += path.data().segments().count();
                    if segments > MAX_SVG_SEGMENTS {
                        return Err("This SVG exceeds the 200,000 path-segment limit.".into());
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn is_image_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tif" | "tiff" | "svg"
        )
    })
}

/// Deterministic filesystem-name order, preserving paths that are not UTF-8.
pub fn sibling_images(path: &Path) -> Result<Vec<PathBuf>, String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let entries = fs::read_dir(parent).map_err(|e| format!("Cannot list sibling images: {e}"))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if is_image_path(&path) && entry.file_type().map_err(|e| e.to_string())?.is_file() {
            paths.push(path);
            if paths.len() > MAX_SIBLINGS {
                return Err("This folder has more than 10,000 images; open an image directly.".into());
            }
        }
    }
    paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    Ok(paths)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewState {
    pub zoom: f32,
    pub quarter_turns: u8,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            quarter_turns: 0,
        }
    }
}

impl ViewState {
    pub fn actual_size(&mut self) {
        self.zoom = 1.0;
    }

    pub fn zoom_by(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.zoom = (self.zoom * factor).clamp(0.01, 16.0);
        }
    }

    pub fn rotate_clockwise(&mut self) {
        self.quarter_turns = (self.quarter_turns + 1) % 4;
    }

    pub fn displayed_dimensions(&self, image: (u32, u32)) -> (f32, f32) {
        let (width, height) = if self.quarter_turns.is_multiple_of(2) {
            image
        } else {
            (image.1, image.0)
        };
        (width as f32 * self.zoom, height as f32 * self.zoom)
    }

    pub fn fit(&mut self, image: (u32, u32), viewport: (f32, f32)) {
        if image.0 == 0
            || image.1 == 0
            || !viewport.0.is_finite()
            || !viewport.1.is_finite()
            || viewport.0 <= 0.0
            || viewport.1 <= 0.0
        {
            return;
        }
        self.zoom = 1.0;
        let (width, height) = self.displayed_dimensions(image);
        self.zoom = (viewport.0 / width).min(viewport.1 / height).clamp(0.01, 16.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    #[test]
    fn dimension_gate_rejects_zero_area_and_overflow_sized_headers() {
        assert!(check_dimensions(0, 12).is_err());
        assert!(check_dimensions(u32::MAX, u32::MAX).is_err());
        assert!(check_dimensions(10_000, 5_000).is_err());
        assert!(check_dimensions(16_385, 1).is_err());
        assert!(check_dimensions(8_000, 5_000).is_ok());
    }

    #[test]
    fn dimensions_are_read_without_decoding_pixel_data() {
        // A BMP header describing a picture, with no pixel data at all.
        let mut bmp = vec![0u8; 54];
        bmp[..2].copy_from_slice(b"BM");
        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&120u32.to_le_bytes());
        bmp[22..26].copy_from_slice(&80u32.to_le_bytes());
        bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
        bmp[28..30].copy_from_slice(&24u16.to_le_bytes());
        assert_eq!(header_dimensions(&bmp).unwrap(), (120, 80));
        bmp[18..22].copy_from_slice(&12_000u32.to_le_bytes());
        bmp[22..26].copy_from_slice(&12_000u32.to_le_bytes());
        assert!(header_dimensions(&bmp).unwrap_err().contains("40 megapixel"));
    }

    #[test]
    fn regular_png_decodes_and_special_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.png");
        let pixels = image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]));
        pixels.save(&path).unwrap();
        let loaded = load_image(&path).unwrap();
        assert_eq!((loaded.width, loaded.height), (3, 2));
        assert_eq!(&loaded.rgba[..4], &[10, 20, 30, 255]);
        assert!(load_image(dir.path()).is_err());
        let link = dir.path().join("device.png");
        symlink("/dev/zero", &link).unwrap();
        assert!(load_image(&link).unwrap_err().contains("regular files"));
        let fifo = dir.path().join("pipe.png");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: CString provides a live NUL-terminated filename.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(load_image(&fifo).unwrap_err().contains("regular files"));
    }

    #[test]
    fn too_large_regular_file_is_rejected_before_reading_it() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"BM").unwrap();
        file.as_file().set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(load_image(file.path()).unwrap_err().contains("64 MiB"));
    }

    #[test]
    fn raster_formats_decode_through_the_same_bounded_path() {
        let dir = tempfile::tempdir().unwrap();
        let pixels = image::RgbImage::from_pixel(3, 2, image::Rgb([40, 80, 120]));
        for extension in ["jpg", "webp", "bmp", "tiff"] {
            let path = dir.path().join(format!("sample.{extension}"));
            pixels.save(&path).unwrap();
            let loaded = load_image(&path).unwrap();
            assert_eq!((loaded.width, loaded.height), (3, 2), "{extension}");
            assert_eq!(loaded.rgba.len(), 3 * 2 * 4);
        }
    }

    #[test]
    fn exif_inspector_extracts_camera_and_ignores_malformed_metadata() {
        // A complete little-endian TIFF with one ASCII Make field.
        let mut raw = b"II\x2a\x00\x08\x00\x00\x00\x01\x00".to_vec();
        raw.extend_from_slice(&0x010fu16.to_le_bytes());
        raw.extend_from_slice(&2u16.to_le_bytes());
        raw.extend_from_slice(&6u32.to_le_bytes());
        raw.extend_from_slice(&26u32.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(b"Maker\0");
        let mut metadata = Vec::new();
        append_exif(&raw, &mut metadata);
        assert_eq!(metadata[0].0, "Camera make");
        assert!(metadata[0].1.contains("Maker"));
        append_exif(b"invalid", &mut metadata);
        append_exif(&vec![0; MAX_EXIF_BYTES + 1], &mut metadata);
        assert_eq!(metadata.len(), 1);
    }

    #[test]
    fn svg_is_bounded_and_does_not_read_image_references() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="2"><rect width="4" height="2" fill="#ff0000" fill-opacity="0.5"/></svg>"##;
        let loaded = load_svg(svg).unwrap();
        assert_eq!((loaded.width, loaded.height), (4, 2));
        assert_eq!(loaded.rgba[0], 255);
        assert!((127..=128).contains(&loaded.rgba[3]));
        assert!(
            load_svg(br#"<!DOCTYPE svg [<!ENTITY title "example">]><svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#)
                .unwrap_err()
                .contains("document type")
        );
        assert!(load_svg(br#"<svg xmlns="http://www.w3.org/2000/svg" width="20000" height="20000"/>"#).is_err());
        assert!(load_svg(br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><image href="/dev/zero" width="4" height="4"/></svg>"#).unwrap_err().contains("linked or embedded"));
    }

    #[test]
    fn sibling_discovery_is_sorted_and_skips_directories_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["z.png", "a.JPG", "note.txt", "middle.tiff"] {
            fs::write(dir.path().join(name), []).unwrap();
        }
        fs::create_dir(dir.path().join("folder.png")).unwrap();
        symlink("/dev/zero", dir.path().join("pipe.png")).unwrap();
        let paths = sibling_images(&dir.path().join("z.png")).unwrap();
        assert_eq!(
            paths
                .iter()
                .map(|p| p.file_name().unwrap().to_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["a.JPG", "middle.tiff", "z.png"]
        );
    }

    #[test]
    fn svg_refuses_unbounded_intermediate_layers_before_rendering() {
        let cases = [
            r#"<defs><filter id="f" x="-100000" y="-100000" width="200000" height="200000" filterUnits="userSpaceOnUse"><feFlood flood-color="red"/></filter></defs><rect width="4" height="4" filter="url(#f)"/>"#,
            r#"<defs><mask id="m"><rect width="4" height="4" fill="white"/></mask></defs><rect width="4" height="4" mask="url(#m)"/>"#,
            r#"<defs><clipPath id="c"><rect width="4" height="4"/></clipPath></defs><rect width="4" height="4" clip-path="url(#c)"/>"#,
            r#"<g opacity="0.5"><rect width="4" height="4"/></g>"#,
            r#"<style>.faded { opacity: 0.5 }</style><g class="faded"><rect width="4" height="4"/></g>"#,
            r#"<defs><pattern id="p" patternUnits="userSpaceOnUse" width="100000" height="100000"><rect width="4" height="4"/></pattern></defs><rect width="4" height="4" fill="url(#p)"/>"#,
        ];
        for body in cases {
            let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">{body}</svg>"#);
            assert!(load_svg(svg.as_bytes()).is_err(), "accepted {body}");
        }
    }

    #[test]
    fn svg_keeps_simple_vectors_gradients_and_bundled_text() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="40"><defs><linearGradient id="g"><stop stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient></defs><path d="M0 0H20V20H0Z" fill="url(#g)"/><text x="22" y="24" font-size="16">Viewer</text></svg>"##;
        let loaded = load_svg(svg).unwrap();
        assert_eq!((loaded.width, loaded.height), (100, 40));
        assert!(loaded.rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0));
        assert!(
            loaded
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .any(|(index, pixel)| index % 100 > 22 && pixel[3] > 0),
            "text must remain visible"
        );
    }

    #[test]
    fn zoom_fit_rotation_and_invalid_input_preserve_valid_geometry() {
        let mut state = ViewState::default();
        state.fit((800, 400), (200.0, 200.0));
        assert_eq!(state.displayed_dimensions((800, 400)), (200.0, 100.0));
        state.rotate_clockwise();
        assert_eq!(state.displayed_dimensions((800, 400)), (100.0, 200.0));
        state.actual_size();
        state.zoom_by(f32::INFINITY);
        state.zoom_by(-1.0);
        assert_eq!(state.zoom, 1.0);
        state.zoom_by(100.0);
        assert_eq!(state.zoom, 16.0);
        for _ in 0..3 {
            state.rotate_clockwise();
        }
        assert_eq!(state.quarter_turns, 0);
        state.fit((0, 400), (200.0, 200.0));
        assert_eq!(state.zoom, 16.0);
    }
}
