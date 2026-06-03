use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::effects::Effects;
use crate::resume::{ResumeMode, ResumeStore};
use crate::shortcuts::{MouseBindings, Shortcuts};
use crate::subtitles::Subtitles;

/// Cheaply-cloneable bag of per-window shared state.
///
/// Everything inside is reference-counted, so cloning `AppState` just bumps
/// refcounts - handlers each get their own copy and can read/write the same
/// underlying cells.
#[derive(Clone)]
pub struct AppState {
    /// Source is a local file:// URI - the whole video is on disk, buffer
    /// indicator should show 100% and watchdog can skip download checks.
    pub is_local: Rc<Cell<bool>>,
    /// User clicked the pause button. The watchdog won't auto-resume in
    /// this case (would fight the user's intent).
    pub user_paused: Rc<Cell<bool>>,
    /// Preferences → Show network stats. Controls the debug overlay.
    pub show_debug: Rc<Cell<bool>>,
    /// Preferences → Show FPS. Controls the live frame-rate overlay.
    pub show_fps: Rc<Cell<bool>>,
    /// Timestamp of the last user-driven seek (change-value event).
    /// `is_dragging_now()` checks this; the watchdog also uses it as a grace
    /// period after a seek so we don't fire mid-buffer-refill.
    pub last_user_seek: Rc<Cell<Instant>>,
    /// Watchdog state: (last_pos_ns, consecutive_stuck_ticks).
    pub stall_state: Rc<Cell<(u64, u32)>>,
    /// Download-rate tracker: (last_sample_instant, last_queue_bytes).
    pub dl_state: Rc<Cell<(Instant, u64)>>,
    /// First queue2/downloadbuffer element from the playbin3 deep tree.
    /// Held cross-thread because deep-element-added fires on the streaming
    /// thread, not the main thread.
    pub queue_ref: Arc<Mutex<Option<gst::Element>>>,
    /// The HTTP source element (souphttpsrc) - used to query the total
    /// byte size of the stream for the buffer indicator.
    pub source_ref: Arc<Mutex<Option<gst::Element>>>,
    /// True while the current source is a network stream that should cache to
    /// disk (the `download` play flag is on). Read on the streaming thread by
    /// the deep-element-added hook, which redirects the download temp file off
    /// the default tmpfs onto real disk - so it must be an atomic, not a Cell.
    pub net_download: Arc<AtomicBool>,
    /// User-configurable keyboard bindings.
    pub shortcuts: Shortcuts,
    /// Mouse single / double-click bindings.
    pub mouse: MouseBindings,
    /// After a hard pipeline reload (watchdog escalation), the bus AsyncDone
    /// handler picks this up and seeks back to the saved position.
    pub pending_restore_pos: Rc<Cell<Option<u64>>>,
    /// Tracks consecutive sink-stall watchdog fires at the *same* position;
    /// when it hits 3, we escalate to a hard reload instead of pause/resume.
    pub consecutive_stalls: Rc<Cell<(u64, u32)>>,
    /// Seek coalescing: the most-recently-requested seek target (ns). The
    /// AsyncDone handler issues this once the in-flight seek completes, so we
    /// only ever have ONE seek in flight - rapid scrubbing can't flood the
    /// hardware decoder with flush-seeks (which wedges nvh264dec).
    pub pending_seek: Rc<Cell<Option<u64>>>,
    /// True while a flush-seek is in flight (awaiting AsyncDone).
    pub seek_in_flight: Rc<Cell<bool>>,
    /// Subtitle state: style, hooked overlay, track collection.
    pub subtitles: Subtitles,
    /// When an external subtitle is being loaded, holds the playback position
    /// to restore. The bus AsyncDone handler (PAUSED preroll) seeks here and
    /// selects the external track, so the external subtitle source shares the
    /// video's seek and stays in sync. None = no pending sub load.
    pub pending_sub_restore: Rc<Cell<Option<u64>>>,
    /// Sorted video files in the current file's folder (the playlist).
    pub playlist: Rc<RefCell<Vec<std::path::PathBuf>>>,
    /// Index of the currently-playing file within `playlist`.
    pub playlist_idx: Rc<Cell<usize>>,
    /// Last volume (0.0–1.0), mirrored from the slider and persisted.
    pub volume: Rc<Cell<f64>>,
    /// Live audio/video effects (quick-settings panel). Element refs are filled
    /// in by `build_pipeline`; the params reset per video.
    pub effects: Effects,
    /// Subtitle timing offset in ns (positive = subs appear later). Consulted
    /// when looking up the active cue. Resets per video.
    pub subtitle_delay_ns: Rc<Cell<i64>>,
    /// Subtitle size multiplier (1.0 = the style's font size). Resets per video.
    pub subtitle_scale: Rc<Cell<f64>>,
    /// Subtitle vertical position as a bottom margin in px. Resets per video.
    pub subtitle_margin: Rc<Cell<i32>>,
    /// The currently-loaded URI, kept so a hardware-decode toggle can reload.
    pub current_uri: Rc<RefCell<Option<String>>>,
    /// Per-file "resume where you left off" history + how to use it.
    pub resume_store: ResumeStore,
    pub resume_mode: Rc<Cell<ResumeMode>>,
    /// Set on load when we should auto-seek to a remembered position once the
    /// pipeline prerolls (Always mode); the AsyncDone handler consumes it.
    pub pending_resume_pos: Rc<Cell<Option<u64>>>,
    /// In Ask mode, the position the resume banner would jump to (None = no
    /// pending prompt).
    pub resume_prompt_pos: Rc<Cell<Option<u64>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            is_local: Rc::new(Cell::new(false)),
            user_paused: Rc::new(Cell::new(false)),
            show_debug: Rc::new(Cell::new(false)),
            show_fps: Rc::new(Cell::new(false)),
            last_user_seek: Rc::new(Cell::new(
                Instant::now() - std::time::Duration::from_secs(10),
            )),
            stall_state: Rc::new(Cell::new((0, 0))),
            dl_state: Rc::new(Cell::new((Instant::now(), 0))),
            queue_ref: Arc::new(Mutex::new(None)),
            source_ref: Arc::new(Mutex::new(None)),
            net_download: Arc::new(AtomicBool::new(false)),
            shortcuts: Shortcuts::defaults(),
            mouse: MouseBindings::defaults(),
            pending_restore_pos: Rc::new(Cell::new(None)),
            consecutive_stalls: Rc::new(Cell::new((0, 0))),
            pending_seek: Rc::new(Cell::new(None)),
            seek_in_flight: Rc::new(Cell::new(false)),
            subtitles: Subtitles::new(),
            pending_sub_restore: Rc::new(Cell::new(None)),
            playlist: Rc::new(RefCell::new(Vec::new())),
            playlist_idx: Rc::new(Cell::new(0)),
            volume: Rc::new(Cell::new(1.0)),
            effects: Effects::new(),
            subtitle_delay_ns: Rc::new(Cell::new(0)),
            subtitle_scale: Rc::new(Cell::new(crate::theme::SUBTITLE_SCALE_DEFAULT)),
            subtitle_margin: Rc::new(Cell::new(crate::theme::SUBTITLE_MARGIN_DEFAULT)),
            current_uri: Rc::new(RefCell::new(None)),
            resume_store: ResumeStore::load(),
            resume_mode: Rc::new(Cell::new(ResumeMode::default())),
            pending_resume_pos: Rc::new(Cell::new(None)),
            resume_prompt_pos: Rc::new(Cell::new(None)),
        }
    }

    /// Request a seek to `target_ns`, coalesced. If no seek is in flight,
    /// issue it immediately; otherwise stash it so the AsyncDone handler
    /// issues the latest target when the current seek completes.
    pub fn request_seek(&self, pipeline: &gst::Element, target_ns: u64) {
        self.last_user_seek.set(Instant::now());
        if self.seek_in_flight.get() {
            self.pending_seek.set(Some(target_ns));
        } else {
            self.seek_in_flight.set(true);
            self.pending_seek.set(None);
            do_seek(pipeline, target_ns, self.effects.speed.get());
        }
    }

    /// Called from the AsyncDone bus handler. If another seek was requested
    /// while this one was in flight, issue it now; else mark idle.
    pub fn on_seek_done(&self, pipeline: &gst::Element) {
        if let Some(target_ns) = self.pending_seek.take() {
            do_seek(pipeline, target_ns, self.effects.speed.get());
        } else {
            self.seek_in_flight.set(false);
        }
    }

    /// True if the user touched the seek bar in the last 300 ms.
    /// Used everywhere we need to back off so we don't fight the user.
    pub fn is_dragging_now(&self) -> bool {
        self.last_user_seek.get().elapsed() < std::time::Duration::from_millis(300)
    }
}

fn do_seek(pipeline: &gst::Element, target_ns: u64, rate: f64) {
    use gst::prelude::ElementExtManual;
    let flags = gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE;
    let target = gst::ClockTime::from_nseconds(target_ns);
    // ACCURATE (not KEY_UNIT|SNAP_NEAREST): land exactly on the requested
    // time. Keyframe-snapping made repeated clicks at the same spot jump to
    // different sparse keyframes (-10/-20s). Coalescing keeps only one of
    // these in flight so the decoder isn't flooded.
    if (rate - 1.0).abs() < 1e-6 {
        pipeline.seek_simple(flags, target).ok();
    } else {
        // A full seek is needed to carry a non-1.0 playback rate; seek_simple
        // would silently reset it to 1.0.
        pipeline
            .seek(
                rate,
                flags,
                gst::SeekType::Set,
                target,
                gst::SeekType::End,
                gst::ClockTime::ZERO,
            )
            .ok();
    }
}
