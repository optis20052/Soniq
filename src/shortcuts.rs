use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::gdk;

/// Every player action that can be triggered by a keyboard binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    PlayPause,
    Fullscreen,
    Mute,
    VolumeUp,
    VolumeDown,
    SeekBackwardSmall,
    SeekForwardSmall,
    SeekBackwardLarge,
    SeekForwardLarge,
    JumpStart,
    JumpEnd,
    NextTrack,
    PrevTrack,
    OpenFile,
    OpenUrl,
}

impl Action {
    /// Human-readable name, shown in preferences.
    pub fn title(self) -> &'static str {
        match self {
            Action::PlayPause => "Play / Pause",
            Action::Fullscreen => "Toggle fullscreen",
            Action::Mute => "Mute",
            Action::VolumeUp => "Volume up",
            Action::VolumeDown => "Volume down",
            Action::SeekBackwardSmall => "Seek backward 5 s",
            Action::SeekForwardSmall => "Seek forward 5 s",
            Action::SeekBackwardLarge => "Seek backward 10 s",
            Action::SeekForwardLarge => "Seek forward 10 s",
            Action::JumpStart => "Jump to start",
            Action::JumpEnd => "Jump to end",
            Action::NextTrack => "Next file",
            Action::PrevTrack => "Previous file",
            Action::OpenFile => "Open file…",
            Action::OpenUrl => "Open URL…",
        }
    }

    /// Stable string key for config persistence.
    pub fn key(self) -> &'static str {
        match self {
            Action::PlayPause => "play_pause",
            Action::Fullscreen => "fullscreen",
            Action::Mute => "mute",
            Action::VolumeUp => "volume_up",
            Action::VolumeDown => "volume_down",
            Action::SeekBackwardSmall => "seek_back_small",
            Action::SeekForwardSmall => "seek_fwd_small",
            Action::SeekBackwardLarge => "seek_back_large",
            Action::SeekForwardLarge => "seek_fwd_large",
            Action::JumpStart => "jump_start",
            Action::JumpEnd => "jump_end",
            Action::NextTrack => "next_track",
            Action::PrevTrack => "prev_track",
            Action::OpenFile => "open_file",
            Action::OpenUrl => "open_url",
        }
    }

    /// Parse a stable key back into an Action.
    pub fn from_key(key: &str) -> Option<Action> {
        Action::all().iter().copied().find(|a| a.key() == key)
    }

    /// Stable iteration order for preferences UI.
    pub fn all() -> &'static [Action] {
        &[
            Action::PlayPause,
            Action::Mute,
            Action::Fullscreen,
            Action::SeekBackwardSmall,
            Action::SeekForwardSmall,
            Action::SeekBackwardLarge,
            Action::SeekForwardLarge,
            Action::VolumeUp,
            Action::VolumeDown,
            Action::JumpStart,
            Action::JumpEnd,
            Action::NextTrack,
            Action::PrevTrack,
            Action::OpenFile,
            Action::OpenUrl,
        ]
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Shortcut {
    pub key: gdk::Key,
    pub mods: gdk::ModifierType,
}

impl Shortcut {
    /// Format as a human-friendly accelerator label (e.g. "Ctrl+O", "Space").
    /// Uses GTK's own translation so the labels match other GNOME apps.
    pub fn label(self) -> String {
        gtk::accelerator_get_label(self.key, self.mods).to_string()
    }

    /// Parse from a GTK accelerator string like "<Control>o" or "space".
    pub fn parse(accel: &str) -> Option<Self> {
        let (key, mods) = gtk::accelerator_parse(accel)?;
        Some(Self { key, mods })
    }

    /// Serialize to a GTK accelerator string (for config persistence).
    pub fn accel_name(self) -> Option<String> {
        let name = gtk::accelerator_name(self.key, self.mods).to_string();
        if name.is_empty() { None } else { Some(name) }
    }

    /// Normalize a captured key event to a Shortcut. We strip "consumed"
    /// modifiers (like Shift on already-shifted keys) so e.g. Shift+? doesn't
    /// double-up. We also keep only Ctrl / Alt / Super / Shift bits.
    pub fn from_event(key: gdk::Key, mods: gdk::ModifierType) -> Self {
        let kept = gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::SUPER_MASK
            | gdk::ModifierType::SHIFT_MASK;
        Self {
            key,
            mods: mods & kept,
        }
    }
}

