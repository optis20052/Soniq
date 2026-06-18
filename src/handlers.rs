//! The app controller: builds all shared player state + closures into a [`Ctx`]
//! ([`build`]), connects every UI callback ([`wire`]), runs the background
//! housekeeping timer ([`housekeeping`]) and the CLI auto-launch
//! ([`launch_cli`]). `main` is just the composition root that calls these in
//! order; this is the spike's analog of the GTK app's `handlers.rs`.

#![allow(clippy::too_many_lines)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::config::{Config, ResumeMode, SubStyle};
use crate::shortcuts::ACTIONS;
use crate::store::Store;
use crate::subs::{apply_adjust, apply_sub_style};
use crate::util::{basename, fmt_time, system_font_families, track_label};
use crate::video::VideoBridge;
use crate::{prefs, render, shortcuts, wl_dnd, wl_opaque, App, QueueItem, RecentUi, TrackUi};

const SUBTITLE_EXTS: [&str; 5] = [".srt", ".ass", ".ssa", ".vtt", ".sub"];
const VIDEO_EXTS_DND: [&str; 13] = [
    ".mp4", ".mkv", ".webm", ".mov", ".avi", ".m4v", ".ts", ".flv", ".wmv", ".mpg", ".mpeg",
    ".ogv", ".3gp",
];

/// Shared player state that `main` builds once and the controller borrows.
pub struct Ctx {
    pub bridge: Rc<RefCell<Option<VideoBridge>>>,
    pub opaque_region: Rc<RefCell<Option<wl_opaque::OpaqueRegion>>>,
    pub last_activity: Rc<Cell<Instant>>,
    pub store: Rc<RefCell<Store>>,
    pub current_path: Rc<RefCell<Option<String>>>,
    pub pending_resume: Rc<Cell<f64>>,
    pub pending_seek: Rc<Cell<f64>>,
    pub want_autosize: Rc<Cell<bool>>,
    pub resize_anim: Rc<Cell<Option<(f32, f32, f32, f32, Instant)>>>,
    pub reveal_frames: Rc<Cell<u32>>,
    pub media_loaded: Rc<Cell<bool>>,
    pub resume_mode: Rc<Cell<ResumeMode>>,
    pub mouse_single: Rc<Cell<i32>>,
    pub mouse_double: Rc<Cell<i32>>,
    pub mouse_right: Rc<Cell<i32>>,
    pub sub_style: Rc<RefCell<SubStyle>>,
    pub cache_fwd: Rc<Cell<i64>>,
    pub cache_back: Rc<Cell<i64>>,
    pub font_families: Rc<Vec<String>>,
    pub bindings: Rc<RefCell<Vec<String>>>,
    pub persist: Rc<dyn Fn()>,
    pub refresh_recents: Rc<dyn Fn()>,
    pub show_osd: Rc<dyn Fn(&str)>,
    pub load_target: Rc<dyn Fn(String)>,
    pub handle_drop: Rc<dyn Fn(String)>,
    pub video_model: Rc<VecModel<TrackUi>>,
    pub audio_model: Rc<VecModel<TrackUi>>,
    pub sub_model: Rc<VecModel<TrackUi>>,
    pub queue_model: Rc<VecModel<QueueItem>>,
    pub dnd: Rc<RefCell<Option<wl_dnd::DndListener>>>,
    pub saved_volume: f64,
    pub saved_muted: bool,
    pub saved_sub_style: SubStyle,
}

impl Ctx {
    /// Bundle the state the GPU render bridge needs (see render.rs).
    pub fn render_deps(&self) -> render::RenderDeps {
        render::RenderDeps {
            bridge: self.bridge.clone(),
            opaque_region: self.opaque_region.clone(),
            current_path: self.current_path.clone(),
            pending_seek: self.pending_seek.clone(),
            want_autosize: self.want_autosize.clone(),
            resize_anim: self.resize_anim.clone(),
            reveal_frames: self.reveal_frames.clone(),
            media_loaded: self.media_loaded.clone(),
            show_osd: self.show_osd.clone(),
            dnd: self.dnd.clone(),
            handle_drop: self.handle_drop.clone(),
            saved_volume: self.saved_volume,
            saved_muted: self.saved_muted,
            saved_sub_style: self.saved_sub_style.clone(),
            saved_cache_fwd: self.cache_fwd.get(),
            saved_cache_back: self.cache_back.get(),
        }
    }
}

