//! JSON persistence for recently-played files/streams and resume positions.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Cap the history so it can't grow without bound — oldest entries evicted past
/// this count (the list is kept newest-first, so truncation drops the tail).
const MAX_ENTRIES: usize = 200;
/// Consider a file "finished" once watched past this fraction of its length, so
/// we don't offer to resume the closing seconds. The recents entry is kept, but
/// its resume point is cleared.
const RESUME_FINISHED_PCT: f64 = 0.92;
/// Don't offer to resume within the first N seconds — too little watched.
pub const RESUME_MIN: f64 = 5.0;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct RecentEntry {
    pub path: String,
    pub title: String,
    pub last_pos: f64,
    pub duration: f64,
}

impl RecentEntry {
    pub fn is_stream(&self) -> bool {
        self.path.contains("://")
    }
    /// The position worth resuming from, if any (enough watched, not finished).
    pub fn resume_at(&self) -> Option<f64> {
        (self.last_pos > RESUME_MIN).then_some(self.last_pos)
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct Store {
    pub recents: Vec<RecentEntry>,
}

fn store_path() -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join("soniq-spike");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("store.json"))
}

impl Store {
    pub fn load() -> Self {
        let mut s: Store = store_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        s.prune_missing();
        s
    }

    pub fn save(&self) {
        let Some(path) = store_path() else { return };
        let Ok(text) = serde_json::to_string_pretty(self) else { return };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Record (or update) an entry's last-played position, newest first. Streams
    /// are kept too (keyed by URL). A finished file keeps its entry but loses its
    /// resume point.
    pub fn record(&mut self, path: &str, title: &str, pos: f64, dur: f64) {
        let finished = dur > 0.0 && pos >= dur * RESUME_FINISHED_PCT;
        self.recents.retain(|r| r.path != path);
        self.recents.insert(
            0,
            RecentEntry {
                path: path.to_string(),
                title: title.to_string(),
                last_pos: if finished { 0.0 } else { pos },
                duration: dur,
            },
        );
        self.recents.truncate(MAX_ENTRIES);
    }

    pub fn find(&self, path: &str) -> Option<&RecentEntry> {
        self.recents.iter().find(|r| r.path == path)
    }

    /// Forget a single entry (its recents row + resume point).
    pub fn forget(&mut self, path: &str) {
        self.recents.retain(|r| r.path != path);
    }

    pub fn clear(&mut self) {
        self.recents.clear();
    }

    /// Drop recents whose local file no longer exists (streams are always kept).
    pub fn prune_missing(&mut self) {
        self.recents.retain(|r| r.is_stream() || Path::new(&r.path).exists());
    }
}