#[derive(Clone)]
pub struct Shortcuts {
    inner: Rc<RefCell<HashMap<Action, Shortcut>>>,
}

/// Mouse-click bindings on the video area. `None` = no action.
#[derive(Clone)]
pub struct MouseBindings {
    pub single: Rc<Cell<Option<Action>>>,
    pub double: Rc<Cell<Option<Action>>>,
}

impl MouseBindings {
    pub fn defaults() -> Self {
        Self {
            single: Rc::new(Cell::new(None)),
            double: Rc::new(Cell::new(Some(Action::Fullscreen))),
        }
    }
}

impl Shortcuts {
    pub fn defaults() -> Self {
        let mut m = HashMap::new();
        let none = gdk::ModifierType::empty();
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        m.insert(Action::PlayPause, Shortcut { key: gdk::Key::space, mods: none });
        m.insert(Action::Mute, Shortcut { key: gdk::Key::m, mods: none });
        m.insert(Action::Fullscreen, Shortcut { key: gdk::Key::f, mods: none });
        m.insert(
            Action::SeekBackwardSmall,
            Shortcut { key: gdk::Key::Left, mods: none },
        );
        m.insert(
            Action::SeekForwardSmall,
            Shortcut { key: gdk::Key::Right, mods: none },
        );
        m.insert(
            Action::SeekBackwardLarge,
            Shortcut { key: gdk::Key::j, mods: none },
        );
        m.insert(
            Action::SeekForwardLarge,
            Shortcut { key: gdk::Key::l, mods: none },
        );
        m.insert(Action::VolumeUp, Shortcut { key: gdk::Key::Up, mods: none });
        m.insert(
            Action::VolumeDown,
            Shortcut { key: gdk::Key::Down, mods: none },
        );
        m.insert(
            Action::JumpStart,
            Shortcut { key: gdk::Key::Home, mods: none },
        );
        m.insert(Action::JumpEnd, Shortcut { key: gdk::Key::End, mods: none });
        m.insert(Action::NextTrack, Shortcut { key: gdk::Key::n, mods: none });
        m.insert(Action::PrevTrack, Shortcut { key: gdk::Key::p, mods: none });
        m.insert(Action::OpenFile, Shortcut { key: gdk::Key::o, mods: ctrl });
        m.insert(Action::OpenUrl, Shortcut { key: gdk::Key::l, mods: ctrl });
        Self {
            inner: Rc::new(RefCell::new(m)),
        }
    }

    pub fn get(&self, a: Action) -> Option<Shortcut> {
        self.inner.borrow().get(&a).copied()
    }

    pub fn set(&self, a: Action, s: Shortcut) {
        self.inner.borrow_mut().insert(a, s);
    }

    /// Reset all bindings to the built-in defaults.
    pub fn reset_to_defaults(&self) {
        let defaults = Self::defaults();
        let new_inner = defaults.inner.borrow().clone();
        *self.inner.borrow_mut() = new_inner;
    }

    /// Find which Action (if any) the given key event invokes. Tries
    /// case-insensitive match for letter keys so Shift+M still triggers
    /// "M" binding.
    pub fn lookup(&self, key: gdk::Key, mods: gdk::ModifierType) -> Option<Action> {
        let kept = gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::SUPER_MASK
            | gdk::ModifierType::SHIFT_MASK;
        let mods_clean = mods & kept;
        let key_lower = key.to_lower();

        for (action, sc) in self.inner.borrow().iter() {
            let sc_mods = sc.mods & kept;
            // Match modifiers ignoring Shift for letter-key bindings (so the
            // user can type M or Shift+M for a binding defined as "m"), but
            // require exact modifier match otherwise.
            let modifiers_match = sc_mods == mods_clean
                || (sc.mods.is_empty() && (mods_clean - gdk::ModifierType::SHIFT_MASK).is_empty());
            if !modifiers_match {
                continue;
            }
            if sc.key == key || sc.key.to_lower() == key_lower {
                return Some(*action);
            }
        }
        None
    }
}
