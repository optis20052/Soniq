//! Small standalone helpers: time formatting, file-name extraction, track
//! labels, headless video probing, default window sizing and font enumeration.
//! None of these touch the running UI state, so they live apart from `main`.

use std::time::Duration;

use crate::video::TrackData;

pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as i64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

pub fn track_label(t: &TrackData) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !t.lang.is_empty() {
        parts.push(t.lang.to_uppercase());
    }
    if !t.title.is_empty() {
        parts.push(t.title.clone());
    }
    if parts.is_empty() {
        format!("Track {}", t.id)
    } else {
        format!("{}  (#{})", parts.join(" · "), t.id)
    }
}

/// Probe a local file's native video dimensions with a throwaway headless mpv
/// (no window, no GPU, no audio) so we can size the window to the video *before*
/// it's created — avoiding the black resize-flash you get from resizing a live
/// GL window (a known winit/glutin limitation that GTK4 sidesteps with its own
/// compositor). Returns None for streams, audio-only, or on timeout.
pub fn probe_video_size(path: &str) -> Option<(u32, u32)> {
    use libmpv2::Mpv;
    let mpv = Mpv::with_initializer(|init| {
        init.set_option("vo", "null")?;
        init.set_option("ao", "null")
    })
    .ok()?;
    let _ = mpv.set_property("hwdec", "no");
    let _ = mpv.set_property("pause", true);
    mpv.command("loadfile", &[path]).ok()?;
    for _ in 0..200 {
        let dw: i64 = mpv.get_property("dwidth").unwrap_or(0);
        let dh: i64 = mpv.get_property("dheight").unwrap_or(0);
        if dw > 0 && dh > 0 {
            return Some((dw as u32, dh as u32));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

/// Fit a video's pixel size into a sensible default window box (logical px),
/// preserving aspect and never upscaling beyond native.
pub fn fit_window_size((w, h): (u32, u32)) -> (f32, f32) {
    let scale = (1280.0 / w as f64).min(760.0 / h as f64).min(1.0);
    (
        ((w as f64 * scale).round() as f32).max(480.0),
        ((h as f64 * scale).round() as f32).max(320.0),
    )
}

/// Read the system clipboard as UTF-8 text. Slint's own clipboard is unreliable
/// on the `unstable-winit-030` path (winit 0.30 dropped built-in clipboard), so
/// we shell out to the platform's standard clipboard tool. Returns None if no
/// tool is available or the clipboard is empty/non-text.
pub fn read_clipboard() -> Option<String> {
    // Ask for TEXT specifically — without this, `wl-paste`/`xclip` happily return
    // an IMAGE on the clipboard as raw bytes, and feeding megabytes of binary into
    // the text field hangs the layout (the "paste an image → app freezes" crash).
    //
    // Pick the tool by session type. On Wayland use ONLY `wl-paste`: the X11 tools
    // (`xclip`/`xsel`) spin up a transient Xwayland client whose window flashes in
    // the dock on every read, and they're never needed when wl-paste is present.
    // When an image is on the clipboard wl-paste yields no text — we return None
    // rather than falling through to X11 (which is what caused the dock flash).
    #[cfg(target_os = "linux")]
    let candidates: &[(&str, &[&str])] = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[("wl-paste", &["-t", "text/plain", "-n"])]
    } else {
        &[
            ("xclip", &["-selection", "clipboard", "-o", "-t", "UTF8_STRING"]),
            ("xsel", &["-b"]),
        ]
    };
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbpaste", &[])];
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] = &[("powershell", &["-NoProfile", "-Command", "Get-Clipboard"])];

    // Cap: clipboard text larger than this isn't a sane URL/credential paste, and
    // huge strings stall the layout. (Generous: ~300 long links.)
    const MAX: usize = 256 * 1024;

    for (cmd, args) in candidates {
        if let Ok(out) = std::process::Command::new(cmd).args(*args).output() {
            if out.status.success() && out.stdout.len() <= MAX {
                // Strict UTF-8: reject anything that isn't valid text (binary).
                if let Ok(s) = String::from_utf8(out.stdout) {
                    let s = s.trim().to_string();
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

/// Whether a string is plausibly a playable target — a URL with a scheme, a
/// magnet link, or an existing local file. Used to reject garbage like "SS"
/// before switching to the player and letting mpv fail.
pub fn looks_playable(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && (s.contains("://") || s.starts_with("magnet:") || std::path::Path::new(s).exists())
}

/// Write `text` to the OS clipboard (for the context-menu Cut/Copy). Slint's own
/// clipboard is dead on this backend, so we pipe to the platform clipboard tool.
pub fn write_clipboard(text: &str) {
    // Same session split as read: on Wayland use only wl-copy (no Xwayland flash).
    #[cfg(target_os = "linux")]
    let candidates: &[(&str, &[&str])] = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[("wl-copy", &[])]
    } else {
        &[("xclip", &["-selection", "clipboard"]), ("xsel", &["-bi"])]
    };
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] = &[("clip", &[])];

    for (cmd, args) in candidates {
        let spawned = std::process::Command::new(cmd)
            .args(*args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut child) = spawned {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(text.as_bytes());
                // stdin dropped here → EOF, so the tool reads and exits/daemonizes.
            }
            let _ = child.wait();
            return;
        }
    }
}

/// Inject optional HTTP basic-auth credentials into a URL's authority (IINA-style
/// optional sign-in): `https://host/path` + (user, pass) → `https://user:pass@host/path`,
/// with user/pass percent-encoded. No-op when `user` is empty, when there's no
/// `scheme://`, or when the URL already carries userinfo.
pub fn apply_credentials(url: &str, user: &str, pass: &str) -> String {
    let user = user.trim();
    if user.is_empty() {
        return url.to_string();
    }
    let Some(i) = url.find("://") else {
        return url.to_string();
    };
    let scheme = &url[..i + 3];
    let rest = &url[i + 3..];
    // Already has userinfo (something before an '@' in the authority)? leave it.
    let authority_end = rest.find('/').unwrap_or(rest.len());
    if rest[..authority_end].contains('@') {
        return url.to_string();
    }
    if pass.is_empty() {
        format!("{scheme}{}@{rest}", encode_userinfo(user))
    } else {
        format!("{scheme}{}:{}@{rest}", encode_userinfo(user), encode_userinfo(pass))
    }
}

/// Percent-encode a userinfo component (RFC 3986 unreserved set passes through).
fn encode_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// All installed font family names, sorted and de-duplicated, for the subtitle
/// font picker (mpv/libass resolves these by name). Uses fontdb's directory
/// scan — cross-platform (Linux font dirs, macOS, Windows), no native API.
pub fn system_font_families() -> Vec<String> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut names: Vec<String> = db
        .faces()
        .filter_map(|f| f.families.first().map(|(name, _)| name.clone()))
        .collect();
    names.sort_by_key(|s| s.to_lowercase());
    names.dedup();
    names
}
