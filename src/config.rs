//! On-disk user settings — keybindings, volume, resume mode, mouse actions and
//! subtitle style. Mirrors the original GTK app's `config.rs` so these settings
//! persist across launches instead of resetting every time (the spike used to
//! only persist the recents list).
//!
//! Stored as `config.json` alongside the recents store. Every field is optional
//! so older/newer config files load gracefully (missing fields fall back to
//! defaults).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How the player treats a previously-watched file on reopen.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
pub enum ResumeMode {
    /// Never remember / never resume.
    Off,
    /// Show a prompt offering to resume.
    #[default]
    Ask,
    /// Resume automatically, no prompt.
    Always,
}

impl ResumeMode {
    pub fn from_index(i: i32) -> Self {
        match i {
            0 => ResumeMode::Off,
            2 => ResumeMode::Always,
            _ => ResumeMode::Ask,
        }
    }
    pub fn to_index(self) -> i32 {
        match self {
            ResumeMode::Off => 0,
            ResumeMode::Ask => 1,
            ResumeMode::Always => 2,
        }
    }
}

/// Persisted subtitle appearance. Indices match the spike's palette/font/combo
/// orderings in `main.rs` and `app.slint`.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SubStyle {
    // Colours are now free-form hex ("#RRGGBB"), set via a full RGB picker
    // rather than an 8-swatch palette index. Older configs (which stored the
    // dropped `color`/`border_color` ints) fall back to these defaults.
    #[serde(default = "white_hex")]
    pub text_color: String,
    // Font family name (mpv `sub-font`), e.g. "Sans", "DejaVu Serif".
    #[serde(default = "default_font_name")]
    pub font_name: String,
    #[serde(default = "default_font_size")]
    pub font_size: i64, // mpv `sub-font-size`
    pub scale: f64,        // sub-scale
    pub pos_mode: i32,     // 0 bottom / 1 top / 2 center
    pub pos: f64,          // sub-pos vertical offset
    pub outline: bool, // draw outline
    #[serde(default = "black_hex")]
    pub outline_color: String,
    #[serde(default = "default_outline_size")]
    pub outline_size: f64, // mpv sub-border-size (outline width)
    pub shadow: bool, // draw drop shadow
    #[serde(default = "black_hex")]
    pub shadow_color: String,
    #[serde(default = "default_shadow_offset")]
    pub shadow_offset: f64, // mpv sub-shadow-offset
    pub shaded_bg: bool, // shaded background box behind the text
    #[serde(default = "black_hex")]
    pub bg_color: String,
    #[serde(default = "default_bg_opacity")]
    pub bg_opacity: i64, // 0–100 (alpha of the background box)
    #[serde(default = "default_box_padding")]
    pub box_padding: f64, // space between text and box edge (mpv border-size in box mode)
    #[serde(default)]
    pub line_spacing: f64, // extra gap between subtitle lines (mpv sub-line-spacing)
}

fn default_font_name() -> String {
    "Sans".to_string()
}
fn default_font_size() -> i64 {
    55
}
fn white_hex() -> String {
    "#FFFFFF".to_string()
}
fn black_hex() -> String {
    "#000000".to_string()
}
fn default_bg_opacity() -> i64 {
    50
}
fn default_outline_size() -> f64 {
    3.0
}
fn default_shadow_offset() -> f64 {
    2.0
}
fn default_box_padding() -> f64 {
    6.0
}

impl Default for SubStyle {
    fn default() -> Self {
        Self {
            text_color: white_hex(),
            font_name: default_font_name(),
            font_size: default_font_size(),
            scale: 1.0,
            pos_mode: 0,
            pos: 100.0,
            outline: true,
            outline_color: black_hex(),
            outline_size: default_outline_size(),
            shadow: true,
            shadow_color: black_hex(),
            shadow_offset: default_shadow_offset(),
            shaded_bg: false,
            bg_color: black_hex(),
            bg_opacity: default_bg_opacity(),
            box_padding: default_box_padding(),
            line_spacing: 0.0,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// action key → key-name binding (the spike's normalised KeyName strings).
    pub shortcuts: Option<HashMap<String, String>>,
    pub volume: Option<f64>,
    pub muted: Option<bool>,
    pub resume_mode: Option<ResumeMode>,
    /// Mouse-on-video click actions, by combo index (see app.slint Mouse page).
    pub mouse_single: Option<i32>,
    pub mouse_double: Option<i32>,
    pub mouse_right: Option<i32>,
    pub show_fps: Option<bool>,
    pub sub_style: Option<SubStyle>,
}

fn config_path() -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join("soniq-spike");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("config.json"))
}

impl Config {
    pub fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist atomically (temp file + rename) so a crash mid-write can't
    /// corrupt the existing config.
    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        let Ok(text) = serde_json::to_string_pretty(self) else { return };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}