/// Create every shared `Rc` of state and the core closures (persist, OSD,
/// load-a-file, drop routing), seed the recents list, and return them bundled.
pub fn build(app: &App) -> Ctx {
    let bridge: Rc<RefCell<Option<VideoBridge>>> = Rc::new(RefCell::new(None));
    // GTK-style Wayland opaque region (body opaque, corners blended) — created
    // once the winit window exists (render setup).
    let opaque_region: Rc<RefCell<Option<wl_opaque::OpaqueRegion>>> = Rc::new(RefCell::new(None));
    let last_activity = Rc::new(Cell::new(Instant::now()));
    let store = Rc::new(RefCell::new(Store::load()));
    let current_path = Rc::new(RefCell::new(None::<String>));
    let pending_resume = Rc::new(Cell::new(0.0f64));
    // Seek to apply once the freshly-loaded file reports a duration (used for
    // both "Always" auto-resume and the "Ask" banner's Resume button, so the
    // seek lands after mpv has actually opened the file).
    let pending_seek = Rc::new(Cell::new(0.0f64));
    // Resize the window to the video's aspect on open (while the home screen
    // still covers, so it's home → resize → video, not a double black flash).
    let want_autosize = Rc::new(Cell::new(false));
    // In-flight smooth window-resize animation (IINA-style):
    // (from_w, from_h, to_w, to_h, started) in logical px.
    #[allow(clippy::type_complexity)]
    let resize_anim: Rc<Cell<Option<(f32, f32, f32, f32, Instant)>>> = Rc::new(Cell::new(None));
    // Counts consecutive frames where the FBO is settled at the video's native
    // size; the video is revealed only once this passes a small threshold, so
    // the nvdec-init / resize churn at load stays hidden.
    let reveal_frames = Rc::new(Cell::new(0u32));
    // A file is loaded and should be rendered into the FBO (in the background,
    // before reveal). Lets the home screen stay up as the load cover — and the
    // window auto-resize happen — *before* we flip to the video, so opening a
    // file goes home → resize → video instead of flashing two black screens.
    let media_loaded = Rc::new(Cell::new(false));

    // ---- persisted user settings (config.json) ----
    let cfg = Config::load();
    let resume_mode = Rc::new(Cell::new(cfg.resume_mode.unwrap_or_default()));
    // Mouse-on-video actions, by combo index (see app.slint Mouse page):
    // 0 None · 1 Play/Pause · 2 Toggle controls · 3 Toggle fullscreen · 4 Mute.
    let mouse_single = Rc::new(Cell::new(cfg.mouse_single.unwrap_or(2)));
    let mouse_double = Rc::new(Cell::new(cfg.mouse_double.unwrap_or(3)));
    // Right click defaults to Play / Pause (action index 1).
    let mouse_right = Rc::new(Cell::new(cfg.mouse_right.unwrap_or(1)));
    let sub_style = Rc::new(RefCell::new(cfg.sub_style.clone().unwrap_or_default()));
    // Demuxer cache window (MiB), user-tunable in Prefs. Defaults 64 ahead /
    // 32 behind — generous enough for smooth streaming and recent-seek scrubbing
    // without the old 256/256 (≈512MiB) RAM hoard.
    let cache_fwd = Rc::new(Cell::new(cfg.cache_fwd_mib.unwrap_or(64)));
    let cache_back = Rc::new(Cell::new(cfg.cache_back_mib.unwrap_or(32)));
    // All installed font families (for the subtitle font picker), enumerated
    // once at startup; the prefs window filters this list as the user types.
    let font_families = Rc::new(system_font_families());
    let saved_volume = cfg.volume.unwrap_or(1.0);
    let saved_muted = cfg.muted.unwrap_or(false);
    let saved_sub_style = sub_style.borrow().clone();

    // Keyboard bindings, seeded from saved shortcuts (action key → key name).
    let bindings: Rc<RefCell<Vec<String>>> = {
        let mut v: Vec<String> = ACTIONS.iter().map(|a| a.2.to_string()).collect();
        if let Some(map) = &cfg.shortcuts {
            for (i, a) in ACTIONS.iter().enumerate() {
                if let Some(k) = map.get(a.0) {
                    v[i] = k.clone();
                }
            }
        }
        Rc::new(RefCell::new(v))
    };

    // Rebuild the on-disk config from the current in-memory settings and save.
    // Called after any persisted setting changes and on close.
    let persist: Rc<dyn Fn()> = {
        let bindings = bindings.clone();
        let resume_mode = resume_mode.clone();
        let mouse_single = mouse_single.clone();
        let mouse_double = mouse_double.clone();
        let mouse_right = mouse_right.clone();
        let sub_style = sub_style.clone();
        let cache_fwd = cache_fwd.clone();
        let cache_back = cache_back.clone();
        let weak = app.as_weak();
        Rc::new(move || {
            let shortcuts: HashMap<String, String> = ACTIONS
                .iter()
                .enumerate()
                .map(|(i, a)| (a.0.to_string(), bindings.borrow()[i].clone()))
                .collect();
            let (volume, muted) = weak
                .upgrade()
                .map(|a| (a.get_volume() as f64, a.get_muted()))
                .unwrap_or((1.0, false));
            Config {
                shortcuts: Some(shortcuts),
                volume: Some(volume),
                muted: Some(muted),
                resume_mode: Some(resume_mode.get()),
                mouse_single: Some(mouse_single.get()),
                mouse_double: Some(mouse_double.get()),
                mouse_right: Some(mouse_right.get()),
                show_fps: None,
                sub_style: Some(sub_style.borrow().clone()),
                cache_fwd_mib: Some(cache_fwd.get()),
                cache_back_mib: Some(cache_back.get()),
            }
            .save();
        })
    };

    // dynamic models
    let video_model = Rc::new(VecModel::<TrackUi>::default());
    let audio_model = Rc::new(VecModel::<TrackUi>::default());
    let sub_model = Rc::new(VecModel::<TrackUi>::default());
    let recents_model = Rc::new(VecModel::<RecentUi>::default());
    let queue_model = Rc::new(VecModel::<QueueItem>::default());
    app.set_video_tracks(ModelRc::from(video_model.clone()));
    app.set_audio_tracks(ModelRc::from(audio_model.clone()));
    app.set_sub_tracks(ModelRc::from(sub_model.clone()));
    app.set_recents(ModelRc::from(recents_model.clone()));
    app.set_queue(ModelRc::from(queue_model.clone()));

    let refresh_recents: Rc<dyn Fn()> = {
        let store = store.clone();
        let recents_model = recents_model.clone();
        Rc::new(move || {
            let items: Vec<RecentUi> = store
                .borrow()
                .recents
                .iter()
                .map(|r| RecentUi {
                    path: r.path.clone().into(),
                    title: if r.title.is_empty() {
                        basename(&r.path)
                    } else {
                        r.title.clone()
                    }
                    .into(),
                    // GTK format: "Resume pos / dur"; streams without a
                    // position show their URL; finished local files nothing.
                    subtitle: match r.resume_at() {
                        Some(pos) => {
                            format!("Resume {} / {}", fmt_time(pos), fmt_time(r.duration)).into()
                        }
                        None if r.is_stream() => r.path.clone().into(),
                        None => "".into(),
                    },
                    is_stream: r.is_stream(),
                })
                .collect();
            recents_model.set_vec(items);
        })
    };
    refresh_recents();

    // OSD toast helper
    let osd_timer = Rc::new(slint::Timer::default());
    // Auto-dismiss for the resume banner (it's a prompt, not a modal — it goes
    // away on its own like a toast if the user ignores it).
    let resume_timer = Rc::new(slint::Timer::default());
    let show_osd: Rc<dyn Fn(&str)> = {
        let weak = app.as_weak();
        let timer = osd_timer.clone();
        Rc::new(move |msg: &str| {
            if let Some(a) = weak.upgrade() {
                a.set_osd_text(msg.into());
                a.set_osd_shown(true);
            }
            let weak2 = weak.clone();
            timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(1300),
                move || {
                    if let Some(a) = weak2.upgrade() {
                        a.set_osd_shown(false);
                    }
                },
            );
        })
    };

    // load a file/URL and switch to the player view
    let load_target: Rc<dyn Fn(String)> = {
        let bridge = bridge.clone();
        let weak = app.as_weak();
        let last_activity = last_activity.clone();
        let store = store.clone();
        let current_path = current_path.clone();
        let pending_resume = pending_resume.clone();
        let pending_seek = pending_seek.clone();
        let want_autosize = want_autosize.clone();
        let reveal_frames = reveal_frames.clone();
        let media_loaded = media_loaded.clone();
        let refresh_recents = refresh_recents.clone();
        let resume_mode = resume_mode.clone();
        let resume_timer = resume_timer.clone();
        Rc::new(move |target: String| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.load(&target);
            }
            *current_path.borrow_mut() = Some(target.clone());
            last_activity.set(Instant::now());

            let is_url = target.contains("://");
            let mode = resume_mode.get();
            let mut resume_at = 0.0;

            // Resume + recents bookkeeping. Off disables recording entirely;
            // streams are recorded too (keyed by URL).
            if mode != ResumeMode::Off {
                let prev = store.borrow().find(&target).cloned();
                // resume_at() already gates on enough-watched; the store clears
                // the position once a file is finished (>92%), so no extra
                // near-end guard is needed (and an absolute one breaks short clips).
                if let Some(e) = prev.as_ref() {
                    if let Some(p) = e.resume_at() {
                        resume_at = p;
                    }
                }
                let (pos, dur) = prev.map(|e| (e.last_pos, e.duration)).unwrap_or((0.0, 0.0));
                let title = if is_url { target.clone() } else { basename(&target) };
                store.borrow_mut().record(&target, &title, pos, dur);
                store.borrow().save();
                refresh_recents();
            }
            pending_resume.set(resume_at);
            pending_seek.set(0.0);
            want_autosize.set(true);
            reveal_frames.set(0);
            media_loaded.set(true);

            if let Some(a) = weak.upgrade() {
                // Switch to the player view INSTANTLY so clicking a file feels
                // immediate — a loading spinner covers the open/resize/settle until
                // the render loop flips video-ready and the real frame is shown
                // (video-ready gates the Image, so no half-decoded frames leak).
                a.set_has_video(true);
                a.set_video_ready(false);
                a.set_chrome_shown(true);
                a.set_drawer_open(false);
                a.set_url_open(false);
                match (mode, resume_at > 0.0) {
                    // Always: resume silently once the file is open.
                    (ResumeMode::Always, true) => {
                        pending_seek.set(resume_at);
                        a.set_resume_open(false);
                    }
                    // Ask: offer the banner; the seek runs when the user accepts.
                    // Auto-dismisses after 5 s like a toast.
                    (ResumeMode::Ask, true) => {
                        a.set_resume_text(format!("Resume from {}?", fmt_time(resume_at)).into());
                        a.set_resume_open(true);
                        let weak2 = weak.clone();
                        resume_timer.start(
                            slint::TimerMode::SingleShot,
                            Duration::from_secs(5),
                            move || {
                                if let Some(a) = weak2.upgrade() {
                                    a.set_resume_open(false);
                                }
                            },
                        );
                    }
                    _ => a.set_resume_open(false),
                }
            }
        })
    };

    // Route a dropped path: a subtitle drops onto the current video, anything
    // else is played. Shared by winit's DroppedFile (X11/macOS/Windows) and the
    // Wayland data-device listener (wl_dnd).
    let handle_drop: Rc<dyn Fn(String)> = {
        let load = load_target.clone();
        let bridge = bridge.clone();
        let osd = show_osd.clone();
        let weak = app.as_weak();
        Rc::new(move |p: String| {
            let lower = p.to_lowercase();
            let is_sub = SUBTITLE_EXTS.iter().any(|e| lower.ends_with(e));
            let is_vid = VIDEO_EXTS_DND.iter().any(|e| lower.ends_with(e));
            let has_video = weak.upgrade().map(|a| a.get_has_video()).unwrap_or(false);
            if is_sub {
                if has_video {
                    if let Some(b) = bridge.borrow().as_ref() {
                        b.add_subtitle(&p);
                    }
                    osd("Subtitle added");
                } else {
                    osd("Open a video first");
                }
            } else if is_vid {
                load(p);
            } else {
                osd("Unsupported file type");
            }
        })
    };

    // Wayland file drag-and-drop (winit emits DroppedFile only on X11). Lazily
    // attached in the render loop once the window/surface exists; pumped each
    // frame. No-op stub off-Linux.
    let dnd: Rc<RefCell<Option<wl_dnd::DndListener>>> = Rc::new(RefCell::new(None));

    Ctx {
        bridge,
        opaque_region,
        last_activity,
        store,
        current_path,
        pending_resume,
        pending_seek,
        want_autosize,
        resize_anim,
        reveal_frames,
        media_loaded,
        resume_mode,
        mouse_single,
        mouse_double,
        mouse_right,
        sub_style,
        cache_fwd,
        cache_back,
        font_families,
        bindings,
        persist,
        refresh_recents,
        show_osd,
        load_target,
        handle_drop,
        video_model,
        audio_model,
        sub_model,
        queue_model,
        dnd,
        saved_volume,
        saved_muted,
        saved_sub_style,
    }
}

