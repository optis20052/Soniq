//! mpv ⇄ Slint bridge (OpenGL, GPU path).
//!
//! mpv's OpenGL render API draws each decoded frame into an off-screen FBO whose
//! colour attachment is a texture; that texture is handed to Slint as a borrowed
//! GL texture. Hardware-decoded frames stay on the GPU — no CPU readback.
//!
//! Slint's Skia renderer caches GL state aggressively, so we back up and restore
//! the full GL state around mpv's render call (the standard technique for mixing
//! GL libraries); otherwise Skia draws with a stale cache and the video is black.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_char, c_void};
use std::num::NonZeroU32;

use glow::HasContext;
use libmpv2::Mpv;
use libmpv2::render::{OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType};

type GlLoader<'a> = &'a dyn Fn(&CStr) -> *const c_void;

pub struct TrackData {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub lang: String,
    pub selected: bool,
}

/// One entry of the current mpv playlist (the folder's files, in order).
pub struct PlaylistEntry {
    pub path: String,
    pub current: bool,
}

const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "flv", "wmv", "mpg", "mpeg", "ogv", "3gp",
];

fn folder_playlist(path: &std::path::Path) -> Vec<String> {
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
        })
        .filter_map(|p| p.to_str().map(|s| s.to_string()))
        .collect();
    files.sort();
    files
}

/// Whether `c` belongs to a right-to-left script (Hebrew / Arabic / Persian /
/// Syriac and the Arabic presentation forms) — used to decide which subtitle
/// lines get an RTL base direction.
fn is_rtl_char(c: char) -> bool {
    matches!(c as u32,
        0x0590..=0x05FF   // Hebrew
        | 0x0600..=0x06FF // Arabic
        | 0x0700..=0x074F // Syriac
        | 0x0750..=0x077F // Arabic Supplement
        | 0x08A0..=0x08FF // Arabic Extended-A
        | 0xFB50..=0xFDFF // Arabic Presentation Forms-A
        | 0xFE70..=0xFEFF) // Arabic Presentation Forms-B
}

/// Prepare an external subtitle for mpv: decode legacy Windows-1256 (common for
/// Persian/Arabic SRTs) to UTF-8, and fix bidi punctuation placement on lines
/// containing RTL script by prepending U+200E (LRM, a left-to-right base).
///
/// Why LTR base for RTL text: Persian SRTs very commonly store a sentence's
/// terminal punctuation at the LOGICAL start of the line (the author typed "!"
/// first, so "آخ!" is stored as "!آخ"). libass's default (first-strong, RTL)
/// base then renders that leading "!" on the visual RIGHT — wrong. An LTR base
/// keeps leading neutrals (punctuation, dashes) on the LEFT where the sentence
/// ends, while the Persian words still form a single RTL run that shapes and
/// orders correctly; trailing-punctuation and pure-Persian lines render
/// identically either way. This matches how subtitle viewers / browsers (which
/// default to an LTR base) show these files. Pure-LTR lines are left untouched.
///
/// Returns a temp-file path when anything was changed, else None (load the
/// original as-is). Only plain-text formats are touched — ASS/SSA are
/// structured, so we never rewrite their lines.
fn prepare_subtitle(path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let without_bom = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let (text, reencoded) = match std::str::from_utf8(without_bom) {
        Ok(s) => (s.to_string(), false),
        Err(_) => {
            let (cow, _enc, _err) = encoding_rs::WINDOWS_1256.decode(&bytes);
            eprintln!("[subs] re-encoded '{path}' from Windows-1256 to UTF-8");
            (cow.into_owned(), true)
        }
    };

    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let plain = matches!(ext.as_str(), "srt" | "vtt" | "sub" | "txt");

    let mut injected = false;
    let processed = if plain {
        text.split_inclusive('\n')
            .map(|line| {
                let content = line.trim_end_matches(['\r', '\n']);
                if content.chars().any(is_rtl_char) && !content.starts_with('\u{200E}') {
                    injected = true;
                    // Prepend an LRM (left-to-right mark) for an LTR base — keeps
                    // mis-placed leading punctuation at the visual sentence-end.
                    // The text itself is never moved/rewritten.
                    format!("\u{200E}{line}")
                } else {
                    line.to_string()
                }
            })
            .collect::<String>()
    } else {
        text.clone()
    };

    if !reencoded && !injected {
        return None; // pure UTF-8 with no RTL plain lines — load original as-is
    }
    let src = std::path::Path::new(path);
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("subtitle");
    let ext2 = src.extension().and_then(|s| s.to_str()).unwrap_or("srt");
    let out = std::env::temp_dir().join(format!("soniq-{stem}.utf8.{ext2}"));
    std::fs::write(&out, processed.as_bytes()).ok()?;
    Some(out.to_string_lossy().into_owned())
}

/// External subtitle files in the same folder whose name matches the video's
/// (exact stem, or `stem.lang.ext`) — replicates mpv's `sub-auto` discovery so
/// we can load them through `prepare_subtitle` (we disable mpv's own auto-load).
fn sibling_subs(video_path: &str) -> Vec<String> {
    const EXTS: &[&str] = &["srt", "ass", "ssa", "vtt", "sub"];
    let p = std::path::Path::new(video_path);
    let (Some(dir), Some(stem)) = (
        p.parent(),
        p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_lowercase()),
    ) else {
        return vec![];
    };
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let fp = e.path();
            let ext = fp.extension().and_then(|s| s.to_str()).map(|s| s.to_lowercase());
            let fstem = fp.file_stem().and_then(|s| s.to_str()).map(|s| s.to_lowercase());
            if let (Some(ext), Some(fstem)) = (ext, fstem) {
                if EXTS.contains(&ext.as_str())
                    && (fstem == stem || fstem.starts_with(&format!("{stem}.")))
                {
                    out.push(fp.to_string_lossy().into_owned());
                }
            }
        }
    }
    out.sort();
    out
}

