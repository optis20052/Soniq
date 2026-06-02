use gst::prelude::*;
use gst_app;
use gtk::gdk;
#[allow(unused_imports)]
use gtk::glib; // needed because gst::glib::closure! macro expands to `glib::` paths

use crate::state::AppState;

/// Handles returned by `build_pipeline` - the application drives them.
pub struct PipelineHandles {
    pub pipeline: gst::Element,
    /// The paintable from gtk4paintablesink, bound to the gtk::Picture in the UI.
    pub paintable: gdk::Paintable,
}

/// Build the playbin pipeline with gtk4paintablesink, tuned sinks, and the
/// queue2/source hooks needed for our buffer indicator.
///
/// We use the classic `playbin` (not playbin3): it has rock-solid external
/// subtitle support via `suburi` + `current-text`/`n-text`, whereas playbin3's
/// suburi path (separate urisourcebins) is racy and throws not-linked errors.
pub fn build_pipeline(state: &AppState) -> PipelineHandles {
    let pipeline = gst::ElementFactory::make("playbin")
        .build()
        .expect("playbin element is required (gstreamer1.0-plugins-base)");

    let sink = gst::ElementFactory::make("gtk4paintablesink")
        .build()
        .expect("gtk4paintablesink element is required (gstreamer1.0-gtk4)");

    // Don't drop "late" buffers after a seek - default 20 ms max-lateness
    // makes the picture freeze after many flushing seeks.
    sink.set_property("max-lateness", -1i64);
    sink.set_property("qos", false);

    let paintable: gdk::Paintable = sink.property("paintable");
    pipeline.set_property("video-sink", &sink);

    // Same hardening for the audio sink (tuned once the actual sink appears).
    let audio_sink = gst::ElementFactory::make("autoaudiosink")
        .build()
        .expect("autoaudiosink element is required (gstreamer1.0-plugins-good)");
    if let Some(bin) = audio_sink.dynamic_cast_ref::<gst::Bin>() {
        bin.connect_deep_element_added(|_, _, element| {
            if element.has_property("max-lateness", None) {
                element.set_property("max-lateness", -1i64);
            }
            if element.has_property("qos", None) {
                element.set_property("qos", false);
            }
        });
    }
    pipeline.set_property("audio-sink", &audio_sink);

    // Default flags WITHOUT +buffering. load_file adds +buffering only when
    // the URI is network (http/https/etc). For local files queue2/buffering
    // is pure overhead and was triggering watchdog stalls on disk reads.
    let _ = pipeline.set_property_from_str(
        "flags",
        "soft-colorbalance+deinterlace+soft-volume+text+audio+video",
    );

    // On-disk download cache. When the `download` play flag is on (network
    // streams), GStreamer's download buffer would default to g_get_tmp_dir()
    // which is often a small tmpfs (-> "Disk quota exceeded" on a large file).
    // We redirect it to a real cache dir and clear any leftovers from a
    // previous (crashed) run, so seeks within the watched range are served
    // from this file instead of re-downloading.
    let cache_template = download_cache_template();
    let queue_ref_setter = state.queue_ref.clone();
    let net_download = state.net_download.clone();
    let subs_overlay_setter = state.subtitles.overlay_ref.clone();
    let subs_style_setter = state.subtitles.style.clone();
    pipeline.connect_closure(
        "deep-element-added",
        false,
        gst::glib::closure!(move |_pb: gst::Element, _bin: gst::Bin, element: gst::Element| {
            use std::sync::atomic::Ordering;
            let factory_name = element
                .factory()
                .map(|f| f.name().to_string())
                .unwrap_or_default();

            // Redirect the download cache off the default tmpfs onto real disk.
            // `downloadbuffer` only exists in download mode; a `queue2` is the
            // download element only once it already has a temp-template (small
            // inline queue2s have none, so we never disk-back those). Gated on
            // net_download so local playback never touches disk.
            if let Some(ref template) = cache_template
                && net_download.load(Ordering::Relaxed)
                && element.has_property("temp-template", None)
            {
                let is_download_elem = factory_name == "downloadbuffer"
                    || (factory_name == "queue2"
                        && element
                            .property::<Option<String>>("temp-template")
                            .map(|t| !t.is_empty())
                            .unwrap_or(false));
                if is_download_elem {
                    element.set_property("temp-template", template.as_str());
                    if element.has_property("temp-remove", None) {
                        element.set_property("temp-remove", true);
                    }
                }
            }

            // Log decode/render/subtitle elements so we can confirm the active
            // decode path and whether the subtitle overlay is created.
            if factory_name.contains("dec")
                || factory_name.contains("glupload")
                || factory_name.contains("videoscale")
                || factory_name.contains("overlay")
                || factory_name.contains("subparse")
                || factory_name.contains("sub")
            {
                eprintln!("[chain] {} (factory={})", element.name(), factory_name);
            }

            if factory_name == "urisourcebin" {
                // Only enable buffering for network sources. For file:// URIs
                // disk reads are instant and queue2 is pure overhead.
                let uri: String = if element.has_property("uri", None) {
                    element.property("uri")
                } else {
                    String::new()
                };
                let is_network = !uri.is_empty() && !uri.starts_with("file://");
                if is_network {
                    if element.has_property("use-buffering", None) {
                        element.set_property("use-buffering", true);
                    }
                    if element.has_property("buffer-size", None) {
                        element.set_property("buffer-size", 128i32 * 1024 * 1024);
                    }
                    if element.has_property("buffer-duration", None) {
                        element.set_property("buffer-duration", 120i64 * 1_000_000_000);
                    }
                    // No ring-buffer-max-size: avoid disk spill (see queue2).
                }
            }

            // Hook the subtitle text renderer so we can restyle it live.
            if factory_name == "textoverlay" || factory_name == "subtitleoverlay" {
                // textoverlay is the actual renderer; subtitleoverlay wraps it.
                if factory_name == "textoverlay" {
                    crate::subtitles::set_overlay_style(
                        &element,
                        &subs_style_setter.lock().unwrap(),
                    );
                    *subs_overlay_setter.lock().unwrap() = Some(element.clone());
                }
            }

            if factory_name == "queue2" {
                // In-memory look-ahead only. We deliberately do NOT set
                // temp-template / ring-buffer-max-size: that spills to a disk
                // file (default /tmp, a small RAM-backed tmpfs) and a large
                // file fills it → "Disk quota exceeded" crash. 128 MiB of RAM
                // buffer is plenty of look-ahead without touching disk.
                if element.has_property("use-buffering", None) {
                    element.set_property("use-buffering", true);
                }
                if element.has_property("max-size-bytes", None) {
                    element.set_property("max-size-bytes", 128u32 * 1024 * 1024);
                }
                if element.has_property("max-size-time", None) {
                    element.set_property("max-size-time", 0u64);
                }
                if element.has_property("max-size-buffers", None) {
                    element.set_property("max-size-buffers", 0u32);
                }
                if element.has_property("low-watermark", None) {
                    element.set_property("low-watermark", 0.01f64);
                }
                if element.has_property("high-watermark", None) {
                    element.set_property("high-watermark", 0.99f64);
                }
                let mut slot = queue_ref_setter.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(element);
                }
            }
        }),
    );

    // Tune souphttpsrc for max single-connection throughput; also stash the
    // source so the UI can query total bytes from it.
    // Thread-safe closure! (not closure_local!): source-setup can fire on a
    // streaming thread during a live pipeline reload, and a thread-guarded
    // local closure would panic. All captures here are Send (Arc<Mutex>).
    let source_ref_setter = state.source_ref.clone();
    pipeline.connect_closure(
        "source-setup",
        false,
        gst::glib::closure!(move |_pb: gst::Element, source: gst::Element| {
            *source_ref_setter.lock().unwrap() = Some(source.clone());
            let factory_name = source
                .factory()
                .map(|f| f.name().to_string())
                .unwrap_or_default();
            if factory_name == "souphttpsrc" {
                source.set_property("keep-alive", true);
                source.set_property("compress", true);
                source.set_property("retries", 3i32);
                source.set_property("timeout", 15u32);
                if source.has_property("blocksize", None) {
                    source.set_property("blocksize", 262_144u32);
                }
                source.set_property(
                    "user-agent",
                    "Mozilla/5.0 (X11; Linux x86_64) Soniq/0.1",
                );
            }
        }),
    );

    // Custom text-sink: receive embedded subtitle buffers ourselves (instead
    // of playbin compositing them) so we render embedded + external subs with
    // one consistent style via our GTK label.
    let text_sink = gst_app::AppSink::builder()
        .sync(true)
        .max_buffers(2)
        .drop(true)
        .build();
    {
        let embedded_cue = state.subtitles.embedded_cue.clone();
        text_sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink
                        .pull_sample()
                        .map_err(|_| gst::FlowError::Eos)?;
                    if let Some(buffer) = sample.buffer()
                        && let Ok(map) = buffer.map_readable()
                    {
                        let text = String::from_utf8_lossy(&map);
                        let text = crate::subtitles::strip_tags(text.trim());
                        let start = buffer.pts().map(|p| p.nseconds()).unwrap_or(0);
                        let dur = buffer.duration().map(|d| d.nseconds()).unwrap_or(0);
                        let end = if dur > 0 { start + dur } else { start + 5_000_000_000 };
                        if !text.is_empty() {
                            *embedded_cue.lock().unwrap() = Some((start, end, text));
                        }
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    }
    pipeline.set_property("text-sink", text_sink.upcast::<gst::Element>());

    PipelineHandles { pipeline, paintable }
}

/// The on-disk download-cache directory: `$XDG_CACHE_HOME/soniq` (falling back
/// to `~/.cache/soniq`). `None` if neither env var is set.
fn download_cache_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
    Some(base.join("soniq"))
}

/// Delete every `stream-cache-*` file in the cache directory. The downloadbuffer
/// removes its own file on a clean stop (`temp-remove`), but a crash can orphan
/// a multi-GB file; we call this on startup and on shutdown so a stale cache
/// never lingers.
pub fn clear_download_cache() {
    let Some(dir) = download_cache_dir() else { return };
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("stream-cache-")
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Prepare the on-disk download-cache location and return an mkstemp-style
/// template (".../stream-cache-XXXXXX") for queue2/downloadbuffer's
/// `temp-template`. Creates the cache dir and clears any stale cache files left
/// by a previous run. Returns `None` if no cache dir could be created, in which
/// case we leave the default (tmpfs) location untouched.
fn download_cache_template() -> Option<String> {
    let dir = download_cache_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    clear_download_cache();
    Some(dir.join("stream-cache-XXXXXX").to_string_lossy().into_owned())
}
