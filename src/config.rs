use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::shortcuts::{Action, Shortcut};
use crate::state::AppState;
use crate::subtitles::SubtitleStyle;

/// On-disk settings. Every field is optional so older/newer configs load
/// gracefully (missing fields fall back to defaults).
#[derive(Default, Serialize, Deserialize)]
pub struct Config {
    pub subtitle_style: Option<SubtitleStyle>,
    /// Subtitle size multiplier (the quick-settings "Scale").
    pub subtitle_scale: Option<f64>,
    /// Subtitle vertical offset / position (bottom margin in px).
    pub subtitle_margin: Option<i32>,
    pub show_debug: Option<bool>,
    pub volume: Option<f64>,
    /// action key → GTK accelerator string.
    pub shortcuts: Option<HashMap<String, String>>,
    /// action key, or absent = "no action".
    pub mouse_single: Option<String>,
    pub mouse_double: Option<String>,
}

fn config_path() -> Option<PathBuf> {
    let mut dir = dirs_config_dir()?;
    dir.push("soniq");
    std::fs::create_dir_all(&dir).ok()?;
    dir.push("config.json");
    Some(dir)
}

/// XDG config dir ($XDG_CONFIG_HOME or ~/.config), without pulling in a crate.
fn dirs_config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg));
    }
    std::env::var("HOME").ok().map(|h| {
        let mut p = PathBuf::from(h);
        p.push(".config");
        p
    })
}

/// Read settings from `AppState` into a serializable Config.
pub fn from_state(state: &AppState) -> Config {
    let shortcuts = Action::all()
        .iter()
        .filter_map(|a| {
            state
                .shortcuts
                .get(*a)
                .and_then(|s| s.accel_name())
                .map(|accel| (a.key().to_string(), accel))
        })
        .collect();

    Config {
        subtitle_style: Some(state.subtitles.style.lock().unwrap().clone()),
        subtitle_scale: Some(state.subtitle_scale.get()),
        subtitle_margin: Some(state.subtitle_margin.get()),
        show_debug: Some(state.show_debug.get()),
        volume: Some(state.volume.get()),
        shortcuts: Some(shortcuts),
        mouse_single: state.mouse.single.get().map(|a| a.key().to_string()),
        mouse_double: state.mouse.double.get().map(|a| a.key().to_string()),
    }
}

/// Apply a loaded Config onto `AppState` (call before building the UI so the
/// initial widgets reflect saved settings).
pub fn apply_to_state(cfg: &Config, state: &AppState) {
    if let Some(style) = &cfg.subtitle_style {
        *state.subtitles.style.lock().unwrap() = style.clone();
    }
    if let Some(s) = cfg.subtitle_scale {
        state.subtitle_scale.set(
            s.clamp(crate::theme::SUBTITLE_SCALE_MIN, crate::theme::SUBTITLE_SCALE_MAX),
        );
    }
    if let Some(m) = cfg.subtitle_margin {
        state.subtitle_margin.set(m.clamp(0, crate::theme::SUBTITLE_MARGIN_MAX));
    }
    if let Some(debug) = cfg.show_debug {
        state.show_debug.set(debug);
    }
    if let Some(v) = cfg.volume {
        state.volume.set(v.clamp(0.0, 1.0));
    }
    if let Some(map) = &cfg.shortcuts {
        for (key, accel) in map {
            if let (Some(action), Some(sc)) = (Action::from_key(key), Shortcut::parse(accel)) {
                state.shortcuts.set(action, sc);
            }
        }
    }
    state
        .mouse
        .single
        .set(cfg.mouse_single.as_deref().and_then(Action::from_key));
    state
        .mouse
        .double
        .set(cfg.mouse_double.as_deref().and_then(Action::from_key));
}

/// Load the config from disk (or default if absent/unreadable).
pub fn load() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

/// Persist the current state to disk.
pub fn save(state: &AppState) {
    let Some(path) = config_path() else { return };
    let cfg = from_state(state);
    if let Ok(text) = serde_json::to_string_pretty(&cfg) {
        let _ = std::fs::write(&path, text);
    }
}