/// Connect every transport / open / tracks / recents / window callback, plus
/// drag-and-drop. Returns the `set_fullscreen` closure (defined here, but also
/// needed by the scripted diagnostics in `main`).
pub fn wire(app: &App, ctx: &Ctx) -> Rc<dyn Fn(bool)> {
    let bridge = ctx.bridge.clone();
    let last_activity = ctx.last_activity.clone();
    let store = ctx.store.clone();
    let current_path = ctx.current_path.clone();
    let pending_resume = ctx.pending_resume.clone();
    let pending_seek = ctx.pending_seek.clone();
    let media_loaded = ctx.media_loaded.clone();
    let resume_mode = ctx.resume_mode.clone();
    let mouse_single = ctx.mouse_single.clone();
    let mouse_double = ctx.mouse_double.clone();
    let mouse_right = ctx.mouse_right.clone();
    let sub_style = ctx.sub_style.clone();
    let cache_fwd = ctx.cache_fwd.clone();
    let cache_back = ctx.cache_back.clone();
    let font_families = ctx.font_families.clone();
    let bindings = ctx.bindings.clone();
    let persist = ctx.persist.clone();
    let refresh_recents = ctx.refresh_recents.clone();
    let show_osd = ctx.show_osd.clone();
    let load_target = ctx.load_target.clone();
    let handle_drop = ctx.handle_drop.clone();

    // ---- transport ----
    {
        let bridge = bridge.clone();
        app.on_toggle_pause(move || {
            if let Some(b) = bridge.borrow().as_ref() {
                b.toggle_pause();
            }
        });
    }
    // Fullscreen must go through this helper: winit/Mutter does not restore a
    // no-frame window's size when leaving fullscreen (it stays huge and the GL
    // surface goes stale — broken corners / white bands), so we remember the
    // pre-fullscreen size ourselves and re-assert it shortly after exiting.
    let set_fullscreen: Rc<dyn Fn(bool)> = {
        let weak = app.as_weak();
        let bridge = bridge.clone();
        let restore_size = Rc::new(Cell::new(None::<slint::PhysicalSize>));
        Rc::new(move |on: bool| {
            let Some(a) = weak.upgrade() else { return };
            if on {
                if !a.window().is_fullscreen() {
                    restore_size.set(Some(a.window().size()));
                }
                a.window().set_fullscreen(true);
            } else {
                a.window().set_fullscreen(false);
                if let Some(s) = restore_size.get() {
                    // Two-step re-assert: winit/Mutter doesn't restore a
                    // no-frame window's size on fullscreen exit, and a single
                    // same-size set can leave the compositor showing a stale
                    // buffer — the 1px step forces fresh surface configures.
                    let weak2 = weak.clone();
                    slint::Timer::single_shot(Duration::from_millis(120), move || {
                        if let Some(a) = weak2.upgrade() {
                            a.window()
                                .set_size(slint::PhysicalSize::new(s.width, s.height + 1));
                        }
                    });
                    let weak3 = weak.clone();
                    let bridge2 = bridge.clone();
                    slint::Timer::single_shot(Duration::from_millis(200), move || {
                        if let Some(a) = weak3.upgrade() {
                            a.window().set_size(s);
                            // Entering fullscreen makes winit mark the Wayland
                            // surface opaque (an optimisation) and exiting does
                            // NOT undo it — the compositor then ignores our
                            // alpha and the rounded corners render square.
                            // Re-assert per-pixel transparency (CSD mode only).
                            if a.get_alpha_surface() {
                                use slint::winit_030::WinitWindowAccessor;
                                a.window().with_winit_window(|w| {
                                    w.set_transparent(true);
                                });
                            }
                        }
                        // While paused mpv produces no new frames, so nudge it
                        // to repaint the current one into the freshly-presented
                        // surface (no-op visually when playing).
                        if let Some(b) = bridge2.borrow().as_ref() {
                            if b.is_paused() {
                                b.seek_seconds(b.position());
                            }
                        }
                    });
                }
            }
        })
    };

    // Configurable mouse-on-video actions (persisted). Index meaning:
    // 0 None · 1 Play/Pause · 2 Toggle controls · 3 Toggle fullscreen · 4 Mute.
    let mouse_action: Rc<dyn Fn(i32)> = {
        let bridge = bridge.clone();
        let weak = app.as_weak();
        let last_activity = last_activity.clone();
        let set_fullscreen = set_fullscreen.clone();
        Rc::new(move |idx: i32| {
            let Some(a) = weak.upgrade() else { return };
            match idx {
                1 => {
                    if let Some(b) = bridge.borrow().as_ref() {
                        b.toggle_pause();
                    }
                }
                2 => {
                    let show = !a.get_chrome_shown();
                    a.set_chrome_shown(show);
                    // park the activity clock so auto-hide doesn't fight it
                    last_activity.set(if show {
                        Instant::now()
                    } else {
                        Instant::now() - Duration::from_secs(60)
                    });
                }
                3 => set_fullscreen(!a.window().is_fullscreen()),
                4 => {
                    if let Some(b) = bridge.borrow().as_ref() {
                        b.toggle_mute();
                    }
                }
                _ => {}
            }
        })
    };
    // Single click runs its action after a short delay so a double click can
    // cancel it (otherwise every double click also fires the single action).
    let click_timer = Rc::new(slint::Timer::default());
    {
        let click_timer = click_timer.clone();
        let mouse_action = mouse_action.clone();
        let mouse_single = mouse_single.clone();
        app.on_video_clicked(move || {
            let mouse_action = mouse_action.clone();
            let idx = mouse_single.get();
            click_timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(230),
                move || mouse_action(idx),
            );
        });
    }
    {
        let click_timer = click_timer.clone();
        let mouse_action = mouse_action.clone();
        let mouse_double = mouse_double.clone();
        app.on_video_double_clicked(move || {
            click_timer.stop(); // cancel the pending single-click action
            mouse_action(mouse_double.get());
        });
    }
    {
        // Right click runs its own configurable action (default Play / Pause) —
        // it replaces the old context menu. No single/double arbitration here.
        let mouse_action = mouse_action.clone();
        let mouse_right = mouse_right.clone();
        app.on_video_right_clicked(move || mouse_action(mouse_right.get()));
    }
    // Live scrub: fast keyframe seeks while dragging (responsive); exact seek on
    // release so it lands on the precise frame.
    {
        let bridge = bridge.clone();
        app.on_seek(move |frac| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.seek_fraction_scrub(frac as f64);
            }
        });
    }
    {
        let bridge = bridge.clone();
        app.on_seek_commit(move |frac| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.seek_fraction(frac as f64);
            }
        });
    }
    {
        let bridge = bridge.clone();
        let la = last_activity.clone();
        app.on_seek_by(move |secs| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.seek_relative(secs as f64);
            }
            la.set(Instant::now());
        });
    }
    {
        let bridge = bridge.clone();
        let osd = show_osd.clone();
        app.on_set_volume(move |frac| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.set_volume(frac as f64);
            }
            osd(&format!("Volume {}%", (frac * 100.0).round() as i64));
        });
    }
    {
        let bridge = bridge.clone();
        let osd = show_osd.clone();
        let weak = app.as_weak();
        app.on_nudge_volume(move |d| {
            if let Some(b) = bridge.borrow().as_ref() {
                let v = (b.volume() + d as f64).clamp(0.0, 1.0);
                b.set_volume(v);
                if let Some(a) = weak.upgrade() {
                    a.set_volume(v as f32);
                }
                osd(&format!("Volume {}%", (v * 100.0).round() as i64));
            }
        });
    }
    {
        let bridge = bridge.clone();
        let osd = show_osd.clone();
        app.on_toggle_mute(move || {
            if let Some(b) = bridge.borrow().as_ref() {
                b.toggle_mute();
                osd(if b.is_muted() { "Muted" } else { "Unmuted" });
            }
        });
    }
    {
        let bridge = bridge.clone();
        let osd = show_osd.clone();
        app.on_prev(move || {
            if let Some(b) = bridge.borrow().as_ref() {
                b.playlist_prev();
            }
            osd("Previous");
        });
    }
    {
        // Play a specific file picked from the queue panel.
        let bridge = bridge.clone();
        app.on_play_queue_index(move |i| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.playlist_play(i as i64);
            }
        });
    }
    {
        let bridge = bridge.clone();
        let osd = show_osd.clone();
        app.on_next(move || {
            if let Some(b) = bridge.borrow().as_ref() {
                b.playlist_next();
            }
            osd("Next");
        });
    }
    {
        // Stop = GTK behaviour: flush the watch position, tear down playback
        // and return to the home screen with a freshly-refreshed recents list
        // (so the just-stopped file immediately shows "Resume at …").
        let bridge = bridge.clone();
        let store = store.clone();
        let current_path = current_path.clone();
        let media_loaded = media_loaded.clone();
        let refresh_recents = refresh_recents.clone();
        let resume_mode = resume_mode.clone();
        let weak = app.as_weak();
        app.on_stop(move || {
            if resume_mode.get() != ResumeMode::Off {
                if let (Some(b), Some(path)) =
                    (bridge.borrow().as_ref(), current_path.borrow().clone())
                {
                    let dur = b.duration();
                    if dur > 0.0 {
                        let mt = b.media_title();
                        let title = if !mt.is_empty() {
                            mt
                        } else if path.contains("://") {
                            path.clone()
                        } else {
                            basename(&path)
                        };
                        store.borrow_mut().record(&path, &title, b.position(), dur);
                        store.borrow().save();
                    }
                }
            }
            if let Some(b) = bridge.borrow().as_ref() {
                b.stop();
            }
            *current_path.borrow_mut() = None;
            media_loaded.set(false);
            refresh_recents();
            if let Some(a) = weak.upgrade() {
                a.set_has_video(false);
                a.set_video_ready(false);
                a.set_title_text("".into());
                a.set_resume_open(false);
                // Re-show the chrome: it may have auto-hidden during playback,
                // and the home screen's top bar (window controls) follows it.
                a.set_chrome_shown(true);
                // The window may have auto-resized to a portrait/narrow shape for
                // a vertical (reel) video; the home is a landscape layout, so
                // restore a comfortable landscape size when one of those is left.
                if !a.window().is_fullscreen() && !a.window().is_maximized() {
                    let s = a.window().size();
                    let sf = a.window().scale_factor().max(0.1);
                    let (w, h) = (s.width as f32 / sf, s.height as f32 / sf);
                    if w < 860.0 || h > w {
                        a.window().set_size(slint::LogicalSize::new(980.0, 600.0));
                    }
                }
            }
        });
    }

    // A file handed in by another launch (single-instance handoff): load it in
    // this window and try to raise/focus it. The bridge is normally ready (the
    // primary has been running), but guard for the just-launched race with a
    // short retry, mirroring launch_cli.
    {
        let load = load_target.clone();
        let bridge = bridge.clone();
        let weak = app.as_weak();
        app.on_external_open(move |path| {
            let path = path.to_string();
            if let Some(a) = weak.upgrade() {
                use slint::winit_030::WinitWindowAccessor;
                a.window().with_winit_window(|w| {
                    w.focus_window();
                });
            }
            if bridge.borrow().is_some() {
                load(path);
            } else {
                let load = load.clone();
                let bridge = bridge.clone();
                let once = Rc::new(Cell::new(false));
                let t = slint::Timer::default();
                t.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
                    if once.get() {
                        return;
                    }
                    if bridge.borrow().is_some() {
                        once.set(true);
                        load(path.clone());
                    }
                });
                std::mem::forget(t);
            }
        });
    }

    // ---- open file / url ----
    {
        let load = load_target.clone();
        app.on_open_file(move || {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    "Video",
                    &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "flv", "wmv", "mpg", "mpeg"],
                )
                .add_filter("All files", &["*"])
                .pick_file()
            {
                load(path.to_string_lossy().to_string());
            }
        });
    }
    {
        let weak = app.as_weak();
        app.on_open_url(move || {
            if let Some(a) = weak.upgrade() {
                a.set_url_open(true);
            }
        });
    }
    {
        // The Open-URL dialog's pending links are a Rust-managed model so the UI
        // can virtualize them (a single big TextInput lagged on every keystroke).
        let url_model = Rc::new(VecModel::<slint::SharedString>::default());
        app.set_url_list(ModelRc::from(url_model.clone()));
        {
            let url_model = url_model.clone();
            let weak = app.as_weak();
            app.on_url_add(move |s| {
                let mut added = 0;
                let mut had_invalid = false;
                for line in s.lines() {
                    let l = line.trim();
                    if l.is_empty() {
                        continue;
                    }
                    // Reject garbage (e.g. "sss") right here, so the list only ever
                    // holds real targets — no silently-broken rows to discover later.
                    if !crate::util::looks_playable(l) {
                        had_invalid = true;
                        continue;
                    }
                    let dup = (0..url_model.row_count()).any(|i| url_model.row_data(i).as_deref() == Some(l));
                    if !dup {
                        url_model.push(l.into());
                        added += 1;
                    }
                }
                // Flash the inline hint only when the user added text that wasn't a
                // valid link AND nothing landed — a duplicate (already in the list)
                // isn't an error, so it must not light up the red "not a link" hint.
                if let Some(a) = weak.upgrade() {
                    a.set_url_rejected(had_invalid && added == 0);
                }
            });
        }
        {
            let url_model = url_model.clone();
            app.on_url_remove(move |i| {
                let i = i as usize;
                if i < url_model.row_count() {
                    url_model.remove(i);
                }
            });
        }
        {
            let url_model = url_model.clone();
            app.on_url_clear(move || url_model.set_vec(Vec::new()));
        }
        {
            // Open: fold optional username/password into each link, play the first
            // and queue the rest, then clear the list and close the dialog.
            let load = load_target.clone();
            let bridge = bridge.clone();
            let url_model = url_model.clone();
            let weak = app.as_weak();
            let osd = show_osd.clone();
            app.on_submit_urls(move |user, pass| {
                let urls: Vec<String> = (0..url_model.row_count())
                    .map(|i| url_model.row_data(i).unwrap_or_default())
                    .map(|u| crate::util::apply_credentials(u.trim(), &user, &pass))
                    .filter(|u| !u.is_empty())
                    .collect();
                // Reject garbage (e.g. "SS") BEFORE switching to the player so the
                // user doesn't land on a stuck spinner + a "couldn't play" toast.
                // Keep the dialog open so they can fix the entry.
                let valid: Vec<String> =
                    urls.into_iter().filter(|u| crate::util::looks_playable(u)).collect();
                if valid.is_empty() {
                    osd("The link you entered is invalid. Please enter a valid URL.");
                    return;
                }
                url_model.set_vec(Vec::new());
                if let Some(a) = weak.upgrade() {
                    a.set_url_open(false);
                }
                let Some((first, rest)) = valid.split_first() else {
                    return;
                };
                load(first.clone());
                if let Some(b) = bridge.borrow().as_ref() {
                    for u in rest {
                        b.playlist_append(u);
                    }
                }
            });
        }
    }
    {
        // Read the OS clipboard (Slint's clipboard is dead on this winit backend)
        // and hand it back to the field's Ctrl+V handler.
        app.on_read_clipboard(move || crate::util::read_clipboard().unwrap_or_default().into());
    }
    {
        // Paste into the URL list — but read the clipboard OFF the UI thread.
        // Shelling out to wl-paste/xclip synchronously from the Ctrl+V handler
        // froze the event loop for the duration of the spawn, which desynced the
        // Ctrl modifier from the "v" key (the field then typed a literal "V") and
        // stalled on an image clipboard. Read on a worker thread, then hop back to
        // the event loop to add the links through the normal (validating) path.
        let weak = app.as_weak();
        let dnd = ctx.dnd.clone();
        app.on_request_paste(move || {
            // On Wayland, read the clipboard NATIVELY over wl_data_device (no
            // subprocess → nothing flashes in the dock, and an image clipboard is
            // a true no-op because we only transfer when a text mime is present).
            // It's a sub-ms local-pipe read, so running it on the UI thread is fine.
            if let Some(d) = dnd.borrow_mut().as_mut() {
                if let Some(text) = d.read_clipboard_text() {
                    if let Some(a) = weak.upgrade() {
                        a.invoke_url_add(text.into());
                    }
                    return;
                }
                // No text read. If we DO hold the selection (it's just image-only),
                // stop here — don't fall back to a subprocess (that's the dock
                // flash). Only fall through if we never saw a Selection event.
                if d.has_selection() {
                    return;
                }
            }
            // Off-Wayland (X11/macOS/Windows): no native listener — shell out, but
            // off the UI thread so the spawn can't desync the keyboard.
            let weak = weak.clone();
            std::thread::spawn(move || {
                let text = crate::util::read_clipboard().unwrap_or_default();
                if text.is_empty() {
                    return;
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(a) = weak.upgrade() {
                        a.invoke_url_add(text.into());
                    }
                });
            });
        });
    }
    {
        app.on_write_clipboard(move |t| crate::util::write_clipboard(&t));
    }

    // ---- tracks / subtitles / speed ----
    {
        let bridge = bridge.clone();
        let osd = show_osd.clone();
        app.on_select_audio(move |id| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.set_audio(id as i64);
            }
            osd("Audio track changed");
        });
    }
    {
        let bridge = bridge.clone();
        let osd = show_osd.clone();
        app.on_select_sub(move |id| {
            if let Some(b) = bridge.borrow().as_ref() {
                if id < 0 {
                    b.disable_sub();
                    osd("Subtitles off");
                } else {
                    b.set_sub(id as i64);
                    osd("Subtitles on");
                }
            }
        });
    }
    {
        let bridge = bridge.clone();
        app.on_add_subtitle(move || {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Subtitles", &["srt", "ass", "ssa", "vtt", "sub"])
                .pick_file()
            {
                if let Some(b) = bridge.borrow().as_ref() {
                    b.add_subtitle(&path.to_string_lossy());
                }
            }
        });
    }
    const EQ_PRESETS: [[f64; 10]; 6] = [
        [0.0; 10],                                              // Flat
        [4.0, 3.0, 1.5, -1.0, -1.0, 0.5, 2.0, 3.0, 3.5, 4.0],  // Rock
        [3.0, 2.0, 1.0, 1.5, -1.0, -1.0, 0.0, 1.0, 2.0, 3.0],  // Jazz
        [-1.0, 0.5, 2.0, 3.0, 3.5, 2.5, 1.0, 0.0, -0.5, -1.0], // Pop
        [6.0, 5.0, 4.0, 2.5, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],    // Bass Boost
        [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.5, 4.0, 5.0, 6.0],    // Treble Boost
    ];
    let eq_bands = Rc::new(Cell::new([0.0f64; 10]));

    let aspect_str = |i: i32| -> &'static str {
        ["-1", "4:3", "16:9", "16:10", "21:9", "5:4"][i.clamp(0, 5) as usize]
    };
    let crop_wh = |i: i32| -> (i64, i64) {
        [(0, 0), (4, 3), (16, 9), (16, 10), (21, 9), (5, 4)][i.clamp(0, 5) as usize]
    };

    {
        let bridge = bridge.clone();
        let sub_style = sub_style.clone();
        let persist = persist.clone();
        app.on_adjust(move |key, val| {
            if let Some(b) = bridge.borrow().as_ref() {
                apply_adjust(b, key.as_str(), val);
            }
            // Only subtitle scale/position are global persisted style; video
            // adjustments (brightness, speed, …) reset per-video, so don't save.
            match key.as_str() {
                "sub-scale" => {
                    sub_style.borrow_mut().scale = val as f64;
                    persist();
                }
                "sub-pos" => {
                    sub_style.borrow_mut().pos = val as f64;
                    persist();
                }
                _ => {}
            }
        });
    }
    {
        let bridge = bridge.clone();
        app.on_select_video(move |id| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.set_video_track(id as i64);
            }
        });
    }
    {
        let bridge = bridge.clone();
        app.on_set_aspect(move |i| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.set_prop_str("video-aspect-override", aspect_str(i));
            }
        });
    }
    {
        let bridge = bridge.clone();
        app.on_set_crop(move |i| {
            if let Some(b) = bridge.borrow().as_ref() {
                let (w, h) = crop_wh(i);
                b.set_crop_aspect(w, h);
            }
        });
    }
    {
        let bridge = bridge.clone();
        app.on_set_rotate(move |i| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.set_prop_i64("video-rotate", [0, 90, 180, 270][i.clamp(0, 3) as usize]);
            }
        });
    }
    {
        let bridge = bridge.clone();
        app.on_set_hwdec(move |on| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.set_hwdec(on);
            }
        });
    }
    {
        let bridge = bridge.clone();
        app.on_set_deint(move |on| {
            if let Some(b) = bridge.borrow().as_ref() {
                b.set_deinterlace(on);
            }
        });
    }
    {
        let bridge = bridge.clone();
        let eq_bands = eq_bands.clone();
        app.on_set_eq_band(move |i, v| {
            let mut bands = eq_bands.get();
            if (0..10).contains(&i) {
                bands[i as usize] = v as f64;
            }
            eq_bands.set(bands);
            if let Some(b) = bridge.borrow().as_ref() {
                b.set_equalizer(&bands);
            }
        });
    }
    {
        let bridge = bridge.clone();
        let eq_bands = eq_bands.clone();
        app.on_set_eq_preset(move |idx| {
            if (0..6).contains(&idx) {
                let bands = EQ_PRESETS[idx as usize];
                eq_bands.set(bands);
                if let Some(b) = bridge.borrow().as_ref() {
                    b.set_equalizer(&bands);
                }
            }
        });
    }
    {
        let bridge = bridge.clone();
        let sub_style = sub_style.clone();
        let persist = persist.clone();
        app.on_set_shaded_bg(move |on| {
            sub_style.borrow_mut().shaded_bg = on;
            if let Some(b) = bridge.borrow().as_ref() {
                apply_sub_style(b, &sub_style.borrow());
            }
            persist();
        });
    }

    // ---- keyboard: data-driven bindings ----
    let (shortcuts_model, rebuild_shortcuts) = shortcuts::install(
        &app,
        bridge.clone(),
        bindings.clone(),
        set_fullscreen.clone(),
        show_osd.clone(),
    );

    prefs::install(&app, prefs::PrefsDeps {
        bridge: bridge.clone(),
        bindings: bindings.clone(),
        shortcuts_model: shortcuts_model.clone(),
        rebuild_shortcuts: rebuild_shortcuts.clone(),
        persist: persist.clone(),
        resume_mode: resume_mode.clone(),
        mouse_single: mouse_single.clone(),
        mouse_double: mouse_double.clone(),
        mouse_right: mouse_right.clone(),
        sub_style: sub_style.clone(),
        cache_fwd: cache_fwd.clone(),
        cache_back: cache_back.clone(),
        font_families: font_families.clone(),
    });

    // ---- recents / resume ----
    {
        let load = load_target.clone();
        app.on_play_recent(move |p| {
            load(p.to_string());
        });
    }
    {
        let store = store.clone();
        let refresh_recents = refresh_recents.clone();
        app.on_clear_recents(move || {
            store.borrow_mut().clear();
            store.borrow().save();
            refresh_recents();
        });
    }
    {
        let store = store.clone();
        let refresh_recents = refresh_recents.clone();
        app.on_remove_recent(move |p| {
            store.borrow_mut().forget(&p);
            store.borrow().save();
            refresh_recents();
        });
    }
    {
        let weak = app.as_weak();
        let pending_resume = pending_resume.clone();
        let pending_seek = pending_seek.clone();
        let resume_mode = resume_mode.clone();
        let persist = persist.clone();
        app.on_resume_accept(move |always| {
            // The user opted in via the banner checkbox: remember and
            // auto-resume from now on (mirrors the GTK behaviour).
            if always {
                resume_mode.set(ResumeMode::Always);
                persist();
            }
            // Defer the seek until the file reports a duration (see render poll).
            pending_seek.set(pending_resume.get());
            if let Some(a) = weak.upgrade() {
                a.set_resume_open(false);
            }
        });
    }
    {
        let weak = app.as_weak();
        app.on_resume_dismiss(move || {
            if let Some(a) = weak.upgrade() {
                a.set_resume_open(false);
            }
        });
    }

    // ---- window controls + escape ----
    // Frameless (CSD) window: move/resize are driven by the app chrome and
    // forwarded to the compositor through winit.
    //
    // The compositor takes over the pointer for the whole move/resize, so the
    // matching button-release never reaches Slint — the initiating TouchArea
    // would hold the pointer grab forever (stuck resize cursor, clicks
    // restarting the resize, hover-reveal dead). Synthesise a release + exit
    // right after handing off so Slint's input state is clean.
    fn unstick_pointer(window: &slint::Window) {
        use slint::platform::{PointerEventButton, WindowEvent};
        // Far outside the window: a release inside would register as a click on
        // whatever initiated the drag (e.g. toggling the chrome after a
        // drag-from-video).
        let _ = window.try_dispatch_event(WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(-100000.0, -100000.0),
            button: PointerEventButton::Left,
        });
        let _ = window.try_dispatch_event(WindowEvent::PointerExited);
    }
    {
        use slint::winit_030::WinitWindowAccessor;
        let weak = app.as_weak();
        app.on_start_move(move || {
            // Defer out of the input-processing stack: synthetic events
            // dispatched re-entrantly from inside a pointer callback are
            // dropped, leaving the grab stuck.
            let weak = weak.clone();
            slint::Timer::single_shot(Duration::from_millis(1), move || {
                let Some(a) = weak.upgrade() else { return };
                a.window().with_winit_window(|w| {
                    let _ = w.drag_window();
                });
                unstick_pointer(a.window());
                let weak2 = weak.clone();
                slint::Timer::single_shot(Duration::from_millis(200), move || {
                    if let Some(a) = weak2.upgrade() {
                        unstick_pointer(a.window());
                    }
                });
            });
        });
    }
    {
        use slint::winit_030::{WinitWindowAccessor, winit::window::ResizeDirection};
        let weak = app.as_weak();
        app.on_start_resize(move |dir| {
            let d = match dir.as_str() {
                "n" => ResizeDirection::North,
                "s" => ResizeDirection::South,
                "e" => ResizeDirection::East,
                "w" => ResizeDirection::West,
                "ne" => ResizeDirection::NorthEast,
                "nw" => ResizeDirection::NorthWest,
                "se" => ResizeDirection::SouthEast,
                _ => ResizeDirection::SouthWest,
            };
            let weak = weak.clone();
            slint::Timer::single_shot(Duration::from_millis(1), move || {
                let Some(a) = weak.upgrade() else { return };
                a.window().with_winit_window(|w| {
                    let _ = w.drag_resize_window(d);
                });
                unstick_pointer(a.window());
                let weak2 = weak.clone();
                slint::Timer::single_shot(Duration::from_millis(200), move || {
                    if let Some(a) = weak2.upgrade() {
                        unstick_pointer(a.window());
                    }
                });
            });
        });
    }
    {
        // Enforce the home/video min window size on the winit window directly
        // (Slint's min-width doesn't reach the compositor on this frameless
        // window). The home is landscape and needs room; a portrait video may go
        // narrow. Fires whenever has-video flips, plus once after the window is up.
        let weak = app.as_weak();
        let apply: Rc<dyn Fn()> = Rc::new(move || {
            let Some(a) = weak.upgrade() else { return };
            use slint::winit_030::winit::dpi::LogicalSize;
            use slint::winit_030::WinitWindowAccessor;
            let (w, h) = if a.get_has_video() { (426.0, 240.0) } else { (880.0, 540.0) };
            a.window().with_winit_window(|win| {
                win.set_min_inner_size(Some(LogicalSize::new(w, h)));
            });
        });
        {
            let apply = apply.clone();
            // the winit window isn't realized synchronously; defer the first call
            slint::Timer::single_shot(std::time::Duration::from_millis(150), move || apply());
        }
        app.on_apply_min_size(move || apply());
    }
    {
        let weak = app.as_weak();
        app.on_minimize(move || {
            if let Some(a) = weak.upgrade() {
                a.window().set_minimized(true);
            }
        });
    }
    {
        let weak = app.as_weak();
        let maximized = Cell::new(false);
        app.on_maximize(move || {
            if let Some(a) = weak.upgrade() {
                let m = !maximized.get();
                maximized.set(m);
                a.window().set_maximized(m);
            }
        });
    }
    {
        let weak = app.as_weak();
        let set_fullscreen = set_fullscreen.clone();
        app.on_fullscreen(move || {
            if let Some(a) = weak.upgrade() {
                set_fullscreen(!a.window().is_fullscreen());
            }
        });
    }
    {
        let persist = persist.clone();
        app.on_close_window(move || {
            persist();
            let _ = slint::quit_event_loop();
        });
    }
    {
        let weak = app.as_weak();
        let set_fullscreen = set_fullscreen.clone();
        app.on_escape_pressed(move || {
            if let Some(a) = weak.upgrade() {
                if a.get_drawer_open() {
                    a.set_drawer_open(false);
                } else if a.get_url_open() {
                    a.set_url_open(false);
                } else if a.window().is_fullscreen() {
                    set_fullscreen(false);
                }
            }
        });
    }
    {
        let weak = app.as_weak();
        let la = last_activity.clone();
        app.on_pointer_activity(move || {
            la.set(Instant::now());
            if let Some(a) = weak.upgrade() {
                a.set_chrome_shown(true);
                // Reveal the cursor immediately — the 250ms housekeeping tick
                // would otherwise leave it hidden for a beat after the mouse moves.
                use slint::winit_030::WinitWindowAccessor;
                a.window().with_winit_window(|w| w.set_cursor_visible(true));
            }
        });
    }

    // ---- drag-and-drop (X11 / macOS / Windows): winit delivers DroppedFile.
    // On Wayland this never fires — wl_dnd handles it instead. ----
    {
        use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
        let handle_drop = handle_drop.clone();
        app.window().on_winit_window_event(move |_win, ev| {
            if let WindowEvent::DroppedFile(path) = ev {
                handle_drop(path.to_string_lossy().to_string());
            }
            EventResult::Propagate
        });
    }

    set_fullscreen
}

