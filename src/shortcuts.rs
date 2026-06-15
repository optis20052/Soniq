//! Keyboard shortcuts: the action table, the data-driven dispatcher that maps
//! an action name to a player/UI command, and the key→action binding lookup.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, VecModel};

use crate::util::fmt_time;
use crate::video::VideoBridge;
use crate::{App, ShortcutItem};

/// "1:23 / 45:00" OSD string for a seek target.
fn time_osd(t: f64, dur: f64) -> String {
    format!("{} / {}", fmt_time(t.clamp(0.0, dur.max(0.0))), fmt_time(dur))
}

// (action key, display title, default binding) — order matches the prefs list.
pub const ACTIONS: &[(&str, &str, &str)] = &[
    ("play_pause", "Play / Pause", "Space"),
    ("mute", "Mute", "m"),
    ("fullscreen", "Toggle fullscreen", "f"),
    ("seek_back_small", "Seek backward 5 s", "Left"),
    ("seek_fwd_small", "Seek forward 5 s", "Right"),
    ("seek_back_large", "Seek backward 10 s", "j"),
    ("seek_fwd_large", "Seek forward 10 s", "l"),
    ("volume_up", "Volume up", "Up"),
    ("volume_down", "Volume down", "Down"),
    ("jump_start", "Jump to start", "Home"),
    ("jump_end", "Jump to end", "End"),
    ("next_track", "Next file", "n"),
    ("prev_track", "Previous file", "p"),
    ("open_file", "Open file…", "Ctrl+o"),
    ("open_url", "Open URL…", "Ctrl+l"),
];

/// Wire keyboard handling: builds the shortcuts list model (shown in prefs),
/// the action dispatcher, and the `key → action` lookup. Returns the model and
/// a `rebuild` closure so the prefs window can re-render the list after a
/// binding changes.
pub fn install(
    app: &App,
    bridge: Rc<RefCell<Option<VideoBridge>>>,
    bindings: Rc<RefCell<Vec<String>>>,
    set_fullscreen: Rc<dyn Fn(bool)>,
    show_osd: Rc<dyn Fn(&str)>,
) -> (Rc<VecModel<ShortcutItem>>, Rc<dyn Fn()>) {
    let shortcuts_model = Rc::new(VecModel::<ShortcutItem>::default());
    let rebuild_shortcuts: Rc<dyn Fn()> = {
        let bindings = bindings.clone();
        let shortcuts_model = shortcuts_model.clone();
        Rc::new(move || {
            let b = bindings.borrow();
            let items: Vec<ShortcutItem> = ACTIONS
                .iter()
                .enumerate()
                .map(|(i, a)| ShortcutItem {
                    title: a.1.into(),
                    keys: b[i].clone().into(),
                })
                .collect();
            shortcuts_model.set_vec(items);
        })
    };
    rebuild_shortcuts();

    let dispatch: Rc<dyn Fn(&str)> = {
        let bridge = bridge.clone();
        let weak = app.as_weak();
        let set_fullscreen = set_fullscreen.clone();
        let show_osd = show_osd.clone();
        Rc::new(move |action: &str| {
            if let Some(a) = weak.upgrade() {
                match action {
                    "open_file" => {
                        a.invoke_open_file();
                        return;
                    }
                    "open_url" => {
                        a.set_url_open(true);
                        return;
                    }
                    "fullscreen" => {
                        let fs = !a.window().is_fullscreen();
                        set_fullscreen(fs);
                        show_osd(if fs { "Fullscreen" } else { "Windowed" });
                        return;
                    }
                    _ => {}
                }
            }
            if let Some(b) = bridge.borrow().as_ref() {
                // mpv applies seeks/state async, so OSD targets are computed from
                // the pre-action state rather than read back immediately.
                let pos = b.position();
                let dur = b.duration();
                match action {
                    "play_pause" => {
                        let now_paused = !b.is_paused();
                        b.toggle_pause();
                        show_osd(if now_paused { "Paused" } else { "Playing" });
                    }
                    "mute" => {
                        let now_muted = !b.is_muted();
                        b.toggle_mute();
                        show_osd(if now_muted { "Muted" } else { "Unmuted" });
                    }
                    "seek_back_small" => { b.seek_relative(-5.0); show_osd(&time_osd(pos - 5.0, dur)); }
                    "seek_fwd_small" => { b.seek_relative(5.0); show_osd(&time_osd(pos + 5.0, dur)); }
                    "seek_back_large" => { b.seek_relative(-10.0); show_osd(&time_osd(pos - 10.0, dur)); }
                    "seek_fwd_large" => { b.seek_relative(10.0); show_osd(&time_osd(pos + 10.0, dur)); }
                    "volume_up" => {
                        let v = (b.volume() + 0.05).clamp(0.0, 1.0);
                        b.set_volume(v);
                        show_osd(&format!("Volume {}%", (v * 100.0).round() as i32));
                    }
                    "volume_down" => {
                        let v = (b.volume() - 0.05).clamp(0.0, 1.0);
                        b.set_volume(v);
                        show_osd(&format!("Volume {}%", (v * 100.0).round() as i32));
                    }
                    "jump_start" => { b.seek_seconds(0.0); show_osd(&time_osd(0.0, dur)); }
                    "jump_end" => {
                        let t = (dur - 1.0).max(0.0);
                        b.seek_seconds(t);
                        show_osd(&time_osd(t, dur));
                    }
                    "next_track" => { b.playlist_next(); show_osd("Next"); }
                    "prev_track" => { b.playlist_prev(); show_osd("Previous"); }
                    _ => {}
                }
            }
        })
    };
    {
        let bindings = bindings.clone();
        let dispatch = dispatch.clone();
        app.on_key_event(move |name| {
            let name = name.to_string();
            let idx = bindings.borrow().iter().position(|x| *x == name);
            if let Some(idx) = idx {
                dispatch(ACTIONS[idx].0);
            }
        });
    }

    (shortcuts_model, rebuild_shortcuts)
}
