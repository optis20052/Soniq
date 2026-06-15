//! The mpv → GL → Slint rendering bridge: the rendering notifier that drives
//! mpv's OpenGL render API each frame, plus the redraw heartbeat. This is the
//! hottest, most delicate code in the app (GL state hygiene, HDR, the rounded
//! CSD corner punch, the load-reveal and window-autosize animation), so it
//! lives in its own file. Behaviour is unchanged from when it was inline in
//! `main` — the captured state is passed in via `RenderDeps`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use crate::config::SubStyle;
use crate::subs::apply_sub_style;
use crate::util::{basename, fit_window_size, fmt_time};
use crate::video::VideoBridge;
use crate::{wl_dnd, wl_opaque, App};

/// State the render loop borrows from `main` (all the shared Rc handles plus the
/// saved settings applied to the freshly-created `VideoBridge`).
pub struct RenderDeps {
    pub bridge: Rc<RefCell<Option<VideoBridge>>>,
    pub opaque_region: Rc<RefCell<Option<wl_opaque::OpaqueRegion>>>,
    pub current_path: Rc<RefCell<Option<String>>>,
    pub pending_seek: Rc<Cell<f64>>,
    pub want_autosize: Rc<Cell<bool>>,
    pub resize_anim: Rc<Cell<Option<(f32, f32, f32, f32, Instant)>>>,
    pub reveal_frames: Rc<Cell<u32>>,
    pub media_loaded: Rc<Cell<bool>>,
    pub show_osd: Rc<dyn Fn(&str)>,
    pub dnd: Rc<RefCell<Option<wl_dnd::DndListener>>>,
    pub handle_drop: Rc<dyn Fn(String)>,
    pub saved_volume: f64,
    pub saved_muted: bool,
    pub saved_sub_style: SubStyle,
}

