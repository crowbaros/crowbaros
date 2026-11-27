// SPDX-License-Identifier: MIT
//! Deterministic dominant colors and accessible semantic palettes from RGBA pixels.
use crate::{Appearance, ThemeColors, builtin, contrast_ratio, ensure_contrast, from_rgb, hsl_to_rgb, rgb_to_hsl};
use std::collections::BTreeMap;

/// Rank up to eight quantized colors by their alpha-weighted coverage. Ties
/// use RGB order, so identical pixels always produce identical candidates.
/// Transparent pixels contribute nothing. Input is bounded to the wallpaper limit.
pub fn dominant_colors(rgba: &[u8]) -> Result<Vec<u32>, String> {
    if rgba.is_empty() || !rgba.len().is_multiple_of(4) || rgba.len() > crate::wallpaper::MAX_PIXELS as usize * 4 {
        return Err("Expected a bounded, nonempty RGBA image.".into());
    }
    let mut bins = BTreeMap::<u32, (u64, [u64; 3])>::new();
    for pixel in rgba.as_chunks::<4>().0 {
        let weight = u64::from(pixel[3]);
        if weight == 0 {
            continue;
        }
        let key = from_rgb(pixel[0] & 0xe0, pixel[1] & 0xe0, pixel[2] & 0xe0);
        let bin = bins.entry(key).or_default();
        bin.0 += weight;
        for (sum, value) in bin.1.iter_mut().zip(pixel) {
            *sum += u64::from(*value) * weight;
        }
    }
    let mut bins: Vec<_> = bins.into_iter().collect();
    bins.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
    let colors = bins
        .into_iter()
        .take(8)
        .map(|(_, (weight, sum))| {
            from_rgb(
                (sum[0] / weight) as u8,
                (sum[1] / weight) as u8,
                (sum[2] / weight) as u8,
            )
        })
        .collect::<Vec<_>>();
    if colors.is_empty() {
        return Err("The image is entirely transparent.".into());
    }
    Ok(colors)
}

/// Derive a palette around the chosen candidate. Text, muted text, accents,
/// and status colors reach 4.5:1 on all three surfaces; focus/lines reach 3:1.
/// The accent label reaches 4.5:1 on its fill, including monochrome inputs.
pub fn palette_from_accent(accent: u32, appearance: Appearance) -> ThemeColors {
    let dark = appearance.is_dark();
    let (h, saturation, _) = rgb_to_hsl(accent);
    let neutral = saturation.min(0.18);
    let mut colors = if dark {
        builtin::nullkeel_dark().colors
    } else {
        builtin::nullkeel_light().colors
    };
    colors.canvas = hsl_to_rgb(h, neutral, if dark { 0.045 } else { 0.97 });
    colors.surface = hsl_to_rgb(h, neutral, if dark { 0.085 } else { 0.94 });
    colors.surface_elevated = hsl_to_rgb(h, neutral, if dark { 0.13 } else { 0.90 });
    let worst = colors.surface_elevated;
    let extreme = if dark { 0xffffff } else { 0 };
    // Choose the correct side of the surfaces before adjusting for contrast.
    let accessible = |color, ratio| {
        let (_, s, _) = rgb_to_hsl(color);
        let (h, _, _) = rgb_to_hsl(color);
        ensure_contrast(hsl_to_rgb(h, s, if dark { 0.75 } else { 0.25 }), worst, ratio)
    };
    colors.text = extreme;
    colors.muted = accessible(hsl_to_rgb(h, neutral, 0.5), 4.5);
    colors.accent = accessible(accent, 4.5);
    colors.on_accent = if contrast_ratio(0, colors.accent) >= 4.5 {
        0
    } else {
        0xffffff
    };
    colors.focus = colors.accent;
    colors.line = accessible(hsl_to_rgb(h, neutral, 0.5), 3.0);
    colors.danger = accessible(0xe05267, 4.5);
    colors.warning = accessible(0xdca24a, 4.5);
    colors.success = accessible(0x48a982, 4.5);
    colors.ansi = [
        colors.surface,
        colors.danger,
        colors.success,
        colors.warning,
        accessible(0x608ddd, 4.5),
        accessible(0xc66cd5, 4.5),
        colors.accent,
        colors.text,
        colors.muted,
        colors.danger,
        colors.success,
        colors.warning,
        accessible(0x608ddd, 4.5),
        accessible(0xc66cd5, 4.5),
        colors.accent,
        colors.text,
    ];
    colors
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranking_is_deterministic_alpha_weighted_and_rejects_invalid_pixels() {
        let pixels = [255, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0, 255, 0, 255, 0, 0];
        assert_eq!(dominant_colors(&pixels).unwrap(), vec![0xff0000, 0x0000ff]);
        assert_eq!(dominant_colors(&pixels), dominant_colors(&pixels));
        assert!(dominant_colors(&[]).is_err());
        assert!(dominant_colors(&[0; 3]).is_err());
        assert!(dominant_colors(&[0; 4]).is_err());
    }
    #[test]
    fn every_candidate_and_mode_meets_contrast_contract() {
        for appearance in Appearance::ALL {
            for accent in (0..=0xffffff).step_by(8191) {
                let c = palette_from_accent(accent, appearance);
                for bg in [c.canvas, c.surface, c.surface_elevated] {
                    for fg in [c.text, c.muted, c.accent, c.danger, c.warning, c.success] {
                        assert!(contrast_ratio(fg, bg) >= 4.5, "{fg:x} on {bg:x}");
                    }
                    assert!(contrast_ratio(c.line, bg) >= 3.0);
                    assert!(contrast_ratio(c.focus, bg) >= 3.0);
                }
                assert!(contrast_ratio(c.on_accent, c.accent) >= 4.5);
            }
        }
    }
}
