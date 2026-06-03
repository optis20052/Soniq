//! Per-file "resume where you left off" history.
//!
//! We remember the playback position of every file/stream (keyed by its URI)
//! in `resume.json`, and on reopening offer to resume. The behaviour is gated
//! by [`ResumeMode`]: off, ask each time, or always resume.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

/// How the player treats a previously-watched file on reopen.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumeMode {
    /// Never remember / never resume.
    Off,
    /// Show a prompt offering to resume.
    Ask,
    /// Resume automatically, no prompt.
    Always,
}

impl Default for ResumeMode {
    fn default() -> Self {
        ResumeMode::Ask
    }
}

impl ResumeMode {
    pub fn from_index(i: u32) -> Self {
        match i {
            0 => ResumeMode::Off,
            2 => ResumeMode::Always,
            _ => ResumeMode::Ask,
        }
    }
    pub fn to_index(self) -> u32 {
        match self {
            ResumeMode::Off => 0,
            ResumeMode::Ask => 1,
            ResumeMode::Always => 2,
        }
    }
}

/// Don't offer to resume within the first N seconds — too little watched.
const RESUME_MIN_NS: u64 = 5 * 1_000_000_000;
/// Consider a file "finished" (and drop its entry) once watched past this
/// fraction of its length, so we don't prompt to resume the closing minutes.
/// Percentage-based so it scales from short clips to long movies.
const RESUME_FINISHED_PCT: u64 = 92;

/// Cap the history so it can't grow without bound — the oldest entries are
/// evicted past this count.
const MAX_ENTRIES: usize = 500;

/// One remembered file: where we stopped, how long it is, and when we last
/// touched it (Unix seconds, for least-recently-used eviction).
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct WatchEntry {
    pub pos_ns: u64,
    pub dur_ns: u64,
    #[serde(default)]
    pub updated_at: u64,
}

/// In-memory watch history, flushed to disk lazily.
#[derive(Clone)]
pub struct ResumeStore {
    map: Rc<RefCell<HashMap<String, WatchEntry>>>,
    dirty: Rc<Cell<bool>>,
}

impl ResumeStore {
    pub fn load() -> Self {
        // Prefer the current (XDG_STATE_HOME) location; otherwise migrate the
        // legacy ~/.config copy and mark dirty so it's rewritten to the new
        // home on the next flush.
        let (map, dirty) = match read_map(store_path()) {
            Some(m) => (m, false),
            None => match legacy_path().and_then(|lp| {
                let m = read_map(Some(lp.clone()))?;
                let _ = std::fs::remove_file(&lp); // migrated; clean up
                Some(m)
            }) {
                Some(m) => (m, true),
                None => (HashMap::new(), false),
            },
        };
        Self {
            map: Rc::new(RefCell::new(map)),
            dirty: Rc::new(Cell::new(dirty)),
        }
    }

    /// Record the current position for `uri`. Prunes the entry once the file is
    /// effectively finished so it won't prompt next time.
    pub fn record(&self, uri: &str, pos_ns: u64, dur_ns: u64) {
        if uri.is_empty() {
            return;
        }
        let mut map = self.map.borrow_mut();
        if dur_ns > 0 && pos_ns.saturating_mul(100) >= dur_ns.saturating_mul(RESUME_FINISHED_PCT) {
            if map.remove(uri).is_some() {
                self.dirty.set(true);
            }
            return;
        }
        map.insert(
            uri.to_string(),
            WatchEntry { pos_ns, dur_ns, updated_at: now_secs() },
        );
        // Bound growth: drop the least-recently-updated entries past the cap.
        while map.len() > MAX_ENTRIES {
            let Some(oldest) = map.iter().min_by_key(|(_, e)| e.updated_at).map(|(k, _)| k.clone())
            else {
                break;
            };
            map.remove(&oldest);
        }
        self.dirty.set(true);
    }

    /// The resume position for `uri`, if it's worth offering (enough watched).
    pub fn resumable(&self, uri: &str) -> Option<WatchEntry> {
        let entry = *self.map.borrow().get(uri)?;
        (entry.pos_ns >= RESUME_MIN_NS).then_some(entry)
    }

    /// Forget a file (e.g. after the user chooses "start over").
    pub fn forget(&self, uri: &str) {
        if self.map.borrow_mut().remove(uri).is_some() {
            self.dirty.set(true);
        }
    }

    /// Write to disk if anything changed since the last flush. The write is
    /// atomic (temp file + rename) so a crash mid-write can't corrupt or wipe
    /// the existing history.
    pub fn flush(&self) {
        if !self.dirty.get() {
            return;
        }
        let Some(path) = store_path() else { return };
        let Ok(text) = serde_json::to_string(&*self.map.borrow()) else { return };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
            self.dirty.set(false);
        } else {
            let _ = std::fs::remove_file(&tmp); // don't leave a stray temp file
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_map(path: Option<PathBuf>) -> Option<HashMap<String, WatchEntry>> {
    let text = std::fs::read_to_string(path?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Current store location: `$XDG_STATE_HOME/soniq/resume.json` (watch history
/// is *state*, not user config, per the XDG base-directory spec).
fn store_path() -> Option<PathBuf> {
    let mut dir = state_dir()?;
    dir.push("soniq");
    std::fs::create_dir_all(&dir).ok()?;
    dir.push("resume.json");
    Some(dir)
}

/// Legacy location used before the move to XDG_STATE_HOME, for one-time
/// migration. Not created here.
fn legacy_path() -> Option<PathBuf> {
    let mut dir = config_dir()?;
    dir.push("soniq");
    dir.push("resume.json");
    Some(dir)
}

fn state_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg));
    }
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/state"))
}

fn config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg));
    }
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config"))
}
