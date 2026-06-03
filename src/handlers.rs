use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use gst::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::pango;
use gtk::glib::{self, ControlFlow, Propagation};

use crate::effects::{AspectMode, EQ_PRESETS};
use crate::pipeline::PipelineHandles;
use crate::resume::ResumeMode;
use crate::shortcuts::Action;
use crate::state::AppState;
use crate::theme;
use crate::ui::UiHandles;
use crate::util::format_time;

/// Wire every event handler, timer, and bus watcher onto the widgets.
/// Wire all event handlers and return the shared `load_file` closure so the
/// application's `open` handler (file-manager "Open With") can load files.
pub fn wire(
    ui: &UiHandles,
    pipe: &PipelineHandles,
    state: &AppState,
) -> Rc<dyn Fn(&gio::File)> {
    let pipeline = &pipe.pipeline;

    // ---------- Shared load_file closure ----------
    let load_file: Rc<dyn Fn(&gio::File)> = {
        let pipeline = pipeline.clone();
        let play_btn = ui.play_btn.clone();
        let title_label = ui.title_label.clone();
        let window_weak = ui.window.downgrade();
        let content_stack = ui.content_stack.clone();
        let controls = ui.controls.clone();
        let seek_scale = ui.seek_scale.clone();
        let is_local = state.is_local.clone();
        let user_paused = state.user_paused.clone();
        let queue_ref = state.queue_ref.clone();
        let source_ref = state.source_ref.clone();
        let net_download = state.net_download.clone();
        let subs_load = state.subtitles.clone();
        let playlist = state.playlist.clone();
        let playlist_idx = state.playlist_idx.clone();
        let volume_scale = ui.volume_scale.clone();
        let osd = ui.osd.clone();
        let effects_load = state.effects.clone();
        let subtitle_delay_load = state.subtitle_delay_ns.clone();
        let current_uri_load = state.current_uri.clone();
        let resume_store_load = state.resume_store.clone();
        let resume_mode_load = state.resume_mode.clone();
        let pending_resume_load = state.pending_resume_pos.clone();
        let resume_prompt_load = state.resume_prompt_pos.clone();
        let resume_banner = ui.resume_banner.clone();
        let resume_label = ui.resume_label.clone();
        let resume_check = ui.resume_check.clone();
        Rc::new(move |file: &gio::File| {
            // Persist the outgoing file's position before we switch away.
            resume_store_load.flush();
            let uri = file.uri();
            let local = uri.starts_with("file://");
            is_local.set(local);
            user_paused.set(false);
            // Build the folder playlist for Next/Previous + auto-advance.
            if local && let Some(path) = file.path() {
                let (list, idx) = scan_playlist(&path);
                *playlist.borrow_mut() = list;
                playlist_idx.set(idx);
            } else {
                playlist.borrow_mut().clear();
            }
            // Reset stashed element refs - the new load creates fresh source /
            // queue2 elements, and the deep-element-added / source-setup hooks
            // re-stash them. Without this, switching local→stream leaves the
            // streaming indicators pointing at the previous (dead) elements.
            *queue_ref.lock().unwrap() = None;
            *source_ref.lock().unwrap() = None;
            pipeline.set_state(gst::State::Null).ok();
            // Set playbin3 flags based on source type. For local files the
            // +buffering flag (and the queue2 it inserts) is dead weight that
            // can wedge on heavy seeking. For network streams we add +download
            // so playbin caches the stream to a disk file (relocated off the
            // tmpfs in pipeline.rs): seeks within the watched range are then
            // served from disk instead of re-requesting from the server.
            net_download.store(!local, std::sync::atomic::Ordering::Relaxed);
            let flags = if local {
                crate::subtitles::DEFAULT_FLAGS.to_string()
            } else {
                format!("{}+buffering+download", crate::subtitles::DEFAULT_FLAGS)
            };
            subs_load.set_flags(&pipeline, &flags);
            pipeline.set_property("uri", uri.as_str());
            // Hand the chosen subtitle font to playbin3 up front.
            pipeline.set_property("subtitle-font-desc", subs_load.font_desc());
            // Re-apply the current UI volume - the reload can reset playbin's
            // volume to 100%, which would desync from the slider.
            pipeline.set_property("volume", volume_scale.value());
            pipeline.set_state(gst::State::Playing).ok();
            // Reset content-specific quick settings for the new file. Subtitle
            // appearance (style/scale/position) is a user preference and is kept
            // (and persisted); only the subtitle delay (a per-file sync offset)
            // resets here. Track lists repopulate when the panel is next opened.
            *current_uri_load.borrow_mut() = Some(uri.to_string());
            effects_load.reset_for_new_video(&pipeline);
            subtitle_delay_load.set(0);

            // Resume: if this file was watched before, either auto-seek
            // (Always) or offer a banner (Ask). Capture the target now, before
            // the position timer overwrites the stored entry with ~0.
            resume_banner.set_visible(false);
            pending_resume_load.set(None);
            resume_prompt_load.set(None);
            let mode = resume_mode_load.get();
            if mode != ResumeMode::Off
                && let Some(entry) = resume_store_load.resumable(uri.as_str())
            {
                let at = format_time(gst::ClockTime::from_nseconds(entry.pos_ns));
                match mode {
                    ResumeMode::Always => {
                        pending_resume_load.set(Some(entry.pos_ns));
                        osd.show("media-seek-forward-symbolic", &format!("Resuming at {at}"));
                    }
                    ResumeMode::Ask => {
                        resume_prompt_load.set(Some(entry.pos_ns));
                        resume_label.set_text(&format!("Resume from {at}?"));
                        resume_check.set_active(false);
                        resume_banner.set_visible(true);
                        let banner = resume_banner.clone();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_secs(12),
                            move || banner.set_visible(false),
                        );
                    }
                    ResumeMode::Off => {}
                }
            }
            play_btn.set_icon_name("media-playback-pause-symbolic");
            content_stack.set_visible_child_name("video");
            controls.set_visible(true);
            seek_scale.set_fill_level(if local { 1.0 } else { 0.0 });
            if let Some(name) = file.basename().map(|p| p.to_string_lossy().into_owned()) {
                title_label.set_text(&name);
                osd.show("media-playback-start-symbolic", &name);
                if let Some(window) = window_weak.upgrade() {
                    window.set_title(Some(&format!("{name} \u{2014} Soniq")));
                }
            }
        })
    };

    // ---------- Playlist navigation (Next / Previous / auto-advance) ----------
    // navigate(+1) = next file, navigate(-1) = previous. No wrap-around: at the
    // ends it does nothing (auto-advance past the last file simply stops).
    let navigate: Rc<dyn Fn(i32)> = {
        let load_file = load_file.clone();
        let playlist = state.playlist.clone();
        let playlist_idx = state.playlist_idx.clone();
        Rc::new(move |delta: i32| {
            let list = playlist.borrow();
            if list.is_empty() {
                return;
            }
            let cur = playlist_idx.get() as i32;
            let next = cur + delta;
            if next < 0 || next as usize >= list.len() {
                return; // at a boundary
            }
            let path = list[next as usize].clone();
            drop(list);
            load_file(&gio::File::for_path(&path));
        })
    };
    {
        let navigate = navigate.clone();
        ui.next_btn.connect_clicked(move |_| navigate(1));
    }
    {
        let navigate = navigate.clone();
        ui.prev_btn.connect_clicked(move |_| navigate(-1));
    }

    // ---------- Open file dialog ----------
    {
        let window_weak = ui.window.downgrade();
        let load_file = load_file.clone();
        ui.open_btn.connect_clicked(move |_| {
            let Some(window) = window_weak.upgrade() else { return };
            let video_filter = gtk::FileFilter::new();
            video_filter.set_name(Some("Video files"));
            video_filter.add_mime_type("video/*");

            let all_filter = gtk::FileFilter::new();
            all_filter.set_name(Some("All files"));
            all_filter.add_pattern("*");

            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&video_filter);
            filters.append(&all_filter);

            let dialog = gtk::FileDialog::builder()
                .title("Open Video")
                .modal(true)
                .filters(&filters)
                .default_filter(&video_filter)
                .build();

            let load_file = load_file.clone();
            dialog.open(Some(&window), gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                load_file(&file);
            });
        });
    }

    // ---------- Open URL dialog ----------
    {
        let window_weak = ui.window.downgrade();
        let load_file = load_file.clone();
        let url_osd = ui.osd.clone();
        ui.link_btn.connect_clicked(move |_| {
            let Some(window) = window_weak.upgrade() else { return };

            let entry = gtk::Entry::builder()
                .placeholder_text("https://example.com/video.mp4")
                .hexpand(true)
                .activates_default(true)
                .input_purpose(gtk::InputPurpose::Url)
                .build();

            let clipboard = window.clipboard();
            let entry_for_clip = entry.clone();
            clipboard.read_text_async(gio::Cancellable::NONE, move |result| {
                if let Ok(Some(text)) = result {
                    let t = text.trim();
                    if t.starts_with("http://")
                        || t.starts_with("https://")
                        || t.starts_with("rtsp://")
                        || t.starts_with("rtmp://")
                    {
                        entry_for_clip.set_text(t);
                        entry_for_clip.select_region(0, -1);
                    }
                }
            });

            let dialog = adw::AlertDialog::builder()
                .heading("Open URL")
                .body("Direct video URL \u{2014} http(s), rtsp, rtmp, file://")
                .extra_child(&entry)
                .default_response("open")
                .close_response("cancel")
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("open", "Open");
            dialog.set_response_appearance("open", adw::ResponseAppearance::Suggested);

            let load_file = load_file.clone();
            let entry_for_resp = entry.clone();
            let url_osd = url_osd.clone();
            dialog.connect_response(None, move |_, response| {
                if response != "open" {
                    return;
                }
                let raw = entry_for_resp.text().to_string();
                let url = raw.trim();
                if url.is_empty() {
                    return;
                }
                // Basic sanity check: a real URL has a scheme (foo://) or at
                // least a dotted host (example.com) - otherwise it's junk like
                // "XX" that would only produce a cryptic playback error.
                let looks_like_url = url.contains("://")
                    || (url.contains('.') && !url.starts_with('.') && !url.ends_with('.'));
                if !looks_like_url {
                    url_osd.show("dialog-warning-symbolic", "That doesn't look like a valid URL");
                    return;
                }
                let full = if url.contains("://") {
                    url.to_string()
                } else {
                    format!("https://{url}")
                };
                let file = gio::File::for_uri(&full);
                load_file(&file);
            });

            dialog.present(Some(&window));
        });
    }

    // ---------- Preferences window ----------
    {
        let show_debug = state.show_debug.clone();
        let debug_label = ui.debug_label.clone();
        let shortcuts = state.shortcuts.clone();
        let state_mouse = state.mouse.clone();
        let subs_prefs = state.subtitles.clone();
        let subtitle_css_prefs = ui.subtitle_css.clone();
        let subtitle_label_prefs = ui.subtitle_label.clone();
        let subtitle_scale_prefs = state.subtitle_scale.clone();
        let subtitle_margin_prefs = state.subtitle_margin.clone();
        let resume_mode_prefs = state.resume_mode.clone();
        ui.settings_btn.connect_clicked(move |_| {
            // === General page ===
            let dev_group = adw::PreferencesGroup::builder()
                .title("Developer")
                .description("Tools for debugging playback")
                .build();

            let net_row = adw::SwitchRow::builder()
                .title("Show network stats")
                .subtitle("Overlay download progress and pipeline state on the video")
                .active(show_debug.get())
                .build();
            {
                let show_debug = show_debug.clone();
                let debug_label = debug_label.clone();
                net_row.connect_active_notify(move |row| {
                    let on = row.is_active();
                    show_debug.set(on);
                    debug_label.set_visible(on);
                });
            }
            dev_group.add(&net_row);

            // Playback group: resume-where-you-left-off behaviour.
            let playback_group = adw::PreferencesGroup::builder().title("Playback").build();
            let resume_row = adw::ComboRow::builder()
                .title("Resume playback")
                .subtitle("Continue files where you left off")
                .model(&gtk::StringList::new(&["Off", "Ask each time", "Always resume"]))
                .selected(resume_mode_prefs.get().to_index())
                .build();
            {
                let resume_mode = resume_mode_prefs.clone();
                resume_row.connect_selected_notify(move |r| {
                    resume_mode.set(ResumeMode::from_index(r.selected()));
                });
            }
            playback_group.add(&resume_row);

            let general_page = adw::PreferencesPage::builder()
                .title("General")
                .icon_name("applications-system-symbolic")
                .build();
            general_page.add(&playback_group);
            general_page.add(&dev_group);

            // === Shortcuts page ===
            let sc_group = adw::PreferencesGroup::builder()
                .title("Keyboard shortcuts")
                .description("Click a binding to change it. Esc cancels capture.")
                .build();

            let reset_btn = gtk::Button::with_label("Reset to defaults");
            reset_btn.add_css_class("flat");
            sc_group.set_header_suffix(Some(&reset_btn));

            let mut row_labels: Vec<(Action, gtk::Label)> = Vec::new();

            // Create the window up front so we can pass it as the parent to
            // the per-row capture-shortcut dialogs.
            let pref_window = adw::PreferencesWindow::builder()
                .title("Preferences")
                .default_width(680)
                .default_height(560)
                .modal(false)
                .build();

            for &action in crate::shortcuts::Action::all() {
                let row = adw::ActionRow::builder()
                    .title(action.title())
                    .activatable(true)
                    .build();
                let key_label = gtk::Label::new(Some(""));
                key_label.add_css_class("dim-label");
                key_label.add_css_class("numeric");
                if let Some(s) = shortcuts.get(action) {
                    key_label.set_text(&s.label());
                }
                row.add_suffix(&key_label);
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

                {
                    let shortcuts = shortcuts.clone();
                    let key_label = key_label.clone();
                    let pref_window_weak = pref_window.downgrade();
                    row.connect_activated(move |_| {
                        let Some(pw) = pref_window_weak.upgrade() else { return };
                        capture_shortcut(pw.upcast_ref::<gtk::Window>(), action, &shortcuts, &key_label);
                    });
                }

                sc_group.add(&row);
                row_labels.push((action, key_label));
            }

            {
                let shortcuts = shortcuts.clone();
                reset_btn.connect_clicked(move |_| {
                    shortcuts.reset_to_defaults();
                    for (action, label) in &row_labels {
                        if let Some(s) = shortcuts.get(*action) {
                            label.set_text(&s.label());
                        }
                    }
                });
            }

            let shortcuts_page = adw::PreferencesPage::builder()
                .title("Shortcuts")
                .icon_name("input-keyboard-symbolic")
                .build();
            shortcuts_page.add(&sc_group);

            // === Subtitles + Mouse pages ===
            let subtitles_page = build_subtitles_page(
                &subs_prefs,
                &subtitle_css_prefs,
                &subtitle_label_prefs,
                &subtitle_scale_prefs,
                &subtitle_margin_prefs,
            );
            let mouse_page = build_mouse_page(&state_mouse);

            pref_window.add(&general_page);
            pref_window.add(&subtitles_page);
            pref_window.add(&shortcuts_page);
            pref_window.add(&mouse_page);
            pref_window.present();
        });
    }

    // ---------- Empty-state actions ----------
    {
        let open_btn = ui.open_btn.clone();
        ui.action_open.connect_clicked(move |_| open_btn.emit_clicked());
    }
    {
        let link_btn = ui.link_btn.clone();
        ui.action_url.connect_clicked(move |_| link_btn.emit_clicked());
    }

    // ---------- Drag-and-drop ----------
    // A dropped subtitle file loads onto the current video; anything else is
    // treated as a media file to play.
    {
        let drop_target = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
        let load_file = load_file.clone();
        let drop_pipeline = pipeline.clone();
        let drop_state = state.clone();
        let drop_osd = ui.osd.clone();
        drop_target.connect_drop(move |_, value, _, _| {
            let Ok(file) = value.get::<gio::File>() else {
                return false;
            };
            // Subtitle → load onto current video.
            if is_subtitle_uri(&file.uri()) {
                load_subtitle(&drop_pipeline, &drop_state, &drop_osd, &file);
                return true;
            }
            // Video file (by extension) → play it. Anything else (images,
            // docs, …) is rejected instead of reloading the current video.
            let is_video = file
                .path()
                .map(|p| is_video_path(&p))
                .unwrap_or(true); // non-local URI: let playbin try
            if is_video {
                load_file(&file);
            } else {
                drop_osd.show("dialog-warning-symbolic", "Unsupported file type");
            }
            true
        });
        ui.window.add_controller(drop_target);
    }

    // ---------- Auto-resize window to video aspect ratio ----------
    {
        let window_weak = ui.window.downgrade();
        pipe.paintable.connect_invalidate_size(move |p| {
            let Some(window) = window_weak.upgrade() else { return };
            if window.is_fullscreen() || window.is_maximized() {
                return;
            }
            let vw = p.intrinsic_width();
            let vh = p.intrinsic_height();
            if vw <= 0 || vh <= 0 {
                return;
            }
            let (mut max_w, mut max_h) = (1280, 800);
            if let Some(surface) = window.surface()
                && let Some(monitor) = gdk::Display::default()
                    .and_then(|d| d.monitor_at_surface(&surface))
            {
                let geom = monitor.geometry();
                max_w = (geom.width() as f64 * 0.85) as i32;
                max_h = (geom.height() as f64 * 0.85) as i32;
            }
            let (video_w, video_h) = if vw * max_h > vh * max_w {
                (max_w, (max_w as i64 * vh as i64 / vw as i64) as i32)
            } else {
                ((max_h as i64 * vw as i64 / vh as i64) as i32, max_h)
            };
            let target_w = video_w.max(360);
            let target_h = video_h.max(280);
            window.set_default_size(target_w, target_h);
        });
    }

    // ---------- Play / pause ----------
    {
        let pipeline = pipeline.clone();
        let user_paused = state.user_paused.clone();
        let osd = ui.osd.clone();
        ui.play_btn.connect_clicked(move |btn| {
            let showing_pause = btn
                .icon_name()
                .map(|s| s == "media-playback-pause-symbolic")
                .unwrap_or(false);
            if showing_pause {
                user_paused.set(true);
                pipeline.set_state(gst::State::Paused).ok();
                btn.set_icon_name("media-playback-start-symbolic");
                osd.show("media-playback-pause-symbolic", "Paused");
            } else {
                osd.show("media-playback-start-symbolic", "Playing");
                user_paused.set(false);
                if let (Some(pos), Some(dur)) = (
                    pipeline.query_position::<gst::ClockTime>(),
                    pipeline.query_duration::<gst::ClockTime>(),
                ) && dur.nseconds() > 0
                    && pos.nseconds() + 500_000_000 >= dur.nseconds()
                {
                    pipeline
                        .seek_simple(
                            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                            gst::ClockTime::ZERO,
                        )
                        .ok();
                }
                pipeline.set_state(gst::State::Playing).ok();
                btn.set_icon_name("media-playback-pause-symbolic");
            }
        });
    }

    // ---------- Stop (rewind to first frame + pause) ----------
    {
        let pipeline = pipeline.clone();
        let play_btn = ui.play_btn.clone();
        let user_paused = state.user_paused.clone();
        let osd = ui.osd.clone();
        ui.stop_btn.connect_clicked(move |_| {
            user_paused.set(true);
            // Pause first, then flush-seek to the start so the head frame is
            // shown rather than continuing to play from zero.
            pipeline.set_state(gst::State::Paused).ok();
            pipeline
                .seek_simple(
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                    gst::ClockTime::ZERO,
                )
                .ok();
            play_btn.set_icon_name("media-playback-start-symbolic");
            osd.show("media-playback-stop-symbolic", "Stopped");
        });
    }

    // ---------- Volume + click-to-mute ----------
    let last_volume = Rc::new(Cell::new(1.0_f64));
    // Suppresses the volume OSD for the one-shot startup restore below.
    let suppress_volume_osd = Rc::new(Cell::new(false));
    {
        let pipeline = pipeline.clone();
        let volume_btn = ui.volume_btn.clone();
        let last_volume = last_volume.clone();
        let state_volume = state.volume.clone();
        let osd = ui.osd.clone();
        let suppress_volume_osd = suppress_volume_osd.clone();
        ui.volume_scale.connect_value_changed(move |scale| {
            let v = scale.value();
            pipeline.set_property("volume", v);
            state_volume.set(v); // mirror for persistence
            if v > 0.001 {
                last_volume.set(v);
            }
            let icon = if v <= 0.001 {
                "audio-volume-muted-symbolic"
            } else if v < 0.34 {
                "audio-volume-low-symbolic"
            } else if v < 0.67 {
                "audio-volume-medium-symbolic"
            } else {
                "audio-volume-high-symbolic"
            };
            volume_btn.set_icon_name(icon);
            // Skip the OSD for the initial restore (don't pop a toast on a
            // fresh launch showing the welcome screen).
            if suppress_volume_osd.replace(false) {
                return;
            }
            if v <= 0.001 {
                osd.show("audio-volume-muted-symbolic", "Muted");
            } else {
                osd.show(
                    icon,
                    &format!("Volume {}%", (v * 100.0).round() as i32),
                );
            }
        });
    }
    {
        let volume_scale = ui.volume_scale.clone();
        let last_volume = last_volume.clone();
        ui.volume_btn.connect_clicked(move |_| {
            let current = volume_scale.value();
            if current > 0.001 {
                volume_scale.set_value(0.0);
            } else {
                volume_scale.set_value(last_volume.get().max(0.1));
            }
        });
    }

    // Volume slider is shown inline at all times now (no reveal-on-hover).

    // ---------- Fullscreen ----------
    {
        let window_weak = ui.window.downgrade();
        ui.fullscreen_btn.connect_clicked(move |btn| {
            let Some(window) = window_weak.upgrade() else { return };
            if window.is_fullscreen() {
                window.unfullscreen();
                btn.set_icon_name("view-fullscreen-symbolic");
            } else {
                window.fullscreen();
                btn.set_icon_name("view-restore-symbolic");
            }
        });
    }

    // ---------- Seek (user-initiated) ----------
    // Coalesced through AppState::request_seek - only one flush-seek in flight
    // at a time, so mad scrubbing can't flood (and wedge) the HW decoder.
    {
        let pipeline = pipeline.clone();
        let state = state.clone();
        ui.seek_scale.connect_change_value(move |_, _, value| {
            if let Some(dur) = pipeline.query_duration::<gst::ClockTime>() {
                let v = value.clamp(0.0, 1.0);
                let target_ns = (v * dur.nseconds() as f64) as u64;
                state.request_seek(&pipeline, target_ns);
            }
            Propagation::Proceed
        });
    }

    // ---------- Periodic timer ----------
    install_timer(ui, pipe, state);

    // ---------- Bus watcher ----------
    let bus_watch = install_bus_watch(ui, pipe, state, navigate.clone());

    // ---------- Keyboard shortcuts ----------
    install_keyboard(ui, pipe, state);

    // ---------- Window control buttons ----------
    {
        let window_weak = ui.window.downgrade();
        ui.minimize_btn.connect_clicked(move |_| {
            if let Some(w) = window_weak.upgrade() {
                w.minimize();
            }
        });
    }
    {
        let window_weak = ui.window.downgrade();
        ui.maximize_btn.connect_clicked(move |btn| {
            let Some(w) = window_weak.upgrade() else { return };
            if w.is_maximized() {
                w.unmaximize();
                btn.set_icon_name("window-maximize-symbolic");
            } else {
                w.maximize();
                btn.set_icon_name("window-restore-symbolic");
            }
        });
    }
    {
        let window_weak = ui.window.downgrade();
        ui.close_btn.connect_clicked(move |_| {
            if let Some(w) = window_weak.upgrade() {
                w.close();
            }
        });
    }

    // ---------- Mouse: single / double click ----------
    install_mouse_clicks(ui, pipe, state);

    // ---------- Scroll over the video adjusts volume ----------
    {
        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL,
        );
        let volume_scale = ui.volume_scale.clone();
        scroll.connect_scroll(move |_, _dx, dy| {
            // dy < 0 = scroll up = louder; dy > 0 = scroll down = quieter.
            let step = 0.05;
            let v = (volume_scale.value() - dy * step).clamp(0.0, 1.0);
            volume_scale.set_value(v); // updates pipeline + OSD via its handler
            Propagation::Stop
        });
        ui.picture.add_controller(scroll);
    }

    // ---------- Right-click context menu ----------
    install_context_menu(ui);

    // ---------- Auto-hide chrome + draggable control bar ----------
    install_overlay_chrome(ui);

    // ---------- Subtitles ----------
    install_subtitles(ui, pipe, state);

    // ---------- Quick-settings drawer (Video / Audio / Subtitles) ----------
    install_quick_settings(ui, pipe, state);

    // ---------- Resume banner + periodic history flush ----------
    install_resume(ui, pipe, state);
    // Apply the initial subtitle style to the label so it's styled from the
    // first cue (and reflects any persisted preferences).
    apply_subtitle_style(
        &state.subtitles,
        &ui.subtitle_css,
        &ui.subtitle_label,
        &state.subtitle_scale,
        &state.subtitle_margin,
    );

    // Restore the persisted volume onto the slider (its value-changed handler
    // applies it to the pipeline and updates the icon). Suppress the OSD for
    // this one - we don't want a volume toast on a fresh launch.
    suppress_volume_osd.set(true);
    ui.volume_scale.set_value(state.volume.get());
    suppress_volume_osd.set(false);

    // ---------- Clean shutdown ----------
    {
        let pipeline = pipeline.clone();
        let save_state = state.clone();
        ui.window.connect_close_request(move |_| {
            crate::config::save(&save_state); // persist settings
            save_state.resume_store.flush(); // persist watch history
            pipeline.set_state(gst::State::Null).ok(); // finalizes downloadbuffer (temp-remove)
            crate::pipeline::clear_download_cache(); // belt-and-suspenders: wipe any cache file
            let _ = &bus_watch; // keep the bus watch guard alive until the window dies
            Propagation::Proceed
        });
    }

    load_file
}

