//! Atom One Dark and One Light palettes. Colors are fetched every frame, so theme switching is immediate.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use eframe::egui::Color32;

static LIGHT: AtomicBool = AtomicBool::new(false);

pub fn set_light(light: bool) {
    LIGHT.store(light, Relaxed);
}

pub fn is_light() -> bool {
    LIGHT.load(Relaxed)
}

const fn rgb(h: u32) -> Color32 {
    Color32::from_rgb((h >> 16) as u8, (h >> 8) as u8, h as u8)
}

macro_rules! palette {
    ($($(#[$doc:meta])* $name:ident = $dark:literal, $light:literal;)*) => {
        $( $(#[$doc])* pub fn $name() -> Color32 { rgb(if is_light() { $light } else { $dark }) } )*
    };
}

// Colors from the @alamos-research/ui themes: workspace-dark and workspace-light (white with purple tones).
palette! {
    ok = 0x83e1bb, 0x0b7a55;
    err = 0xff9fb5, 0xb72b50;
    warn = 0xffd27a, 0x8f5300;
    accent = 0xb8b0ff, 0x6254d9;
    purple = 0xc5bdff, 0x5042c4;
    cyan = 0xa9c8ff, 0x2459c2;
    /// Secondary text.
    muted = 0xc1c5df, 0x575d78;
    /// Text on accent buttons.
    on_accent = 0x191b37, 0xffffff;
    /// Top and bottom bars, text fields.
    deep = 0x111429, 0xedf1fb;
    /// Central area.
    surface = 0x191d31, 0xf7f8fc;
    /// Side panels.
    panel = 0x242a43, 0xedf1fb;
    faint = 0x303652, 0xe7e6ff;
    line = 0x464d6a, 0xe1e5f0;
    text = 0xf4f5ff, 0x252946;
    bright = 0xffffff, 0x181b34;
    field = 0x383e5d, 0xffffff;
    selection = 0x4b4f8a, 0xe7e6ff;
    hover = 0x373b68, 0xe7e6ff;
    active = 0x464d6a, 0xd6d3fb;
    // Syntax
    syn_key = 0xb8b0ff, 0x6254d9;
    syn_str = 0x83e1bb, 0x087d68;
    syn_num = 0xffd27a, 0x8f5300;
    syn_kw = 0xff9fb5, 0xb72b50;
    syn_fn = 0xa9c8ff, 0x2459c2;
    syn_comment = 0x8c92b0, 0x8a8fa8;
    syn_punct = 0xc1c5df, 0x575d78;
}

/// Faint background behind {{variables}}.
pub fn var_bg() -> Color32 {
    if is_light() { Color32::from_rgba_premultiplied(11, 10, 24, 28) } else { Color32::from_rgba_premultiplied(43, 41, 60, 60) }
}
