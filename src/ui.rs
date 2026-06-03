use adw::prelude::*;
use gtk::{gdk, pango};

use crate::theme;

/// All the widgets that event handlers need to reference, in one bag.
pub struct UiHandles {
    pub window: adw::ApplicationWindow,
    pub toast_overlay: adw::ToastOverlay,
    pub content_stack: gtk::Stack,
    pub controls: gtk::Box,
    pub top_bar: gtk::Box,
    /// The widget that displays the video frames - click gestures attach here.
    pub picture: gtk::Picture,

    // Top bar
    pub open_btn: gtk::Button,
    pub link_btn: gtk::Button,
    pub settings_btn: gtk::Button,
    pub minimize_btn: gtk::Button,
    pub maximize_btn: gtk::Button,
    pub close_btn: gtk::Button,

    // Bottom controls
    pub seek_scale: gtk::Scale,
    pub play_btn: gtk::Button,
    pub stop_btn: gtk::Button,
    pub prev_btn: gtk::Button,
    pub next_btn: gtk::Button,
    pub fullscreen_btn: gtk::Button,
    pub subtitle_btn: gtk::Button,
    pub title_label: gtk::Label,
    pub position_label: gtk::Label,
    pub duration_label: gtk::Label,

    // Quick-settings drawer
    pub settings_panel_btn: gtk::Button,
    pub settings_close_btn: gtk::Button,
    pub settings_revealer: gtk::Revealer,
    pub settings_view_stack: adw::ViewStack,

    // Volume cluster
    pub volume_btn: gtk::Button,
    pub volume_scale: gtk::Scale,
    pub volume_revealer: gtk::Revealer,
    pub volume_box: gtk::Box,

    // Empty state
    pub action_open: gtk::Button,
    pub action_url: gtk::Button,

    // Buffering chip + debug overlay
    pub buffer_chip: gtk::Box,
    pub buffer_label: gtk::Label,
    pub debug_label: gtk::Label,
    /// Custom external-subtitle renderer (we draw SRT cues here ourselves).
    pub subtitle_label: gtk::Label,
    /// Dedicated CSS provider for the subtitle label, regenerated live when
    /// subtitle style preferences change.
    pub subtitle_css: gtk::CssProvider,
    /// On-screen-display notifications for player actions.
    pub osd: crate::osd::Osd,
}

