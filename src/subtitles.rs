use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gst::prelude::*;

/// One subtitle cue: shown while playback position is in [start_ns, end_ns).
#[derive(Clone, Debug)]
pub struct Cue {
    pub start_ns: u64,
    pub end_ns: u64,
    pub text: String,
}

/// Parse SRT text (already decoded to UTF-8) into timed cues. Tolerant of
/// WebVTT-ish "." millisecond separators and stray blank lines.
pub fn parse_srt(content: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    // Normalize line endings, split on blank lines into blocks.
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    for block in normalized.split("\n\n") {
        let lines: Vec<&str> = block.lines().collect();
        if lines.is_empty() {
            continue;
        }
        // Find the timing line (contains "-->"); the index line above is optional.
        let timing_idx = lines.iter().position(|l| l.contains("-->"));
        let Some(ti) = timing_idx else { continue };
        let timing = lines[ti];
        let Some((start, end)) = parse_timing(timing) else { continue };
        let text = lines[ti + 1..].join("\n");
        let text = strip_tags(&text);
        if text.trim().is_empty() {
            continue;
        }
        cues.push(Cue {
            start_ns: start,
            end_ns: end,
            text,
        });
    }
    cues
}

/// Parse "00:00:35,351 --> 00:01:15,021" into (start_ns, end_ns).
fn parse_timing(line: &str) -> Option<(u64, u64)> {
    let (a, b) = line.split_once("-->")?;
    Some((parse_ts(a.trim())?, parse_ts(b.trim())?))
}

/// Parse "HH:MM:SS,mmm" (or with '.') into nanoseconds.
fn parse_ts(s: &str) -> Option<u64> {
    let s = s.split_whitespace().next().unwrap_or(s); // drop trailing cue settings
    let (hms, ms) = s.split_once([',', '.'])?;
    let parts: Vec<&str> = hms.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h: u64 = parts[0].parse().ok()?;
    let m: u64 = parts[1].parse().ok()?;
    let sec: u64 = parts[2].parse().ok()?;
    let millis: u64 = ms.trim().parse().ok()?;
    Some(((h * 3600 + m * 60 + sec) * 1000 + millis) * 1_000_000)
}

/// Strip basic SRT/HTML markup tags (<i>, </i>, <b>, font tags, etc.).
pub fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Vertical placement of the subtitle text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VAlign {
    Bottom,
    Top,
    Center,
}

impl VAlign {
    /// textoverlay's "valignment" enum nick.
    fn nick(self) -> &'static str {
        match self {
            VAlign::Bottom => "bottom",
            VAlign::Top => "top",
            VAlign::Center => "center",
        }
    }
}

/// User-tunable subtitle appearance. All fields have sensible defaults that
/// match a typical "white text, black outline, bottom" look.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SubtitleStyle {
    /// Pango font description, e.g. "Sans Bold 24".
    pub font_desc: String,
    /// Text fill color, big-endian ARGB (0xAARRGGBB).
    pub color: u32,
    /// Outline color, big-endian ARGB.
    pub outline_color: u32,
    pub draw_outline: bool,
    pub draw_shadow: bool,
    /// Shade a translucent box behind the text.
    pub shaded_background: bool,
    pub valign: VAlign,
}

impl Default for SubtitleStyle {
    fn default() -> Self {
        Self {
            font_desc: "Sans Bold 22".to_string(),
            color: 0xFFFF_FFFF,        // opaque white
            outline_color: 0xFF00_0000, // opaque black
            draw_outline: true,
            draw_shadow: true,
            shaded_background: false,
            valign: VAlign::Bottom,
        }
    }
}

/// Shared subtitle state: the live style, the hooked textoverlay element, and
/// the pipeline + stream-collection bookkeeping for track selection.
/// The playbin `flags` property is a `GstPlayFlags` enum that gstreamer-rs
/// doesn't expose as a Rust type, so we can't read it as u32. Instead we
/// track the flags as the nick string (which `set_property_from_str` accepts)
/// and toggle the "text" token for subtitle on/off.
// No "text" flag: playbin never renders subtitles itself. We render external
// subtitles ourselves in a GTK label (reliable + styleable), which avoids
// double-rendering with embedded tracks.
pub const DEFAULT_FLAGS: &str =
    "soft-colorbalance+deinterlace+soft-volume+audio+video";

/// One loaded external subtitle: its display name and parsed cues.
#[derive(Clone)]
pub struct ExternalSub {
    pub name: String,
    pub cues: Vec<Cue>,
}

/// What's currently displayed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Active {
    Off,
    /// An embedded playbin text track (rendered by playbin's textoverlay).
    Embedded(i32),
    /// One of our loaded external subs (rendered by our GTK label).
    External(usize),
}