fn install_timer(ui: &UiHandles, pipe: &PipelineHandles, state: &AppState) {
    let pipeline = pipe.pipeline.clone();
    let seek_scale = ui.seek_scale.clone();
    let position_label = ui.position_label.clone();
    let duration_label = ui.duration_label.clone();
    let play_btn = ui.play_btn.clone();
    let debug_label = ui.debug_label.clone();
    let queue_ref = state.queue_ref.clone();
    let source_ref = state.source_ref.clone();
    let stall_state = state.stall_state.clone();
    let dl_state = state.dl_state.clone();
    let user_paused = state.user_paused.clone();
    let is_local = state.is_local.clone();
    let show_debug = state.show_debug.clone();
    let last_user_seek = state.last_user_seek.clone();
    let pending_restore_pos = state.pending_restore_pos.clone();
    let consecutive_stalls = state.consecutive_stalls.clone();
    let state_for_drag = state.clone();
    let subtitles = state.subtitles.clone();
    let subtitle_label = ui.subtitle_label.clone();
    let subtitle_delay_ns = state.subtitle_delay_ns.clone();
    let resume_store_t = state.resume_store.clone();
    let resume_mode_t = state.resume_mode.clone();
    let current_uri_t = state.current_uri.clone();

    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        let (_, gst_state, _) = pipeline.state(gst::ClockTime::ZERO);
        let expected_icon = if gst_state == gst::State::Playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        };
        if play_btn.icon_name().as_deref() != Some(expected_icon)
            && (gst_state == gst::State::Playing || gst_state == gst::State::Paused)
        {
            play_btn.set_icon_name(expected_icon);
        }

        let pos = pipeline.query_position::<gst::ClockTime>();
        let dur = pipeline.query_duration::<gst::ClockTime>();

        // Remember where we are for resume-on-reopen (in-memory; flushed
        // periodically and on close). record() prunes finished files itself.
        if resume_mode_t.get() != ResumeMode::Off
            && (gst_state == gst::State::Playing || gst_state == gst::State::Paused)
            && let (Some(p), Some(d)) = (pos, dur)
            && p.nseconds() > 0
            && d.nseconds() > 0
            && let Some(uri) = current_uri_t.borrow().as_ref()
        {
            resume_store_t.record(uri, p.nseconds(), d.nseconds());
        }

        let queue_bytes_now: u64 = queue_ref
            .lock()
            .ok()
            .and_then(|slot| {
                slot.as_ref().and_then(|q| {
                    if q.has_property("current-level-bytes", None) {
                        Some(q.property::<u32>("current-level-bytes") as u64)
                    } else {
                        None
                    }
                })
            })
            .unwrap_or(0);

        let pos_ns = pos.map(|p| p.nseconds()).unwrap_or(0);
        // Grace after a seek: long enough for normal HW-decoder seek latency,
        // short enough that a real post-seek freeze recovers quickly.
        let recent_seek = last_user_seek.get().elapsed() < std::time::Duration::from_millis(1800);

        if !state_for_drag.is_dragging_now() && !recent_seek && pos_ns > 0 {
            let stuck_playing = gst_state == gst::State::Playing && pos_ns == stall_state.get().0;
            // Only network streams can stall in Paused waiting for their buffer
            // to refill. A LOCAL file has no queue (q is always 0), so a Paused
            // local file is just paused — never a stall. Treating it as one made
            // the watchdog hard-reload in a loop (visible blinking), especially
            // during the brief preroll after a Ready-cycle.
            let stuck_paused = gst_state == gst::State::Paused
                && !is_local.get()
                && queue_bytes_now == 0
                && !user_paused.get();

            if stuck_playing || stuck_paused {
                let (last, ticks) = stall_state.get();
                let new_ticks = ticks + 1;
                // 2 ticks * 100ms = 200ms of stall detection.
                if new_ticks >= 2 {
                    // Unified recovery: pause/resume first try, Ready→Playing
                    // cycle on the second same-pos try. NEVER do a flushing
                    // seek - it snaps to keyframes and jumps the visible
                    // position backward by 5–10s, looping the user backward
                    // through the movie.
                    let (last_pos, count) = consecutive_stalls.get();
                    let new_count = if last_pos == pos_ns { count + 1 } else { 1 };
                    consecutive_stalls.set((pos_ns, new_count));

                    if new_count >= 2 {
                        eprintln!(
                            "[watchdog] persistent stall at {pos_ns}ns q={}MB \u{2014} ready-cycle",
                            queue_bytes_now / (1024 * 1024)
                        );
                        pending_restore_pos.set(Some(pos_ns));
                        pipeline.set_state(gst::State::Ready).ok();
                        let pl = pipeline.clone();
                        glib::idle_add_local(move || {
                            pl.set_state(gst::State::Playing).ok();
                            ControlFlow::Break
                        });
                        consecutive_stalls.set((0, 0));
                    } else {
                        eprintln!(
                            "[watchdog] stall at {pos_ns}ns q={}MB \u{2014} pause/resume",
                            queue_bytes_now / (1024 * 1024)
                        );
                        pipeline.set_state(gst::State::Paused).ok();
                        let pl = pipeline.clone();
                        glib::idle_add_local(move || {
                            pl.set_state(gst::State::Playing).ok();
                            ControlFlow::Break
                        });
                    }
                    last_user_seek.set(std::time::Instant::now());
                    stall_state.set((pos_ns, 0));
                } else {
                    stall_state.set((last, new_ticks));
                }
            } else {
                stall_state.set((pos_ns, 0));
                consecutive_stalls.set((0, 0));
            }
        }

        if let Some(p) = pos {
            position_label.set_text(&format_time(p));

            // Render the active subtitle cue (if any) in our label, shifted by
            // the user's subtitle delay (positive = subs appear later).
            let lookup_ns = (p.nseconds() as i64 - subtitle_delay_ns.get()).max(0) as u64;
            match subtitles.active_cue_text(lookup_ns) {
                Some(text) => {
                    if subtitle_label.text() != text {
                        subtitle_label.set_text(&text);
                    }
                    if !subtitle_label.is_visible() {
                        subtitle_label.set_visible(true);
                    }
                }
                None => {
                    if subtitle_label.is_visible() {
                        subtitle_label.set_visible(false);
                    }
                }
            }
        }
        if let (Some(p), Some(d)) = (pos, dur)
            && d.nseconds() > 0
        {
            // Right-hand label shows time *remaining* (IINA-style, e.g. -1:23).
            let remaining = gst::ClockTime::from_nseconds(d.nseconds().saturating_sub(p.nseconds()));
            duration_label.set_text(&format!("-{}", format_time(remaining)));
        }
        if let (Some(p), Some(d)) = (pos, dur)
            && d.nseconds() > 0
        {
            if !state_for_drag.is_dragging_now() {
                seek_scale.set_value(p.nseconds() as f64 / d.nseconds() as f64);
            }

            let fill = compute_fill(&pipeline, &queue_ref, &source_ref, &is_local, p, d);
            seek_scale.set_fill_level(fill);

            if show_debug.get() {
                let now = std::time::Instant::now();
                let (last_t, last_b) = dl_state.get();
                let dt = now.duration_since(last_t).as_secs_f64().max(0.001);
                let speed_bps = if queue_bytes_now >= last_b {
                    (queue_bytes_now - last_b) as f64 / dt
                } else {
                    0.0
                };
                dl_state.set((now, queue_bytes_now));
                let mb = |b: u64| (b as f64) / (1024.0 * 1024.0);
                debug_label.set_text(&format!(
                    "pos {} | q {:.1} MiB | net {:.1} MiB/s | fill {:.1}% | state {:?} u_paused {}",
                    format_time(p),
                    mb(queue_bytes_now),
                    speed_bps / (1024.0 * 1024.0),
                    fill * 100.0,
                    gst_state,
                    user_paused.get(),
                ));
            }
        }
        ControlFlow::Continue
    });
}