/// Install the application-wide CSS once, before any UI is built.
pub fn install_css() {
    // A media player reads best dark; this also keeps the embedded Adwaita rows
    // in the quick-settings drawer and the Preferences window consistently dark.
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);

    let css = gtk::CssProvider::new();
    css.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("No display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Build the window and all child widgets. Returns a `UiHandles` for the
/// caller to wire events onto.
pub fn build_ui(app: &adw::Application, paintable: &gdk::Paintable) -> UiHandles {
    // Video area
    let picture = gtk::Picture::for_paintable(paintable);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.set_content_fit(gtk::ContentFit::Contain);

    let video_area = gtk::Box::new(gtk::Orientation::Vertical, 0);
    video_area.add_css_class("video-area");
    video_area.append(&picture);

    // ---- Top bar ----
    let open_btn = gtk::Button::from_icon_name("document-open-symbolic");
    open_btn.set_tooltip_text(Some("Open Video (Ctrl+O)"));
    open_btn.add_css_class("osd-button");
    open_btn.set_focus_on_click(false);

    let link_btn = gtk::Button::from_icon_name("insert-link-symbolic");
    link_btn.set_tooltip_text(Some("Open URL (Ctrl+L)"));
    link_btn.add_css_class("osd-button");
    link_btn.set_focus_on_click(false);

    let left_btns = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    left_btns.append(&open_btn);
    left_btns.append(&link_btn);

    // Two expanding drag handles flank the centered title so the filename sits
    // dead-centre in the top bar (and both sides stay window-drag zones).
    let make_drag_handle = || {
        let h = gtk::WindowHandle::new();
        let filler = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        filler.set_hexpand(true);
        h.set_child(Some(&filler));
        h.set_hexpand(true);
        h
    };
    let drag_handle_l = make_drag_handle();
    let drag_handle_r = make_drag_handle();

    // Filename, shown centered at the top of the player (outside the control
    // panel). Empty until a file loads.
    let title_label = gtk::Label::new(Some(""));
    title_label.add_css_class("player-title");
    title_label.set_max_width_chars(48);
    title_label.set_ellipsize(pango::EllipsizeMode::End);

    let settings_btn = gtk::Button::from_icon_name("emblem-system-symbolic");
    settings_btn.set_tooltip_text(Some("Preferences"));
    settings_btn.add_css_class("osd-button");
    settings_btn.set_focus_on_click(false);

    // Custom window-control buttons styled exactly like the other OSD buttons
    // (gtk::WindowControls brings its own theming that fights our CSS). Wired
    // to window.minimize()/maximize()/close() in handlers.rs.
    let minimize_btn = gtk::Button::from_icon_name("window-minimize-symbolic");
    minimize_btn.add_css_class("osd-button");
    minimize_btn.set_focus_on_click(false);
    minimize_btn.set_tooltip_text(Some("Minimize"));

    let maximize_btn = gtk::Button::from_icon_name("window-maximize-symbolic");
    maximize_btn.add_css_class("osd-button");
    maximize_btn.set_focus_on_click(false);
    maximize_btn.set_tooltip_text(Some("Maximize"));

    let close_btn = gtk::Button::from_icon_name("window-close-symbolic");
    close_btn.add_css_class("osd-button");
    close_btn.add_css_class("close-button");
    close_btn.set_focus_on_click(false);
    close_btn.set_tooltip_text(Some("Close"));

    let right_btns = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    right_btns.append(&settings_btn);
    right_btns.append(&minimize_btn);
    right_btns.append(&maximize_btn);
    right_btns.append(&close_btn);

    let top_bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Start)
        .build();
    top_bar.add_css_class("player-top-bar");
    top_bar.append(&left_btns);
    top_bar.append(&drag_handle_l);
    top_bar.append(&title_label);
    top_bar.append(&drag_handle_r);
    top_bar.append(&right_btns);

    // ---- Bottom controls ----
    let position_label = gtk::Label::new(Some("0:00"));
    position_label.add_css_class("time");
    position_label.set_width_chars(5);
    position_label.set_xalign(0.0);

    let duration_label = gtk::Label::new(Some("0:00"));
    duration_label.add_css_class("time");
    duration_label.set_width_chars(6); // fits the leading "-" of remaining time
    duration_label.set_xalign(1.0);

    let seek_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.001);
    seek_scale.set_hexpand(true);
    seek_scale.set_draw_value(false);
    seek_scale.set_show_fill_level(true);
    seek_scale.set_restrict_to_fill_level(false);
    seek_scale.set_fill_level(0.0);

    let row_top = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .build();
    row_top.append(&position_label);
    row_top.append(&seek_scale);
    row_top.append(&duration_label);

    let prev_btn = gtk::Button::from_icon_name("media-skip-backward-symbolic");
    prev_btn.add_css_class("osd-button");
    prev_btn.set_tooltip_text(Some("Previous"));
    prev_btn.set_focus_on_click(false);

    let play_btn = gtk::Button::from_icon_name("media-playback-start-symbolic");
    play_btn.add_css_class("osd-button");
    play_btn.add_css_class("play-btn");
    play_btn.set_tooltip_text(Some("Play/Pause (Space)"));
    play_btn.set_focus_on_click(false);

    let stop_btn = gtk::Button::from_icon_name("media-playback-stop-symbolic");
    stop_btn.add_css_class("osd-button");
    stop_btn.set_tooltip_text(Some("Stop (back to start)"));
    stop_btn.set_focus_on_click(false);

    let next_btn = gtk::Button::from_icon_name("media-skip-forward-symbolic");
    next_btn.add_css_class("osd-button");
    next_btn.set_tooltip_text(Some("Next"));
    next_btn.set_focus_on_click(false);

    let volume_btn = gtk::Button::from_icon_name("audio-volume-high-symbolic");
    volume_btn.add_css_class("osd-button");
    volume_btn.set_tooltip_text(Some("Volume (click to mute)"));
    volume_btn.set_focus_on_click(false);

    let volume_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.02);
    volume_scale.set_value(1.0);
    volume_scale.set_size_request(theme::VOLUME_SLIDER_WIDTH, -1);
    volume_scale.set_draw_value(false);
    volume_scale.set_hexpand(false);

    // Volume slider is shown inline (IINA-style), always visible. The revealer
    // is kept so existing handlers still reference it, but it stays open.
    let volume_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideLeft)
        .transition_duration(220)
        .reveal_child(true)
        .child(&volume_scale)
        .build();

    let volume_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .build();
    volume_box.add_css_class("volume-area");
    volume_box.append(&volume_btn);
    volume_box.append(&volume_revealer);

    let fullscreen_btn = gtk::Button::from_icon_name("view-fullscreen-symbolic");
    fullscreen_btn.add_css_class("osd-button");
    fullscreen_btn.set_tooltip_text(Some("Fullscreen (F)"));
    fullscreen_btn.set_focus_on_click(false);

    let subtitle_btn = gtk::Button::from_icon_name("media-view-subtitles-symbolic");
    subtitle_btn.add_css_class("osd-button");
    subtitle_btn.set_tooltip_text(Some("Subtitles"));
    subtitle_btn.set_focus_on_click(false);

    // Gear: opens the IINA-style quick-settings drawer.
    let settings_panel_btn = gtk::Button::from_icon_name("emblem-system-symbolic");
    settings_panel_btn.add_css_class("osd-button");
    settings_panel_btn.set_tooltip_text(Some("Quick settings"));
    settings_panel_btn.set_focus_on_click(false);

    // Keep every control vertically centred so its round hover stays a circle.
    // (Otherwise the shorter buttons stretch to the play button's height and
    // their pill-radius hover renders as an oval.)
    for b in [
        &prev_btn,
        &play_btn,
        &stop_btn,
        &next_btn,
        &subtitle_btn,
        &settings_panel_btn,
        &fullscreen_btn,
        &volume_btn,
    ] {
        b.set_valign(gtk::Align::Center);
    }
    volume_box.set_valign(gtk::Align::Center);

    // Secondary controls (the right cluster) are smaller than the transport.
    for b in [&stop_btn, &subtitle_btn, &settings_panel_btn, &fullscreen_btn] {
        b.add_css_class("secondary-btn");
    }

    // Center transport cluster (prev / play / next) — the play button sits dead
    // centre of the bar; stop moves to the right cluster.
    let transport = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(14)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    transport.append(&prev_btn);
    transport.append(&play_btn);
    transport.append(&next_btn);

    // Right utility cluster (stop / subtitles / settings / fullscreen).
    let right_cluster = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(2)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .build();
    right_cluster.append(&stop_btn);
    right_cluster.append(&subtitle_btn);
    right_cluster.append(&settings_panel_btn);
    right_cluster.append(&fullscreen_btn);

    // IINA-style 3-zone row via CenterBox: volume far-left, transport ALWAYS
    // dead-centre of the panel (independent of side widths), utilities far-right.
    let row_bot = gtk::CenterBox::new();
    row_bot.set_hexpand(true);
    row_bot.set_start_widget(Some(&volume_box));
    row_bot.set_center_widget(Some(&transport));
    row_bot.set_end_widget(Some(&right_cluster));

    // Centered, floating bar (IINA-style), wide enough to spread the controls.
    // It hugs its content and sits centered near the bottom, so its side margins
    // are always symmetric. The seek scale's width is capped responsively in
    // handlers.rs so the whole bar always fits the window with breathing room.
    seek_scale.set_width_request(theme::SEEK_WIDTH_DEFAULT);
    let controls = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::End)
        .margin_bottom(theme::BAR_EDGE_INSET)
        .build();
    controls.add_css_class("controls-bar");
    // Buttons on top, seek bar pinned to the bottom of the panel.
    controls.append(&row_bot);
    controls.append(&row_top);

    // ---- Empty state ----
    let action_open = gtk::Button::with_label("Open File");
    action_open.add_css_class("pill");
    action_open.add_css_class("suggested-action");
    action_open.set_halign(gtk::Align::Center);

    let action_url = gtk::Button::with_label("Open URL");
    action_url.add_css_class("pill");
    action_url.set_halign(gtk::Align::Center);

    let actions_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    actions_row.set_halign(gtk::Align::Center);
    actions_row.set_margin_top(8);
    actions_row.append(&action_open);
    actions_row.append(&action_url);

    let empty_state = adw::StatusPage::builder()
        .icon_name(crate::WORDMARK_ICON)
        .title("Soniq")
        .description("Drop a video here, open a file, or paste a URL")
        .child(&actions_row)
        .build();
    empty_state.add_css_class("empty-state");

    let content_stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(220)
        .build();
    content_stack.add_named(&empty_state, Some("empty"));
    content_stack.add_named(&video_area, Some("video"));
    content_stack.set_visible_child_name("empty");

    controls.set_visible(false);

    // ---- Debug overlay + buffering chip ----
    let debug_label = gtk::Label::new(Some(""));
    debug_label.add_css_class("debug-overlay");
    debug_label.set_halign(gtk::Align::End);
    debug_label.set_valign(gtk::Align::Start);
    debug_label.set_margin_top(50);
    debug_label.set_margin_end(14);
    debug_label.set_xalign(1.0);
    debug_label.set_wrap(false);
    debug_label.set_visible(false);

    let buffer_spinner = gtk::Spinner::new();
    buffer_spinner.set_size_request(22, 22);
    buffer_spinner.set_spinning(true);

    let buffer_label = gtk::Label::new(Some("Buffering…"));
    buffer_label.add_css_class("buffer-text");

    let buffer_chip = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    buffer_chip.add_css_class("buffer-chip");
    buffer_chip.append(&buffer_spinner);
    buffer_chip.append(&buffer_label);

    // ---- External-subtitle label (we render dropped SRTs ourselves) ----
    let subtitle_label = gtk::Label::new(None);
    subtitle_label.add_css_class("subtitle-text");
    // Dedicated provider (above the base CSS) for live subtitle styling.
    let subtitle_css = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("No display"),
        &subtitle_css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    subtitle_label.set_halign(gtk::Align::Center);
    subtitle_label.set_valign(gtk::Align::End);
    subtitle_label.set_justify(gtk::Justification::Center);
    subtitle_label.set_wrap(true);
    subtitle_label.set_margin_bottom(theme::SUBTITLE_MARGIN_DEFAULT); // small default gap
    subtitle_label.set_margin_start(40);
    subtitle_label.set_margin_end(40);
    subtitle_label.set_visible(false);

    // ---- OSD notifications (top-center fading pill) ----
    let (osd, osd_widget) = crate::osd::Osd::new();

    // ---- Quick-settings drawer (slides in from the right) ----
    // The tab pages' content is (re)built on each open by handlers.rs so it
    // always reflects live track lists and effect values.
    let settings_view_stack = adw::ViewStack::new();

    let settings_switcher = adw::ViewSwitcher::builder()
        .stack(&settings_view_stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .hexpand(true)
        .build();

    let settings_close_btn = gtk::Button::from_icon_name("window-close-symbolic");
    settings_close_btn.add_css_class("osd-button");
    settings_close_btn.set_valign(gtk::Align::Center);
    settings_close_btn.set_tooltip_text(Some("Close"));

    let settings_header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    settings_header.add_css_class("quick-settings-header");
    settings_header.append(&settings_switcher);
    settings_header.append(&settings_close_btn);

    let settings_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&settings_view_stack)
        .build();

    let settings_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    settings_panel.add_css_class("quick-settings-panel");
    settings_panel.set_size_request(theme::PANEL_WIDTH, -1);
    settings_panel.append(&settings_header);
    settings_panel.append(&settings_scroll);

    let settings_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideLeft)
        .transition_duration(theme::PANEL_TRANSITION_MS)
        .reveal_child(false)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Fill)
        .child(&settings_panel)
        .build();

    // ---- Root overlay + window ----
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&content_stack));
    overlay.add_overlay(&top_bar);
    overlay.add_overlay(&buffer_chip);
    overlay.add_overlay(&debug_label);
    overlay.add_overlay(&subtitle_label);
    overlay.add_overlay(&osd_widget);
    overlay.add_overlay(&controls);
    overlay.add_overlay(&settings_revealer);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&overlay));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Soniq")
        .default_width(960)
        .default_height(560)
        .content(&toast_overlay)
        .build();

    UiHandles {
        window,
        toast_overlay,
        content_stack,
        controls,
        top_bar,
        picture,
        open_btn,
        link_btn,
        settings_btn,
        minimize_btn,
        maximize_btn,
        close_btn,
        seek_scale,
        play_btn,
        stop_btn,
        prev_btn,
        next_btn,
        fullscreen_btn,
        subtitle_btn,
        title_label,
        position_label,
        duration_label,
        settings_panel_btn,
        settings_close_btn,
        settings_revealer,
        settings_view_stack,
        volume_btn,
        volume_scale,
        volume_revealer,
        volume_box,
        action_open,
        action_url,
        buffer_chip,
        buffer_label,
        debug_label,
        subtitle_label,
        subtitle_css,
        osd,
    }
}

