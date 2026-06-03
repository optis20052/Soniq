//! Live audio/video effects, driven by the quick-settings panel.
//!
//! Two filters are installed once at pipeline build (see `pipeline.rs`):
//! a ghost-padded video-filter bin (`videobalance ! gamma ! videocrop !
//! aspectratiocrop ! videoflip`) and an `equalizer-10bands` audio-filter. We
//! keep refs to each child element here so handlers can tweak their properties
//! live during playback — no relinking, no pipeline reload. The bin structure
//! is fixed at build; only properties change at runtime.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gst::prelude::*;

/// Display-aspect / crop target ratios offered in the Video tab.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AspectMode {
    Default,
    R4_3,
    R16_9,
    R16_10,
    R21_9,
    R5_4,
}

impl AspectMode {
    /// All modes, in menu order. The first (`Default`) means "no override".
    pub const ALL: [AspectMode; 6] = [
        AspectMode::Default,
        AspectMode::R4_3,
        AspectMode::R16_9,
        AspectMode::R16_10,
        AspectMode::R21_9,
        AspectMode::R5_4,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AspectMode::Default => "Default",
            AspectMode::R4_3 => "4:3",
            AspectMode::R16_9 => "16:9",
            AspectMode::R16_10 => "16:10",
            AspectMode::R21_9 => "21:9",
            AspectMode::R5_4 => "5:4",
        }
    }

    /// Target ratio as (w, h), or None for `Default` (no crop/override).
    pub fn fraction(self) -> Option<(i32, i32)> {
        match self {
            AspectMode::Default => None,
            AspectMode::R4_3 => Some((4, 3)),
            AspectMode::R16_9 => Some((16, 9)),
            AspectMode::R16_10 => Some((16, 10)),
            AspectMode::R21_9 => Some((21, 9)),
            AspectMode::R5_4 => Some((5, 4)),
        }
    }
}