#[derive(Clone)]
pub struct Subtitles {
    /// Arc<Mutex> (not Rc<RefCell>) because the deep-element-added hook reads
    /// it on the streaming thread to style the overlay as soon as it appears.
    pub style: Arc<Mutex<SubtitleStyle>>,
    /// The internal textoverlay created by subtitleoverlay (embedded subs).
    pub overlay_ref: Arc<Mutex<Option<gst::Element>>>,
    /// The current playbin flags as a nick string (main thread only).
    pub flags: Rc<RefCell<String>>,
    /// All external subs loaded this session (kept so the menu can list them).
    pub externals: Rc<RefCell<Vec<ExternalSub>>>,
    /// What's currently active.
    pub active: Rc<RefCell<Active>>,
    /// Latest embedded subtitle cue (start_ns, end_ns, text), delivered by the
    /// text-sink appsink on the streaming thread. Rendered by our label so
    /// embedded subs match external ones. Arc<Mutex> for cross-thread access.
    pub embedded_cue: Arc<Mutex<Option<(u64, u64, String)>>>,
}

impl Subtitles {
    pub fn new() -> Self {
        Self {
            style: Arc::new(Mutex::new(SubtitleStyle::default())),
            overlay_ref: Arc::new(Mutex::new(None)),
            flags: Rc::new(RefCell::new(DEFAULT_FLAGS.to_string())),
            externals: Rc::new(RefCell::new(Vec::new())),
            active: Rc::new(RefCell::new(Active::Off)),
            embedded_cue: Arc::new(Mutex::new(None)),
        }
    }

    /// Text of the active cue at `pos_ns`, or None. Both embedded (via the
    /// text-sink appsink) and external (parsed SRT) feed the same label.
    pub fn active_cue_text(&self, pos_ns: u64) -> Option<String> {
        match *self.active.borrow() {
            Active::Off => None,
            Active::External(idx) => {
                let externals = self.externals.borrow();
                let sub = externals.get(idx)?;
                sub.cues
                    .iter()
                    .find(|c| pos_ns >= c.start_ns && pos_ns < c.end_ns)
                    .map(|c| c.text.clone())
            }
            Active::Embedded(_) => {
                let cue = self.embedded_cue.lock().ok()?;
                let (start, end, text) = cue.as_ref()?;
                if pos_ns >= *start && pos_ns < *end {
                    Some(text.clone())
                } else {
                    None
                }
            }
        }
    }

    /// Record + apply a new base flags string (called on each video load).
    pub fn set_flags(&self, pipeline: &gst::Element, flags: &str) {
        *self.flags.borrow_mut() = flags.to_string();
        let _ = pipeline.set_property_from_str("flags", flags);
    }

    /// Apply the current style to the hooked textoverlay element (if present).
    pub fn apply_style(&self) {
        let style = self.style.lock().unwrap();
        if let Ok(slot) = self.overlay_ref.lock()
            && let Some(ov) = slot.as_ref()
        {
            set_overlay_style(ov, &style);
        }
    }

    /// Pango font-desc string (used for embedded subs via playbin).
    pub fn font_desc(&self) -> String {
        self.style.lock().unwrap().font_desc.clone()
    }

    /// Add an external sub (parsing SRT text); returns its index. Does not
    /// activate it - the caller selects it.
    pub fn add_external(&self, content: &str, name: String) -> usize {
        let cues = parse_srt(content);
        eprintln!("[subs] parsed {} cues from {name}", cues.len());
        let mut v = self.externals.borrow_mut();
        v.push(ExternalSub { name, cues });
        v.len() - 1
    }

    /// List embedded playbin text tracks as (index, label).
    pub fn embedded_tracks(&self, pipeline: &gst::Element) -> Vec<(i32, String)> {
        let n: i32 = pipeline.property("n-text");
        let mut out = Vec::new();
        for i in 0..n {
            let tags: Option<gst::TagList> = pipeline.emit_by_name("get-text-tags", &[&i]);
            let label = tags
                .as_ref()
                .and_then(|t| t.get::<gst::tags::LanguageName>().map(|v| v.get().to_string()))
                .or_else(|| {
                    tags.as_ref().and_then(|t| {
                        t.get::<gst::tags::LanguageCode>().map(|v| v.get().to_string())
                    })
                })
                .unwrap_or_else(|| format!("Embedded track {}", i + 1));
            out.push((i, label));
        }
        out
    }