fn compute_fill(
    pipeline: &gst::Element,
    queue_ref: &Arc<Mutex<Option<gst::Element>>>,
    source_ref: &Arc<Mutex<Option<gst::Element>>>,
    is_local: &Rc<Cell<bool>>,
    p: gst::ClockTime,
    d: gst::ClockTime,
) -> f64 {
    if is_local.get() {
        return 1.0;
    }
    let queue_bytes: u64 = queue_ref
        .lock()
        .ok()
        .and_then(|slot| {
            slot.as_ref().and_then(|q| {
                if q.has_property("current-level-bytes", None) {
                    Some(q.property::<u32>("current-level-bytes") as u64)
                } else {
                    None
                }
            })
        })
        .unwrap_or(0);
    let try_bytes = |slot: &Arc<Mutex<Option<gst::Element>>>| -> u64 {
        slot.lock()
            .ok()
            .and_then(|s| {
                s.as_ref()
                    .and_then(|e| e.query_duration::<gst::format::Bytes>())
            })
            .map(|b| *b)
            .unwrap_or(0)
    };
    let total_bytes = {
        let t1 = try_bytes(source_ref);
        if t1 == 0 { try_bytes(queue_ref) } else { t1 }
    };
    let mut dl_bytes: u64 = 0;
    let mut bq = gst::query::Buffering::new(gst::Format::Bytes);
    if pipeline.query(bq.query_mut()) {
        let (_s, stop, _t) = bq.range();
        if let gst::GenericFormattedValue::Bytes(Some(b)) = stop {
            dl_bytes = *b;
        }
    }
    if total_bytes > 0 {
        let pos_fraction = p.nseconds() as f64 / d.nseconds() as f64;
        let queue_fraction = queue_bytes as f64 / total_bytes as f64;
        let dl_fraction = dl_bytes as f64 / total_bytes as f64;
        (pos_fraction + queue_fraction).max(dl_fraction).clamp(0.0, 1.0)
    } else {
        (p.nseconds() as f64 / d.nseconds() as f64).clamp(0.0, 1.0)
    }
}

fn install_bus_watch(
    ui: &UiHandles,
    pipe: &PipelineHandles,
    state: &AppState,
    navigate: Rc<dyn Fn(i32)>,
) -> gst::bus::BusWatchGuard {
    let bus = pipe.pipeline.bus().expect("Pipeline has no bus");
    let pipeline = pipe.pipeline.clone();
    let play_btn = ui.play_btn.clone();
    let buffer_chip = ui.buffer_chip.clone();
    let toast_overlay = ui.toast_overlay.clone();
    let pending_restore_pos = state.pending_restore_pos.clone();
    let state_for_bus = state.clone();
    bus.add_watch_local(move |_, msg| {
        use gst::MessageView;
        match msg.view() {
            MessageView::AsyncDone(_) => {
                // After a watchdog-driven hard reload, seek back to where we
                // were so the user sees no jump.
                if let Some(target_ns) = pending_restore_pos.take() {
                    eprintln!("[watchdog] hard-reload complete \u{2014} seeking to {target_ns}ns");
                    pipeline
                        .seek_simple(
                            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                            gst::ClockTime::from_nseconds(target_ns),
                        )
                        .ok();
                } else if let Some(target_ns) = state_for_bus.pending_resume_pos.take() {
                    // Auto-resume (Always mode): seek to the remembered position
                    // once the freshly-loaded pipeline has prerolled.
                    pipeline
                        .seek_simple(
                            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                            gst::ClockTime::from_nseconds(target_ns),
                        )
                        .ok();
                } else {
                    // A user/keyboard seek just finished - issue the next
                    // coalesced one if the user kept scrubbing, else go idle.
                    state_for_bus.on_seek_done(&pipeline);
                }
            }
            MessageView::Eos(_) => {
                // Auto-advance to the next file in the folder. navigate(1) is a
                // no-op at the last file, so playback simply stops there.
                let at_last = {
                    let list = state_for_bus.playlist.borrow();
                    list.is_empty()
                        || state_for_bus.playlist_idx.get() + 1 >= list.len()
                };
                if at_last {
                    pipeline.set_state(gst::State::Paused).ok();
                    play_btn.set_icon_name("media-playback-start-symbolic");
                } else {
                    navigate(1);
                }
            }
            MessageView::Buffering(_) => {
                buffer_chip.set_visible(false);
            }
            MessageView::Error(err) => {
                let src = err
                    .src()
                    .map(|s| s.path_string().to_string())
                    .unwrap_or_else(|| "pipeline".into());
                let msg = err.error().to_string();
                eprintln!("GStreamer error from {src}: {msg} ({:?})", err.debug());

                // Errors from the subtitle source bin (the suburi path) are
                // non-fatal - playbin3 emits a spurious not-linked during its
                // stream reconfiguration. Don't kill the movie or toast for
                // those; only surface errors from the main playback path.
                let is_subtitle_path =
                    src.contains("urisourcebin1") || src.contains("suburidecodebin");
                if !is_subtitle_path {
                    // Map cryptic GStreamer errors to a human message.
                    let low = msg.to_lowercase();
                    let friendly = if low.contains("resolve")
                        || low.contains("not found")
                        || low.contains("could not open resource")
                        || low.contains("connect")
                    {
                        "Couldn't open that link - check the address or your connection."
                    } else if low.contains("internal data stream")
                        || low.contains("not-linked")
                        || low.contains("decode")
                    {
                        "Couldn't play this - the file or stream isn't a supported video."
                    } else {
                        "Couldn't play this file or stream."
                    };
                    let toast = adw::Toast::builder().title(friendly).timeout(5).build();
                    toast_overlay.add_toast(toast);
                    // Don't tear the pipeline down - a teardown loses playback
                    // entirely. Leave it; the user can reopen if truly broken.
                    play_btn.set_icon_name("media-playback-start-symbolic");
                }
            }
            MessageView::Warning(w) => {
                eprintln!(
                    "GStreamer warning from {:?}: {} ({:?})",
                    w.src().map(|s| s.path_string()),
                    w.error(),
                    w.debug()
                );
            }
            MessageView::StateChanged(s) => {
                if s.src()
                    .map(|src| src.path_string().ends_with("playbin3-0"))
                    .unwrap_or(false)
                    && s.old() != s.current()
                {
                    eprintln!("[state] {:?} \u{2192} {:?}", s.old(), s.current());
                }
            }
            _ => {}
        }
        ControlFlow::Continue
    })
    .expect("Failed to add bus watch")
}

