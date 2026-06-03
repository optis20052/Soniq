mod config;
mod effects;
mod handlers;
mod osd;
mod pipeline;
mod resume;
mod shine;
mod shortcuts;
mod state;
mod subtitles;
mod theme;
mod ui;
mod util;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use state::AppState;
use util::APP_ID;

/// The app icon (window/taskbar), embedded so the binary is self-contained.
const LOGO_SVG: &[u8] = include_bytes!("../assets/logo.svg");
/// The wordmark logo shown on the empty/welcome screen.
const WORDMARK_SVG: &[u8] = include_bytes!("../assets/soniq.svg");
/// Icon-theme name for the welcome-screen wordmark.
pub const WORDMARK_ICON: &str = "soniq-wordmark";

fn main() -> glib::ExitCode {
    gst::init().expect("Failed to initialize GStreamer");
    // NVDEC hardware decoding stays at its default (high) rank for smooth,
    // low-CPU 4K playback. Its seek-freeze bug is handled by the stall
    // watchdog (non-flushing pause/resume → Ready-cycle recovery) in
    // handlers.rs rather than by avoiding the decoder.

    // Soniq is a single-instance app (HANDLES_OPEN lets file-manager "Open
    // With Soniq" and `soniq file.mp4` deliver files via the `open` signal),
    // but it supports many windows inside that one process — like Files or a
    // browser. The two GApplication signals split cleanly:
    //
    //   * `activate` — fired on first launch, and again whenever the user asks
    //     for a fresh instance (shift/middle-click the dock icon, the dock's
    //     "New Window" action, or running `soniq` while it is already up). Each
    //     time we open a brand-new window. A plain click on a running icon just
    //     raises the existing window and never reaches here.
    //   * `open` — a file double-click / `soniq file.mp4`. We load it into the
    //     active (or most-recent) window so opening files reuses the window,
    //     creating one only when none exists yet.
    //
    // GtkApplication keeps the process alive while any window is open and quits
    // once the last one closes, so the window registry is all the bookkeeping
    // multi-window needs.
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    // Display-global setup that must run exactly once, before any window.
    app.connect_startup(|_| {
        ui::install_css();
        install_branding();
    });

    // Live windows, so `open` can target the right one and we can drop a window
    // when it closes. Shared by reference into the signal handlers.
    let windows: Rc<RefCell<Vec<Player>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let windows = windows.clone();
        app.connect_activate(move |app| {
            new_window(app, &windows);
        });
    }
    {
        let windows = windows.clone();
        app.connect_open(move |app, files, _hint| {
            let player = active_player(app, &windows)
                .unwrap_or_else(|| new_window(app, &windows));
            for file in files {
                (player.load_file)(file);
            }
            player.window.present();
        });
    }
    app.run()
}

/// A single player window plus the closure that loads a file into it.
#[derive(Clone)]
struct Player {
    window: adw::ApplicationWindow,
    load_file: Rc<dyn Fn(&gio::File)>,
}

/// Build a fresh, fully independent player window (its own state, pipeline and
/// handlers), register it, present it, and return a handle to it.
fn new_window(app: &adw::Application, windows: &Rc<RefCell<Vec<Player>>>) -> Player {
    let state = AppState::new();
    config::apply_to_state(&config::load(), &state);

    let pipe = pipeline::build_pipeline(&state);
    let ui = ui::build_ui(app, &pipe.paintable);
    let load_file = handlers::wire(&ui, &pipe, &state);

    let player = Player {
        window: ui.window.clone(),
        load_file,
    };
    windows.borrow_mut().push(player.clone());

    // Drop the window from the registry once it closes so it can be freed (and
    // so `open` never targets a dead window). GtkApplication quits the process
    // when its last window goes away.
    {
        let windows = windows.clone();
        let window_weak = ui.window.downgrade();
        ui.window.connect_close_request(move |_| {
            if let Some(closing) = window_weak.upgrade() {
                windows.borrow_mut().retain(|p| p.window != closing);
            }
            glib::Propagation::Proceed
        });
    }

    player.window.present();
    player
}

/// The player whose window is currently focused, falling back to the most
/// recently opened one. `None` only when no window is open at all.
fn active_player(app: &adw::Application, windows: &Rc<RefCell<Vec<Player>>>) -> Option<Player> {
    let windows = windows.borrow();
    if let Some(active) = app.active_window() {
        if let Some(p) = windows
            .iter()
            .find(|p| p.window.upcast_ref::<gtk::Window>() == &active)
        {
            return Some(p.clone());
        }
    }
    windows.last().cloned()
}

/// Register the embedded logo as the app icon so the window, taskbar, and the
/// empty-state branding all show it (referenced everywhere by APP_ID).
fn install_branding() {
    // Write the SVG into a runtime icon theme dir and add it to the search
    // path. Scalable icons need no cache, so this works immediately.
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME").ok().map(|h| {
                let mut p = std::path::PathBuf::from(h);
                p.push(".local/share");
                p
            })
        });
    if let Some(mut base) = base {
        base.push("soniq/icons");
        let apps = base.join("hicolor/scalable/apps");
        if std::fs::create_dir_all(&apps).is_ok() {
            let _ = std::fs::write(apps.join(format!("{APP_ID}.svg")), LOGO_SVG);
            let _ = std::fs::write(apps.join(format!("{WORDMARK_ICON}.svg")), WORDMARK_SVG);
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::IconTheme::for_display(&display).add_search_path(&base);
            }
        }
    }
    gtk::Window::set_default_icon_name(APP_ID);
}

