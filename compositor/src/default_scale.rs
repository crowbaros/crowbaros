//! The scale a display starts at when the layout of displays does not name
//! it (see `layout.rs`): picked from its density, so that a person can read
//! a dense display without editing a file.
//!
//! A display's density is its pixels over its physical size, which the
//! kernel reads from its EDID. A scale of 1 suits a density of about 110
//! pixels an inch on a monitor, and about 135 on a laptop's panel, which is
//! looked at from closer. A display under 20 inches counts as a panel. The
//! scale is the density over that, to the nearest quarter, from 1 to 3.
//!
//! A display that reports no size gets 1, and so does one whose size cannot
//! be its own: under 4 inches, or of another shape than its pixels, as an
//! EDID that gives only the aspect ratio is.

use crate::output::SCALE_ONE;

const MM_PER_INCH: f64 = 25.4;
/// The density a scale of 1 suits on a monitor, in pixels an inch.
const MONITOR_DENSITY: f64 = 110.0;
/// The density a scale of 1 suits on a laptop's panel, looked at from
/// closer.
const PANEL_DENSITY: f64 = 135.0;
/// The diagonal under which a display counts as a panel.
const PANEL_INCHES: f64 = 20.0;
/// The smallest diagonal believed.
const SMALLEST_INCHES: f64 = 4.0;
/// How far the shape of the physical size may be from the shape of the
/// pixels, as a ratio of the two.
const SHAPE_SLACK: f64 = 1.2;

/// The scale, in 120ths, a display of `pixels` starts at, given its
/// physical size in millimetres, where (0, 0) is none reported.
pub(crate) fn default_scale_120(pixels: (i32, i32), mm: (u32, u32)) -> u32 {
    let (width, height) = (f64::from(pixels.0), f64::from(pixels.1));
    let (width_mm, height_mm) = (f64::from(mm.0), f64::from(mm.1));
    if width <= 0.0 || height <= 0.0 || width_mm <= 0.0 || height_mm <= 0.0 {
        return SCALE_ONE;
    }
    let inches = width_mm.hypot(height_mm) / MM_PER_INCH;
    if inches < SMALLEST_INCHES {
        return SCALE_ONE;
    }
    // A display turned on its side reports its size the other way round.
    let shape = (width / height) / (width_mm / height_mm);
    let turned = (width / height) / (height_mm / width_mm);
    let near = |ratio: f64| ratio.max(1.0 / ratio) <= SHAPE_SLACK;
    if !near(shape) && !near(turned) {
        return SCALE_ONE;
    }
    let density = width.hypot(height) / inches;
    let suits = if inches < PANEL_INCHES {
        PANEL_DENSITY
    } else {
        MONITOR_DENSITY
    };
    let quarters = (density / suits * 4.0).round().clamp(4.0, 12.0);
    quarters as u32 * (SCALE_ONE / 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_1080p_monitor_starts_at_1() {
        // 24 inches.
        assert_eq!(default_scale_120((1920, 1080), (531, 299)), 120);
        // 27 inches.
        assert_eq!(default_scale_120((1920, 1080), (598, 336)), 120);
    }

    #[test]
    fn a_4k_27_inch_monitor_starts_at_one_and_a_half() {
        assert_eq!(default_scale_120((3840, 2160), (597, 336)), 180);
    }

    #[test]
    fn a_dense_laptop_panel_starts_above_1() {
        // A 13.5-inch 2256x1504 panel.
        assert_eq!(default_scale_120((2256, 1504), (285, 190)), 180);
        // A 13.3-inch 2560x1600 panel.
        assert_eq!(default_scale_120((2560, 1600), (286, 179)), 210);
        // A 15.6-inch 4K panel.
        assert_eq!(default_scale_120((3840, 2160), (344, 194)), 240);
    }

    #[test]
    fn an_ordinary_laptop_panel_starts_at_1() {
        // A 15.6-inch 1080p panel.
        assert_eq!(default_scale_120((1920, 1080), (344, 194)), 120);
    }

    #[test]
    fn a_display_that_reports_no_size_starts_at_1() {
        assert_eq!(default_scale_120((3840, 2160), (0, 0)), 120);
        assert_eq!(default_scale_120((3840, 2160), (597, 0)), 120);
    }

    #[test]
    fn a_size_that_cannot_be_the_displays_own_starts_at_1() {
        // An EDID that gives only the aspect ratio, in centimetres.
        assert_eq!(default_scale_120((3840, 2160), (16, 9)), 120);
        // A size of another shape than the pixels.
        assert_eq!(default_scale_120((3840, 2160), (300, 300)), 120);
    }

    #[test]
    fn a_display_on_its_side_starts_as_it_would_upright() {
        assert_eq!(default_scale_120((2160, 3840), (597, 336)), 180);
        assert_eq!(default_scale_120((2160, 3840), (336, 597)), 180);
    }

    #[test]
    fn the_scale_is_never_above_3() {
        // An 8K panel of 13 inches.
        assert_eq!(default_scale_120((7680, 4320), (288, 162)), 360);
    }

    #[test]
    fn a_large_monitor_of_middling_density_is_in_quarter_steps() {
        // A 5K2K 40-inch monitor: 140 pixels an inch.
        assert_eq!(default_scale_120((5120, 2160), (925, 390)), 150);
        // A 5K 27-inch monitor.
        assert_eq!(default_scale_120((5120, 2880), (597, 336)), 240);
    }
}