/// Auto-hide the top bar + control bar after mouse inactivity (revealing them
/// on motion), and make the control bar draggable around the screen.
fn install_overlay_chrome(ui: &UiHandles) {
    let controls = ui.controls.clone();
    let top_bar = ui.top_bar.clone();
    let window = ui.window.clone();

    // Debounce token for the hide timer.
    let hide_token = Rc::new(Cell::new(0u64));

    let show_chrome = {
        let controls = controls.clone();
        let top_bar = top_bar.clone();
        let window = window.clone();
        move || {
            controls.remove_css_class("autohide-hidden");
            top_bar.remove_css_class("autohide-hidden");
            controls.set_can_target(true);
            top_bar.set_can_target(true);
            window.set_cursor(None); // restore default cursor
        }
    };
    let hide_chrome = {
        let controls = controls.clone();
        let top_bar = top_bar.clone();
        let window = window.clone();
        move || {
            // Always hide the panels, regardless of pointer position.
            controls.add_css_class("autohide-hidden");
            top_bar.add_css_class("autohide-hidden");
            controls.set_can_target(false);
            top_bar.set_can_target(false);
            // Hide the cursor only in fullscreen (immersive); keep it in
            // windowed mode so the user can still see/use it.
            if window.is_fullscreen() {
                window.set_cursor(gdk::Cursor::from_name("none", None).as_ref());
            } else {
                window.set_cursor(None);
            }
        }
    };

    // Reset: reveal chrome and schedule a hide ~2.5s later. Hiding is purely
    // time-based - it no longer matters where the pointer is.
    let bump = {
        let show_chrome = show_chrome.clone();
        let hide_chrome = hide_chrome.clone();
        let hide_token = hide_token.clone();
        move || {
            show_chrome();
            let token = hide_token.get().wrapping_add(1);
            hide_token.set(token);
            let hide_chrome = hide_chrome.clone();
            let hide_token = hide_token.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(theme::AUTOHIDE_MS), move || {
                if hide_token.get() == token {
                    hide_chrome();
                }
                ControlFlow::Break
            });
        }
    };

    // Reveal on *real* pointer motion; the timer hides again. We compare the
    // pointer position to the last one and ignore zero-movement events -
    // crucially, hiding the bar (can-target=false) makes the pointer "fall
    // through" to the video, which emits a synthetic same-position motion
    // event that would otherwise instantly re-show the panels (the bug where
    // they never hid while the mouse was inside the window).
    {
        let motion = gtk::EventControllerMotion::new();
        let bump = bump.clone();
        let last_pos = Rc::new(Cell::new((f64::NAN, f64::NAN)));
        motion.connect_motion(move |_, x, y| {
            let (lx, ly) = last_pos.get();
            if !(lx == x && ly == y) {
                last_pos.set((x, y));
                bump();
            }
        });
        ui.window.add_controller(motion);
    }

    // Draggable control bar. The gesture lives on the STATIONARY overlay (not
    // the moving bar) so its coordinates don't drift as the bar moves. We only
    // grab when the press started over the bar; otherwise we deny the gesture
    // so video clicks aren't affected.
    if let Some(overlay) = ui.controls.parent() {
        let drag = gtk::GestureDrag::new();
        // Default gap the centered bar keeps from the window bottom.
        const DEFAULT_BOTTOM: i32 = theme::BAR_EDGE_INSET;
        // base = the bar's top-left (overlay coords) at drag start. We position
        // purely by adding the gesture's *offset* (which is reliable) to this
        // base, so we never read a widget's absolute x/y — those are unreliable
        // in GTK4 (allocation()/compute_bounds() can report 0 here) and were
        // making the bar jump to the top on the first move.
        let base = Rc::new(Cell::new(None::<(f64, f64)>));
        // Whether this press has crossed the drag threshold (so we know it's a
        // drag, not a click). Reset at each press.
        let moved = Rc::new(Cell::new(false));
        let controls = ui.controls.clone();
        {
            let base = base.clone();
            let moved = moved.clone();
            let controls = controls.clone();
            drag.connect_drag_begin(move |g, x, y| {
                moved.set(false);
                let Some(parent) = controls.parent() else {
                    base.set(None);
                    g.set_state(gtk::EventSequenceState::Denied);
                    return;
                };
                // True current size (compute_bounds); width()/height() lag a
                // layout cycle when the bar flips to drag mode, which shrinks
                // the hit-test region and drops the first click.
                let (bar_w, bar_h) = bar_size(&controls, &parent);
                // The bar's current top-left, derived from layout rather than a
                // (flaky) absolute position read.
                let (bx, by) = if controls.halign() == gtk::Align::Start {
                    (controls.margin_start(), controls.margin_top())
                } else {
                    (
                        (parent.width() - bar_w) / 2,
                        parent.height() - bar_h - DEFAULT_BOTTOM,
                    )
                };
                let inside = x >= bx as f64
                    && x <= (bx + bar_w) as f64
                    && y >= by as f64
                    && y <= (by + bar_h) as f64;
                if inside {
                    base.set(Some((bx as f64, by as f64)));
                } else {
                    base.set(None);
                    g.set_state(gtk::EventSequenceState::Denied);
                }
            });
        }
        {
            let base = base.clone();
            let moved = moved.clone();
            let controls = controls.clone();
            drag.connect_drag_update(move |g, off_x, off_y| {
                let Some((bx, by)) = base.get() else { return };
                // Ignore tiny travel so a press-and-release stays a click. Once
                // past the threshold this is a drag: claim the gesture so the
                // button under the pointer doesn't also fire its action.
                if !moved.get() {
                    if off_x.hypot(off_y) < theme::BAR_DRAG_THRESHOLD {
                        return;
                    }
                    moved.set(true);
                    g.set_state(gtk::EventSequenceState::Claimed);
                }
                let mut nx = (bx + off_x) as i32;
                let mut ny = (by + off_y) as i32;
                // Switch to absolute positioning on first move.
                controls.set_halign(gtk::Align::Start);
                controls.set_valign(gtk::Align::Start);
                controls.set_margin_bottom(0);
                if let Some(parent) = controls.parent() {
                    const INSET: i32 = theme::BAR_EDGE_INSET;
                    let (bar_w, bar_h) = bar_size(&controls, &parent);
                    // Keep up to INSET margin per side, but shrink the margin
                    // (down to centered) when an axis has no slack, so a tight
                    // window still allows movement on the other axis.
                    let clamp_pos = |v: i32, avail: i32| {
                        let lo = INSET.min(avail / 2);
                        let hi = (avail - lo).max(lo);
                        v.clamp(lo, hi)
                    };
                    nx = clamp_pos(nx, parent.width() - bar_w);
                    ny = clamp_pos(ny, parent.height() - bar_h);
                }
                controls.set_margin_start(nx);
                controls.set_margin_top(ny);
            });
        }
        overlay.add_controller(drag);
    }

    // Keep the (dragged) bar inside the overlay after any resize / fullscreen.
    // A cheap periodic re-clamp covers every resize path without needing
    // size-allocate signals.
    {
        let controls = ui.controls.clone();
        let seek_scale = ui.seek_scale.clone();
        let volume_revealer = ui.volume_revealer.clone();
        let volume_btn = ui.volume_btn.clone();
        let stop_btn = ui.stop_btn.clone();
        let subtitle_btn = ui.subtitle_btn.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            let Some(parent) = controls.parent() else {
                return ControlFlow::Continue;
            };
            const INSET: i32 = theme::BAR_EDGE_INSET;

            // Progressively collapse non-essential controls as the window
            // narrows so the panel never outgrows the window. Transport + seek
            // always stay; the volume slider drops first, then the volume icon,
            // stop, and subtitles.
            let w = parent.width();
            let show_vol_slider = w >= 500;
            if volume_revealer.reveals_child() != show_vol_slider {
                volume_revealer.set_reveal_child(show_vol_slider);
            }
            for (widget, min) in [(&volume_btn, 440), (&stop_btn, 400), (&subtitle_btn, 360)] {
                let vis = w >= min;
                if widget.is_visible() != vis {
                    widget.set_visible(vis);
                }
            }

            if controls.halign() == gtk::Align::Start {
                // Dragged/absolute mode: keep it inside the window.
                let (cw, ch) = bar_size(&controls, &parent);
                // Only fall back to centered-bottom when the bar is genuinely
                // LARGER than the window (can't be contained at all). Merely
                // being wider than window-minus-margins must NOT snap it back —
                // that fought the drag every tick (blinking) and forced it to
                // the bottom on release when there was no horizontal slack.
                if cw > parent.width() || ch > parent.height() {
                    controls.set_halign(gtk::Align::Center);
                    controls.set_valign(gtk::Align::End);
                    controls.set_margin_start(0);
                    controls.set_margin_top(0);
                    controls.set_margin_bottom(theme::BAR_EDGE_INSET);
                } else {
                    // Clamp within the window, keeping up to INSET margin on
                    // each side (less when space is tight), without ever
                    // repositioning along an axis that still fits.
                    let clamp_pos = |cur: i32, avail: i32| {
                        let lo = INSET.min(avail / 2);
                        let hi = (avail - lo).max(lo);
                        cur.clamp(lo, hi)
                    };
                    let nx = clamp_pos(controls.margin_start(), parent.width() - cw);
                    let ny = clamp_pos(controls.margin_top(), parent.height() - ch);
                    if nx != controls.margin_start() {
                        controls.set_margin_start(nx);
                    }
                    if ny != controls.margin_top() {
                        controls.set_margin_top(ny);
                    }
                }
            } else {
                // Default centered mode. The bar hugs its content (so its side
                // margins are always symmetric); we cap the seek scale's width
                // so the whole bar fits the window with a comfortable gutter
                // on each side instead of running into the edges.
                const SIDE_GUTTER: i32 = theme::BAR_EDGE_INSET;
                // Width of the seek row's fixed chrome (time labels + spacing +
                // the bar's horizontal padding) — everything except the scale.
                const SEEK_ROW_CHROME: i32 = theme::SEEK_ROW_CHROME;
                let avail = parent.width() - 2 * SIDE_GUTTER - SEEK_ROW_CHROME;
                let target = avail.clamp(theme::SEEK_WIDTH_MIN, theme::SEEK_WIDTH_MAX);
                if seek_scale.width_request() != target {
                    seek_scale.set_width_request(target);
                }
            }
            ControlFlow::Continue
        });
    }

    // Reveal initially.
    bump();
}

/// The control bar's true on-screen size in `parent` coordinates. GTK4's
/// `width()`/`height()` lag a layout cycle when the bar flips between centered
/// and dragged mode (reporting the previous, smaller allocation), which makes
/// edge-clamping overflow and shrinks the drag hit-test region. `compute_bounds`
/// reports the current size, so prefer it and fall back to the allocation.
fn bar_size(controls: &gtk::Box, parent: &gtk::Widget) -> (i32, i32) {
    controls
        .compute_bounds(parent)
        .map(|b| (b.width().ceil() as i32, b.height().ceil() as i32))
        .unwrap_or_else(|| (controls.width(), controls.height()))
}

/// Video file extensions we treat as playable for folder navigation.
const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "m4v", "wmv", "flv", "mpeg", "mpg",
    "ts", "m2ts", "mts", "ogv", "3gp", "vob",
];

fn is_video_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Scan the folder of `path` for video files, sorted by name (case-insensitive).
/// Returns the sorted list and the index of `path` within it.
fn scan_playlist(path: &std::path::Path) -> (Vec<std::path::PathBuf>, usize) {
    let Some(dir) = path.parent() else {
        return (vec![path.to_path_buf()], 0);
    };
    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && is_video_path(p))
            .collect(),
        Err(_) => vec![path.to_path_buf()],
    };
    files.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    let idx = files.iter().position(|p| p == path).unwrap_or(0);
    (files, idx)
}

