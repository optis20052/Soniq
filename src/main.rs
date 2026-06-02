mod config;
mod handlers;
mod osd;
mod pipeline;
mod shortcuts;
mod state;
mod subtitles;
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

    // HANDLES_OPEN so file-manager "Open With Soniq" and `soniq file.mp4`
    // deliver the file(s) to us via the `open` signal.
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    // The window + its load_file closure are built lazily on the first
    // activate/open and reused afterwards (single-instance app).
    let player: Rc<RefCell<Option<Player>>> = Rc::new(RefCell::new(None));

    {
        let player = player.clone();
        app.connect_activate(move |app| {
            ensure_player(app, &player); // just show the window (no file)
        });
    }
    {
        let player = player.clone();
        app.connect_open(move |app, files, _hint| {
            let p = ensure_player(app, &player);
            if let Some(file) = files.first() {
                (p.load_file)(file);
            }
        });
    }
    app.run()
}

/// The built UI: the window plus the closure to load a file into it.
struct Player {
    window: adw::ApplicationWindow,
    load_file: Rc<dyn Fn(&gio::File)>,
}

/// Build the player UI once (storing it in `slot`) and present the window.
/// Returns a lightweight handle (clones of the window + load_file).
fn ensure_player(app: &adw::Application, slot: &Rc<RefCell<Option<Player>>>) -> Player {
    if slot.borrow().is_none() {
        ui::install_css();
        install_branding();

        let state = AppState::new();
        config::apply_to_state(&config::load(), &state);

        let pipe = pipeline::build_pipeline(&state);
        let ui = ui::build_ui(app, &pipe.paintable);
        let load_file = handlers::wire(&ui, &pipe, &state);

        *slot.borrow_mut() = Some(Player {
            window: ui.window.clone(),
            load_file,
        });
    }
    let player = slot.borrow();
    let player = player.as_ref().unwrap();
    player.window.present();
    Player {
        window: player.window.clone(),
        load_file: player.load_file.clone(),
    }
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