fn mpv_get_proc_address(ctx: &*mut c_void, name: &str) -> *mut c_void {
    let loader: &GlLoader = unsafe { &*(*ctx as *const GlLoader) };
    let cname = match CString::new(name) {
        Ok(c) => c,
        Err(_) => return std::ptr::null_mut(),
    };
    (loader)(&cname) as *mut c_void
}

const MPV_FLIP_Y: bool = false;

pub struct VideoBridge {
    render_ctx: RenderContext<'static>,
    mpv: Box<Mpv>,
    gl: glow::Context,
    fbo: glow::Framebuffer,
    texture: glow::Texture,
    tex_id: NonZeroU32,
    size: (u32, u32),
    // Throttle the per-frame native-size query — each get_property is a
    // synchronous mpv round-trip, so we only re-check every Nth frame.
    size_poll: u32,
    // The window size seen on the previous size-poll. The FBO is reallocated to
    // the new target only once the window has STOPPED changing (current ==
    // last), so an interactive resize doesn't realloc the mpv↔GL target every
    // frame — that per-frame reconfiguration is what produced RGB noise.
    last_window: (u32, u32),
    // Scrub-seek coalescing: the latest cursor target waits in `seek_pending`
    // and is issued from the render loop only when mpv isn't already seeking,
    // so one seek lands (and its frame paints) before the next is sent.
    seek_pending: Cell<Option<f64>>,
    scrubbing: Cell<bool>,
    // The video path we last auto-loaded sibling subtitles for, so `poll_subs`
    // only does the filesystem scan + sub-add once per file (incl. playlist
    // navigation, where `path` changes without a fresh load() call).
    last_subs_path: RefCell<String>,
}

impl VideoBridge {
    pub fn new(gl: glow::Context, loader: GlLoader, size: (u32, u32)) -> Self {
        let mpv = Box::new(
            Mpv::with_initializer(|init| init.set_option("vo", "libmpv")).expect("create mpv"),
        );
        // hwdec is overridable for diagnosis (e.g. SONIQ_HWDEC=no forces software
        // decode, isolating GPU-decode/interop bugs from the compositing path).
        //
        // macOS: VideoToolbox's zero-copy GL interop (what `auto-safe` picks)
        // doesn't bind the frame's chroma plane through our embedded Skia GL
        // context, so CbCr reads as 0 and the whole video comes out green. Use
        // the `-copy` variant instead: still hardware-decoded, but the frame is
        // read back to system memory and mpv does the YUV→RGB upload itself, so
        // no broken plane interop. On Linux `auto-safe` (VAAPI/NVDEC) is fine.
        let default_hwdec = if cfg!(target_os = "macos") {
            "videotoolbox-copy"
        } else {
            "auto-safe"
        };
        let hwdec = std::env::var("SONIQ_HWDEC").unwrap_or_else(|_| default_hwdec.into());
        let _ = mpv.set_property("hwdec", hwdec.as_str());
        // Tell mpv our render target is 8-bit SDR (BT.709). With the libmpv
        // render API there's no display to probe, so without this mpv leaves HDR
        // (BT.2020/PQ) content un-tone-mapped and crushes it into the 8-bit FBO,
        // where dithering explodes into full-frame RGB speckle. Forcing an SDR
        // target makes libplacebo tone-map + gamut-convert HDR → SDR properly;
        // SDR sources are unaffected (BT.709 → BT.709 is a no-op).
        let _ = mpv.set_property("target-prim", "bt.709");
        let _ = mpv.set_property("target-trc", "bt.1886");
        // Use bilinear scalers. libplacebo's default polyphase scalers build a
        // filter-kernel LUT texture that gets corrupted inside our embedded
        // Skia GL context on the HDR multi-pass path, producing full-frame RGB
        // speckle on 10-bit HDR video (8-bit SDR happened to avoid it). We render
        // mpv at the video's *native* resolution and let Slint scale for display,
        // so mpv's luma scaler is never actually used — bilinear costs nothing
        // here and sidesteps the broken LUT path. cscale (chroma 4:2:0→4:4:4) is
        // the only scaler that runs, and bilinear chroma is perfectly fine.
        let _ = mpv.set_property("scale", "bilinear");
        let _ = mpv.set_property("dscale", "bilinear");
        let _ = mpv.set_property("cscale", "bilinear");
        // Don't let mpv_render_context_render block the UI thread waiting for the
        // frame's display time — otherwise slider drags and UI animations stall.
        // We drive redraws ourselves at ~60fps and just render the current frame.
        let _ = mpv.set_property("video-timing-offset", 0.0);
        // Demuxer cache sized for snappy scrubbing without hoarding RAM. The old
        // 256MiB forward + 256MiB back (≈512MiB, with cache=yes forcing it even
        // for local files) is what made us use far more memory than IINA on the
        // same stream. 64MiB ahead / 32MiB back keeps recent seeks resolving from
        // memory while cutting peak cache ~5×; `cache=auto` lets mpv skip the RAM
        // cache entirely for local files (disk re-reads are cheap on SSD). Env
        // override for power users who want the old hoard-everything behaviour.
        let _ = mpv.set_property("cache", "auto");
        let fwd = std::env::var("SONIQ_CACHE_FWD").unwrap_or_else(|_| "64MiB".into());
        let back = std::env::var("SONIQ_CACHE_BACK").unwrap_or_else(|_| "32MiB".into());
        let _ = mpv.set_property("demuxer-max-bytes", fwd.as_str());
        let _ = mpv.set_property("demuxer-max-back-bytes", back.as_str());
        // hr-seek so exact seeks are precise even on sparse-keyframe content.
        let _ = mpv.set_property("hr-seek", "yes");
        let _ = mpv.set_property("hr-seek-framedrop", "yes");

        let loader_ptr = (&loader) as *const GlLoader as *mut c_void;
        let render_ctx = mpv
            .create_render_context(vec![
                RenderParam::ApiType(RenderParamApiType::OpenGl),
                RenderParam::InitParams(OpenGLInitParams {
                    get_proc_address: mpv_get_proc_address,
                    ctx: loader_ptr,
                }),
            ])
            .expect("create mpv render context");
        let render_ctx: RenderContext<'static> = unsafe { std::mem::transmute(render_ctx) };

        let _ = mpv.set_property("keep-open", "yes");
        // We discover & load external subtitles ourselves (sibling_subs +
        // prepare_subtitle) so they pass through RTL/encoding preprocessing;
        // disable mpv's own auto-load to avoid loading the raw file twice.
        let _ = mpv.set_property("sub-auto", "no");
        // Diagnostic: open files at an offset (seconds) — lets scripted runs
        // land on a specific scene (e.g. a bright shot for chrome readbacks).
        if let Ok(s) = std::env::var("SONIQ_START") {
            let _ = mpv.set_property("start", s.as_str());
        }
        if std::env::var("SONIQ_MPV_LOG").is_ok() {
            let _ = mpv.set_property("terminal", "yes");
            let _ = mpv.set_property("msg-level", "all=v");
        }

        let (fbo, texture, tex_id) = unsafe { create_target(&gl, size) };

        Self {
            render_ctx,
            mpv,
            gl,
            fbo,
            texture,
            tex_id,
            size,
            size_poll: 0,
            last_window: (0, 0),
            seek_pending: Cell::new(None),
            scrubbing: Cell::new(false),
            last_subs_path: RefCell::new(String::new()),
        }
    }