/// Is this file a subtitle (by extension)? Used by the drop handler.
fn is_subtitle_uri(uri: &str) -> bool {
    let lower = uri.to_lowercase();
    [".srt", ".ass", ".ssa", ".vtt", ".sub"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Read a subtitle file and return its text decoded to UTF-8 (handling
/// Windows-1256 for Persian/Arabic), plus the display filename.
fn read_subtitle_text(file: &gio::File) -> Option<(String, String)> {
    let path = file.path()?;
    let bytes = std::fs::read(&path).ok()?;
    let without_bom = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let text = if let Ok(s) = std::str::from_utf8(without_bom) {
        s.to_string()
    } else {
        let (cow, _enc, _err) = encoding_rs::WINDOWS_1256.decode(&bytes);
        eprintln!("[subs] decoded subtitle as Windows-1256");
        cow.into_owned()
    };
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Subtitle".into());
    Some((text, name))
}

/// Load an external subtitle file. We parse the SRT ourselves and render cues
/// in a GTK label synced to the playback position (see the periodic timer) -
/// playbin's own external-subtitle pipeline doesn't render reliably with our
/// hardware-decode + GTK sink setup, so we bypass it entirely.
fn load_subtitle(
    pipeline: &gst::Element,
    state: &AppState,
    osd: &crate::osd::Osd,
    file: &gio::File,
) {
    let Some((text, name)) = read_subtitle_text(file) else {
        eprintln!("[subs] could not read subtitle file");
        osd.show("dialog-warning-symbolic", "Couldn't read subtitle");
        return;
    };
    osd.show(
        "media-view-subtitles-symbolic",
        &format!("Added subtitle: {name}"),
    );
    let idx = state.subtitles.add_external(&text, name);
    state.subtitles.set_external(pipeline, idx);
}

/// A flat menu-row button: [check icon] [label].
fn subtitle_menu_row(label: &str, active: bool) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let check = gtk::Image::from_icon_name("object-select-symbolic");
    check.set_size_request(16, 16);
    check.set_opacity(if active { 1.0 } else { 0.0 });
    row.append(&check);
    let lbl = gtk::Label::new(Some(label));
    lbl.set_xalign(0.0);
    lbl.set_hexpand(true);
    row.append(&lbl);
    let btn = gtk::Button::new();
    btn.set_child(Some(&row));
    btn.add_css_class("flat");
    btn.add_css_class("context-menu-item");
    btn.set_focus_on_click(false);
    btn
}

/// Wire the subtitle button: a popover listing Off / available text tracks /
/// "Add subtitle file…", rebuilt each time it opens.
fn install_subtitles(ui: &UiHandles, pipe: &PipelineHandles, state: &AppState) {
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    popover.set_parent(&ui.subtitle_btn);
    popover.add_css_class("menu");

    let pipeline = pipe.pipeline.clone();
    let subs = state.subtitles.clone();
    let menu_state = state.clone();
    let window_weak = ui.window.downgrade();
    let sub_label = ui.subtitle_label.clone();
    let osd = ui.osd.clone();

    // Clear the on-screen subtitle immediately when switching tracks so the
    // previous track's text doesn't linger until the next cue.
    let clear_label = {
        let sub_label = sub_label.clone();
        move || {
            sub_label.set_text("");
            sub_label.set_visible(false);
        }
    };

    ui.subtitle_btn.connect_clicked(move |_| {
        use crate::subtitles::Active;
        let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        menu_box.add_css_class("context-menu");

        let active = *subs.active.borrow();

        // "Off"
        let off_row = subtitle_menu_row("Off", active == Active::Off);
        {
            let subs = subs.clone();
            let pipeline = pipeline.clone();
            let popover = popover.clone();
            let clear_label = clear_label.clone();
            let osd = osd.clone();
            off_row.connect_clicked(move |_| {
                subs.set_off(&pipeline);
                clear_label();
                osd.show("media-view-subtitles-symbolic", "Subtitles off");
                popover.popdown();
            });
        }
        menu_box.append(&off_row);

        // Embedded tracks (rendered by playbin).
        for (index, label) in subs.embedded_tracks(&pipeline) {
            let row = subtitle_menu_row(&label, active == Active::Embedded(index));
            let subs = subs.clone();
            let pipeline = pipeline.clone();
            let popover = popover.clone();
            let clear_label = clear_label.clone();
            let osd = osd.clone();
            let label_msg = label.clone();
            row.connect_clicked(move |_| {
                subs.set_embedded(&pipeline, index);
                clear_label();
                osd.show("media-view-subtitles-symbolic", &format!("Subtitle: {label_msg}"));
                popover.popdown();
            });
            menu_box.append(&row);
        }

        // External subs loaded this session (rendered by our label).
        for (i, ext) in subs.externals.borrow().iter().enumerate() {
            let row = subtitle_menu_row(&ext.name, active == Active::External(i));
            let subs = subs.clone();
            let pipeline = pipeline.clone();
            let popover = popover.clone();
            let clear_label = clear_label.clone();
            let osd = osd.clone();
            let name_msg = ext.name.clone();
            row.connect_clicked(move |_| {
                subs.set_external(&pipeline, i);
                clear_label();
                osd.show("media-view-subtitles-symbolic", &format!("Subtitle: {name_msg}"));
                popover.popdown();
            });
            menu_box.append(&row);
        }

        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        sep.set_margin_top(4);
        sep.set_margin_bottom(4);
        menu_box.append(&sep);

        // "Add subtitle file…"
        let add_row = subtitle_menu_row("Add subtitle file\u{2026}", false);
        {
            let window_weak = window_weak.clone();
            let pipeline = pipeline.clone();
            let popover = popover.clone();
            let menu_state = menu_state.clone();
            let osd = osd.clone();
            add_row.connect_clicked(move |_| {
                popover.popdown();
                let Some(window) = window_weak.upgrade() else { return };
                let filter = gtk::FileFilter::new();
                filter.set_name(Some("Subtitle files"));
                for p in ["*.srt", "*.ass", "*.ssa", "*.vtt", "*.sub"] {
                    filter.add_pattern(p);
                }
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                let dialog = gtk::FileDialog::builder()
                    .title("Add Subtitle")
                    .modal(true)
                    .filters(&filters)
                    .build();
                let pipeline = pipeline.clone();
                let menu_state = menu_state.clone();
                let osd = osd.clone();
                dialog.open(Some(&window), gio::Cancellable::NONE, move |res| {
                    if let Ok(file) = res {
                        load_subtitle(&pipeline, &menu_state, &osd, &file);
                    }
                });
            });
        }
        menu_box.append(&add_row);

        popover.set_child(Some(&menu_box));
        popover.popup();
    });
}

/// gdk::RGBA (0..1 floats) → big-endian ARGB u32 (0xAARRGGBB) for textoverlay.
fn rgba_to_argb(c: &gdk::RGBA) -> u32 {
    let a = (c.alpha() * 255.0).round() as u32;
    let r = (c.red() * 255.0).round() as u32;
    let g = (c.green() * 255.0).round() as u32;
    let b = (c.blue() * 255.0).round() as u32;
    (a << 24) | (r << 16) | (g << 8) | b
}

/// Big-endian ARGB u32 → gdk::RGBA, for seeding the color buttons.
fn argb_to_rgba(argb: u32) -> gdk::RGBA {
    let a = ((argb >> 24) & 0xFF) as f32 / 255.0;
    let r = ((argb >> 16) & 0xFF) as f32 / 255.0;
    let g = ((argb >> 8) & 0xFF) as f32 / 255.0;
    let b = (argb & 0xFF) as f32 / 255.0;
    gdk::RGBA::new(r, g, b, a)
}

/// Apply the subtitle style to our custom label: regenerate its CSS and set
/// the vertical alignment + margins.
fn apply_subtitle_style(
    subs: &crate::subtitles::Subtitles,
    css: &gtk::CssProvider,
    label: &gtk::Label,
    scale: &Rc<Cell<f64>>,
    margin: &Rc<Cell<i32>>,
) {
    use crate::subtitles::VAlign;
    let style = subs.style.lock().unwrap();
    css.load_from_string(&crate::subtitles::subtitle_css(&style, scale.get()));
    match style.valign {
        VAlign::Bottom => {
            label.set_valign(gtk::Align::End);
            label.set_margin_bottom(margin.get());
            label.set_margin_top(0);
        }
        VAlign::Top => {
            label.set_valign(gtk::Align::Start);
            label.set_margin_top(60);
            label.set_margin_bottom(0);
        }
        VAlign::Center => {
            label.set_valign(gtk::Align::Center);
            label.set_margin_top(0);
            label.set_margin_bottom(0);
        }
    }
}

/// Build the "Subtitles" PreferencesPage. Each control updates the shared
/// style and live-applies it to our subtitle label via `apply_subtitle_style`.
fn build_subtitles_page(
    subs: &crate::subtitles::Subtitles,
    css: &gtk::CssProvider,
    label: &gtk::Label,
    scale: &Rc<Cell<f64>>,
    margin: &Rc<Cell<i32>>,
) -> adw::PreferencesPage {
    use crate::subtitles::VAlign;

    let group = adw::PreferencesGroup::builder().title("Subtitle style").build();
    let apply = {
        let subs = subs.clone();
        let css = css.clone();
        let label = label.clone();
        let scale = scale.clone();
        let margin = margin.clone();
        move || apply_subtitle_style(&subs, &css, &label, &scale, &margin)
    };

    // --- Font ---
    let font_row = adw::ActionRow::builder().title("Font").build();
    let font_btn = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
    font_btn.set_valign(gtk::Align::Center);
    font_btn.set_font_desc(&pango::FontDescription::from_string(
        &subs.style.lock().unwrap().font_desc,
    ));
    font_row.add_suffix(&font_btn);
    {
        let subs = subs.clone();
        let apply = apply.clone();
        font_btn.connect_font_desc_notify(move |btn| {
            if let Some(desc) = btn.font_desc() {
                subs.style.lock().unwrap().font_desc = desc.to_str().to_string();
                apply();
            }
        });
    }
    group.add(&font_row);

    // --- Text color ---
    let color_row = adw::ActionRow::builder().title("Text color").build();
    let color_btn = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    color_btn.set_valign(gtk::Align::Center);
    color_btn.set_rgba(&argb_to_rgba(subs.style.lock().unwrap().color));
    color_row.add_suffix(&color_btn);
    {
        let subs = subs.clone();
        let apply = apply.clone();
        color_btn.connect_rgba_notify(move |btn| {
            subs.style.lock().unwrap().color = rgba_to_argb(&btn.rgba());
            apply();
        });
    }
    group.add(&color_row);

    // --- Outline color ---
    let outline_row = adw::ActionRow::builder().title("Outline color").build();
    let outline_btn = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    outline_btn.set_valign(gtk::Align::Center);
    outline_btn.set_rgba(&argb_to_rgba(subs.style.lock().unwrap().outline_color));
    outline_row.add_suffix(&outline_btn);
    {
        let subs = subs.clone();
        let apply = apply.clone();
        outline_btn.connect_rgba_notify(move |btn| {
            subs.style.lock().unwrap().outline_color = rgba_to_argb(&btn.rgba());
            apply();
        });
    }
    group.add(&outline_row);

    // --- Toggles ---
    let outline_sw = adw::SwitchRow::builder()
        .title("Draw outline")
        .active(subs.style.lock().unwrap().draw_outline)
        .build();
    {
        let subs = subs.clone();
        let apply = apply.clone();
        outline_sw.connect_active_notify(move |r| {
            subs.style.lock().unwrap().draw_outline = r.is_active();
            apply();
        });
    }
    group.add(&outline_sw);

    let shadow_sw = adw::SwitchRow::builder()
        .title("Draw shadow")
        .active(subs.style.lock().unwrap().draw_shadow)
        .build();
    {
        let subs = subs.clone();
        let apply = apply.clone();
        shadow_sw.connect_active_notify(move |r| {
            subs.style.lock().unwrap().draw_shadow = r.is_active();
            apply();
        });
    }
    group.add(&shadow_sw);

    let bg_sw = adw::SwitchRow::builder()
        .title("Shaded background")
        .subtitle("Translucent box behind the text")
        .active(subs.style.lock().unwrap().shaded_background)
        .build();
    {
        let subs = subs.clone();
        let apply = apply.clone();
        bg_sw.connect_active_notify(move |r| {
            subs.style.lock().unwrap().shaded_background = r.is_active();
            apply();
        });
    }
    group.add(&bg_sw);

    // --- Vertical position ---
    let pos_row = adw::ComboRow::builder()
        .title("Position")
        .model(&gtk::StringList::new(&["Bottom", "Top", "Center"]))
        .selected(match subs.style.lock().unwrap().valign {
            VAlign::Bottom => 0,
            VAlign::Top => 1,
            VAlign::Center => 2,
        })
        .build();
    {
        let subs = subs.clone();
        let apply = apply.clone();
        pos_row.connect_selected_notify(move |r| {
            subs.style.lock().unwrap().valign = match r.selected() {
                1 => VAlign::Top,
                2 => VAlign::Center,
                _ => VAlign::Bottom,
            };
            apply();
        });
    }
    group.add(&pos_row);

    // --- Vertical offset (moves bottom-aligned subtitles up/down) ---
    let (offset_row, offset_scale) =
        qs_slider("Vertical offset", 0.0, theme::SUBTITLE_MARGIN_MAX as f64, 4.0, margin.get() as f64);
    {
        let margin = margin.clone();
        let apply = apply.clone();
        offset_scale.connect_value_changed(move |s| {
            margin.set(s.value() as i32);
            apply();
        });
    }
    offset_row.add_suffix(&qs_reset(&offset_scale, theme::SUBTITLE_MARGIN_DEFAULT as f64));
    group.add(&offset_row);

    let page = adw::PreferencesPage::builder()
        .title("Subtitles")
        .icon_name("media-view-subtitles-symbolic")
        .build();
    page.add(&group);
    page
}

/// Build the "Mouse" PreferencesPage with two ComboRows for single/double
/// click actions.
fn build_mouse_page(mouse: &crate::shortcuts::MouseBindings) -> adw::PreferencesPage {
    use crate::shortcuts::Action;

    let actions = Action::all();
    // Combo string list: index 0 = "None", then each action.
    let make_strings = || {
        let mut v: Vec<&str> = Vec::with_capacity(actions.len() + 1);
        v.push("None");
        for a in actions {
            v.push(a.title());
        }
        v
    };
    let index_of = |action: Option<Action>| -> u32 {
        match action {
            None => 0,
            Some(a) => actions
                .iter()
                .position(|x| *x == a)
                .map(|i| (i + 1) as u32)
                .unwrap_or(0),
        }
    };
    let action_from_index = |idx: u32| -> Option<Action> {
        if idx == 0 {
            None
        } else {
            actions.get((idx - 1) as usize).copied()
        }
    };

    let group = adw::PreferencesGroup::builder()
        .title("Mouse on video")
        .description("Action performed when clicking on the video area")
        .build();

    let single_row = adw::ComboRow::builder()
        .title("Single click")
        .model(&gtk::StringList::new(&make_strings()))
        .selected(index_of(mouse.single.get()))
        .build();
    {
        let mouse = mouse.clone();
        single_row.connect_selected_notify(move |row| {
            mouse.single.set(action_from_index(row.selected()));
        });
    }

    let double_row = adw::ComboRow::builder()
        .title("Double click")
        .model(&gtk::StringList::new(&make_strings()))
        .selected(index_of(mouse.double.get()))
        .build();
    {
        let mouse = mouse.clone();
        double_row.connect_selected_notify(move |row| {
            mouse.double.set(action_from_index(row.selected()));
        });
    }

    group.add(&single_row);
    group.add(&double_row);

    let page = adw::PreferencesPage::builder()
        .title("Mouse")
        .icon_name("input-mouse-symbolic")
        .build();
    page.add(&group);
    page
}

/// Run the configured single/double-click action against the player.
fn install_mouse_clicks(ui: &UiHandles, pipe: &PipelineHandles, state: &AppState) {
    // GestureClick reports n_press incrementing for repeated clicks within
    // the system double-click time. We delay the single-click action by a
    // short timeout so a double-click doesn't briefly fire the single-click
    // action first.
    let gesture = gtk::GestureClick::new();
    gesture.set_button(1);

    let pending_single: Rc<Cell<u32>> = Rc::new(Cell::new(0));

    let pipeline = pipe.pipeline.clone();
    let play_btn = ui.play_btn.clone();
    let fullscreen_btn = ui.fullscreen_btn.clone();
    let open_btn = ui.open_btn.clone();
    let link_btn = ui.link_btn.clone();
    let volume_btn = ui.volume_btn.clone();
    let volume_scale = ui.volume_scale.clone();
    let next_btn = ui.next_btn.clone();
    let prev_btn = ui.prev_btn.clone();
    let osd = ui.osd.clone();
    let last_user_seek = state.last_user_seek.clone();
    let mouse = state.mouse.clone();

    // Clone for the cancel handler before `pending_single` is moved into the
    // pressed closure below.
    let pending_single_cancel = pending_single.clone();

    gesture.connect_pressed(move |_, n_press, _, _| {
        if n_press == 1 {
            // Tentatively schedule the single-click action.
            let token = pending_single.get().wrapping_add(1);
            pending_single.set(token);
            let pending_single = pending_single.clone();
            let pipeline = pipeline.clone();
            let play_btn = play_btn.clone();
            let fullscreen_btn = fullscreen_btn.clone();
            let open_btn = open_btn.clone();
            let link_btn = link_btn.clone();
            let volume_btn = volume_btn.clone();
            let volume_scale = volume_scale.clone();
            let next_btn = next_btn.clone();
            let prev_btn = prev_btn.clone();
            let osd = osd.clone();
            let last_user_seek = last_user_seek.clone();
            let mouse = mouse.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(260), move || {
                if pending_single.get() == token
                    && let Some(action) = mouse.single.get()
                {
                    run_action(
                        action,
                        &pipeline,
                        &play_btn,
                        &fullscreen_btn,
                        &open_btn,
                        &link_btn,
                        &volume_btn,
                        &volume_scale,
                        &next_btn,
                        &prev_btn,
                        &osd,
                        &last_user_seek,
                    );
                }
                ControlFlow::Break
            });
        } else if n_press == 2 {
            // Cancel any pending single-click and fire double-click action.
            pending_single.set(pending_single.get().wrapping_add(1));
            if let Some(action) = mouse.double.get() {
                run_action(
                    action,
                    &pipeline,
                    &play_btn,
                    &fullscreen_btn,
                    &open_btn,
                    &link_btn,
                    &volume_btn,
                    &volume_scale,
                    &next_btn,
                    &prev_btn,
                    &osd,
                    &last_user_seek,
                );
            }
        }
    });
    // If the press turns into a window-move drag (below), the click gesture is
    // cancelled by the compositor grab — void any pending single-click so the
    // drag doesn't also toggle play/pause.
    gesture.connect_cancel(move |_, _| {
        pending_single_cancel.set(pending_single_cancel.get().wrapping_add(1));
    });
    ui.picture.add_controller(gesture);

    // Drag anywhere on the video to move the window. We hand off to the
    // compositor (Toplevel::begin_move) once the pointer passes a small
    // threshold, so plain clicks still toggle play/pause. The controls panel is
    // a separate overlay child and intercepts its own events, so it's excluded.
    {
        let window = ui.window.clone();
        let move_drag = gtk::GestureDrag::new();
        move_drag.set_button(gdk::BUTTON_PRIMARY);
        let moving = Rc::new(Cell::new(false));
        {
            let moving = moving.clone();
            move_drag.connect_drag_begin(move |_, _, _| moving.set(false));
        }
        {
            let moving = moving.clone();
            let window = window.clone();
            move_drag.connect_drag_update(move |g, off_x, off_y| {
                if moving.get() || off_x.hypot(off_y) < 8.0 {
                    return;
                }
                moving.set(true);
                let Some(surface) = window.surface() else { return };
                let Ok(toplevel) = surface.downcast::<gdk::Toplevel>() else {
                    return;
                };
                let Some(device) = g.current_event_device() else { return };
                let (sx, sy) = g.start_point().unwrap_or((0.0, 0.0));
                toplevel.begin_move(
                    &device,
                    g.current_button() as i32,
                    sx,
                    sy,
                    g.current_event_time(),
                );
            });
        }
        ui.picture.add_controller(move_drag);
    }
}

/// Right-click anywhere on the player surface opens a small menu with
/// Open file / Open URL / Preferences. We build it with custom buttons
/// (instead of `gtk::PopoverMenu` + `gio::Menu`) because GTK4's menu model
/// rendering doesn't show icons.
fn install_context_menu(ui: &UiHandles) {
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    popover.set_parent(&ui.window);
    popover.add_css_class("menu");

    let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    menu_box.add_css_class("context-menu");

    // Build a single row: [icon] [label], styled as a flat button.
    let make_row = |icon_name: &str, label: &str| -> gtk::Button {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.append(&gtk::Image::from_icon_name(icon_name));
        let lbl = gtk::Label::new(Some(label));
        lbl.set_xalign(0.0);
        lbl.set_hexpand(true);
        row.append(&lbl);
        let btn = gtk::Button::new();
        btn.set_child(Some(&row));
        btn.add_css_class("flat");
        btn.add_css_class("context-menu-item");
        btn.set_focus_on_click(false);
        btn
    };

    let open_item = make_row("document-open-symbolic", "Open File\u{2026}");
    {
        let popover_clone = popover.clone();
        let open_btn = ui.open_btn.clone();
        open_item.connect_clicked(move |_| {
            popover_clone.popdown();
            open_btn.emit_clicked();
        });
    }
    let link_item = make_row("insert-link-symbolic", "Open URL\u{2026}");
    {
        let popover_clone = popover.clone();
        let link_btn = ui.link_btn.clone();
        link_item.connect_clicked(move |_| {
            popover_clone.popdown();
            link_btn.emit_clicked();
        });
    }
    let prefs_item = make_row("emblem-system-symbolic", "Preferences\u{2026}");
    {
        let popover_clone = popover.clone();
        let settings_btn = ui.settings_btn.clone();
        prefs_item.connect_clicked(move |_| {
            popover_clone.popdown();
            settings_btn.emit_clicked();
        });
    }

    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    separator.set_margin_top(4);
    separator.set_margin_bottom(4);

    menu_box.append(&open_item);
    menu_box.append(&link_item);
    menu_box.append(&separator);
    menu_box.append(&prefs_item);
    popover.set_child(Some(&menu_box));

    let right_click = gtk::GestureClick::new();
    right_click.set_button(3); // right mouse button
    let popover_clone = popover.clone();
    right_click.connect_pressed(move |_, _n_press, x, y| {
        popover_clone.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover_clone.popup();
    });
    ui.window.add_controller(right_click);
}

/// Open a small dialog that captures the next key the user presses and
/// assigns it as the binding for `action`. Esc cancels.
fn capture_shortcut(
    parent: &gtk::Window,
    action: Action,
    shortcuts: &crate::shortcuts::Shortcuts,
    row_label: &gtk::Label,
) {
    let hint = gtk::Label::new(Some("Press a key combination…\n(Esc to cancel)"));
    hint.set_justify(gtk::Justification::Center);
    hint.set_margin_top(8);
    hint.set_margin_bottom(8);

    let dialog = adw::AlertDialog::builder()
        .heading(format!("Set shortcut: {}", action.title()))
        .extra_child(&hint)
        .close_response("cancel")
        .build();
    dialog.add_response("cancel", "Cancel");

    let key_controller = gtk::EventControllerKey::new();
    {
        let shortcuts = shortcuts.clone();
        let row_label = row_label.clone();
        let dialog_weak = dialog.downgrade();
        key_controller.connect_key_pressed(move |_, key, _, mods| {
            // Ignore lone modifier presses - wait for the real key.
            let is_modifier_only = matches!(
                key,
                gdk::Key::Control_L
                    | gdk::Key::Control_R
                    | gdk::Key::Shift_L
                    | gdk::Key::Shift_R
                    | gdk::Key::Alt_L
                    | gdk::Key::Alt_R
                    | gdk::Key::Super_L
                    | gdk::Key::Super_R
                    | gdk::Key::Meta_L
                    | gdk::Key::Meta_R
            );
            if is_modifier_only {
                return Propagation::Stop;
            }
            if key == gdk::Key::Escape {
                if let Some(d) = dialog_weak.upgrade() {
                    d.close();
                }
                return Propagation::Stop;
            }
            let sc = crate::shortcuts::Shortcut::from_event(key, mods);
            shortcuts.set(action, sc);
            row_label.set_text(&sc.label());
            if let Some(d) = dialog_weak.upgrade() {
                d.close();
            }
            Propagation::Stop
        });
    }
    dialog.add_controller(key_controller);
    dialog.present(Some(parent));
}

fn install_keyboard(ui: &UiHandles, pipe: &PipelineHandles, state: &AppState) {
    let key_controller = gtk::EventControllerKey::new();
    // Capture phase: we see keys before the focused widget does, so Space
    // triggers Play/Pause even when a button happens to be focused (e.g.
    // after a dialog closes and focus snaps back to the button that opened it).
    key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let play_btn = ui.play_btn.clone();
    let fullscreen_btn = ui.fullscreen_btn.clone();
    let open_btn = ui.open_btn.clone();
    let link_btn = ui.link_btn.clone();
    let volume_btn = ui.volume_btn.clone();
    let volume_scale = ui.volume_scale.clone();
    let next_btn = ui.next_btn.clone();
    let prev_btn = ui.prev_btn.clone();
    let osd = ui.osd.clone();
    let pipeline = pipe.pipeline.clone();
    let shortcuts = state.shortcuts.clone();
    let last_user_seek = state.last_user_seek.clone();

    key_controller.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(action) = shortcuts.lookup(key, modifiers) else {
            return Propagation::Proceed;
        };
        run_action(
            action,
            &pipeline,
            &play_btn,
            &fullscreen_btn,
            &open_btn,
            &link_btn,
            &volume_btn,
            &volume_scale,
            &next_btn,
            &prev_btn,
            &osd,
            &last_user_seek,
        );
        Propagation::Stop
    });
    ui.window.add_controller(key_controller);
}