/// Background 250ms timer: auto-hide chrome, re-assert the CSD opaque region on
/// resize, refresh the drawer's track lists, and persist playback position.
pub fn housekeeping(app: &App, ctx: &Ctx) {
    let bridge = ctx.bridge.clone();
    let store = ctx.store.clone();
    let current_path = ctx.current_path.clone();
    let last_activity = ctx.last_activity.clone();
    let video_model = ctx.video_model.clone();
    let audio_model = ctx.audio_model.clone();
    let sub_model = ctx.sub_model.clone();
    let queue_model = ctx.queue_model.clone();
    let resume_mode = ctx.resume_mode.clone();
    let opaque_region = ctx.opaque_region.clone();
    // housekeeping: auto-hide chrome, refresh track lists, persist position
    let weak = app.as_weak();
    let house = slint::Timer::default();
    let tick = Cell::new(0u32);
    let track_sig = RefCell::new(String::new());
    let queue_sig = RefCell::new(String::new());
    let last_size = Cell::new((0u32, 0u32));
    // Mirror the chrome's visibility onto the OS cursor: when the bars fade out
    // during playback the pointer must vanish with them (it floats over the
    // video otherwise). Starts visible; deduped so we only poke winit on change.
    let cursor_shown = Cell::new(true);
    {
        let bridge = bridge.clone();
        let store = store.clone();
        let current_path = current_path.clone();
        let last_activity = last_activity.clone();
        let queue_model = queue_model.clone();
        let video_model = video_model.clone();
        let audio_model = audio_model.clone();
        let sub_model = sub_model.clone();
        let resume_mode = resume_mode.clone();
        let opaque_hk = opaque_region.clone();
        house.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(250),
            move || {
                let Some(a) = weak.upgrade() else { return };

                // Load external subtitles for the current file (RTL/encoding
                // aware) — on this timer rather than the render loop so it runs
                // even when the compositor throttles rendering.
                if let Some(b) = bridge.borrow().as_ref() {
                    b.poll_subs();
                }

                // square the CSD corners (and hide the resize handles) while
                // fullscreen or maximized
                let square_now = a.window().is_fullscreen() || a.window().is_maximized();
                // Whenever the window's size or square-state changed
                // (fullscreen, maximize, interactive resizes — they all
                // reconfigure the Wayland surface): re-assert per-pixel
                // transparency (winit re-marks the surface opaque and never
                // undoes it) and then install the GTK-style opaque region —
                // body opaque for the compositor's fast path, only the corner
                // squares alpha-blended.
                let s_now = a.window().size();
                if a.get_alpha_surface()
                    && ((a.get_win_square() != square_now)
                        || (s_now.width, s_now.height) != last_size.get())
                {
                    last_size.set((s_now.width, s_now.height));
                    use slint::winit_030::WinitWindowAccessor;
                    a.window().with_winit_window(|w| {
                        w.set_transparent(true);
                    });
                    if let Some(reg) = opaque_hk.borrow().as_ref() {
                        // Cut-outs larger than the 12px radius — see the
                        // attach-time comment (bright-scene corner overflow).
                        let sf = a.window().scale_factor();
                        reg.set(
                            (s_now.width as f32 / sf).ceil() as i32,
                            (s_now.height as f32 / sf).ceil() as i32,
                            if square_now { 0 } else { 15 },
                        );
                    }
                }
                a.set_win_square(square_now);


                // auto-hide chrome — but never while the pointer is resting over
                // the top bar or floating controls (the user is reaching for
                // them; a still cursor fires no motion events, so without this
                // the bar would vanish under their hand).
                if a.get_has_video()
                    && !a.get_paused()
                    && !a.get_drawer_open()
                    && !a.get_url_open()
                    && !a.get_chrome_hovered()
                    && last_activity.get().elapsed() > Duration::from_millis(2500)
                {
                    a.set_chrome_shown(false);
                }

                // Keep the OS cursor in lock-step with the chrome. Reveal is
                // also done instantly in on_pointer_activity so the cursor
                // doesn't lag a tick behind the mouse; this catches the hide
                // and any non-motion show (e.g. pause re-revealing the bars).
                let want_cursor = a.get_chrome_shown();
                if cursor_shown.get() != want_cursor {
                    cursor_shown.set(want_cursor);
                    use slint::winit_030::WinitWindowAccessor;
                    a.window().with_winit_window(|w| w.set_cursor_visible(want_cursor));
                }

                let Some(b) = bridge.borrow().as_ref().map(|_| ()) else {
                    return;
                };
                let _ = b;

                // refresh track lists when the drawer is visible (and changed)
                if a.get_drawer_open() {
                    if let Some(b) = bridge.borrow().as_ref() {
                        let tracks = b.tracks();
                        let sig = tracks
                            .iter()
                            .map(|t| format!("{}{}{}", t.kind, t.id, t.selected))
                            .collect::<String>();
                        if *track_sig.borrow() != sig {
                            *track_sig.borrow_mut() = sig;
                            let by_kind = |kind: &str| -> Vec<TrackUi> {
                                tracks
                                    .iter()
                                    .filter(|t| t.kind == kind)
                                    .map(|t| TrackUi {
                                        id: t.id as i32,
                                        label: track_label(t).into(),
                                        selected: t.selected,
                                    })
                                    .collect()
                            };
                            video_model.set_vec(by_kind("video"));
                            audio_model.set_vec(by_kind("audio"));
                            sub_model.set_vec(by_kind("sub"));
                        }
                    }
                }

                // refresh the playback queue when its panel is open (and changed)
                if a.get_queue_open() {
                    if let Some(b) = bridge.borrow().as_ref() {
                        let pl = b.playlist();
                        let cur = pl.iter().find(|e| e.current).map(|e| e.path.as_str()).unwrap_or("");
                        let sig = format!("{}|{cur}", pl.len());
                        if *queue_sig.borrow() != sig {
                            *queue_sig.borrow_mut() = sig;
                            let items: Vec<QueueItem> = pl
                                .iter()
                                .map(|e| QueueItem {
                                    title: basename(&e.path).into(),
                                    current: e.current,
                                })
                                .collect();
                            queue_model.set_vec(items);
                        }
                    }
                }

                // Playlist navigation (next/prev or a queue pick) switches files
                // WITHOUT going through load_target, leaving current_path stale and
                // the new file unrecorded. Detect mpv's current file, adopt it, and
                // record it in recents so it survives navigating + closing.
                if a.get_has_video() {
                    if let Some(b) = bridge.borrow().as_ref() {
                        if let Some(p) = b.path() {
                            let changed = current_path.borrow().as_deref() != Some(p.as_str());
                            if changed && b.duration() > 0.0 {
                                *current_path.borrow_mut() = Some(p.clone());
                                if resume_mode.get() != ResumeMode::Off {
                                    let (pos, dur) = {
                                        let s = store.borrow();
                                        s.find(&p)
                                            .map(|e| (e.last_pos, e.duration))
                                            .unwrap_or((0.0, b.duration()))
                                    };
                                    let mt = b.media_title();
                                    let title = if mt.is_empty() { basename(&p) } else { mt };
                                    store.borrow_mut().record(&p, &title, pos, dur);
                                    store.borrow().save();
                                }
                            }
                        }
                    }
                }

                // persist playback position ~every 3s (files and streams), unless
                // resume recording is turned off.
                let t = tick.get().wrapping_add(1);
                tick.set(t);
                if t % 12 == 0 && a.get_has_video() && resume_mode.get() != ResumeMode::Off {
                    if let (Some(b), Some(path)) =
                        (bridge.borrow().as_ref(), current_path.borrow().clone())
                    {
                        let pos = b.position();
                        let dur = b.duration();
                        if dur > 0.0 {
                            let is_url = path.contains("://");
                            let mt = b.media_title();
                            let title = if !mt.is_empty() {
                                mt
                            } else if is_url {
                                path.clone()
                            } else {
                                basename(&path)
                            };
                            store.borrow_mut().record(&path, &title, pos, dur);
                            store.borrow().save();
                        }
                    }
                }
            },
        );
    }
    std::mem::forget(house);
}

/// If a file/URL was passed on the CLI, poll until the video bridge exists then
/// load it (the bridge is created lazily in the first render frame).
pub fn launch_cli(ctx: &Ctx, cli_file: Option<String>) {
    let load_target = ctx.load_target.clone();
    let bridge = ctx.bridge.clone();
    if let Some(f) = cli_file {
        let load = load_target.clone();
        let bridge = bridge.clone();
        let t = slint::Timer::default();
        let once = Rc::new(Cell::new(false));
        t.start(slint::TimerMode::Repeated, Duration::from_millis(50), {
            move || {
                if once.get() {
                    return;
                }
                if bridge.borrow().is_some() {
                    once.set(true);
                    load(f.clone());
                }
            }
        });
        std::mem::forget(t);
    }
}