    /// Load external sibling subtitles for the currently-playing file (once per
    /// file), through `prepare_subtitle` so Persian/Arabic subs are decoded and
    /// given an RTL base direction. Cheap: a single `path` query guards the
    /// filesystem scan to the moment the file actually changes.
    pub fn poll_subs(&self) {
        let path: String = self.mpv.get_property("path").unwrap_or_default();
        if path.is_empty() || path.contains("://") {
            return;
        }
        if *self.last_subs_path.borrow() == path {
            return;
        }
        *self.last_subs_path.borrow_mut() = path.clone();
        for sub in sibling_subs(&path) {
            let prepared = prepare_subtitle(&sub);
            let target = prepared.as_deref().unwrap_or(&sub);
            // "auto" selects the first added sub if none is selected yet.
            let _ = self.mpv.command("sub-add", &[target, "auto"]);
        }
    }

    /// The size mpv should render at. We match the WINDOW's aspect ratio,
    /// keeping the video's native resolution in the fitting dimension — so mpv
    /// itself draws the letterbox/pillarbox bars (and renders subtitles INTO
    /// them, like GDK/IINA) instead of Slint adding the bars around a
    /// native-sized texture. The video is never scaled down to make room; only
    /// black bars are added. The FBO only changes when the window *aspect*
    /// changes, and `render()` defers the realloc until the window stops moving
    /// (so a resize drag doesn't reconfigure the mpv↔GL target every frame —
    /// the cause of the old RGB resize noise). Falls back to the window size
    /// until the video's dimensions are known.
    fn target_size(&self, window: (u32, u32)) -> (u32, u32) {
        let dw: i64 = self.mpv.get_property("dwidth").unwrap_or(0);
        let dh: i64 = self.mpv.get_property("dheight").unwrap_or(0);
        if dw <= 0 || dh <= 0 || window.0 == 0 || window.1 == 0 {
            return window;
        }
        let (dw, dh) = (dw as f64, dh as f64);
        let (ww, wh) = (window.0 as f64, window.1 as f64);
        let video_ar = dw / dh;
        let win_ar = ww / wh;
        if win_ar > video_ar {
            // Window wider than the video → pillarbox: keep native height, grow
            // width to the window aspect (bars left/right).
            (((dh * win_ar).round() as u32).max(2), dh as u32)
        } else {
            // Window taller → letterbox: keep native width, grow height (bars
            // top/bottom — where bottom subtitles land).
            (dw as u32, ((dw / win_ar).round() as u32).max(2))
        }
    }