#[allow(clippy::too_many_arguments)]
fn run_action(
    action: Action,
    pipeline: &gst::Element,
    play_btn: &gtk::Button,
    fullscreen_btn: &gtk::Button,
    open_btn: &gtk::Button,
    link_btn: &gtk::Button,
    volume_btn: &gtk::Button,
    volume_scale: &gtk::Scale,
    next_btn: &gtk::Button,
    prev_btn: &gtk::Button,
    osd: &crate::osd::Osd,
    last_user_seek: &Rc<Cell<std::time::Instant>>,
) {
    // Two seek modes:
    //   - precise: ACCURATE for ±N s arrow-key nudges (must land on the exact
    //     time, even if it means decoding from the previous keyframe).
    //   - snap: KEY_UNIT|SNAP_NEAREST for jumps (start/end), instant feel.
    // Using SNAP_NEAREST for arrow nudges caused "forward doesn't work"
    // (snapped to the keyframe behind target) and "backward random amount".
    let seek_precise = |target: gst::ClockTime| {
        last_user_seek.set(std::time::Instant::now());
        pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, target)
            .ok();
    };
    let seek_snap = |target: gst::ClockTime| {
        last_user_seek.set(std::time::Instant::now());
        pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH
                    | gst::SeekFlags::KEY_UNIT
                    | gst::SeekFlags::SNAP_NEAREST,
                target,
            )
            .ok();
    };
    let seek_relative = |delta_secs: i64| {
        let Some(pos) = pipeline.query_position::<gst::ClockTime>() else { return };
        let dur = pipeline.query_duration::<gst::ClockTime>();
        let target_ns = if delta_secs >= 0 {
            let bump = (delta_secs as u64).saturating_mul(1_000_000_000);
            let candidate = pos.nseconds().saturating_add(bump);
            match dur {
                Some(d) if d.nseconds() > 0 => {
                    candidate.min(d.nseconds().saturating_sub(200_000_000))
                }
                _ => candidate,
            }
        } else {
            let dec = ((-delta_secs) as u64).saturating_mul(1_000_000_000);
            pos.nseconds().saturating_sub(dec)
        };
        seek_precise(gst::ClockTime::from_nseconds(target_ns));
    };

    match action {
        Action::PlayPause => play_btn.emit_clicked(),
        Action::Fullscreen => fullscreen_btn.emit_clicked(),
        Action::OpenFile => open_btn.emit_clicked(),
        Action::OpenUrl => link_btn.emit_clicked(),
        Action::NextTrack => next_btn.emit_clicked(),
        Action::PrevTrack => prev_btn.emit_clicked(),
        Action::Mute => volume_btn.emit_clicked(),
        Action::VolumeUp => {
            let v = (volume_scale.value() + 0.05).clamp(0.0, 1.0);
            volume_scale.set_value(v);
        }
        Action::VolumeDown => {
            let v = (volume_scale.value() - 0.05).clamp(0.0, 1.0);
            volume_scale.set_value(v);
        }
        Action::SeekBackwardSmall => {
            seek_relative(-5);
            osd.show("media-seek-backward-symbolic", "-5s");
        }
        Action::SeekForwardSmall => {
            seek_relative(5);
            osd.show("media-seek-forward-symbolic", "+5s");
        }
        Action::SeekBackwardLarge => {
            seek_relative(-10);
            osd.show("media-seek-backward-symbolic", "-10s");
        }
        Action::SeekForwardLarge => {
            seek_relative(10);
            osd.show("media-seek-forward-symbolic", "+10s");
        }
        Action::JumpStart => {
            seek_snap(gst::ClockTime::ZERO);
            osd.show("media-skip-backward-symbolic", "Start");
        }
        Action::JumpEnd => {
            if let Some(d) = pipeline.query_duration::<gst::ClockTime>()
                && d.nseconds() > 0
            {
                seek_snap(gst::ClockTime::from_nseconds(
                    d.nseconds().saturating_sub(2_000_000_000),
                ));
            }
        }
    }
}