    /// Toggle the playbin "text" flag (controls embedded-subtitle rendering).
    fn set_text_flag(&self, pipeline: &gst::Element, on: bool) {
        let new_flags = {
            let cur = self.flags.borrow();
            let mut parts: Vec<&str> = cur.split('+').filter(|p| *p != "text").collect();
            if on {
                parts.push("text");
            }
            parts.join("+")
        };
        *self.flags.borrow_mut() = new_flags.clone();
        let _ = pipeline.set_property_from_str("flags", &new_flags);
    }

    /// Turn all subtitles off.
    pub fn set_off(&self, pipeline: &gst::Element) {
        self.set_text_flag(pipeline, false);
        *self.embedded_cue.lock().unwrap() = None;
        *self.active.borrow_mut() = Active::Off;
    }

    /// Show an embedded track. The TEXT flag makes playbin decode the text
    /// stream; our custom text-sink (appsink) receives the buffers, which the
    /// label renders - so embedded subs match external ones in style.
    pub fn set_embedded(&self, pipeline: &gst::Element, index: i32) {
        *self.embedded_cue.lock().unwrap() = None;
        self.set_text_flag(pipeline, true);
        pipeline.set_property("current-text", index);
        *self.active.borrow_mut() = Active::Embedded(index);
    }

    /// Show one of our loaded external subs (rendered by our label).
    pub fn set_external(&self, pipeline: &gst::Element, idx: usize) {
        self.set_text_flag(pipeline, false);
        *self.active.borrow_mut() = Active::External(idx);
    }
}

/// Generate CSS for the `.subtitle-text` label from a SubtitleStyle. Drives
/// the live look of our custom subtitle renderer.
pub fn subtitle_css(style: &SubtitleStyle) -> String {
    let desc = gtk::pango::FontDescription::from_string(&style.font_desc);
    let family = desc
        .family()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Sans".into());
    let mut size_pt = desc.size() as f32 / gtk::pango::SCALE as f32;
    if size_pt < 1.0 {
        size_pt = 22.0;
    }
    let px = (size_pt * 96.0 / 72.0).round() as i32;
    let weight = if desc.weight() >= gtk::pango::Weight::Bold {
        700
    } else {
        400
    };

    let css_color = |c: u32| {
        let a = ((c >> 24) & 0xFF) as f32 / 255.0;
        format!(
            "rgba({},{},{},{:.3})",
            (c >> 16) & 0xFF,
            (c >> 8) & 0xFF,
            c & 0xFF,
            a
        )
    };
    let color = css_color(style.color);
    let outline = css_color(style.outline_color);

    let effect = if style.draw_outline {
        format!(
            "text-shadow: -2px -2px 0 {o}, 2px -2px 0 {o}, -2px 2px 0 {o}, 2px 2px 0 {o}, \
             0 2px 0 {o}, 0 -2px 0 {o}, 2px 0 0 {o}, -2px 0 0 {o};",
            o = outline
        )
    } else if style.draw_shadow {
        "text-shadow: 2px 2px 3px rgba(0,0,0,0.85);".to_string()
    } else {
        String::new()
    };
    let bg = if style.shaded_background {
        "background-color: rgba(0,0,0,0.55); border-radius: 6px; padding: 2px 10px;"
    } else {
        ""
    };

    format!(
        ".subtitle-text {{ font-family: \"{family}\"; font-size: {px}px; \
         font-weight: {weight}; color: {color}; {effect} {bg} }}"
    )
}

/// Push a SubtitleStyle onto a textoverlay-like element. Each property is
/// guarded by has_property so this also works on assrender/other overlays
/// that expose a subset.
pub fn set_overlay_style(ov: &gst::Element, style: &SubtitleStyle) {
    if ov.has_property("font-desc", None) {
        ov.set_property("font-desc", &style.font_desc);
    }
    if ov.has_property("color", None) {
        ov.set_property("color", style.color);
    }
    if ov.has_property("outline-color", None) {
        ov.set_property("outline-color", style.outline_color);
    }
    if ov.has_property("draw-outline", None) {
        ov.set_property("draw-outline", style.draw_outline);
    }
    if ov.has_property("draw-shadow", None) {
        ov.set_property("draw-shadow", style.draw_shadow);
    }
    if ov.has_property("shaded-background", None) {
        ov.set_property("shaded-background", style.shaded_background);
    }
    if ov.has_property("valignment", None) {
        let _ = ov.set_property_from_str("valignment", style.valign.nick());
    }
    // Lift bottom-aligned subtitles above our floating controls bar (~90px)
    // so they aren't hidden behind it.
    if ov.has_property("ypad", None) && matches!(style.valign, VAlign::Bottom) {
        ov.set_property("ypad", 120i32);
    }
}