    fn resize(&mut self, size: (u32, u32)) {
        if std::env::var("SONIQ_FSTEST").is_ok() {
            eprintln!("[resize] fbo {:?} -> {:?}", self.size, size);
        }
        // Initialise the new storage to opaque black (not `None` = undefined GPU
        // memory) so the brief moment before mpv renders its first frame shows
        // black instead of garbage — the noise flash seen when opening a video.
        let zeros = vec![0u8; size.0 as usize * size.1 as usize * 4];
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                size.0 as i32,
                size.1 as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&zeros)),
            );
            self.gl.bind_texture(glow::TEXTURE_2D, None);
        }
        self.size = size;
    }

    pub fn render(&mut self, window: (u32, u32)) -> slint::Image {
        // Re-evaluate the native video size periodically (not every frame: each
        // get_property is a synchronous mpv call). This only changes on a new
        // file / track switch — never on window resize — so the FBO is stable
        // during a drag.
        self.size_poll = self.size_poll.wrapping_add(1);
        if self.size.0 == 0 || self.size.1 == 0 || self.size_poll % 5 == 0 {
            // Only realloc once the window has settled (this poll's size matches
            // the previous one): during an interactive resize the window keeps
            // changing, so we hold the current FBO (Slint letterboxes it for the
            // interim) and reconfigure mpv's target exactly once, on release.
            let stable = window == self.last_window;
            self.last_window = window;
            let target = self.target_size(window);
            if stable && target != self.size && target.0 > 0 && target.1 > 0 {
                self.resize(target);
            }
        }
        let (w, h) = self.size;

        // Back up the GL state Skia relies on, let mpv render into our FBO, then
        // restore — so Skia's cached state stays valid and the video isn't black.
        let saved = unsafe { GlState::backup(&self.gl) };

        // Neutralise pixel-transfer state before handing the context to mpv.
        // Skia can leave a PIXEL_UNPACK_BUFFER bound and a non-default unpack
        // alignment/row-length; libplacebo's advanced (HDR) path uploads small
        // LUT/helper textures, and those uploads read from the bound unpack
        // buffer / wrong stride instead of their data — producing full-frame RGB
        // speckle on 10-bit HDR content (the simple SDR path uploads nothing, so
        // it was unaffected). These are GL global state, so reset them here.
        unsafe {
            self.gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            self.gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            self.gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
            self.gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 0);
            self.gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 0);
            // Skia can leave SCISSOR_TEST enabled with a small clip box; that
            // would clip libplacebo's intermediate render passes (HDR tone-map /
            // gamut conversion), leaving garbage in the off-clip regions that the
            // next pass samples → full-frame speckle. Disable it (and cull/depth)
            // for mpv; GlState::restore puts them back for Skia.
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.disable(glow::CULL_FACE);
            self.gl.disable(glow::DEPTH_TEST);
        }

        self.render_ctx
            .render::<*mut c_void>(self.fbo.0.get() as i32, w as i32, h as i32, MPV_FLIP_Y)
            .expect("mpv render");

        unsafe {
            saved.restore(&self.gl);
        }

        unsafe {
            slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                self.tex_id,
                euclid::default::Size2D::new(w, h),
            )
            .build()
        }
    }

    pub fn load(&self, target: &str) {
        // Clear per-video adjustments so the previous file's colour/speed/crop
        // don't carry over to this one.
        self.reset_adjustments();
        if target.contains("://") {
            let _ = self.mpv.command("loadfile", &[target, "replace"]);
            let _ = self.mpv.set_property("pause", false);
            return;
        }
        let path = std::path::Path::new(target);
        let siblings = folder_playlist(path);
        let idx = siblings.iter().position(|p| p == target);
        match (siblings.is_empty(), idx) {
            (false, Some(start)) => {
                let _ = self.mpv.command("loadfile", &[&siblings[0], "replace"]);
                for s in &siblings[1..] {
                    let _ = self.mpv.command("loadfile", &[s, "append"]);
                }
                let _ = self.mpv.set_property("playlist-pos", start as i64);
            }
            _ => {
                let _ = self.mpv.command("loadfile", &[target, "replace"]);
            }
        }
        let _ = self.mpv.set_property("pause", false);
    }

    pub fn playlist_next(&self) {
        let _ = self.mpv.command("playlist-next", &["weak"]);
    }
    pub fn playlist_prev(&self) {
        let _ = self.mpv.command("playlist-prev", &["weak"]);
    }

    /// The current playlist (folder files in order), with the playing entry flagged.
    pub fn playlist(&self) -> Vec<PlaylistEntry> {
        let count: i64 = self.mpv.get_property("playlist-count").unwrap_or(0);
        let pos: i64 = self.mpv.get_property("playlist-pos").unwrap_or(-1);
        (0..count)
            .map(|i| PlaylistEntry {
                path: self
                    .mpv
                    .get_property(&format!("playlist/{i}/filename"))
                    .unwrap_or_default(),
                current: i == pos,
            })
            .collect()
    }

    /// Jump to a specific playlist entry and play it.
    pub fn playlist_play(&self, index: i64) {
        let _ = self.mpv.set_property("playlist-pos", index);
        let _ = self.mpv.set_property("pause", false);
    }

    /// Append a file/URL to the end of the playlist (for batch URL opens).
    pub fn playlist_append(&self, target: &str) {
        let _ = self.mpv.command("loadfile", &[target, "append"]);
    }

    /// The path of the file mpv is currently playing (tracks playlist advances
    /// that happen without going through `load`). None until a file is open.
    pub fn path(&self) -> Option<String> {
        let p: String = self.mpv.get_property("path").unwrap_or_default();
        if p.is_empty() {
            None
        } else {
            Some(p)
        }
    }

    pub fn tracks(&self) -> Vec<TrackData> {
        let count: i64 = self.mpv.get_property("track-list/count").unwrap_or(0);
        let mut out = Vec::new();
        for i in 0..count {
            out.push(TrackData {
                id: self.mpv.get_property(&format!("track-list/{i}/id")).unwrap_or(0),
                kind: self.mpv.get_property(&format!("track-list/{i}/type")).unwrap_or_default(),
                title: self.mpv.get_property(&format!("track-list/{i}/title")).unwrap_or_default(),
                lang: self.mpv.get_property(&format!("track-list/{i}/lang")).unwrap_or_default(),
                selected: self.mpv.get_property(&format!("track-list/{i}/selected")).unwrap_or(false),
            });
        }
        out
    }

    pub fn set_audio(&self, id: i64) {
        let _ = self.mpv.set_property("aid", id);
    }
    pub fn set_video_track(&self, id: i64) {
        if id < 0 {
            let _ = self.mpv.set_property("vid", "no");
        } else {
            let _ = self.mpv.set_property("vid", id);
        }
    }
    pub fn set_hwdec(&self, on: bool) {
        let _ = self.mpv.set_property("hwdec", if on { "auto-safe" } else { "no" });
    }
    /// Resize the demuxer cache window (MiB) live — the Prefs sliders call this so
    /// changes take effect without a reload. `SONIQ_CACHE_FWD`/`SONIQ_CACHE_BACK`
    /// env vars still override (debugging). `fwd` is the read-ahead buffer; `back`
    /// is how much already-played data is kept for instant rewind.
    pub fn set_cache_limits(&self, fwd_mib: i64, back_mib: i64) {
        let fwd =
            std::env::var("SONIQ_CACHE_FWD").unwrap_or_else(|_| format!("{}MiB", fwd_mib.max(8)));
        let back =
            std::env::var("SONIQ_CACHE_BACK").unwrap_or_else(|_| format!("{}MiB", back_mib.max(0)));
        let _ = self.mpv.set_property("demuxer-max-bytes", fwd.as_str());
        let _ = self.mpv.set_property("demuxer-max-back-bytes", back.as_str());
    }
    pub fn set_deinterlace(&self, on: bool) {
        let _ = self.mpv.set_property("deinterlace", on);
    }

    pub fn set_crop_aspect(&self, w: i64, h: i64) {
        if w <= 0 || h <= 0 {
            let _ = self.mpv.set_property("video-crop", "");
            return;
        }
        let dw: i64 = self.mpv.get_property("dwidth").unwrap_or(0);
        let dh: i64 = self.mpv.get_property("dheight").unwrap_or(0);
        if dw <= 0 || dh <= 0 {
            return;
        }
        let target = w as f64 / h as f64;
        let source = dw as f64 / dh as f64;
        let geom = if source > target {
            let nw = (dh as f64 * target).round() as i64;
            format!("{}x{}+{}+0", nw, dh, (dw - nw) / 2)
        } else {
            let nh = (dw as f64 / target).round() as i64;
            format!("{}x{}+0+{}", dw, nh, (dh - nh) / 2)
        };
        let _ = self.mpv.set_property("video-crop", geom.as_str());
    }

    pub fn set_equalizer(&self, gains: &[f64; 10]) {
        const FREQS: [f64; 10] = [
            32.0, 64.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
        ];
        if gains.iter().all(|g| g.abs() < 0.01) {
            let _ = self.mpv.set_property("af", "");
            return;
        }
        let bands = |chan: usize| -> String {
            FREQS
                .iter()
                .zip(gains.iter())
                .map(|(f, g)| format!("c{chan} f={f} w={} g={g}", f / 2.0))
                .collect::<Vec<_>>()
                .join("|")
        };
        let af = format!("anequalizer=[{}|{}]", bands(0), bands(1));
        let _ = self.mpv.set_property("af", af.as_str());
    }

    pub fn set_sub(&self, id: i64) {
        let _ = self.mpv.set_property("sid", id);
    }
    pub fn disable_sub(&self) {
        let _ = self.mpv.set_property("sid", "no");
    }
    pub fn add_subtitle(&self, path: &str) {
        // Match the original app: external subs are often Windows-1256
        // (Persian/Arabic) and render as mojibake if handed raw to mpv. Detect
        // non-UTF-8 and load a decoded UTF-8 copy instead; UTF-8 files pass
        // through unchanged.
        let load = prepare_subtitle(path);
        let target = load.as_deref().unwrap_or(path);
        let _ = self.mpv.command("sub-add", &[target, "select"]);
    }
    pub fn speed(&self) -> f64 {
        self.mpv.get_property("speed").unwrap_or(1.0)
    }
    pub fn media_title(&self) -> String {
        self.mpv.get_property::<String>("media-title").unwrap_or_default()
    }
    pub fn is_buffering(&self) -> bool {
        self.mpv.get_property("paused-for-cache").unwrap_or(false)
    }
    /// Current off-screen render (FBO/texture) size — the video's native
    /// resolution once known. Decoupled from the window size.
    pub fn render_size(&self) -> (u32, u32) {
        self.size
    }
    /// The video's native display resolution (`dwidth`×`dheight`), or (0,0) if
    /// not yet known. Queried directly from mpv (not the throttled cache).
    pub fn native_size(&self) -> (u32, u32) {
        let dw: i64 = self.mpv.get_property("dwidth").unwrap_or(0);
        let dh: i64 = self.mpv.get_property("dheight").unwrap_or(0);
        (dw.max(0) as u32, dh.max(0) as u32)
    }
    pub fn hwdec_current(&self) -> String {
        self.mpv.get_property::<String>("hwdec-current").unwrap_or_else(|_| "?".into())
    }

    pub fn set_prop_f64(&self, name: &str, v: f64) {
        let _ = self.mpv.set_property(name, v);
    }
    pub fn set_prop_i64(&self, name: &str, v: i64) {
        let _ = self.mpv.set_property(name, v);
    }
    pub fn set_prop_str(&self, name: &str, v: &str) {
        let _ = self.mpv.set_property(name, v);
    }

    /// Reset the per-video adjustments (colour, geometry, speed, A/V & subtitle
    /// delay) to defaults for a freshly-loaded file, so they don't leak from the
    /// previous one. Persisted *global* subtitle style (scale / position /
    /// colour / font) is deliberately left untouched.
    pub fn reset_adjustments(&self) {
        for p in ["brightness", "contrast", "saturation", "gamma", "hue", "video-rotate"] {
            let _ = self.mpv.set_property(p, 0i64);
        }
        let _ = self.mpv.set_property("video-aspect-override", "-1");
        let _ = self.mpv.set_property("video-crop", "");
        let _ = self.mpv.set_property("sub-delay", 0.0);
        let _ = self.mpv.set_property("audio-delay", 0.0);
        let _ = self.mpv.set_property("speed", 1.0);
    }

    pub fn toggle_pause(&self) {
        let _ = self.mpv.set_property("pause", !self.is_paused());
    }
    pub fn is_paused(&self) -> bool {
        self.mpv.get_property("pause").unwrap_or(false)
    }
    pub fn stop(&self) {
        // Real teardown (used by the Stop button → home screen), not a
        // seek-to-start: unloads the file and clears the playlist.
        let _ = self.mpv.command("stop", &[]);
    }
    pub fn seek_relative(&self, secs: f64) {
        let _ = self.mpv.command("seek", &[&secs.to_string(), "relative"]);
    }
    pub fn seek_seconds(&self, secs: f64) {
        let _ = self.mpv.command("seek", &[&secs.to_string(), "absolute"]);
    }
    pub fn volume(&self) -> f64 {
        (self.mpv.get_property::<f64>("volume").unwrap_or(100.0) / 100.0).clamp(0.0, 1.0)
    }
    pub fn set_volume(&self, frac: f64) {
        let _ = self.mpv.set_property("volume", (frac.clamp(0.0, 1.0) * 100.0).round());
    }
    pub fn is_muted(&self) -> bool {
        self.mpv.get_property("mute").unwrap_or(false)
    }
    pub fn toggle_mute(&self) {
        let _ = self.mpv.set_property("mute", !self.is_muted());
    }
    pub fn buffered(&self) -> f64 {
        let dur = self.duration();
        if dur <= 0.0 {
            return 0.0;
        }
        let cache: f64 = self.mpv.get_property("demuxer-cache-time").unwrap_or(0.0);
        (cache / dur).clamp(0.0, 1.0)
    }
    pub fn position(&self) -> f64 {
        self.mpv.get_property("time-pos").unwrap_or(0.0)
    }
    pub fn duration(&self) -> f64 {
        self.mpv.get_property("duration").unwrap_or(0.0)
    }

    /// Exact seek to a fraction of the file — used when the drag is released so
    /// playback lands on the precise frame. Clears any pending scrub state.
    pub fn seek_fraction(&self, frac: f64) {
        self.seek_pending.set(None);
        self.scrubbing.set(false);
        let dur = self.duration();
        if dur > 0.0 {
            let target = (frac.clamp(0.0, 1.0) * dur).to_string();
            let _ = self.mpv.command("seek", &[&target, "absolute+exact"]);
        }
    }

    /// Live scrub seek while dragging. Just records the latest cursor target;
    /// the actual seek is issued by `pump_scrub` from the render loop. This
    /// decouples the seek rate from the drag-event rate so fast dragging can't
    /// flood mpv's input queue (which made the preview trail the knob by seconds
    /// and pushed the demuxer into a buffering stall).
    pub fn seek_fraction_scrub(&self, frac: f64) {
        self.scrubbing.set(true);
        self.seek_pending.set(Some(frac.clamp(0.0, 1.0)));
    }

    /// Issue the latest parked scrub target. Called once per rendered frame, so
    /// the seek rate is bounded by the display rate (≤ vsync) and only the most
    /// recent knob position is ever sent — mpv coalesces queued seeks internally
    /// (it overwrites its pending seek target), so this converges to the cursor
    /// aggressively without flooding, giving the snappiest preview. We do NOT
    /// wait for each seek to finish first: that added a full seek-decode of lag
    /// behind the knob.
    ///
    /// Uses **exact** seeks, like the GTK build's `ACCURATE` flag — keyframe
    /// seeks snap to (often sparse) keyframes, so the preview jumps in chunks
    /// instead of following the knob.
    pub fn pump_scrub(&self) {
        let Some(frac) = self.seek_pending.take() else {
            return;
        };
        let pct = (frac * 100.0).to_string();
        self.command_async(&["seek", &pct, "absolute-percent+exact"]);
    }

    /// Debug: alpha of the four window corners, read back from the default
    /// framebuffer (pre-swap). Verifies rounded-CSD transparency end to end —
    /// ~0 means the corner is properly transparent, 255 means something painted
    /// it opaque.
    pub fn debug_corner_alpha(&self, w: u32, h: u32) -> [u8; 4] {
        let mut px = [0u8; 4];
        let mut out = [255u8; 4];
        unsafe {
            let prev = self.gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING);
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            // centre pixel piggybacks on the test: catches "video went black".
            let mut cpx = [0u8; 4];
            self.gl.read_pixels(
                w as i32 / 2,
                h as i32 / 2,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut cpx)),
            );
            if std::env::var("SONIQ_FSTEST").is_ok() {
                eprintln!("[center] rgba {:?}", cpx);
            }
            let pts = [
                (2i32, 2i32),
                (2, h as i32 - 3),
                (w as i32 - 3, 2),
                (w as i32 - 3, h as i32 - 3),
            ];
            for (i, (x, y)) in pts.iter().enumerate() {
                self.gl.read_pixels(
                    *x,
                    *y,
                    1,
                    1,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut px)),
                );
                out[i] = px[3];
            }
            let prev_fb = NonZeroU32::new(prev as u32).map(glow::NativeFramebuffer);
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, prev_fb);
        }
        out
    }

    /// The GL viewport as last set by the renderer — reflects the *real*
    /// drawable size, which under fractional scaling can differ by a pixel or
    /// two from winit's reported window size.
    pub fn gl_viewport(&self) -> (i32, i32, i32, i32) {
        let mut vp = [0i32; 4];
        unsafe { self.gl.get_parameter_i32_slice(glow::VIEWPORT, &mut vp) };
        (vp[0], vp[1], vp[2], vp[3])
    }

    /// Debug: read the window's center pixel from the default framebuffer.
    pub fn debug_read_center(&self, w: u32, h: u32) -> [u8; 4] {
        let mut px = [0u8; 4];
        unsafe {
            let prev = self.gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING);
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            self.gl.read_pixels(
                w as i32 / 2,
                h as i32 / 2,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut px)),
            );
            let prev_fb = NonZeroU32::new(prev as u32).map(glow::NativeFramebuffer);
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, prev_fb);
        }
        px
    }

    /// Debug: read the whole default framebuffer (composited window, pre-swap).
    pub fn debug_read_window(&self, w: u32, h: u32) -> Vec<u8> {
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        unsafe {
            let prev = self.gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING);
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            self.gl.read_pixels(
                0,
                0,
                w as i32,
                h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut buf)),
            );
            let prev_fb = NonZeroU32::new(prev as u32).map(glow::NativeFramebuffer);
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, prev_fb);
        }
        buf
    }

    /// True while the user is dragging the seek bar — the caller can skip its
    /// synchronous state poll so the UI thread stays free for pointer events.
    pub fn scrub_active(&self) -> bool {
        self.scrubbing.get()
    }

    /// Drain pending mpv events (async command replies etc.) so the event queue
    /// doesn't overflow. Called once per rendered frame. Returns any user-facing
    /// playback errors (e.g. a file/URL that failed to open) so the caller can
    /// show a toast instead of failing silently.
    pub fn drain_events(&self) -> Vec<String> {
        let mut errors = Vec::new();
        loop {
            let ev = unsafe { libmpv2_sys::mpv_wait_event(self.mpv.ctx.as_ptr(), 0.0) };
            if ev.is_null() {
                break;
            }
            let id = unsafe { (*ev).event_id };
            if id == libmpv2_sys::mpv_event_id_MPV_EVENT_NONE {
                break;
            }
            if id == libmpv2_sys::mpv_event_id_MPV_EVENT_END_FILE {
                let ef = unsafe { &*((*ev).data as *const libmpv2_sys::mpv_event_end_file) };
                if ef.reason == libmpv2_sys::mpv_end_file_reason_MPV_END_FILE_REASON_ERROR {
                    let msg = unsafe { CStr::from_ptr(libmpv2_sys::mpv_error_string(ef.error)) }
                        .to_string_lossy()
                        .into_owned();
                    errors.push(msg);
                }
            }
        }
        errors
    }

    fn command_async(&self, args: &[&str]) {
        let cstrs: Vec<CString> = match args.iter().map(|a| CString::new(*a)).collect() {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut ptrs: Vec<*const c_char> = cstrs.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        unsafe {
            libmpv2_sys::mpv_command_async(self.mpv.ctx.as_ptr(), 0, ptrs.as_mut_ptr());
        }
    }
}