const CSS: &str = "
    .video-area { background-color: #000; }

    .empty-state { background-color: #0a0a0c; }
    .empty-state > * { color: rgba(255, 255, 255, 0.92); }
    .empty-state .icon { color: rgba(255, 255, 255, 0.70); -gtk-icon-size: 96px; }
    .empty-state .title { color: rgba(255, 255, 255, 0.95); font-weight: 700; }
    .empty-state .description { color: rgba(255, 255, 255, 0.60); }
    .empty-state button.pill {
        min-height: 36px;
        padding: 0 22px;
        border-radius: 999px;
        background-color: rgba(255, 255, 255, 0.10);
        color: rgba(255, 255, 255, 0.95);
        border: 1px solid rgba(255, 255, 255, 0.10);
        box-shadow: none;
    }
    .empty-state button.pill:hover { background-color: rgba(255, 255, 255, 0.18); }
    .empty-state button.pill.suggested-action {
        background-color: #3584e4; border-color: transparent; color: white;
    }
    .empty-state button.pill.suggested-action:hover { background-color: #4593f0; }

    .player-top-bar {
        padding: 8px 10px;
        background: linear-gradient(to bottom,
            rgba(0, 0, 0, 0.55) 0%, rgba(0, 0, 0, 0.0) 100%);
        min-height: 38px;
    }
    /* Filename centered at the top of the player. */
    .player-title {
        color: rgba(255, 255, 255, 0.95);
        font-weight: 600;
        font-size: 0.95em;
        text-shadow: 0 1px 3px rgba(0, 0, 0, 0.7);
        padding: 0 8px;
    }

    /* Quick-settings drawer (slides in from the right). */
    .quick-settings-panel {
        background-color: rgba(28, 28, 32, 0.96);
        border-left: 1px solid rgba(255, 255, 255, 0.08);
        box-shadow: -8px 0 28px rgba(0, 0, 0, 0.45);
    }
    .quick-settings-header {
        padding: 8px 8px 8px 12px;
        border-bottom: 1px solid rgba(255, 255, 255, 0.07);
        background-color: rgba(0, 0, 0, 0.2);
    }
    /* Let the Adwaita preference rows blend into the dark drawer. */
    .quick-settings-panel scrolledwindow,
    .quick-settings-panel preferencespage,
    .quick-settings-panel viewport {
        background-color: transparent;
    }
    .quick-settings-panel preferencespage > * { background-color: transparent; }
    /* Close button gets a red hover tint, like every desktop. */
    .osd-button.close-button:hover { background-color: rgba(232, 67, 62, 0.85); }

    .osd-button {
        color: rgba(255, 255, 255, 0.92);
        background-color: transparent; background-image: none;
        box-shadow: none; border: none;
        min-width: 30px; min-height: 30px;
        padding: 4px; border-radius: 999px;
    }
    .osd-button:hover { background-color: rgba(255, 255, 255, 0.15); }
    .osd-button:active { background-color: rgba(255, 255, 255, 0.22); }

    .controls-bar {
        background-color: rgba(30, 30, 34, 0.72);
        border: 1px solid rgba(255, 255, 255, 0.07);
        border-radius: 18px;
        padding: 10px 18px 12px 18px;
        box-shadow: 0 10px 34px rgba(0, 0, 0, 0.50);
    }
    /* Faded state for auto-hide (transitions opacity). */
    .controls-bar, .player-top-bar { transition: opacity 220ms ease; }
    .autohide-hidden { opacity: 0; }
    .controls-bar label { color: rgba(255, 255, 255, 0.92); font-feature-settings: 'tnum'; }
    .controls-bar label.time {
        color: rgba(255, 255, 255, 0.62); font-size: 0.82em; padding: 0 2px;
    }
    .controls-bar image { -gtk-icon-size: 18px; }

    .controls-bar button {
        color: rgba(255, 255, 255, 0.88);
        background-color: transparent; background-image: none;
        box-shadow: none; border: none;
        min-width: 34px; min-height: 34px;
        padding: 5px; border-radius: 999px;
        transition: background-color 120ms ease, color 120ms ease;
    }
    .controls-bar button:hover {
        background-color: rgba(255, 255, 255, 0.14);
        color: #ffffff;
    }
    .controls-bar button:active { background-color: rgba(255, 255, 255, 0.22); }
    /* Secondary controls (stop / subtitles / settings / fullscreen) — smaller
       than the transport buttons and tucked to the right edge. */
    .controls-bar button.secondary-btn {
        min-width: 26px; min-height: 26px;
        padding: 3px;
        color: rgba(255, 255, 255, 0.78);
    }
    .controls-bar button.secondary-btn image { -gtk-icon-size: 15px; }

    /* Prominent filled play/pause button — the hero control. */
    .controls-bar button.play-btn {
        background-color: rgba(255, 255, 255, 0.82);
        color: #111;
        min-width: 46px; min-height: 46px;
        padding: 0;
        transition: background-color 140ms ease;
    }
    .controls-bar button.play-btn:hover { background-color: #ffffff; color: #000; }
    .controls-bar button.play-btn:active { background-color: rgba(255, 255, 255, 0.70); }
    .controls-bar button.play-btn image { -gtk-icon-size: 22px; }

    .controls-bar scale { min-height: 16px; padding: 0; }
    .controls-bar scale trough {
        min-height: 4px;
        background-color: rgba(255, 255, 255, 0.20);
        border: none; border-radius: 999px;
    }
    .controls-bar scale highlight {
        background-color: rgba(255, 255, 255, 0.92); border-radius: 999px;
    }
    .controls-bar scale slider {
        background-color: #ffffff; background-image: none; border: none;
        min-width: 13px; min-height: 13px; margin: -6px;
        border-radius: 50%;
        box-shadow: 0 1px 4px rgba(0, 0, 0, 0.5);
        transition: transform 100ms ease;
    }
    .controls-bar scale:hover slider { transform: scale(1.15); }
    .controls-bar scale trough fill {
        background-color: rgba(255, 255, 255, 0.40);
        background-image: none; border: none;
        border-radius: 999px; min-height: 4px;
    }

    /* Volume slider: blue accent, always inline (IINA-style). */
    .volume-area scale { padding: 0 6px; min-width: 96px; }
    .volume-area scale highlight { background-color: #3584e4; }
    .volume-area scale slider { min-width: 12px; min-height: 12px; }

    .debug-overlay {
        background-color: rgba(0, 0, 0, 0.7);
        color: rgba(255, 255, 255, 0.95);
        border: 1px solid rgba(255, 255, 255, 0.10);
        border-radius: 8px;
        padding: 6px 10px;
        font-family: monospace;
        font-size: 0.80em;
        font-feature-settings: 'tnum';
    }

    .buffer-chip {
        background-color: rgba(20, 20, 22, 0.88);
        border: 1px solid rgba(255, 255, 255, 0.08);
        border-radius: 999px;
        padding: 12px 20px;
        box-shadow: 0 6px 22px rgba(0, 0, 0, 0.45);
    }
    .buffer-text {
        color: rgba(255, 255, 255, 0.95);
        font-size: 0.95em; font-weight: 500;
        font-feature-settings: 'tnum';
    }
    .buffer-chip spinner { color: rgba(255, 255, 255, 0.85); }

    /* OSD action notifications (top-center pill). */
    .osd-toast {
        background-color: rgba(20, 20, 22, 0.88);
        color: #ffffff;
        border: 1px solid rgba(255, 255, 255, 0.10);
        border-radius: 999px;
        padding: 7px 16px;
        box-shadow: 0 6px 22px rgba(0, 0, 0, 0.45);
    }
    .osd-toast-icon { -gtk-icon-size: 16px; color: #ffffff; }
    .osd-toast-label { font-size: 0.95em; font-weight: 600; color: #ffffff; }

    /* External subtitle text we render ourselves. Outline via layered
       text-shadow so it's readable on any background. */
    .subtitle-text {
        color: #ffffff;
        font-size: 26px;
        font-weight: 700;
        text-shadow:
            -2px -2px 0 #000, 2px -2px 0 #000,
            -2px  2px 0 #000, 2px  2px 0 #000,
             0px  2px 0 #000, 0px -2px 0 #000,
             2px  0px 0 #000, -2px 0px 0 #000;
    }

    /* Context menus (right-click + subtitle). Explicit dark surface so the
       text stays readable no matter which widget the popover is parented to
       (parenting inside .controls-bar would otherwise inherit white text). */
    popover.menu > contents,
    popover.menu > arrow {
        background-color: rgba(32, 32, 36, 0.98);
        border: 1px solid rgba(255, 255, 255, 0.08);
        color: rgba(255, 255, 255, 0.95);
        box-shadow: 0 6px 22px rgba(0, 0, 0, 0.45);
    }
    .context-menu { padding: 4px; min-width: 200px; }
    .context-menu-item {
        padding: 6px 12px;
        border-radius: 6px;
        min-height: 28px;
        color: rgba(255, 255, 255, 0.95);
    }
    .context-menu-item:hover { background-color: rgba(255, 255, 255, 0.12); }
    .context-menu-item label { color: rgba(255, 255, 255, 0.95); }
    .context-menu-item image { -gtk-icon-size: 16px; opacity: 0.9; }
";
