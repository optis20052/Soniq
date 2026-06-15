# Soniq

A minimal, modern, cross-platform video player built with **Rust**, **[Slint](https://slint.dev)**,
and **[libmpv](https://mpv.io)**.

Soniq pairs a clean, distraction-free UI — a custom-decorated window, a floating
control bar that auto-hides, and a shine-swept wordmark home screen — with mpv's
best-in-class playback engine. The whole interface is GPU-composited in a single
small native binary: no Electron, no webview, one codebase for Linux, macOS, and
Windows.

> Soniq was originally a GTK4/libadwaita + GStreamer app (Linux-only). It has been
> rewritten on Slint + libmpv for portability and a custom UI; that rewrite is now
> the mainline and the GTK version is retired (it lives in git history).

## Features

- **Local files and network streams** — play local video or stream directly from
  `http(s)://`, `rtsp://`, `rtmp://`, `file://`, and magnet/torrent-style URLs.
  The Open-URL dialog accepts **batch input**: paste many links at once and they
  queue up.
- **Hardware-accelerated decoding** via libmpv (uses your platform's decoders —
  NVDEC / VA-API / VideoToolbox / D3D11 where available).
- **Native video compositing** — mpv renders each frame into an off-screen
  OpenGL FBO that is handed to Slint as a borrowed GL texture, so video and UI
  composite together on the GPU with no copies or shader hacks.
- **Folder playlist + queue** — opening a file scans its folder; Next / Previous
  and auto-advance just work, and a slide-in **Queue** panel lists the folder's
  files with the current one highlighted.
- **Subtitles** — embedded tracks plus external files (`.srt`, `.ass`, `.ssa`,
  `.vtt`, `.sub`), with live-configurable font, colours, outline, shadow,
  background box, and position. Handles UTF-8 and **Windows-1256** (Persian /
  Arabic), with correct bidi/punctuation placement for RTL scripts.
- **Resume** — Off / Ask / Always, with a recently-played list (per-row resume
  time, remove, and finished-pruning).
- **Customizable keyboard shortcuts** and **configurable mouse actions** for
  single-, double-, and right-click on the video.
- **Drag-and-drop** (drop a video to play, a subtitle to attach), **OSD toasts**
  for seek/volume/mute/pause, a **buffering** indicator, and seek-bar hover-time
  preview.
- **Modern Preferences** — searchable settings covering playback, shortcuts,
  subtitle styling, and mouse actions.

## Requirements

Soniq links against **libmpv** (discovered via `pkg-config`) and needs a GPU with
working OpenGL. You also need a [Rust toolchain](https://rustup.rs/) (edition
2024, i.e. a recent stable compiler).

**Debian / Ubuntu:**

```sh
sudo apt install libmpv-dev pkg-config
# runtime: a Wayland or X11 session with OpenGL drivers
```

**macOS (Homebrew):**

```sh
brew install mpv pkg-config
```

**Windows:** install an `mpv`/`libmpv` development package and make sure its
`.pc` file is on `PKG_CONFIG_PATH` (e.g. via MSYS2 `mingw-w64-x86_64-mpv`).

## Building and running

```sh
# Debug build
cargo run

# Open a file directly
cargo run -- /path/to/video.mp4

# Optimized release build
cargo build --release
./target/release/soniq
```

The UI is defined in `ui/app.slint` and compiled by `build.rs` (via
`slint-build`); `build.rs` also probes for libmpv.

### Debian package

Packaging metadata is included for [`cargo-deb`](https://github.com/kornelski/cargo-deb):

```sh
cargo install cargo-deb
cargo deb
```

This produces a `.deb` that installs the binary, the `.desktop` entry, and the
app icon. Runtime library dependencies are auto-detected from the linked binary.

## Keyboard shortcuts

All shortcuts are remappable in **Preferences → Shortcuts**. Defaults:

| Action              | Key      |
| ------------------- | -------- |
| Play / Pause        | `Space`  |
| Mute                | `M`      |
| Toggle fullscreen   | `F`      |
| Seek backward 5 s   | `Left`   |
| Seek forward 5 s    | `Right`  |
| Seek backward 10 s  | `J`      |
| Seek forward 10 s   | `L`      |
| Volume up           | `Up`     |
| Volume down         | `Down`   |
| Jump to start       | `Home`   |
| Jump to end         | `End`    |
| Next file           | `N`      |
| Previous file       | `P`      |
| Open file…          | `Ctrl+O` |
| Open URL…           | `Ctrl+L` |

The control bar also has a **Stop** button that returns to the first frame and
pauses. Mouse actions on the video are configurable in **Preferences → Mouse on
video** — defaults: single-click toggles the controls, double-click toggles
fullscreen, right-click toggles Play / Pause.

## Configuration

Settings and the recents store are JSON files under your platform config dir:

```
$XDG_CONFIG_HOME/soniq-spike/config.json   (Linux, ~/.config/soniq-spike/…)
~/Library/Application Support/soniq-spike/  (macOS)
%APPDATA%\soniq-spike\                      (Windows)
```

`config.json` holds subtitle style, saved volume/mute, resume mode, custom
shortcuts, and mouse-action bindings; `store.json` holds the recently-played
list. Both are written on exit and read on launch; missing fields fall back to
defaults, so they're safe to edit or delete.

## Project layout

```
src/        Rust — composition root (main.rs) + controller (handlers.rs),
            the mpv↔GL bridge (video.rs, render.rs), prefs/shortcuts/subs,
            config & recents persistence, and Linux Wayland helpers
            (wl_dnd.rs drag-drop + clipboard, wl_opaque.rs opaque region).
ui/         Slint UI (app.slint) and monochrome SVG icons.
assets/     Logo / wordmark artwork.
packaging/  Freedesktop .desktop entry.
build.rs    Compiles the Slint UI and probes libmpv via pkg-config.
```

## License

MIT. See the `license` field in `Cargo.toml`.