impl Drop for VideoBridge {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_framebuffer(self.fbo);
            self.gl.delete_texture(self.texture);
        }
    }
}

/// Snapshot of the GL state Skia's renderer caches, captured before mpv renders
/// and restored after, so Skia's draws don't operate on a stale cache.
struct GlState {
    framebuffer: i32,
    program: i32,
    vao: i32,
    array_buffer: i32,
    element_buffer: i32,
    active_texture: i32,
    texture_2d: i32,
    blend: bool,
    blend_src_rgb: i32,
    blend_dst_rgb: i32,
    blend_src_a: i32,
    blend_dst_a: i32,
    scissor: bool,
    cull: bool,
    depth: bool,
    viewport: [i32; 4],
}

impl GlState {
    unsafe fn backup(gl: &glow::Context) -> Self {
        unsafe {
            let mut viewport = [0i32; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            Self {
                framebuffer: gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING),
                program: gl.get_parameter_i32(glow::CURRENT_PROGRAM),
                vao: gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING),
                array_buffer: gl.get_parameter_i32(glow::ARRAY_BUFFER_BINDING),
                element_buffer: gl.get_parameter_i32(glow::ELEMENT_ARRAY_BUFFER_BINDING),
                active_texture: gl.get_parameter_i32(glow::ACTIVE_TEXTURE),
                texture_2d: gl.get_parameter_i32(glow::TEXTURE_BINDING_2D),
                blend: gl.is_enabled(glow::BLEND),
                blend_src_rgb: gl.get_parameter_i32(glow::BLEND_SRC_RGB),
                blend_dst_rgb: gl.get_parameter_i32(glow::BLEND_DST_RGB),
                blend_src_a: gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
                blend_dst_a: gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
                scissor: gl.is_enabled(glow::SCISSOR_TEST),
                cull: gl.is_enabled(glow::CULL_FACE),
                depth: gl.is_enabled(glow::DEPTH_TEST),
                viewport,
            }
        }
    }

    unsafe fn restore(&self, gl: &glow::Context) {
        unsafe fn fb(i: i32) -> Option<glow::Framebuffer> {
            NonZeroU32::new(i as u32).map(glow::NativeFramebuffer)
        }
        unsafe fn prog(i: i32) -> Option<glow::Program> {
            NonZeroU32::new(i as u32).map(glow::NativeProgram)
        }
        unsafe fn vao(i: i32) -> Option<glow::VertexArray> {
            NonZeroU32::new(i as u32).map(glow::NativeVertexArray)
        }
        unsafe fn buf(i: i32) -> Option<glow::Buffer> {
            NonZeroU32::new(i as u32).map(glow::NativeBuffer)
        }
        unsafe fn tex(i: i32) -> Option<glow::Texture> {
            NonZeroU32::new(i as u32).map(glow::NativeTexture)
        }
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, fb(self.framebuffer));
            // mpv/libplacebo disables alpha writes (glColorMask) as an
            // opaque-video optimisation and leaves it set; with it leaked,
            // Skia can never write the window's alpha channel again — after a
            // resize allocates fresh buffers the whole window turns opaque and
            // the rounded corners die. Always hand Skia a full write mask.
            gl.color_mask(true, true, true, true);
            gl.use_program(prog(self.program));
            gl.bind_vertex_array(vao(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, buf(self.array_buffer));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, buf(self.element_buffer));
            gl.active_texture(self.active_texture as u32);
            gl.bind_texture(glow::TEXTURE_2D, tex(self.texture_2d));
            if self.blend {
                gl.enable(glow::BLEND);
            } else {
                gl.disable(glow::BLEND);
            }
            gl.blend_func_separate(
                self.blend_src_rgb as u32,
                self.blend_dst_rgb as u32,
                self.blend_src_a as u32,
                self.blend_dst_a as u32,
            );
            if self.scissor {
                gl.enable(glow::SCISSOR_TEST);
            } else {
                gl.disable(glow::SCISSOR_TEST);
            }
            if self.cull {
                gl.enable(glow::CULL_FACE);
            } else {
                gl.disable(glow::CULL_FACE);
            }
            if self.depth {
                gl.enable(glow::DEPTH_TEST);
            } else {
                gl.disable(glow::DEPTH_TEST);
            }
            gl.viewport(
                self.viewport[0],
                self.viewport[1],
                self.viewport[2],
                self.viewport[3],
            );
        }
    }
}

