// SPDX-License-Identifier: GPL-3.0-or-later
use crate::settings::{Preferences, Theme};

#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: u32,
    pub surface: u32,
    pub text: u32,
    pub muted: u32,
    pub line: u32,
    pub accent: u32,
    pub on_accent: u32,
    pub focus: u32,
}
impl Palette {
    pub fn for_preferences(p: &Preferences) -> Self {
        if p.studio {
            Self {
                canvas: 0x111111,
                surface: 0x212121,
                text: 0xe5e5e5,
                muted: 0xb8b8b8,
                line: 0x555555,
                accent: 0xe5e5e5,
                on_accent: 0x111111,
                focus: 0xe5e5e5,
            }
        } else {
            match p.theme {
                Theme::Dark => Self {
                    canvas: 0x080d12,
                    surface: 0x17212b,
                    text: 0xd8e5ec,
                    muted: 0xa8bac7,
                    line: 0x364957,
                    accent: 0x49d7e8,
                    on_accent: 0x080d12,
                    focus: 0x49d7e8,
                },
                Theme::Light => Self {
                    canvas: 0xf4f7f9,
                    surface: 0xeaf0f3,
                    text: 0x080d12,
                    muted: 0x3e515d,
                    line: 0xb2c2cc,
                    accent: 0x49d7e8,
                    on_accent: 0x080d12,
                    focus: 0x086a78,
                },
                Theme::WarmPaper => Self {
                    canvas: 0xf7f2e8,
                    surface: 0xeae3d7,
                    text: 0x211f1c,
                    muted: 0x5c564b,
                    line: 0xbcb3a3,
                    accent: 0x49d7e8,
                    on_accent: 0x080d12,
                    focus: 0x526c6c,
                },
            }
        }
    }
}
