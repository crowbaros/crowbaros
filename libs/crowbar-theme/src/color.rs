// SPDX-License-Identifier: MIT
//! Color conversion, hex serialization, and WCAG 2.1 contrast utilities.

use serde::{Deserialize, Deserializer, Serializer, de};

/// Mask a color to 24-bit RGB (`0xRRGGBB`).
#[inline]
pub const fn mask_rgb(color: u32) -> u32 {
    color & 0x00ff_ffff
}

/// Extract `(r, g, b)` components from a `0xRRGGBB` color.
#[inline]
pub const fn rgb_components(color: u32) -> (u8, u8, u8) {
    (
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
    )
}

/// Pack `(r, g, b)` components into a `0xRRGGBB` color.
#[inline]
pub const fn from_rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Format a `0xRRGGBB` color as a lowercase `"#rrggbb"` string.
pub fn hex(color: u32) -> String {
    format!("#{:06x}", mask_rgb(color))
}

/// Format a `0xRRGGBB` color as a lowercase 6-digit hex string without `#` (for Foot INI).
pub fn hex_bare(color: u32) -> String {
    format!("{:06x}", mask_rgb(color))
}

/// Parse a `"#rrggbb"` or `"rrggbb"` hex string (case-insensitive) into `0xRRGGBB`.
pub fn parse_hex(input: &str) -> Result<u32, String> {
    let trimmed = input.trim();
    let digits = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "Invalid hex color '{input}': expected 6 hex digits like #rrggbb."
        ));
    }
    u32::from_str_radix(digits, 16).map_err(|e| format!("Invalid hex color '{input}': {e}"))
}

/// Convert `0xRRGGBB` to `(hue_deg [0, 360), saturation [0, 1], lightness [0, 1])`.
pub fn rgb_to_hsl(color: u32) -> (f64, f64, f64) {
    let (r, g, b) = rgb_components(color);
    let rf = r as f64 / 255.0;
    let gf = g as f64 / 255.0;
    let bf = b as f64 / 255.0;

    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;
    let l = (max + min) * 0.5;

    if delta <= 1e-9 {
        return (0.0, 0.0, l);
    }

    let s = if l < 0.5 {
        delta / (max + min)
    } else {
        delta / (2.0 - max - min)
    };

    let mut h = if (max - rf).abs() <= 1e-9 {
        ((gf - bf) / delta) % 6.0
    } else if (max - gf).abs() <= 1e-9 {
        (bf - rf) / delta + 2.0
    } else {
        (rf - gf) / delta + 4.0
    };
    h *= 60.0;
    if h < 0.0 {
        h += 360.0;
    }

    (h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0))
}

/// Convert HSL (`hue_deg`, `saturation`, `lightness`) to `0xRRGGBB`.
pub fn hsl_to_rgb(h: f64, s: f64, l: f64) -> u32 {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);

    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c * 0.5;

    let (r1, g1, b1) = match h {
        h if h < 60.0 => (c, x, 0.0),
        h if h < 120.0 => (x, c, 0.0),
        h if h < 180.0 => (0.0, c, x),
        h if h < 240.0 => (0.0, x, c),
        h if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };

    let to_u8 = |v: f64| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    from_rgb(to_u8(r1), to_u8(g1), to_u8(b1))
}

/// Linearly blend two RGB colors (`t = 0.0` returns `a`, `t = 1.0` returns `b`).
pub fn mix(a: u32, b: u32, t: f64) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let (ar, ag, ab) = rgb_components(a);
    let (br, bg, bb) = rgb_components(b);
    let blend = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round().clamp(0.0, 255.0) as u8;
    from_rgb(blend(ar, br), blend(ag, bg), blend(ab, bb))
}

/// Compute WCAG 2.1 relative luminance in `[0.0, 1.0]`.
pub fn relative_luminance(color: u32) -> f64 {
    let (r, g, b) = rgb_components(color);
    let linearize = |c: u8| {
        let s = c as f64 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b)
}