// ===================== Resume (continue where you left off) ==================

/// Wire the resume banner's buttons and a periodic flush of the watch history.
fn install_resume(ui: &UiHandles, pipe: &PipelineHandles, state: &AppState) {
    let banner = ui.resume_banner.clone();

    {
        let banner = banner.clone();
        let check = ui.resume_check.clone();
        let pipeline = pipe.pipeline.clone();
        let state = state.clone();
        ui.resume_btn.connect_clicked(move |_| {
            if let Some(pos) = state.resume_prompt_pos.take() {
                state.request_seek(&pipeline, pos);
            }
            if check.is_active() {
                // The user opted in: remember and auto-resume from now on.
                state.resume_mode.set(ResumeMode::Always);
            }
            banner.set_visible(false);
        });
    }
    {
        let banner = banner.clone();
        let state = state.clone();
        ui.resume_dismiss_btn.connect_clicked(move |_| {
            state.resume_prompt_pos.set(None); // keep playing from the start
            banner.set_visible(false);
        });
    }

    // Flush the in-memory watch history to disk periodically (also done on
    // file switch and on close) so a crash loses at most a few seconds.
    {
        let store = state.resume_store.clone();
        glib::timeout_add_seconds_local(5, move || {
            store.flush();
            ControlFlow::Continue
        });
    }
}

// ===================== Quick-settings drawer (Video/Audio/Subtitles) =========

/// Wire the gear toggle and (re)build the three tabbed pages on each open, so
/// they always reflect the current track lists and live effect values.
fn install_quick_settings(ui: &UiHandles, pipe: &PipelineHandles, state: &AppState) {
    let stack = ui.settings_view_stack.clone();
    let revealer = ui.settings_revealer.clone();
    let pipeline = pipe.pipeline.clone();
    let state = state.clone();
    let ui_subs_css = ui.subtitle_css.clone();
    let ui_subs_label = ui.subtitle_label.clone();

    let rebuild = {
        let stack = stack.clone();
        let pipeline = pipeline.clone();
        let state = state.clone();
        let css = ui_subs_css.clone();
        let label = ui_subs_label.clone();
        move || {
            while let Some(child) = stack.first_child() {
                stack.remove(&child);
            }
            let video = build_video_qs_page(&pipeline, &state);
            let audio = build_audio_qs_page(&pipeline, &state);
            let subs = build_subtitle_qs_page(&pipeline, &state, &css, &label);
            stack.add_titled_with_icon(&video, Some("video"), "Video", "video-display-symbolic");
            stack.add_titled_with_icon(&audio, Some("audio"), "Audio", "audio-volume-high-symbolic");
            stack.add_titled_with_icon(
                &subs,
                Some("subtitles"),
                "Subtitles",
                "media-view-subtitles-symbolic",
            );
        }
    };

    {
        let revealer = revealer.clone();
        ui.settings_panel_btn.connect_clicked(move |_| {
            if revealer.reveals_child() {
                revealer.set_reveal_child(false);
            } else {
                rebuild();
                revealer.set_reveal_child(true);
            }
        });
    }
    {
        let revealer = revealer.clone();
        ui.settings_close_btn.connect_clicked(move |_| revealer.set_reveal_child(false));
    }

    // Click anywhere on the video (outside the drawer) to dismiss it. A
    // capture-phase gesture runs before the play/pause click and claims the
    // press when it closes, so dismissing doesn't also toggle playback. Clicks
    // on the drawer itself go to its own widgets and never reach here.
    {
        let revealer = revealer.clone();
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.connect_pressed(move |g, _, _, _| {
            if revealer.reveals_child() {
                revealer.set_reveal_child(false);
                g.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        ui.picture.add_controller(click);
    }
}

// ---- small builders ----

/// An ActionRow with a horizontal slider in its suffix.
fn qs_slider(title: &str, min: f64, max: f64, step: f64, value: f64) -> (adw::ActionRow, gtk::Scale) {
    let row = adw::ActionRow::builder().title(title).build();
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
    scale.set_value(value.clamp(min, max));
    scale.set_hexpand(true);
    scale.set_size_request(150, -1);
    scale.set_draw_value(false);
    scale.set_valign(gtk::Align::Center);
    row.add_suffix(&scale);
    (row, scale)
}

/// A small reset button that snaps a scale back to `default`.
fn qs_reset(scale: &gtk::Scale, default: f64) -> gtk::Button {
    let btn = gtk::Button::from_icon_name("edit-undo-symbolic");
    btn.add_css_class("flat");
    btn.set_valign(gtk::Align::Center);
    btn.set_tooltip_text(Some("Reset"));
    let scale = scale.clone();
    btn.connect_clicked(move |_| scale.set_value(default));
    btn
}

fn qs_string_model(items: &[String]) -> gtk::StringList {
    let refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
    gtk::StringList::new(&refs)
}

/// Toggle a `GstPlayFlags` token on the playbin (e.g. "video"/"audio"/
/// "deinterlace"), keeping the shared flags string in sync.
fn toggle_play_flag(pipeline: &gst::Element, subs: &crate::subtitles::Subtitles, token: &str, on: bool) {
    let new = {
        let cur = subs.flags.borrow();
        let mut parts: Vec<&str> = cur.split('+').filter(|p| *p != token).collect();
        if on {
            parts.push(token);
        }
        parts.join("+")
    };
    *subs.flags.borrow_mut() = new.clone();
    let _ = pipeline.set_property_from_str("flags", &new);
}

/// Position-preserving reload (Ready→Playing); the AsyncDone bus handler seeks
/// back to `pending_restore_pos`.
fn qs_reload_pipeline(pipeline: &gst::Element, state: &AppState) {
    if let Some(pos) = pipeline.query_position::<gst::ClockTime>() {
        state.pending_restore_pos.set(Some(pos.nseconds()));
    }
    pipeline.set_state(gst::State::Ready).ok();
    let pl = pipeline.clone();
    glib::idle_add_local_once(move || {
        pl.set_state(gst::State::Playing).ok();
    });
}

/// Raise/lower the rank of common hardware decoder factories so playbin's
/// autoplug prefers software decoders when HW decoding is switched off.
fn set_hw_decoders_enabled(on: bool) {
    let registry = gst::Registry::get();
    const HW: &[&str] = &[
        "nvh264dec", "nvh265dec", "nvh264sldec", "nvh265sldec", "nvav1dec", "nvvp8dec",
        "nvvp9dec", "nvmpeg2videodec", "nvmpeg4videodec", "vah264dec", "vah265dec", "vavp9dec",
        "vaav1dec", "vaapidecodebin", "vaapih264dec", "vaapih265dec", "v4l2h264dec",
        "v4l2h265dec", "v4l2slh264dec", "msdkh264dec",
    ];
    let rank = if on { gst::Rank::PRIMARY } else { gst::Rank::NONE };
    for name in HW {
        if let Some(f) = registry.lookup_feature(name) {
            f.set_rank(rank);
        }
    }
}

/// The negotiated source video width/height, or (0, 0) if not yet known.
fn source_video_size(pipeline: &gst::Element) -> (i32, i32) {
    let pad = pipeline.emit_by_name::<Option<gst::Pad>>("get-video-pad", &[&0i32]);
    if let Some(pad) = pad
        && let Some(caps) = pad.current_caps()
        && let Some(s) = caps.structure(0)
    {
        return (
            s.get::<i32>("width").unwrap_or(0),
            s.get::<i32>("height").unwrap_or(0),
        );
    }
    (0, 0)
}

fn qs_tag_label(tags: &Option<gst::TagList>, idx: i32, kind: &str) -> String {
    if let Some(t) = tags {
        if let Some(v) = t.get::<gst::tags::Title>() {
            return v.get().to_string();
        }
        if let Some(v) = t.get::<gst::tags::LanguageName>() {
            return v.get().to_string();
        }
        if let Some(v) = t.get::<gst::tags::LanguageCode>() {
            return v.get().to_string();
        }
    }
    format!("{kind} {}", idx + 1)
}

fn qs_tracks(pipeline: &gst::Element, n_prop: &str, tags_signal: &str, kind: &str) -> Vec<String> {
    let n: i32 = pipeline.property(n_prop);
    (0..n)
        .map(|i| {
            let tags = pipeline.emit_by_name::<Option<gst::TagList>>(tags_signal, &[&i]);
            qs_tag_label(&tags, i, kind)
        })
        .collect()
}

// ---- Video page ----

fn build_video_qs_page(pipeline: &gst::Element, state: &AppState) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    let effects = state.effects.clone();

    // Track
    let track_group = adw::PreferencesGroup::builder().title("Video track").build();
    let mut items = vec!["None".to_string()];
    items.extend(qs_tracks(pipeline, "n-video", "get-video-tags", "Video"));
    let cur: i32 = pipeline.property("current-video");
    let track_row = adw::ComboRow::builder()
        .title("Track")
        .model(&qs_string_model(&items))
        .build();
    track_row.set_selected((cur + 1).max(0) as u32);
    {
        let pipeline = pipeline.clone();
        let subs = state.subtitles.clone();
        track_row.connect_selected_notify(move |row| {
            let sel = row.selected() as i32;
            if sel <= 0 {
                toggle_play_flag(&pipeline, &subs, "video", false);
            } else {
                toggle_play_flag(&pipeline, &subs, "video", true);
                pipeline.set_property("current-video", sel - 1);
            }
        });
    }
    track_group.add(&track_row);
    page.add(&track_group);

    // Geometry
    let geo = adw::PreferencesGroup::builder().title("Geometry").build();

    let aspect_items: Vec<String> = AspectMode::ALL.iter().map(|m| m.label().to_string()).collect();
    let aspect_row = adw::ComboRow::builder()
        .title("Aspect ratio")
        .model(&qs_string_model(&aspect_items))
        .build();
    aspect_row.set_selected(
        AspectMode::ALL.iter().position(|m| *m == effects.aspect.get()).unwrap_or(0) as u32,
    );
    {
        let effects = effects.clone();
        aspect_row.connect_selected_notify(move |row| {
            effects.set_aspect(AspectMode::ALL[row.selected() as usize]);
        });
    }
    geo.add(&aspect_row);

    let crop_items: Vec<String> = AspectMode::ALL
        .iter()
        .map(|m| if *m == AspectMode::Default { "None".to_string() } else { m.label().to_string() })
        .collect();
    let crop_row = adw::ComboRow::builder()
        .title("Crop")
        .model(&qs_string_model(&crop_items))
        .build();
    crop_row.set_selected(
        AspectMode::ALL.iter().position(|m| *m == effects.crop_mode.get()).unwrap_or(0) as u32,
    );
    {
        let effects = effects.clone();
        let pipeline = pipeline.clone();
        crop_row.connect_selected_notify(move |row| {
            let (w, h) = source_video_size(&pipeline);
            effects.set_crop(AspectMode::ALL[row.selected() as usize], w, h);
        });
    }
    geo.add(&crop_row);

    let rot_items: Vec<String> = ["0°", "90°", "180°", "270°"].iter().map(|s| s.to_string()).collect();
    let rot_row = adw::ComboRow::builder()
        .title("Rotation")
        .model(&qs_string_model(&rot_items))
        .build();
    rot_row.set_selected(match effects.rotation.get() {
        90 => 1,
        180 => 2,
        270 => 3,
        _ => 0,
    });
    {
        let effects = effects.clone();
        rot_row.connect_selected_notify(move |row| {
            effects.set_rotation([0u16, 90, 180, 270][row.selected() as usize]);
        });
    }
    geo.add(&rot_row);
    page.add(&geo);

    // Speed
    let speed_group = adw::PreferencesGroup::builder().title("Speed").build();
    let (speed_row, speed_scale) = qs_slider("Playback speed", 0.25, 16.0, 0.05, effects.speed.get());
    speed_scale.set_draw_value(true);
    speed_scale.set_value_pos(gtk::PositionType::Left);
    {
        let effects = effects.clone();
        let pipeline = pipeline.clone();
        let state = state.clone();
        speed_scale.connect_value_changed(move |s| {
            effects.speed.set(s.value());
            let pos = pipeline
                .query_position::<gst::ClockTime>()
                .map(|p| p.nseconds())
                .unwrap_or(0);
            state.request_seek(&pipeline, pos);
        });
    }
    speed_row.add_suffix(&qs_reset(&speed_scale, 1.0));
    speed_group.add(&speed_row);
    page.add(&speed_group);

    // Decoding
    let dec = adw::PreferencesGroup::builder().title("Decoding").build();
    let hw_row = adw::SwitchRow::builder().title("Hardware decoding").active(true).build();
    {
        let pipeline = pipeline.clone();
        let state = state.clone();
        hw_row.connect_active_notify(move |row| {
            set_hw_decoders_enabled(row.is_active());
            qs_reload_pipeline(&pipeline, &state);
        });
    }
    dec.add(&hw_row);

    let deint_on = state.subtitles.flags.borrow().split('+').any(|p| p == "deinterlace");
    let deint_row = adw::SwitchRow::builder().title("Deinterlace").active(deint_on).build();
    {
        let pipeline = pipeline.clone();
        let subs = state.subtitles.clone();
        deint_row.connect_active_notify(move |row| {
            toggle_play_flag(&pipeline, &subs, "deinterlace", row.is_active());
        });
    }
    dec.add(&deint_row);
    page.add(&dec);

    // Color equalizer
    let color = adw::PreferencesGroup::builder().title("Equalizer").build();
    let seed = |slot: &Arc<Mutex<Option<gst::Element>>>, prop: &str, default: f64| -> f64 {
        slot.lock()
            .ok()
            .and_then(|g| g.as_ref().map(|e| e.property::<f64>(prop)))
            .unwrap_or(default)
    };
    let add_color = |group: &adw::PreferencesGroup,
                     title: &str,
                     min: f64,
                     max: f64,
                     value: f64,
                     default: f64,
                     apply: Box<dyn Fn(f64)>| {
        let (row, scale) = qs_slider(title, min, max, (max - min) / 200.0, value);
        scale.connect_value_changed(move |s| apply(s.value()));
        row.add_suffix(&qs_reset(&scale, default));
        group.add(&row);
    };
    add_color(&color, "Brightness", -1.0, 1.0, seed(&effects.videobalance, "brightness", 0.0), 0.0, {
        let e = effects.clone();
        Box::new(move |v| e.set_brightness(v))
    });
    add_color(&color, "Contrast", 0.0, 2.0, seed(&effects.videobalance, "contrast", 1.0), 1.0, {
        let e = effects.clone();
        Box::new(move |v| e.set_contrast(v))
    });
    add_color(&color, "Saturation", 0.0, 2.0, seed(&effects.videobalance, "saturation", 1.0), 1.0, {
        let e = effects.clone();
        Box::new(move |v| e.set_saturation(v))
    });
    add_color(&color, "Gamma", 0.1, 4.0, seed(&effects.gamma, "gamma", 1.0), 1.0, {
        let e = effects.clone();
        Box::new(move |v| e.set_gamma(v))
    });
    add_color(&color, "Hue", -1.0, 1.0, seed(&effects.videobalance, "hue", 0.0), 0.0, {
        let e = effects.clone();
        Box::new(move |v| e.set_hue(v))
    });
    page.add(&color);

    page
}

// ---- Audio page ----

fn build_audio_qs_page(pipeline: &gst::Element, state: &AppState) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    let effects = state.effects.clone();

    // Track
    let track_group = adw::PreferencesGroup::builder().title("Audio track").build();
    let mut items = vec!["None".to_string()];
    items.extend(qs_tracks(pipeline, "n-audio", "get-audio-tags", "Audio"));
    let cur: i32 = pipeline.property("current-audio");
    let track_row = adw::ComboRow::builder()
        .title("Track")
        .model(&qs_string_model(&items))
        .build();
    track_row.set_selected((cur + 1).max(0) as u32);
    {
        let pipeline = pipeline.clone();
        let subs = state.subtitles.clone();
        track_row.connect_selected_notify(move |row| {
            let sel = row.selected() as i32;
            if sel <= 0 {
                toggle_play_flag(&pipeline, &subs, "audio", false);
            } else {
                toggle_play_flag(&pipeline, &subs, "audio", true);
                pipeline.set_property("current-audio", sel - 1);
            }
        });
    }
    track_group.add(&track_row);
    page.add(&track_group);

    // Delay
    let delay_group = adw::PreferencesGroup::builder().title("Audio delay").build();
    let cur_delay = effects.av_offset_ns.get() as f64 / 1e9;
    let (delay_row, delay_scale) = qs_slider("Delay (s)", -5.0, 5.0, 0.05, cur_delay);
    delay_scale.set_draw_value(true);
    delay_scale.set_value_pos(gtk::PositionType::Left);
    {
        let effects = effects.clone();
        let pipeline = pipeline.clone();
        delay_scale.connect_value_changed(move |s| {
            effects.set_av_offset(&pipeline, (s.value() * 1e9) as i64);
        });
    }
    delay_row.add_suffix(&qs_reset(&delay_scale, 0.0));
    delay_group.add(&delay_row);
    page.add(&delay_group);

    // 10-band equalizer
    let eq_group = adw::PreferencesGroup::builder().title("Equalizer").build();
    const FREQS: [&str; 10] = ["32", "64", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];

    let preset_items: Vec<String> = EQ_PRESETS
        .iter()
        .map(|(n, _)| n.to_string())
        .chain(std::iter::once("Manual".to_string()))
        .collect();
    let manual_idx = EQ_PRESETS.len() as u32;
    let preset_row = adw::ComboRow::builder()
        .title("Preset")
        .model(&qs_string_model(&preset_items))
        .build();
    preset_row.set_selected(manual_idx);
    eq_group.add(&preset_row);

    let suppress = Rc::new(Cell::new(false));
    let mut band_scales: Vec<gtk::Scale> = Vec::with_capacity(10);
    for (i, freq) in FREQS.iter().enumerate() {
        let seed = effects
            .equalizer
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|e| e.property::<f64>(format!("band{i}").as_str())))
            .unwrap_or(0.0);
        let (row, scale) = qs_slider(&format!("{freq} Hz"), -12.0, 12.0, 0.5, seed);
        {
            let effects = effects.clone();
            let suppress = suppress.clone();
            let preset_row = preset_row.clone();
            scale.connect_value_changed(move |s| {
                effects.set_eq_band(i, s.value());
                if !suppress.get() {
                    preset_row.set_selected(manual_idx);
                }
            });
        }
        eq_group.add(&row);
        band_scales.push(scale);
    }
    {
        let suppress = suppress.clone();
        preset_row.connect_selected_notify(move |row| {
            let idx = row.selected() as usize;
            if idx < EQ_PRESETS.len() {
                suppress.set(true);
                for (i, scale) in band_scales.iter().enumerate() {
                    scale.set_value(EQ_PRESETS[idx].1[i]);
                }
                suppress.set(false);
            }
        });
    }
    page.add(&eq_group);

    page
}

