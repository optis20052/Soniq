slint::include_modules!();

mod config;
mod diagnostics;
mod handlers;
mod ipc;
mod prefs;
mod render;
mod shortcuts;
mod store;
mod subs;
mod util;
mod video;
mod wl_dnd;
mod wl_opaque;

use util::{fit_window_size, probe_video_size};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse args: a positional file plus an optional `--new-window` flag (also
    // honoured via SONIQ_NEW_WINDOW) that forces a fresh window even when an
    // instance is already running.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let force_new =
        args.iter().any(|a| a == "--new-window") || std::env::var("SONIQ_NEW_WINDOW").is_ok();
    let cli_file = args.into_iter().find(|a| !a.starts_with("--"));

    // Single-instance coordination: hand a file to the running instance, or run a
    // new window, or become the primary (see ipc.rs). `Exit` = handed off already.
    let serve_listener = match ipc::coordinate(cli_file.as_deref(), force_new) {
        ipc::Launch::Exit => return Ok(()),
        ipc::Launch::Primary(l) => Some(l),
        ipc::Launch::Standalone => None,
    };
    let is_primary = serve_listener.is_some();

    // If launched with a (local) file, probe its size first so we can create the
    // window already at the video's aspect — resizing a *live* GL window flashes
    // black (a known winit/glutin limitation), so we avoid the resize entirely by
    // sizing up front, launcher-style. SONIQ_NO_PRESIZE exercises the in-app path.
    let initial_size = cli_file
        .as_deref()
        .filter(|p| !p.contains("://"))
        .filter(|_| std::env::var("SONIQ_NO_PRESIZE").is_err())
        .and_then(probe_video_size)
        .map(fit_window_size);

    // Platform window styles:
    // - Linux: client-side decorated — frameless, transparent surface, our own
    //   punched rounded corners + chrome (SONIQ_OPAQUE=1 falls back to opaque).
    // - macOS: NATIVE decorations — all CSD machinery off via alpha-surface=false.
    let native_decor = cfg!(target_os = "macos");
    let force_opaque = std::env::var("SONIQ_OPAQUE").is_ok() || native_decor;
    // Must equal the installed .desktop basename so the compositor associates the
    // window with it (correct app name + icon in the dock / task switcher).
    const APP_ID: &str = "io.github.alisp.Soniq";
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(move |attrs| {
            let attrs = attrs.with_transparent(!force_opaque);
            // The hook runs after Slint's own attributes — overrides `no-frame`.
            let attrs = if native_decor { attrs.with_decorations(true) } else { attrs };
            // Wayland app_id / X11 WM_CLASS → match io.github.alisp.Soniq.desktop.
            #[cfg(target_os = "linux")]
            let attrs = {
                use slint::winit_030::winit::platform::startup_notify::WindowAttributesExtStartupNotify;
                use slint::winit_030::winit::platform::wayland::WindowAttributesExtWayland;
                use slint::winit_030::winit::platform::x11::WindowAttributesExtX11;
                use slint::winit_030::winit::window::ActivationToken;
                let attrs = WindowAttributesExtWayland::with_name(attrs, APP_ID, "");
                let attrs = WindowAttributesExtX11::with_name(attrs, APP_ID, "");
                // Consume the launcher's startup-notification token so GNOME drops
                // the "launching" busy cursor the moment our window maps — without
                // this it spins until the ~20s startup-notify timeout. Strip it from
                // the env so child processes (mpv, --new-window spawns) don't inherit
                // a spent token.
                let token = std::env::var("XDG_ACTIVATION_TOKEN")
                    .ok()
                    .or_else(|| std::env::var("DESKTOP_STARTUP_ID").ok());
                if let Some(tok) = token {
                    // SAFETY: single-threaded here (window not yet created).
                    unsafe {
                        std::env::remove_var("XDG_ACTIVATION_TOKEN");
                        std::env::remove_var("DESKTOP_STARTUP_ID");
                    }
                    attrs.with_activation_token(ActivationToken::from_raw(tok))
                } else {
                    attrs
                }
            };
            attrs
        })
        .select()?;

    let app = App::new()?;
    app.set_alpha_surface(!force_opaque);
    // Set the probed size before the window is shown so it never resizes.
    if let Some((w, h)) = initial_size {
        app.window().set_size(slint::LogicalSize::new(w, h));
    }
    // Screenshot / diagnostic env hooks (no-op in normal use).
    if std::env::var("SONIQ_SHOT").is_ok() {
        app.window()
            .set_position(slint::LogicalPosition::new(120.0, 90.0));
    }
    if std::env::var("SONIQ_SHOT_DRAWER").is_ok() {
        app.set_has_video(true);
        app.set_drawer_open(true);
    }
    if std::env::var("SONIQ_SHOT_URL").is_ok() {
        app.set_url_open(true);
    }

    // main is just the composition root: build state, wire callbacks, install the
    // render bridge + housekeeping, launch the CLI file. Logic lives in handlers.
    let ctx = handlers::build(&app);
    let set_fullscreen = handlers::wire(&app, &ctx);
    render::install(&app, ctx.render_deps());
    handlers::housekeeping(&app, &ctx);
    handlers::launch_cli(&ctx, cli_file);

    // As the primary, serve file handoffs from later launches into this window.
    if let Some(listener) = serve_listener {
        ipc::serve(listener, app.as_weak());
    }

    // Scripted UI diagnostics (no-op unless a SONIQ_* var is set).
    diagnostics::install(&app, &set_fullscreen);

    app.run()?;

    // Only the primary owns the socket file; remove it on a clean exit.
    if is_primary {
        ipc::cleanup();
    }
    Ok(())
}
