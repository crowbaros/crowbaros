// SPDX-License-Identifier: MIT
//! A bounded wallpaper image shared by Viewer, the launcher service, and the shell.
//!
//! The fixed file is replaced atomically by the person-side writer. Its header
//! contains a fresh revision for each replacement; the shell polls that small
//! header and requests pixels only when the revision changes. Pixels are straight
//! (not premultiplied) RGBA8, in row-major order. No image decoder runs in the shell.

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "wallpaper.rgba";
pub const HEADER_LEN: usize = 32;
pub const MAX_EDGE: u32 = 4096;
pub const MAX_PIXELS: u64 = 4_194_304;
pub const MAX_FILE_BYTES: usize = HEADER_LEN + MAX_PIXELS as usize * 4;
pub const MAX_BASE64_BYTES: usize = MAX_FILE_BYTES.div_ceil(3) * 4;
/// Enough for the bounded image and the launcher's fixed JSON envelope.
pub const MAX_DATA_REPLY_BYTES: usize = MAX_BASE64_BYTES + 1024;
const MAGIC: &[u8; 8] = b"CBWPRG01";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    pub revision: String,
    pub width: u32,
    pub height: u32,
}

/// Whether a revision uses the one wire spelling: 16 bytes as lowercase hex.
pub fn valid_revision(revision: &str) -> bool {
    revision.len() == 32
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn pixel_bytes(width: u32, height: u32) -> Result<usize, String> {
    if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE {
        return Err("Wallpaper dimensions must be 1..=4096 pixels".into());
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err("Wallpaper exceeds 4,194,304 pixels".into());
    }
    Ok(pixels as usize * 4)
}

/// Validates exactly one header against the complete file length. This allows
/// polling without reading or allocating the pixel payload.
pub fn read_header(header: &[u8], file_len: u64) -> Result<Info, String> {
    if header.len() != HEADER_LEN || &header[..8] != MAGIC {
        return Err("Wallpaper has an invalid header".into());
    }
    let width = u32::from_le_bytes(header[8..12].try_into().expect("four-byte width"));
    let height = u32::from_le_bytes(header[12..16].try_into().expect("four-byte height"));
    let bytes = pixel_bytes(width, height)?;
    if file_len != (HEADER_LEN + bytes) as u64 {
        return Err("Wallpaper pixel length does not match its dimensions".into());
    }
    let mut revision = String::with_capacity(32);
    for byte in &header[16..32] {
        use std::fmt::Write;
        write!(&mut revision, "{byte:02x}").expect("writing into a String cannot fail");
    }
    Ok(Info {
        revision,
        width,
        height,
    })
}

/// Validates the complete file and borrows its straight RGBA8 pixels.
pub fn decode(bytes: &[u8]) -> Result<(Info, &[u8]), String> {
    let header = bytes.get(..HEADER_LEN).ok_or("Wallpaper has an incomplete header")?;
    let info = read_header(header, bytes.len() as u64)?;
    Ok((info, &bytes[HEADER_LEN..]))
}

/// Encodes one bounded image. The caller supplies a fresh random revision for
/// each atomic replacement, including replacements with the same dimensions.
pub fn encode(width: u32, height: u32, revision: [u8; 16], rgba: &[u8]) -> Result<Vec<u8>, String> {
    let bytes = pixel_bytes(width, height)?;
    if rgba.len() != bytes {
        return Err("Wallpaper pixel length does not match its dimensions".into());
    }
    let mut result = Vec::with_capacity(HEADER_LEN + bytes);
    result.extend_from_slice(MAGIC);
    result.extend_from_slice(&width.to_le_bytes());
    result.extend_from_slice(&height.to_le_bytes());
    result.extend_from_slice(&revision);
    result.extend_from_slice(rgba);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width: u32, height: u32) -> [u8; HEADER_LEN] {
        let mut header = [0; HEADER_LEN];
        header[..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&width.to_le_bytes());
        header[12..16].copy_from_slice(&height.to_le_bytes());
        header
    }

    #[test]
    fn pixels_and_revision_round_trip_without_changing_alpha() {
        let pixels = [1, 2, 3, 4, 254, 253, 252, 251];
        let mut revision = [0; 16];
        revision[0] = 0xab;
        revision[15] = 0xef;
        let data = encode(2, 1, revision, &pixels).unwrap();
        let (info, decoded) = decode(&data).unwrap();
        assert_eq!(decoded, pixels);
        assert_eq!(info.width, 2);
        assert_eq!(info.height, 1);
        assert_eq!(info.revision, format!("ab{}ef", "00".repeat(14)));
        assert!(valid_revision(&info.revision));
    }

    #[test]
    fn only_canonical_revisions_are_accepted() {
        for invalid in [
            "",
            "ab",
            "AB000000000000000000000000000000ef",
            "zz00000000000000000000000000000000",
        ] {
            assert!(!valid_revision(invalid));
        }
        assert!(valid_revision("0123456789abcdef0123456789abcdef"));
    }

    #[test]
    fn dimensions_bound_both_edges_and_total_pixels() {
        assert!(read_header(&header(4096, 1024), MAX_FILE_BYTES as u64).is_ok());
        assert!(read_header(&header(2048, 2048), MAX_FILE_BYTES as u64).is_ok());
        for (width, height) in [(0, 1), (1, 0), (4097, 1), (1, 4097), (4096, 1025), (u32::MAX, u32::MAX)] {
            assert!(read_header(&header(width, height), MAX_FILE_BYTES as u64).is_err());
            assert!(encode(width, height, [0; 16], &[]).is_err());
        }
    }

    #[test]
    fn malformed_headers_and_lengths_are_rejected() {
        let data = encode(1, 1, [0; 16], &[1, 2, 3, 4]).unwrap();
        for len in 0..data.len() {
            assert!(decode(&data[..len]).is_err());
        }
        let mut trailing = data.clone();
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        let mut bad_magic = data;
        bad_magic[0] ^= 1;
        assert!(decode(&bad_magic).is_err());
        assert!(encode(1, 1, [0; 16], &[1, 2, 3]).is_err());
        assert!(encode(1, 1, [0; 16], &[1, 2, 3, 4, 5]).is_err());
        assert!(read_header(&header(1, 1), u64::MAX).is_err());
    }
}
