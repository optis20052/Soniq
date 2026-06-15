//! Single-instance coordination + cross-process file handoff.
//!
//! GNOME (and most launchers) re-run our `Exec` for "Open With <file>", for a
//! Shift/middle-click on the dock icon, and for the desktop "New Window" action.
//! Without coordination every one of those spawns a fresh process. We want:
//!   - `soniq <file>` while an instance is running → hand the file to it (open in
//!     the existing window, don't spawn another).
//!   - bare `soniq` while running (Shift+click / New Window) → a NEW window.
//!   - the first launch → become the primary that serves the above.
//!
//! The primary listens on a per-user Unix socket; later launches connect to it.

#[cfg(unix)]
mod imp {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::App;

    /// Per-user socket, in the runtime dir (cleared on logout) when available.
    fn socket_path() -> PathBuf {
        dirs::runtime_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("soniq.sock")
    }

    pub enum Launch {
        /// We own the instance — serve handoffs on this listener.
        Primary(UnixListener),
        /// Run as an independent window; don't serve handoffs.
        Standalone,
        /// Handed our file to the running instance — the caller should exit.
        Exit,
    }

    /// Decide how this launch should behave (see module docs).
    pub fn coordinate(file: Option<&str>, force_new: bool) -> Launch {
        if force_new {
            return Launch::Standalone;
        }
        let path = socket_path();
        match UnixStream::connect(&path) {
            // A primary is already listening.
            Ok(mut stream) => match file {
                Some(f) => {
                    // Hand off the absolute path, then let the caller exit.
                    let abs = std::fs::canonicalize(f)
                        .ok()
                        .and_then(|p| p.to_str().map(String::from))
                        .unwrap_or_else(|| f.to_string());
                    let _ = stream.write_all(abs.as_bytes());
                    let _ = stream.flush();
                    Launch::Exit
                }
                // Bare launch while running = an explicit "new window" request.
                None => Launch::Standalone,
            },
            // No live primary — clear any stale socket and claim the role.
            Err(_) => {
                let _ = std::fs::remove_file(&path);
                match UnixListener::bind(&path) {
                    Ok(l) => Launch::Primary(l),
                    Err(_) => Launch::Standalone, // lost a startup race; just run
                }
            }
        }
    }

    /// Spawn the accept loop: each connection delivers a path, handed to the UI
    /// thread to open in this (the primary) window.
    pub fn serve(listener: UnixListener, app: slint::Weak<App>) {
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { continue };
                // Don't let a stuck client wedge the accept loop.
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buf = String::new();
                if stream.read_to_string(&mut buf).is_err() {
                    continue;
                }
                let path = buf.trim().to_string();
                if path.is_empty() {
                    continue;
                }
                let app = app.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(a) = app.upgrade() {
                        a.invoke_external_open(path.into());
                    }
                });
            }
        });
    }

    /// Remove the socket file — call on the primary's clean exit.
    pub fn cleanup() {
        let _ = std::fs::remove_file(socket_path());
    }
}

#[cfg(unix)]
pub use imp::{cleanup, coordinate, serve, Launch};

// Non-unix (Windows): no Unix-socket single-instance yet — every launch is its
// own window. Stubs keep `main` cross-platform.
#[cfg(not(unix))]
mod imp {
    use crate::App;

    pub enum Launch {
        Primary(()),
        Standalone,
        Exit,
    }
    pub fn coordinate(_file: Option<&str>, _force_new: bool) -> Launch {
        Launch::Standalone
    }
    pub fn serve(_listener: (), _app: slint::Weak<App>) {}
    pub fn cleanup() {}
}

#[cfg(not(unix))]
pub use imp::{cleanup, coordinate, serve, Launch};