impl VideoBridge {
    /// Punch rounded corners (alpha 0) straight into the *window's* framebuffer
    /// — called at AfterRendering, after Skia has fully painted the frame and
    /// before the swap. Being the last write of the frame, it cannot be undone
    /// by any renderer state/cache quirk (the corner alpha repeatedly broke
    /// after resizes when the rounding relied on the scene's own compositing).
    /// `radius_px == 0` still runs (it forces the window's alpha fully opaque
    /// without carving arcs — needed in fullscreen/maximized too).
    pub fn punch_window_corners(&self, w: u32, h: u32, radius_px: f32) {
        unsafe {
            erase_corners_on(&self.gl, None, w as i32, h as i32, radius_px.round() as i32)
        };
    }
}

/// Erase rounded corners (radius `r` px) from a framebuffer's alpha channel,
/// antialiased: per scan-row, the fully-outside run is cleared to alpha 0 in one
/// scissor strip, then the 1–2 boundary pixels get fractional alpha equal to
/// their arc coverage (1px smooth edge, like any AA'd rounded rect). Colour
/// channels are masked off throughout. All four corners are symmetric, so
/// orientation doesn't matter. `fbo: None` = the default framebuffer.
unsafe fn erase_corners_on(
    gl: &glow::Context,
    fbo: Option<glow::Framebuffer>,
    w: i32,
    h: i32,
    r: i32,
) {
    unsafe {
        let prev = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
        gl.bind_framebuffer(glow::FRAMEBUFFER, fbo);
        gl.enable(glow::SCISSOR_TEST);
        gl.color_mask(false, false, false, true);
        // First force the WHOLE window opaque (alpha only): Skia's opacity-layer
        // compositing (chrome fades) corrupts the scene's alpha in the panel
        // regions, which the compositor renders as see-through flickers.
        gl.scissor(0, 0, w, h);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        // Then carve the corner arcs — with FULL colour mask: the compositor
        // treats the buffer as PREMULTIPLIED alpha (screen = RGB + dst·(1−A)),
        // so a "transparent" pixel must have RGB 0 too or its colour is ADDED
        // on top of the desktop (a movie-coloured ghost hugging the arc in
        // bright scenes). Boundary pixels get (rgb·cov, cov) — the scene's
        // OWN colour scaled by coverage. (0,0,0,cov) is premul-"valid" but
        // fades BLACK in rather than the scene out: over a light desktop it
        // drew a dark pencil arc hugging every corner ("splash of black at
        // the corner radius").
        gl.color_mask(true, true, true, true);
        if r > 0 && w >= 2 * r && h >= 2 * r {
            // Read the four r×r corner blocks once (before any carving) so the
            // AA pixels can sample the scene colour. Tiny reads (~18×18 px).
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 0);
            gl.pixel_store_i32(glow::PACK_SKIP_ROWS, 0);
            gl.pixel_store_i32(glow::PACK_SKIP_PIXELS, 0);
            let mut blocks = [const { Vec::new() }; 4];
            let origins = [(0, 0), (0, h - r), (w - r, 0), (w - r, h - r)];
            for (block, (bx, by)) in blocks.iter_mut().zip(origins) {
                *block = vec![0u8; (r * r * 4) as usize];
                gl.read_pixels(
                    bx,
                    by,
                    r,
                    r,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(block.as_mut_slice())),
                );
            }
            let rf = r as f32;
            for row in 0..r {
                let dyc = rf - row as f32 - 0.5;
                // length of the fully-outside run (coverage 0): distance from
                // the arc centre > r + 0.5
                let mut hard = 0;
                while hard < r {
                    let dxc = rf - hard as f32 - 0.5;
                    if (dxc * dxc + dyc * dyc).sqrt() < rf + 0.5 {
                        break;
                    }
                    hard += 1;
                }
                if hard > 0 {
                    gl.clear_color(0.0, 0.0, 0.0, 0.0);
                    for (x, y) in [
                        (0, row),
                        (0, h - 1 - row),
                        (w - hard, row),
                        (w - hard, h - 1 - row),
                    ] {
                        gl.scissor(x, y, hard, 1);
                        gl.clear(glow::COLOR_BUFFER_BIT);
                    }
                }
                // boundary pixels: the scene colour faded out by arc coverage
                // (ramps 0→1 over ~1px); each of the 4 mirrored pixels has its
                // own colour, sampled from the pre-read corner blocks.
                let mut x = hard;
                while x < r {
                    let dxc = rf - x as f32 - 0.5;
                    let cov = (rf + 0.5 - (dxc * dxc + dyc * dyc).sqrt()).clamp(0.0, 1.0);
                    if cov >= 1.0 {
                        break;
                    }
                    // (block idx, local x/y inside the block, framebuffer x/y)
                    for (bi, lx, ly, sx, sy) in [
                        (0usize, x, row, x, row),
                        (1, x, r - 1 - row, x, h - 1 - row),
                        (2, r - 1 - x, row, w - 1 - x, row),
                        (3, r - 1 - x, r - 1 - row, w - 1 - x, h - 1 - row),
                    ] {
                        let o = ((ly * r + lx) * 4) as usize;
                        let b = &blocks[bi];
                        gl.clear_color(
                            b[o] as f32 / 255.0 * cov,
                            b[o + 1] as f32 / 255.0 * cov,
                            b[o + 2] as f32 / 255.0 * cov,
                            cov,
                        );
                        gl.scissor(sx, sy, 1, 1);
                        gl.clear(glow::COLOR_BUFFER_BIT);
                    }
                    x += 1;
                }
            }
        }
        gl.disable(glow::SCISSOR_TEST);
        let prev_fb = NonZeroU32::new(prev as u32).map(glow::NativeFramebuffer);
        gl.bind_framebuffer(glow::FRAMEBUFFER, prev_fb);
    }
}

unsafe fn create_target(
    gl: &glow::Context,
    (w, h): (u32, u32),
) -> (glow::Framebuffer, glow::Texture, NonZeroU32) {
    // Opaque-black initial contents (not undefined GPU memory) so nothing
    // garbage-y is ever composited before mpv's first frame lands.
    let zeros = vec![0u8; w as usize * h as usize * 4];
    unsafe {
        let texture = gl.create_texture().expect("create texture");
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            w as i32,
            h as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&zeros)),
        );
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);

        let fbo = gl.create_framebuffer().expect("create fbo");
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture),
            0,
        );
        assert_eq!(
            gl.check_framebuffer_status(glow::FRAMEBUFFER),
            glow::FRAMEBUFFER_COMPLETE,
            "framebuffer incomplete"
        );

        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.bind_texture(glow::TEXTURE_2D, None);

        let tex_id = texture.0;
        (fbo, texture, tex_id)
    }
}