// ---- Subtitle page ----

fn build_subtitle_qs_page(
    pipeline: &gst::Element,
    state: &AppState,
    css: &gtk::CssProvider,
    label: &gtk::Label,
) -> adw::PreferencesPage {
    use crate::subtitles::Active;
    let page = adw::PreferencesPage::new();
    let subs = state.subtitles.clone();

    let apply = {
        let subs = subs.clone();
        let css = css.clone();
        let label = label.clone();
        let scale = state.subtitle_scale.clone();
        let margin = state.subtitle_margin.clone();
        Rc::new(move || apply_subtitle_style(&subs, &css, &label, &scale, &margin))
    };

    // Track selection (Off / embedded / external).
    let track_group = adw::PreferencesGroup::builder().title("Subtitle track").build();
    let mut items = vec!["Off".to_string()];
    let embedded = subs.embedded_tracks(pipeline);
    let mut mapping: Vec<Active> = vec![Active::Off];
    for (idx, lbl) in &embedded {
        items.push(lbl.clone());
        mapping.push(Active::Embedded(*idx));
    }
    for (i, ext) in subs.externals.borrow().iter().enumerate() {
        items.push(ext.name.clone());
        mapping.push(Active::External(i));
    }
    let cur_active = *subs.active.borrow();
    let sel = mapping
        .iter()
        .position(|a| match (a, cur_active) {
            (Active::Off, Active::Off) => true,
            (Active::Embedded(x), Active::Embedded(y)) => *x == y,
            (Active::External(x), Active::External(y)) => *x == y,
            _ => false,
        })
        .unwrap_or(0);
    let track_row = adw::ComboRow::builder()
        .title("Track")
        .model(&qs_string_model(&items))
        .build();
    track_row.set_selected(sel as u32);
    {
        let pipeline = pipeline.clone();
        let subs = subs.clone();
        track_row.connect_selected_notify(move |row| {
            match mapping.get(row.selected() as usize).copied().unwrap_or(Active::Off) {
                Active::Off => subs.set_off(&pipeline),
                Active::Embedded(i) => subs.set_embedded(&pipeline, i),
                Active::External(i) => subs.set_external(&pipeline, i),
            }
        });
    }
    track_group.add(&track_row);
    page.add(&track_group);

    // Timing & geometry
    let timing = adw::PreferencesGroup::builder().title("Timing & position").build();

    let (delay_row, delay_scale) =
        qs_slider("Delay (s)", -10.0, 10.0, 0.1, state.subtitle_delay_ns.get() as f64 / 1e9);
    delay_scale.set_draw_value(true);
    delay_scale.set_value_pos(gtk::PositionType::Left);
    {
        let delay = state.subtitle_delay_ns.clone();
        delay_scale.connect_value_changed(move |s| delay.set((s.value() * 1e9) as i64));
    }
    delay_row.add_suffix(&qs_reset(&delay_scale, 0.0));
    timing.add(&delay_row);

    let (pos_row, pos_scale) =
        qs_slider("Position", 0.0, theme::SUBTITLE_MARGIN_MAX as f64, 4.0, state.subtitle_margin.get() as f64);
    {
        let margin = state.subtitle_margin.clone();
        let apply = apply.clone();
        pos_scale.connect_value_changed(move |s| {
            margin.set(s.value() as i32);
            apply();
        });
    }
    pos_row.add_suffix(&qs_reset(&pos_scale, theme::SUBTITLE_MARGIN_DEFAULT as f64));
    timing.add(&pos_row);

    let (scale_row, scale_scale) = qs_slider("Scale", theme::SUBTITLE_SCALE_MIN, theme::SUBTITLE_SCALE_MAX, 0.05, state.subtitle_scale.get());
    scale_scale.set_draw_value(true);
    scale_scale.set_value_pos(gtk::PositionType::Left);
    {
        let scale = state.subtitle_scale.clone();
        let apply = apply.clone();
        scale_scale.connect_value_changed(move |s| {
            scale.set(s.value());
            apply();
        });
    }
    scale_row.add_suffix(&qs_reset(&scale_scale, theme::SUBTITLE_SCALE_DEFAULT));
    timing.add(&scale_row);
    page.add(&timing);

    // Text style (mutates the global SubtitleStyle)
    let style_group = adw::PreferencesGroup::builder().title("Text style").build();

    let color_row = adw::ActionRow::builder().title("Text color").build();
    let color_btn = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    color_btn.set_valign(gtk::Align::Center);
    color_btn.set_rgba(&argb_to_rgba(subs.style.lock().unwrap().color));
    {
        let subs = subs.clone();
        let apply = apply.clone();
        color_btn.connect_rgba_notify(move |b| {
            subs.style.lock().unwrap().color = rgba_to_argb(&b.rgba());
            apply();
        });
    }
    color_row.add_suffix(&color_btn);
    style_group.add(&color_row);

    let outline_row = adw::ActionRow::builder().title("Outline color").build();
    let outline_btn = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    outline_btn.set_valign(gtk::Align::Center);
    outline_btn.set_rgba(&argb_to_rgba(subs.style.lock().unwrap().outline_color));
    {
        let subs = subs.clone();
        let apply = apply.clone();
        outline_btn.connect_rgba_notify(move |b| {
            subs.style.lock().unwrap().outline_color = rgba_to_argb(&b.rgba());
            apply();
        });
    }
    outline_row.add_suffix(&outline_btn);
    style_group.add(&outline_row);

    let (width_row, width_scale) =
        qs_slider("Outline width", 0.0, 6.0, 0.5, subs.style.lock().unwrap().outline_width as f64);
    width_scale.set_draw_value(true);
    width_scale.set_value_pos(gtk::PositionType::Left);
    {
        let subs = subs.clone();
        let apply = apply.clone();
        width_scale.connect_value_changed(move |s| {
            let mut st = subs.style.lock().unwrap();
            st.outline_width = s.value() as f32;
            st.draw_outline = s.value() > 0.0;
            drop(st);
            apply();
        });
    }
    width_row.add_suffix(&qs_reset(&width_scale, 2.0));
    style_group.add(&width_row);

    let bg_row = adw::SwitchRow::builder()
        .title("Shaded background")
        .active(subs.style.lock().unwrap().shaded_background)
        .build();
    {
        let subs = subs.clone();
        let apply = apply.clone();
        bg_row.connect_active_notify(move |r| {
            subs.style.lock().unwrap().shaded_background = r.is_active();
            apply();
        });
    }
    style_group.add(&bg_row);
    page.add(&style_group);

    page
}