/// Compute WCAG 2.1 contrast ratio in `[1.0, 21.0]`.
pub fn contrast_ratio(a: u32, b: u32) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (lighter, darker) = if la >= lb { (la, lb) } else { (lb, la) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Adjust `fg` (preserving hue as much as possible) until `contrast_ratio(result, bg) >= min_ratio`.
///
/// If `fg` already satisfies `min_ratio`, returns `fg` unchanged (masked to 24-bit RGB).
/// If `min_ratio` exceeds the theoretical maximum achievable against `bg`, returns the
/// extreme (`#ffffff` or `#000000`) with the highest contrast against `bg`.
pub fn ensure_contrast(fg: u32, bg: u32, min_ratio: f64) -> u32 {
    let fg = mask_rgb(fg);
    let bg = mask_rgb(bg);
    if contrast_ratio(fg, bg) >= min_ratio {
        return fg;
    }

    let bg_lum = relative_luminance(bg);
    let fg_lum = relative_luminance(fg);
    let white_ratio = contrast_ratio(0x00ff_ffff, bg);
    let black_ratio = contrast_ratio(0x0000_0000, bg);

    // Prefer keeping fg on the same side of bg if that side can meet min_ratio;
    // otherwise move toward whichever extreme (white or black) offers higher contrast.
    let go_lighter = if fg_lum >= bg_lum {
        if white_ratio >= min_ratio {
            true
        } else {
            white_ratio >= black_ratio
        }
    } else if black_ratio >= min_ratio {
        false
    } else {
        white_ratio > black_ratio
    };

    let target_extreme = if go_lighter { 0x00ff_ffff } else { 0x0000_0000 };
    let max_possible = if go_lighter { white_ratio } else { black_ratio };
    if max_possible < min_ratio {
        return target_extreme;
    }

    // Binary search lightness along fg's hue/saturation first so hue stays intact.
    let (h, s, l) = rgb_to_hsl(fg);
    let mut lo = if go_lighter { l } else { 0.0 };
    let mut hi = if go_lighter { 1.0 } else { l };
    let mut best = target_extreme;

    for _ in 0..24 {
        let mid = (lo + hi) * 0.5;
        let candidate = hsl_to_rgb(h, s, mid);
        if contrast_ratio(candidate, bg) >= min_ratio {
            best = candidate;
            if go_lighter {
                hi = mid;
            } else {
                lo = mid;
            }
        } else if go_lighter {
            lo = mid;
        } else {
            hi = mid;
        }
    }

    if contrast_ratio(best, bg) >= min_ratio {
        return best;
    }

    // Fallback: binary search direct RGB mix toward pure white/black.
    let mut lo = 0.0_f64;
    let mut hi = 1.0_f64;
    let mut best_mix = target_extreme;
    for _ in 0..24 {
        let mid = (lo + hi) * 0.5;
        let candidate = mix(fg, target_extreme, mid);
        if contrast_ratio(candidate, bg) >= min_ratio {
            best_mix = candidate;
            hi = mid;
        } else {
            lo = mid;
        }
    }
    best_mix
}

/// Serde helper for single `u32` RGB fields serialized as `"#rrggbb"`.
pub(crate) mod hex_rgb {
    use super::*;

    pub fn serialize<S: Serializer>(value: &u32, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex(*value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
        let s = String::deserialize(deserializer)?;
        parse_hex(&s).map_err(de::Error::custom)
    }
}

/// Serde helper for `[u32; 16]` ANSI palettes serialized as 16 `"#rrggbb"` strings.
pub(crate) mod hex_ansi {
    use super::*;
    use serde::ser::SerializeSeq;

    pub fn serialize<S: Serializer>(values: &[u32; 16], serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(16))?;
        for &c in values {
            seq.serialize_element(&hex(c))?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u32; 16], D::Error> {
        let raw = Vec::<String>::deserialize(deserializer)?;
        if raw.len() != 16 {
            return Err(de::Error::custom(format!(
                "Expected 16 ANSI colors, found {}.",
                raw.len()
            )));
        }
        let mut out = [0u32; 16];
        for (slot, item) in out.iter_mut().zip(&raw) {
            *slot = parse_hex(item).map_err(de::Error::custom)?;
        }
        Ok(out)
    }
}