/// Install the rendering notifier + redraw heartbeat on the app window.
pub fn install(app: &App, deps: RenderDeps) {
    let RenderDeps {
        bridge,
        opaque_region,
        current_path,
        pending_seek,
        want_autosize,
        resize_anim,
        reveal_frames,
        media_loaded,
        show_osd,
        dnd,
        handle_drop,
        saved_volume,
        saved_muted,
        saved_sub_style,
    } = deps;

    let weak = app.as_weak();
    let bridge_rn = bridge.clone();
    let opaque_rn = opaque_region.clone();
    let current_rn = current_path.clone();
    let pending_seek_rn = pending_seek.clone();
    let want_autosize_rn = want_autosize.clone();
    let resize_anim_rn = resize_anim.clone();
    let reveal_frames_rn = reveal_frames.clone();
    let media_loaded_rn = media_loaded.clone();
    let osd_rn = show_osd.clone();
    let dnd_rn = dnd.clone();
    let handle_drop_rn = handle_drop.clone();
    // Previous frame's FBO size, for the "is the render target settled?" check.
    let last_rs = std::cell::Cell::new((0u32, 0u32));
    app.window()
        .set_rendering_notifier(move |state, graphics_api| match state {
            slint::RenderingState::RenderingSetup => {
                if let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api {
                    let gl = unsafe {
                        glow::Context::from_loader_function_cstr(|s| get_proc_address(s) as *const _)
                    };
                    if std::env::var("SONIQ_GLINFO").is_ok() {
                        use glow::HasContext;
                        let a = unsafe {
                            gl.get_framebuffer_attachment_parameter_i32(
                                glow::FRAMEBUFFER,
                                glow::BACK_LEFT,
                                glow::FRAMEBUFFER_ATTACHMENT_ALPHA_SIZE,
                            )
                        };
                        eprintln!("[gl] back-buffer alpha bits: {a}");
                    }
                    let app = weak.upgrade().unwrap();
                    let s = app.window().size();
                    let b = VideoBridge::new(
                        gl,
                        *get_proc_address,
                        (s.width.max(16), s.height.max(16)),
                    );
    // Restore persisted settings onto the fresh mpv instance.
                    b.set_volume(saved_volume);
                    if saved_muted {
                        b.toggle_mute();
                    }
                    apply_sub_style(&b, &saved_sub_style);
                    *bridge_rn.borrow_mut() = Some(b);
                    // GTK-style opaque region: body opaque, corners blended.
                    if opaque_rn.borrow().is_none() {
                        *opaque_rn.borrow_mut() = wl_opaque::OpaqueRegion::attach(app.window());
                        if let Some(reg) = opaque_rn.borrow().as_ref() {
                            // Corner cut-outs LARGER than the 12px radius: any
                            // punched-arc pixel left inside the opaque region
                            // shows the movie's RGB instead of the desktop
                            // (visible as bright-scene overflow at the corners).
                            // The extra margin is alpha-1 body — blends
                            // identically.
                            let sf = app.window().scale_factor();
                            reg.set(
                                (s.width as f32 / sf).ceil() as i32,
                                (s.height as f32 / sf).ceil() as i32,
                                15,
                            );
                        }
                    }
                    // Wayland file drag-and-drop (winit only does X11 DnD).
                    if dnd_rn.borrow().is_none() {
                        *dnd_rn.borrow_mut() = wl_dnd::DndListener::attach(app.window());
                    }
                }
            }
            slint::RenderingState::BeforeRendering => {
                // Deliver any files dropped onto the window (Wayland path). Defer
                // the actual load to the next event-loop turn: handling it inline
                // would mutate UI state / resize the window from *inside* the Skia
                // render callback, re-entering the renderer's borrowed RefCell and
                // aborting ("RefCell already mutably borrowed" on drop).
                let dropped: Vec<String> = match dnd_rn.borrow_mut().as_mut() {
                    Some(d) => d.pump(),
                    None => Vec::new(),
                };
                for path in dropped {
                    let handle = handle_drop_rn.clone();
                    slint::Timer::single_shot(Duration::from_millis(0), move || handle(path));
                }
                if std::env::var("SONIQ_FPS").is_ok() {
                    use std::cell::Cell;
                    thread_local!(static LAST: Cell<Option<Instant>> = const { Cell::new(None) });
                    thread_local!(static ACC: Cell<(f64, u32)> = const { Cell::new((0.0, 0)) });
                    LAST.with(|l| {
                        if let Some(prev) = l.get() {
                            let dt = prev.elapsed().as_secs_f64() * 1000.0;
                            ACC.with(|a| {
                                let (sum, n) = a.get();
                                let (sum, n) = (sum + dt, n + 1);
                                if n >= 100 {
                                    eprintln!("[fps] avg frame interval {:.2}ms ({:.0} fps)", sum / n as f64, 1000.0 / (sum / n as f64));
                                    a.set((0.0, 0));
                                } else {
                                    a.set((sum, n));
                                }
                            });
                        }
                        l.set(Some(Instant::now()));
                    });
                }
                if let (Some(b), Some(app)) = (bridge_rn.borrow_mut().as_mut(), weak.upgrade()) {
                    for err in b.drain_events() {
                        osd_rn(&format!("Couldn't play that — {err}"));
                        // If the load failed before the video ever revealed, don't
                        // strand the user on a spinning player — fall back to home.
                        // (A valid-looking URL that 404s / times out, etc.)
                        if !app.get_video_ready() {
                            app.set_has_video(false);
                            media_loaded_rn.set(false);
                            reveal_frames_rn.set(0);
                            want_autosize_rn.set(false);
                            resize_anim_rn.set(None);
                        }
                    }
                    // Issue the latest parked scrub target (coalesced, one seek
                    // in flight) so dragging the seek bar stays buttery.
                    b.pump_scrub();
                    // Render whenever a file is loaded — even before reveal — so
                    // we can get its native size, auto-resize the window, and let
                    // the FBO settle while the home screen still covers it.
                    if !media_loaded_rn.get() {
                        return;
                    }
                    let s = app.window().size();
                    let t0 = Instant::now();
                    let frame = b.render((s.width.max(16), s.height.max(16)));
                    if std::env::var("SONIQ_PERF").is_ok() {
                        use std::sync::atomic::{AtomicU32, Ordering};
                        static N: AtomicU32 = AtomicU32::new(0);
                        if N.fetch_add(1, Ordering::Relaxed) % 60 == 0 {
                            let (rw, rh) = b.render_size();
                            eprintln!(
                                "[perf] gpu-render {:.2}ms  hwdec={}  render={}x{}  window={}x{}",
                                t0.elapsed().as_secs_f64() * 1000.0,
                                b.hwdec_current(),
                                rw,
                                rh,
                                s.width,
                                s.height
                            );
                        }
                    }
                    app.set_video_frame(frame);

                    // Keep the UI's notion of the render-target size in sync
                    // (drives the contain-fit in Slint). Must compare BOTH dims:
                    // in the letterbox case the FBO width stays native and only
                    // the height changes with the window aspect — checking width
                    // alone left a stale height and stretched the frame.
                    let (rw, rh) = b.render_size();
                    if rw > 0
                        && rh > 0
                        && (app.get_video_w() != rw as f32 || app.get_video_h() != rh as f32)
                    {
                        app.set_video_w(rw as f32);
                        app.set_video_h(rh as f32);
                    }

                    // Resize the window to the video's aspect once its size is
                    // known — while the home screen still covers (we're rendering
                    // in the background), so it's home → resize → video. On a CLI
                    // launch the window is already this size, so this is a no-op.
                    if want_autosize_rn.get() {
                        let (dw, dh) = b.native_size();
                        if dw > 0
                            && dh > 0
                            && !app.window().is_fullscreen()
                            && !app.window().is_maximized()
                        {
                            want_autosize_rn.set(false);
                            let (tw, th) = fit_window_size((dw, dh));
                            let scale = app.window().scale_factor();
                            let cur = app.window().size();
                            let (cw, ch) =
                                (cur.width as f32 / scale, cur.height as f32 / scale);
                            // Animate only if the size meaningfully changes
                            // (CLI launches are pre-sized — no-op there).
                            if (cw - tw).abs() > 4.0 || (ch - th).abs() > 4.0 {
                                resize_anim_rn.set(Some((cw, ch, tw, th, Instant::now())));
                            }
                        }
                    }

                    // Step the IINA-style smooth resize: ease the window from its
                    // current size to the video's aspect over ~280 ms (ease-out
                    // cubic), one step per rendered frame, under the home cover.
                    if let Some((fw, fh, tw, th, t0)) = resize_anim_rn.get() {
                        let t = (t0.elapsed().as_secs_f32() / 0.28).min(1.0);
                        let e = 1.0 - (1.0 - t).powi(3);
                        app.window().set_size(slint::LogicalSize::new(
                            fw + (tw - fw) * e,
                            fh + (th - fh) * e,
                        ));
                        if t >= 1.0 {
                            resize_anim_rn.set(None);
                        }
                    }

                    // Reveal the video (drop the loading spinner) once the FBO has
                    // SETTLED — its size is unchanged for a few frames and the
                    // window isn't mid-resize — and the video has a known size.
                    // NB: do NOT require render_size == native_size: the FBO is
                    // rendered at the WINDOW aspect (letterbox/pillarbox), so it
                    // only equals native when the window matches the video aspect
                    // exactly — switching between similar-aspect clips never
                    // resizes, so that check could never pass and the spinner hung.
                    if !app.get_video_ready() && resize_anim_rn.get().is_none() {
                        let (nw, nh) = b.native_size();
                        let rs = b.render_size();
                        if nw > 0 && nh > 0 && rs.0 > 0 && rs == last_rs.get() {
                            let c = reveal_frames_rn.get() + 1;
                            reveal_frames_rn.set(c);
                            if c >= 3 {
                                app.set_has_video(true);
                                app.set_video_ready(true);
                            }
                        } else {
                            reveal_frames_rn.set(0);
                        }
                        last_rs.set(rs);
                    }

                    // Poll playback state at ~15Hz, not 60Hz: each `get_property`
                    // is a synchronous mpv call, and doing ~9 of them every frame
                    // stalls the UI thread while mpv's core is busy seeking.
                    use std::sync::atomic::{AtomicU32, Ordering};
                    static POLL: AtomicU32 = AtomicU32::new(0);
                    // Skip the synchronous state poll while actively scrubbing so
                    // the UI thread stays responsive to drag events.
                    if POLL.fetch_add(1, Ordering::Relaxed) % 4 == 0 && !b.scrub_active() {
                        let pos = b.position();
                        let dur = b.duration();
                        // Apply a deferred resume seek once the file is open.
                        if pending_seek_rn.get() > 0.0 && dur > 0.0 {
                            b.seek_seconds(pending_seek_rn.get());
                            pending_seek_rn.set(0.0);
                        }
                        // Buffer/cache indicators only make sense for streams.
                        // Local files are fully seekable, and our demuxer cache
                        // makes mpv read the whole file into memory, so the band
                        // would otherwise fill like a fake download progress.
                        let is_stream =
                            current_rn.borrow().as_deref().map(|p| p.contains("://")).unwrap_or(false);
                        app.set_position_text(fmt_time(pos).into());
                        app.set_duration_text(fmt_time(dur).into());
                        app.set_duration_secs(dur as f32);
                        app.set_progress(if dur > 0.0 { (pos / dur) as f32 } else { 0.0 });
                        app.set_buffered(if is_stream { b.buffered() as f32 } else { 0.0 });
                        let paused = b.is_paused();
                        app.set_paused(paused);
                        if paused {
                            app.set_chrome_shown(true);
                        }
                        app.set_muted(b.is_muted());
                        app.set_volume(b.volume() as f32);
                        app.set_speed(b.speed() as f32);
                        // Same rationale as the buffered band: local files briefly
                        // report paused-for-cache while seeking, which isn't real
                        // buffering and shouldn't flash the indicator.
                        app.set_buffering(is_stream && b.is_buffering());

                        let title = {
                            let t = b.media_title();
                            if t.is_empty() {
                                current_rn.borrow().as_deref().map(basename).unwrap_or_default()
                            } else {
                                t
                            }
                        };
                        app.set_title_text(title.into());
                    }
                }
            }
            slint::RenderingState::AfterRendering => {
                // Finalize the frame's alpha channel (the last write before the
                // swap — immune to renderer quirks): force the body fully
                // opaque (Skia's fade layers corrupt alpha → see-through
                // "blinks") and carve the antialiased rounded corners (radius 0
                // when fullscreen/maximized — opaque only).
                if let (Some(b), Some(a)) = (bridge_rn.borrow().as_ref(), weak.upgrade()) {
                    if a.get_alpha_surface() {
                        let s = a.window().size();
                        if std::env::var("SONIQ_VPLOG").is_ok() {
                            use std::sync::atomic::{AtomicU32, Ordering};
                            static V: AtomicU32 = AtomicU32::new(0);
                            if V.fetch_add(1, Ordering::Relaxed) % 120 == 0 {
                                eprintln!(
                                    "[vp] winit {}x{}  viewport {:?}",
                                    s.width, s.height, b.gl_viewport()
                                );
                            }
                        }
                        let radius = if a.get_win_square() {
                            0.0
                        } else {
                            12.0 * a.window().scale_factor()
                        };
                        b.punch_window_corners(s.width, s.height, radius);
                    }
                }
                // Diagnostic: detect single-frame video blinks (center pixel
                // luma collapsing between consecutive frames).
                if std::env::var("SONIQ_BLINKDET").is_ok() {
                    use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
                    static LAST: AtomicI32 = AtomicI32::new(-1);
                    static FRAME: AtomicU32 = AtomicU32::new(0);
                    let n = FRAME.fetch_add(1, Ordering::Relaxed);
                    if let (Some(b), Some(a)) = (bridge_rn.borrow().as_ref(), weak.upgrade()) {
                        let s = a.window().size();
                        let buf = b.debug_read_center(s.width, s.height);
                        let luma = (buf[0] as i32 + buf[1] as i32 + buf[2] as i32) / 3;
                        let prev = LAST.swap(luma, Ordering::Relaxed);
                        if prev >= 0 && (prev - luma).abs() > 40 {
                            eprintln!("[blink] frame {n}: luma {prev} -> {luma}");
                        }
                    }
                }
                // Diagnostic: dump the composited window (default framebuffer)
                // to /tmp/soniq_win.raw at frame N (SONIQ_DUMPWIN=N, default
                // 300). "N+" dumps a 16-frame burst (soniq_win_<i>.raw).
                if let Ok(v) = std::env::var("SONIQ_DUMPWIN") {
                    use std::sync::atomic::{AtomicU32, Ordering};
                    use std::sync::OnceLock;
                    static W: AtomicU32 = AtomicU32::new(0);
                    static T0: OnceLock<Instant> = OnceLock::new();
                    let t0 = *T0.get_or_init(Instant::now);
                    let burst = v.ends_with('+');
                    let timed = v.starts_with('@'); // "@ms+": burst after a delay
                    let at = v
                        .trim_start_matches('@')
                        .trim_end_matches('+')
                        .parse::<u32>()
                        .unwrap_or(300);
                    let n = W.fetch_add(1, Ordering::Relaxed);
                    static B: AtomicU32 = AtomicU32::new(0);
                    let idx = if timed {
                        if t0.elapsed() >= Duration::from_millis(at as u64) {
                            let i = B.fetch_add(1, Ordering::Relaxed);
                            (i < 16).then_some(i)
                        } else {
                            None
                        }
                    } else if burst {
                        (n >= at && n < at + 16).then(|| n - at)
                    } else {
                        (n == at).then_some(0)
                    };
                    if let Some(i) = idx {
                        if let (Some(b), Some(a)) = (bridge_rn.borrow().as_ref(), weak.upgrade()) {
                            let s = a.window().size();
                            let buf = b.debug_read_window(s.width, s.height);
                            let path = if burst {
                                format!("/tmp/soniq_win_{i}.raw")
                            } else {
                                "/tmp/soniq_win.raw".into()
                            };
                            std::fs::write(&path, &buf).ok();
                            eprintln!("[dumpwin] {}x{} -> {path}", s.width, s.height);
                            // SONIQ_FADETEST: start the chrome fade right at
                            // burst frame 0, so the burst frames the animation.
                            if i == 0 && std::env::var("SONIQ_FADETEST").is_ok() {
                                a.set_chrome_shown(false);
                            }
                        }
                    }
                }
                // Diagnostic: verify the window's rounded corners composite as
                // transparent (alpha ≈ 0) in the final frame.
                if std::env::var("SONIQ_CORNERTEST").is_ok() {
                    use std::sync::atomic::{AtomicU32, Ordering};
                    static C: AtomicU32 = AtomicU32::new(0);
                    if C.fetch_add(1, Ordering::Relaxed) % 60 == 30 {
                        if let (Some(b), Some(a)) = (bridge_rn.borrow().as_ref(), weak.upgrade()) {
                            let s = a.window().size();
                            eprintln!(
                                "[corners] alpha tl/bl/tr/br = {:?}",
                                b.debug_corner_alpha(s.width, s.height)
                            );
                        }
                    }
                }
                // Schedule the next frame immediately so the render loop runs at
                // the display's refresh rate (vsync-paced, even cadence) instead
                // of a fixed 60fps timer that judders on a 100Hz panel.
                if let Some(a) = weak.upgrade() {
                    a.window().request_redraw();
                }
            }
            slint::RenderingState::RenderingTeardown => {
                drop(bridge_rn.borrow_mut().take());
            }
            _ => {}
        })
        .expect("set_rendering_notifier failed (GL backend required)");

    // Heartbeat: keep the loop alive even if no AfterRendering fires (e.g. when
    // fully idle). The AfterRendering hook above does the smooth vsync pacing.
    let weak = app.as_weak();
    let redraw = slint::Timer::default();
    redraw.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
        if let Some(a) = weak.upgrade() {
            a.window().request_redraw();
        }
    });
}
