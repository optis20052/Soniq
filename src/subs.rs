//! Subtitle styling: hex/Slint colour conversion and pushing a persisted
//! `SubStyle` onto mpv (the tricky mpv border-style / back-colour rules live
//! here, out of `main`).

use crate::config::SubStyle;
use crate::video::VideoBridge;

pub const SUB_POS_MODES: [&str; 3] = ["bottom", "top", "center"];

/// "#RRGGBB" → (r, g, b). Tolerates a missing '#'; defaults to white.
fn parse_rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    let n = u32::from_str_radix(h, 16).unwrap_or(0xFFFFFF);
    (((n >> 16) & 0xFF) as u8, ((n >> 8) & 0xFF) as u8, (n & 0xFF) as u8)
}

/// mpv `sub-back-color` is `#AARRGGBB` (straight alpha: 00 = transparent,
/// FF = opaque). Combine a "#RRGGBB" with a 0–100 opacity.
pub fn mpv_back_color(hex: &str, opacity: i64) -> String {
    let (r, g, b) = parse_rgb(hex);
    let a = ((opacity.clamp(0, 100) as f32 / 100.0) * 255.0).round() as u8;
    format!("#{a:02X}{r:02X}{g:02X}{b:02X}")
}

/// Slint `Color` ⇄ "#RRGGBB" (for pushing colours into / out of the picker).
pub fn slint_color_from_hex(hex: &str) -> slint::Color {
    let (r, g, b) = parse_rgb(hex);
    slint::Color::from_rgb_u8(r, g, b)
}
pub fn hex_from_slint(c: slint::Color) -> String {
    format!("#{:02X}{:02X}{:02X}", c.red(), c.green(), c.blue())
}

/// Apply a slider value to an mpv property, routing the integer-valued ones
/// (brightness/contrast/…/sub-pos) through `set_prop_i64` and the rest through
/// `set_prop_f64`. Shared by the in-player quick-settings drawer and the
/// Preferences subtitle page.
pub fn apply_adjust(b: &VideoBridge, key: &str, val: f32) {
    const INT_PROPS: &[&str] = &[
        "brightness",
        "contrast",
        "saturation",
        "gamma",
        "hue",
        "video-rotate",
        "sub-pos",
        "sub-font-size",
    ];
    if INT_PROPS.contains(&key) {
        b.set_prop_i64(key, val.round() as i64);
    } else {
        b.set_prop_f64(key, val as f64);
    }
}

/// Push a persisted subtitle style onto mpv. Called once at startup (so saved
/// appearance is restored) and whenever a subtitle control changes.
pub fn apply_sub_style(b: &VideoBridge, s: &SubStyle) {
    b.set_prop_str("sub-color", &s.text_color);
    b.set_prop_str("sub-font", &s.font_name);
    b.set_prop_i64("sub-font-size", s.font_size);
    b.set_prop_f64("sub-scale", s.scale);
    b.set_prop_str("sub-align-y", SUB_POS_MODES[s.pos_mode.clamp(0, 2) as usize]);
    b.set_prop_i64("sub-pos", s.pos.round() as i64);
    // NB on this mpv: `sub-shadow-color` is an ALIAS for `sub-back-color`, and a
    // box needs `sub-border-style=background-box` (back-color alone draws
    // nothing). So shadow and the background box share one colour property and
    // are mutually exclusive: the box wins when on, else outline-and-shadow runs
    // with back-color doubling as the shadow colour.
    if s.shaded_bg {
        // `background-box` with border-size 0 draws ONE unified rectangle behind
        // ALL lines (clean, no overlap, no per-glyph halo). The box fill is
        // `sub-back-color`; border-size MUST stay 0 (any value turns it into
        // per-glyph outlines — the "shaded bg adds an outline" bug). mpv has no
        // per-side or adjustable box padding, so the box auto-pads.
        let box_col = mpv_back_color(&s.bg_color, s.bg_opacity);
        b.set_prop_str("sub-border-style", "background-box");
        b.set_prop_str("sub-back-color", &box_col);
        b.set_prop_str("sub-border-color", &box_col);
        b.set_prop_f64("sub-border-size", 0.0);
        b.set_prop_f64("sub-shadow-offset", 0.0);
        b.set_prop_f64("sub-line-spacing", s.line_spacing);
    } else {
        b.set_prop_str("sub-border-style", "outline-and-shadow");
        b.set_prop_str("sub-border-color", &s.outline_color);
        b.set_prop_f64("sub-border-size", if s.outline { s.outline_size } else { 0.0 });
        b.set_prop_str("sub-back-color", if s.shadow { &s.shadow_color } else { "#00000000" });
        b.set_prop_f64("sub-shadow-offset", if s.shadow { s.shadow_offset } else { 0.0 });
        b.set_prop_f64("sub-line-spacing", s.line_spacing);
    }
}