/// Built-in 10-band equalizer presets (gains in dB for the standard
/// 32/64/125/250/500/1k/2k/4k/8k/16k band layout).
pub const EQ_PRESETS: &[(&str, [f64; 10])] = &[
    ("Flat", [0.0; 10]),
    (
        "Rock",
        [4.0, 3.0, 1.5, -1.0, -1.0, 0.5, 2.0, 3.0, 3.5, 4.0],
    ),
    ("Jazz", [3.0, 2.0, 1.0, 1.5, -1.0, -1.0, 0.0, 1.0, 2.0, 3.0]),
    ("Pop", [-1.0, 0.5, 2.0, 3.0, 3.5, 2.5, 1.0, 0.0, -0.5, -1.0]),
    (
        "Bass Boost",
        [6.0, 5.0, 4.0, 2.5, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    ),
    (
        "Treble Boost",
        [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.5, 4.0, 5.0, 6.0],
    ),
];

/// Run `f` on the element in `slot` if it has been built.
fn with<F: FnOnce(&gst::Element)>(slot: &Arc<Mutex<Option<gst::Element>>>, f: F) {
    if let Ok(g) = slot.lock()
        && let Some(e) = g.as_ref()
    {
        f(e);
    }
}

/// Live effect controls. Cheap to clone (everything is reference-counted), so
/// it lives inside `AppState`.
#[derive(Clone)]
pub struct Effects {
    pub videobalance: Arc<Mutex<Option<gst::Element>>>,
    pub gamma: Arc<Mutex<Option<gst::Element>>>,
    pub videoflip: Arc<Mutex<Option<gst::Element>>>,
    pub videocrop: Arc<Mutex<Option<gst::Element>>>,
    pub aspectratiocrop: Arc<Mutex<Option<gst::Element>>>,
    pub equalizer: Arc<Mutex<Option<gst::Element>>>,
    /// Whether the (CPU) video-filter bin is currently attached to the playbin.
    /// It's installed lazily — only when a video effect is actually used — so
    /// default playback stays on the fast GPU path (no per-frame round-trips).
    pub video_filter_on: Rc<Cell<bool>>,

    // Per-video parameters (reset on each new file).
    pub speed: Rc<Cell<f64>>,
    pub av_offset_ns: Rc<Cell<i64>>,
    pub aspect: Rc<Cell<AspectMode>>,
    pub crop_mode: Rc<Cell<AspectMode>>,
    pub rotation: Rc<Cell<u16>>,
}

impl Effects {
    pub fn new() -> Self {
        Self {
            videobalance: Arc::new(Mutex::new(None)),
            gamma: Arc::new(Mutex::new(None)),
            videoflip: Arc::new(Mutex::new(None)),
            videocrop: Arc::new(Mutex::new(None)),
            aspectratiocrop: Arc::new(Mutex::new(None)),
            equalizer: Arc::new(Mutex::new(None)),
            video_filter_on: Rc::new(Cell::new(false)),
            speed: Rc::new(Cell::new(1.0)),
            av_offset_ns: Rc::new(Cell::new(0)),
            aspect: Rc::new(Cell::new(AspectMode::Default)),
            crop_mode: Rc::new(Cell::new(AspectMode::Default)),
            rotation: Rc::new(Cell::new(0)),
        }
    }

    /// Build the video-filter bin and stash refs to its child elements. Returns
    /// the bin to set as playbin's `video-filter`, or None if any element is
    /// missing (in which case effects are simply unavailable).
    pub fn build_video_filter(&self) -> Option<gst::Element> {
        let videobalance = gst::ElementFactory::make("videobalance").build().ok()?;
        let gamma = gst::ElementFactory::make("gamma").build().ok()?;
        let videocrop = gst::ElementFactory::make("videocrop").build().ok()?;
        let aspectratiocrop = gst::ElementFactory::make("aspectratiocrop").build().ok()?;
        let videoflip = gst::ElementFactory::make("videoflip").build().ok()?;

        let bin = gst::Bin::builder().name("soniq-vfilter").build();
        let chain = [&videobalance, &gamma, &videocrop, &aspectratiocrop, &videoflip];
        bin.add_many(chain).ok()?;
        gst::Element::link_many(chain).ok()?;

        // Expose the chain's ends so playbin can link to the bin.
        let sink_target = videobalance.static_pad("sink")?;
        let ghost_sink = gst::GhostPad::with_target(&sink_target).ok()?;
        ghost_sink.set_active(true).ok()?;
        bin.add_pad(&ghost_sink).ok()?;
        let src_target = videoflip.static_pad("src")?;
        let ghost_src = gst::GhostPad::with_target(&src_target).ok()?;
        ghost_src.set_active(true).ok()?;
        bin.add_pad(&ghost_src).ok()?;

        *self.videobalance.lock().unwrap() = Some(videobalance);
        *self.gamma.lock().unwrap() = Some(gamma);
        *self.videocrop.lock().unwrap() = Some(videocrop);
        *self.aspectratiocrop.lock().unwrap() = Some(aspectratiocrop);
        *self.videoflip.lock().unwrap() = Some(videoflip);

        Some(bin.upcast())
    }

    /// Attach the video-filter bin to the playbin if it isn't already. Returns
    /// true if it was just attached (the caller must Ready-cycle the pipeline
    /// for playbin to pick it up). Building it lazily keeps default playback on
    /// the fast GPU path.
    pub fn ensure_video_filter(&self, pipeline: &gst::Element) -> bool {
        if self.video_filter_on.get() {
            return false;
        }
        if let Some(bin) = self.build_video_filter() {
            pipeline.set_property("video-filter", &bin);
            self.video_filter_on.set(true);
            true
        } else {
            false
        }
    }

    /// Detach the video-filter bin and drop the element refs, returning playback
    /// to the fast no-filter path. Call while the pipeline is in NULL (e.g. at
    /// the start of loading a new file) so the next preroll has no filter.
    pub fn detach_video_filter(&self, pipeline: &gst::Element) {
        pipeline.set_property("video-filter", None::<gst::Element>);
        self.video_filter_on.set(false);
        *self.videobalance.lock().unwrap() = None;
        *self.gamma.lock().unwrap() = None;
        *self.videocrop.lock().unwrap() = None;
        *self.aspectratiocrop.lock().unwrap() = None;
        *self.videoflip.lock().unwrap() = None;
    }

    /// Build the `equalizer-10bands` audio-filter element (also stashed).
    pub fn build_audio_filter(&self) -> Option<gst::Element> {
        let eq = gst::ElementFactory::make("equalizer-10bands").build().ok()?;
        *self.equalizer.lock().unwrap() = Some(eq.clone());
        Some(eq)
    }

    // ---- Live setters (Video) ----

    pub fn set_brightness(&self, v: f64) {
        with(&self.videobalance, |e| e.set_property("brightness", v));
    }
    pub fn set_contrast(&self, v: f64) {
        with(&self.videobalance, |e| e.set_property("contrast", v));
    }
    pub fn set_saturation(&self, v: f64) {
        with(&self.videobalance, |e| e.set_property("saturation", v));
    }
    pub fn set_hue(&self, v: f64) {
        with(&self.videobalance, |e| e.set_property("hue", v));
    }
    pub fn set_gamma(&self, v: f64) {
        with(&self.gamma, |e| e.set_property("gamma", v));
    }

    /// Rotation in degrees (0/90/180/270) via videoflip's `method`.
    pub fn set_rotation(&self, deg: u16) {
        self.rotation.set(deg);
        let nick = match deg {
            90 => "clockwise",
            180 => "rotate-180",
            270 => "counterclockwise",
            _ => "none",
        };
        with(&self.videoflip, |e| {
            let _ = e.set_property_from_str("method", nick);
        });
    }

    /// Display-aspect override via `aspectratiocrop` (crop-to-aspect; `Default`
    /// disables with a 0/1 fraction).
    pub fn set_aspect(&self, mode: AspectMode) {
        self.aspect.set(mode);
        let (n, d) = mode.fraction().unwrap_or((0, 1));
        with(&self.aspectratiocrop, |e| {
            e.set_property("aspect-ratio", gst::Fraction::new(n, d));
        });
    }

    /// Manual crop-to-ratio using explicit `videocrop` margins computed from the
    /// source dimensions. `Default` clears the crop.
    pub fn set_crop(&self, mode: AspectMode, src_w: i32, src_h: i32) {
        self.crop_mode.set(mode);
        let (mut top, mut bottom, mut left, mut right) = (0i32, 0i32, 0i32, 0i32);
        if let Some((aw, ah)) = mode.fraction()
            && src_w > 0
            && src_h > 0
        {
            let target = aw as f64 / ah as f64;
            let source = src_w as f64 / src_h as f64;
            if source > target {
                let new_w = (src_h as f64 * target).round() as i32;
                let crop = ((src_w - new_w) / 2).max(0);
                left = crop;
                right = crop;
            } else if source < target {
                let new_h = (src_w as f64 / target).round() as i32;
                let crop = ((src_h - new_h) / 2).max(0);
                top = crop;
                bottom = crop;
            }
        }
        with(&self.videocrop, |e| {
            e.set_property("top", top);
            e.set_property("bottom", bottom);
            e.set_property("left", left);
            e.set_property("right", right);
        });
    }

    // ---- Live setters (Audio) ----

    /// Set one equalizer band gain (band index 0..=9, gain in dB).
    pub fn set_eq_band(&self, band: usize, gain_db: f64) {
        with(&self.equalizer, |e| {
            e.set_property(format!("band{band}").as_str(), gain_db);
        });
    }

    pub fn set_av_offset(&self, pipeline: &gst::Element, ns: i64) {
        self.av_offset_ns.set(ns);
        pipeline.set_property("av-offset", ns);
    }

    /// Reset everything to defaults for a freshly-loaded video.
    pub fn reset_for_new_video(&self, pipeline: &gst::Element) {
        self.speed.set(1.0);
        self.set_av_offset(pipeline, 0);
        self.set_aspect(AspectMode::Default);
        self.set_crop(AspectMode::Default, 0, 0);
        self.set_rotation(0);
        self.set_brightness(0.0);
        self.set_contrast(1.0);
        self.set_saturation(1.0);
        self.set_hue(0.0);
        self.set_gamma(1.0);
        for b in 0..10 {
            self.set_eq_band(b, 0.0);
        }
    }
}
