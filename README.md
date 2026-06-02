# Soniq

A minimal, modern video player built with Rust, GTK4, libadwaita, and GStreamer.

Soniq aims for a clean, distraction-free playback experience: a floating
control bar that auto-hides, a self-contained binary with embedded branding, and
solid streaming support with a buffering indicator.

## Features

- **Local files and network streams** - play local video or stream directly from
  `http(s)://`, `rtsp://`, `rtmp://`, and `file://` URLs.
- **Hardware-accelerated decoding** via GStreamer (uses your platform's decoders,
  e.g. NVDEC where available).
- **Folder playlist** - opening a file scans its folder so Next / Previous and
  auto-advance just work, sorted by name.
- **Subtitles** - embedded tracks plus external files (`.srt`, `.ass`, `.ssa`,
  `.vtt`, `.sub`), with live-configurable font, colors, outline, shadow,
  background, and position. Handles UTF-8 and Windows-1256 (Persian/Arabic).
- **Customizable keyboard shortcuts** and configurable mouse click actions.

## Requirements

Soniq depends on GTK4, libadwaita, and GStreamer (with the GTK4 sink and the
common plugin sets). On Debian/Ubuntu:

```sh
sudo apt install \
    libgtk-4-dev libadwaita-1-dev \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    gstreamer1.0-gtk4 \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav
```

You also need a [Rust toolchain](https://rustup.rs/) (edition 2024, so a recent
stable compiler).

## Building and running

```sh
# Run a debug build
cargo run

# Open a file directly
cargo run -- /path/to/video.mp4

# Optimized release build
cargo build --release
./target/release/soniq
```

### Debian package

Packaging metadata is included for [`cargo-deb`](https://github.com/kornelski/cargo-deb):

```sh
cargo install cargo-deb
cargo deb
```

This produces a `.deb` that installs the binary, the `.desktop` entry, and the
app icon.

## Keyboard shortcuts

All shortcuts are remappable in **Preferences > Shortcuts**. Defaults:

| Action               | Key        |
| -------------------- | ---------- |
| Play / Pause         | `Space`    |
| Mute                 | `M`        |
| Toggle fullscreen    | `F`        |
| Seek backward 5 s    | `Left`     |
| Seek forward 5 s     | `Right`    |
| Seek backward 10 s   | `J`        |
| Seek forward 10 s    | `L`        |
| Volume up            | `Up`       |
| Volume down          | `Down`     |
| Jump to start        | `Home`     |
| Jump to end          | `End`      |
| Next file            | `N`        |
| Previous file        | `P`        |
| Open file...         | `Ctrl+O`   |
| Open URL...          | `Ctrl+L`   |

The control bar also has a **Stop** button that returns playback to the first
frame and pauses. By default a double-click on the video toggles fullscreen.

## Configuration

Settings are stored as JSON at:

```
$XDG_CONFIG_HOME/soniq/config.json   (falls back to ~/.config/soniq/config.json)
```

It holds the subtitle style, saved volume, custom shortcuts, mouse bindings, and
the debug-overlay toggle. The file is written on exit and read on launch; missing
fields fall back to defaults, so it is safe to edit or delete.

## License

MIT. See the `license` field in `Cargo.toml`.
