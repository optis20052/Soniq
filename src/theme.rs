//! Centralized compile-time layout / behaviour tunables.
//!
//! These are *developer* design values — the numbers you reach for when tuning
//! how the UI looks and feels. They are deliberately NOT in `config.rs`: that
//! file is for user-facing settings persisted to disk (volume, subtitle style,
//! …). Keeping these here means a value used in several places (the subtitle
//! margin, the control-bar inset) has a single source of truth instead of
//! drifting out of sync across files.
//!
//! Rule of thumb: a number that is reused across files, or that you tune often,
//! belongs here. A number used exactly once, right where it matters, is clearer
//! left inline.

// ---- Floating control bar ----

/// Gap the floating control bar keeps from each window edge — used for the
/// default bottom rest position and the drag clamp on all four sides.
pub const BAR_EDGE_INSET: i32 = 8;

/// Fixed (non-seek) width of the seek row: the time labels, their spacing and
/// the bar's horizontal padding. Used to size the responsive seek scale.
pub const SEEK_ROW_CHROME: i32 = 150;

/// Responsive seek-scale width: clamped between these, with this natural width.
pub const SEEK_WIDTH_MIN: i32 = 110;
pub const SEEK_WIDTH_MAX: i32 = 460;
pub const SEEK_WIDTH_DEFAULT: i32 = 440;

/// Width of the inline volume slider.
pub const VOLUME_SLIDER_WIDTH: i32 = 96;

/// Hide the top bar + controls after this long without pointer motion (ms).
pub const AUTOHIDE_MS: u64 = 2500;

// ---- Quick-settings drawer ----

/// Width of the slide-in quick-settings panel.
pub const PANEL_WIDTH: i32 = 360;

/// Slide animation duration of the quick-settings panel (ms).
pub const PANEL_TRANSITION_MS: u32 = 250;

// ---- Subtitles ----

/// Default vertical position (bottom margin, px) of bottom-aligned subtitles,
/// and the maximum the Position/Offset sliders allow.
pub const SUBTITLE_MARGIN_DEFAULT: i32 = 40;
pub const SUBTITLE_MARGIN_MAX: i32 = 300;

/// Default subtitle size multiplier and the range the Scale slider allows.
pub const SUBTITLE_SCALE_DEFAULT: f64 = 1.0;
pub const SUBTITLE_SCALE_MIN: f64 = 0.5;
pub const SUBTITLE_SCALE_MAX: f64 = 3.0;
