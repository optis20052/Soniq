//! Wayland file drag-and-drop for the spike.
//!
//! winit 0.30 only emits `WindowEvent::DroppedFile` from its **X11** backend —
//! the Wayland backend has no `wl_data_device` implementation, so dropping a
//! file onto the window does nothing on a Wayland session (which is what the
//! user runs). We implement DnD ourselves over the `wl_data_device` protocol,
//! reusing winit's own Wayland connection (the same foreign-display trick as
//! `wl_opaque`). Because the whole tree builds `wayland-backend` with
//! `client_system`, winit talks to libwayland, libwayland owns the socket
//! reads and demultiplexes incoming events to *every* event queue on the
//! connection — including the parallel queue we create here. So pumping our
//! queue with `dispatch_pending` each frame delivers the drag/drop events.
//!
//! Flow: on `enter` we accept the `text/uri-list` mime and the copy action; on
//! `drop` we `receive()` the data over a pipe and read it (non-blocking, across
//! frames) into a `file://` URI list, which `pump()` returns as plain paths.

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;
    use std::io::Read;
    use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

    use slint::winit_030::WinitWindowAccessor;
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::{
        wl_data_device::{self, WlDataDevice},
        wl_data_device_manager::{DndAction, WlDataDeviceManager},
        wl_data_offer::{self, WlDataOffer},
        wl_registry,
        wl_seat::{self, WlSeat},
    };
    use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};

    const URI_MIME: &str = "text/uri-list";

    #[derive(Default)]
    struct State {
        // mime types advertised per offer, keyed by the offer's protocol id
        offer_mimes: HashMap<u32, Vec<String>>,
        // the offer currently hovering the surface + its `enter` serial
        current: Option<WlDataOffer>,
        // negotiated wl_data_device version (set_actions/finish need >= 3)
        dd_version: u32,
        // in-progress read of a dropped uri-list (pipe + accumulated bytes +
        // the offer kept alive until the transfer finishes)
        reading: Option<(std::fs::File, Vec<u8>, WlDataOffer)>,
        // finished, parsed file paths waiting to be drained by pump()
        dropped: Vec<String>,
        // the current CLIPBOARD selection offer (kept alive so we can read it on
        // paste). Reading the clipboard natively over this — instead of shelling
        // out to wl-paste — avoids spawning any subprocess at all.
        selection: Option<WlDataOffer>,
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    impl Dispatch<WlSeat, ()> for State {
        fn event(
            _: &mut Self,
            _: &WlSeat,
            _: wl_seat::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    impl Dispatch<WlDataDeviceManager, ()> for State {
        fn event(
            _: &mut Self,
            _: &WlDataDeviceManager,
            _: <WlDataDeviceManager as Proxy>::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<WlDataOffer, ()> for State {
        fn event(
            state: &mut Self,
            offer: &WlDataOffer,
            event: wl_data_offer::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            // Accumulate the mime types the source advertises for this offer.
            if let wl_data_offer::Event::Offer { mime_type } = event {
                state
                    .offer_mimes
                    .entry(offer.id().protocol_id())
                    .or_default()
                    .push(mime_type);
            }
        }
    }

    impl Dispatch<WlDataDevice, ()> for State {
        fn event(
            state: &mut Self,
            _: &WlDataDevice,
            event: wl_data_device::Event,
            _: &(),
            conn: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                // A new offer was introduced; its Offer (mime) events follow.
                wl_data_device::Event::DataOffer { id } => {
                    state.offer_mimes.entry(id.id().protocol_id()).or_default();
                }
                // The drag entered our surface. Accept the uri-list mime and the
                // copy action so the compositor shows a "copy" cursor and will
                // deliver a Drop (without an accept it sends "no-drop" and no
                // drop event arrives).
                wl_data_device::Event::Enter { serial, id, .. } => {
                    if let Some(offer) = id {
                        let oid = offer.id().protocol_id();
                        let has_uri = state
                            .offer_mimes
                            .get(&oid)
                            .is_some_and(|m| m.iter().any(|t| t == URI_MIME));
                        if has_uri {
                            if state.dd_version >= 3 {
                                offer.set_actions(DndAction::Copy, DndAction::Copy);
                            }
                            offer.accept(serial, Some(URI_MIME.to_string()));
                            state.current = Some(offer);
                        } else {
                            offer.accept(serial, None);
                            offer.destroy();
                            state.offer_mimes.remove(&oid);
                        }
                        let _ = conn.flush();
                    }
                }
                wl_data_device::Event::Leave => {
                    if let Some(offer) = state.current.take() {
                        state.offer_mimes.remove(&offer.id().protocol_id());
                        offer.destroy();
                        let _ = conn.flush();
                    }
                }
                wl_data_device::Event::Motion { .. } => {}
                // Released over our surface: pull the uri-list through a pipe.
                wl_data_device::Event::Drop => {
                    if let Some(offer) = state.current.take() {
                        if let Some((read_fd, write_fd)) = make_pipe() {
                            // SAFETY: write_fd is a fresh, owned pipe end.
                            let wb = unsafe { BorrowedFd::borrow_raw(write_fd.as_raw_fd()) };
                            offer.receive(URI_MIME.to_string(), wb);
                            // Drop our write end so the reader sees EOF once the
                            // source has written everything.
                            drop(write_fd);
                            if state.dd_version >= 3 {
                                offer.finish();
                            }
                            let _ = conn.flush();
                            set_nonblocking(read_fd.as_raw_fd());
                            let file = std::fs::File::from(read_fd);
                            state.reading = Some((file, Vec::new(), offer));
                        } else {
                            offer.destroy();
                        }
                    }
                }
                wl_data_device::Event::Selection { id } => {
                    // The clipboard selection changed (or we just gained focus).
                    // Keep the offer + its advertised mimes so paste can read it
                    // natively; destroy the previous one we were holding.
                    if let Some(old) = state.selection.take() {
                        state.offer_mimes.remove(&old.id().protocol_id());
                        old.destroy();
                    }
                    state.selection = id;
                }
                _ => {}
            }
        }

        // wl_data_device.data_offer (opcode 0) creates a wl_data_offer child;
        // tell wayland-client how to build it and where to dispatch its events.
        event_created_child!(State, WlDataDevice, [
            wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
        ]);
    }

    pub struct DndListener {
        conn: Connection,
        queue: wayland_client::EventQueue<State>,
        state: State,
        // Kept alive for the duration; their proxies live on `queue`.
        _seat: WlSeat,
        _manager: WlDataDeviceManager,
        _data_device: WlDataDevice,
    }

    impl DndListener {
        /// Hook onto winit's Wayland connection. Returns None off-Wayland or if
        /// the compositor lacks a data-device manager / seat.
        pub fn attach(window: &slint::Window) -> Option<Self> {
            window
                .with_winit_window(|w| {
                    use slint::winit_030::winit::raw_window_handle::{
                        HasDisplayHandle, RawDisplayHandle,
                    };
                    let display = match w.display_handle().ok()?.as_raw() {
                        RawDisplayHandle::Wayland(d) => d.display.as_ptr(),
                        _ => return None,
                    };
                    let backend = unsafe {
                        wayland_backend::client::Backend::from_foreign_display(display.cast())
                    };
                    let conn = Connection::from_backend(backend);
                    let (globals, queue) = registry_queue_init::<State>(&conn).ok()?;
                    let qh = queue.handle();

                    let seat: WlSeat = globals.bind(&qh, 1..=7, ()).ok()?;
                    let manager: WlDataDeviceManager = globals.bind(&qh, 1..=3, ()).ok()?;
                    let dd_version = manager.version();
                    let data_device = manager.get_data_device(&seat, &qh, ());
                    let _ = conn.flush();

                    Some(DndListener {
                        conn,
                        queue,
                        state: State { dd_version, ..State::default() },
                        _seat: seat,
                        _manager: manager,
                        _data_device: data_device,
                    })
                })
                .flatten()
        }

        /// Process any buffered drag/drop events and advance an in-progress
        /// transfer. Returns file paths from any drop that completed this tick.
        pub fn pump(&mut self) -> Vec<String> {
            // Non-blocking: only processes events libwayland already read for
            // this queue (winit's reads fill it); never touches the socket.
            let _ = self.queue.dispatch_pending(&mut self.state);
            let _ = self.conn.flush();

            // Advance a dropped uri-list read without blocking the frame.
            let mut done: Option<Vec<u8>> = None;
            if let Some((file, buf, _)) = self.state.reading.as_mut() {
                let mut tmp = [0u8; 4096];
                loop {
                    match file.read(&mut tmp) {
                        Ok(0) => {
                            done = Some(std::mem::take(buf));
                            break;
                        }
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(_) => {
                            done = Some(std::mem::take(buf));
                            break;
                        }
                    }
                }
            }
            if let Some(bytes) = done {
                if let Some((_, _, offer)) = self.state.reading.take() {
                    self.state.offer_mimes.remove(&offer.id().protocol_id());
                    offer.destroy();
                    let _ = self.conn.flush();
                }
                for line in String::from_utf8_lossy(&bytes).lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if let Some(path) = uri_to_path(line) {
                        self.state.dropped.push(path);
                    }
                }
            }

            std::mem::take(&mut self.state.dropped)
        }

        /// Whether we're currently tracking a clipboard selection offer. Lets the
        /// caller tell "we have the selection but it's image-only" (don't fall back
        /// to a subprocess — that's what reintroduces the dock flash) apart from
        /// "we never saw a Selection event" (fall back is safe).
        pub fn has_selection(&self) -> bool {
            self.state.selection.is_some()
        }

        /// Read the clipboard selection as text, natively over Wayland — NO
        /// subprocess (no `wl-paste`/`cat`/`xclip`), so nothing flashes in the
        /// dock. Returns None when there's no selection or it carries no text
        /// (e.g. an image is on the clipboard) — crucially we inspect the
        /// advertised mime types and only start a transfer when text is present,
        /// so an image clipboard is a true no-op.
        pub fn read_clipboard_text(&mut self) -> Option<String> {
            // Make sure we're holding the latest selection + its mime list.
            let _ = self.queue.dispatch_pending(&mut self.state);

            let dbg = std::env::var_os("SONIQ_CLIP").is_some();
            let offer = match self.state.selection.as_ref() {
                Some(o) => o,
                None => {
                    if dbg {
                        eprintln!("[clip] no selection offer tracked");
                    }
                    return None;
                }
            };
            let oid = offer.id().protocol_id();
            let mimes = self.state.offer_mimes.get(&oid)?;
            if dbg {
                eprintln!("[clip] selection mimes: {mimes:?}");
            }
            // Prefer UTF-8 text; fall back to the X11 atom names Xwayland mirrors.
            const PREF: &[&str] =
                &["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING", "TEXT"];
            let mime = PREF.iter().find(|p| mimes.iter().any(|m| m == *p))?;

            let (read_fd, write_fd) = make_pipe()?;
            // SAFETY: write_fd is a fresh, owned pipe end.
            let wb = unsafe { BorrowedFd::borrow_raw(write_fd.as_raw_fd()) };
            offer.receive((*mime).to_string(), wb);
            // Drop our write end so the reader sees EOF once the source is done.
            drop(write_fd);
            let _ = self.conn.flush();

            // Bounded blocking read — a local pipe the compositor fills right away,
            // so this is sub-millisecond in practice; the timeout only guards
            // against a misbehaving source so we never freeze the UI thread.
            let bytes = read_with_timeout(read_fd, 300)?;
            const MAX: usize = 256 * 1024;
            if bytes.len() > MAX {
                return None;
            }
            let s = String::from_utf8(bytes).ok()?;
            let s = s.trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
    }

    /// Read a pipe to EOF, but give up after `timeout_ms` so a stalled clipboard
    /// source can't hang the caller. `poll` guards each (blocking) read.
    fn read_with_timeout(fd: OwnedFd, timeout_ms: i32) -> Option<Vec<u8>> {
        use std::time::Instant;
        let raw = fd.as_raw_fd();
        let start = Instant::now();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let remaining = timeout_ms - start.elapsed().as_millis() as i32;
            if remaining <= 0 {
                return if buf.is_empty() { None } else { Some(buf) };
            }
            let mut pfd = libc::pollfd { fd: raw, events: libc::POLLIN, revents: 0 };
            let r = unsafe { libc::poll(&mut pfd, 1, remaining) };
            if r < 0 {
                return None;
            }
            if r == 0 {
                return if buf.is_empty() { None } else { Some(buf) };
            }
            let n = unsafe { libc::read(raw, tmp.as_mut_ptr().cast(), tmp.len()) };
            if n < 0 {
                return None;
            }
            if n == 0 {
                return Some(buf); // EOF
            }
            buf.extend_from_slice(&tmp[..n as usize]);
            if buf.len() > 256 * 1024 {
                return Some(buf);
            }
        }
    }

    /// `file:///path/with%20space` → `/path/with space`.
    fn uri_to_path(uri: &str) -> Option<String> {
        let rest = uri.strip_prefix("file://")?;
        // Strip an optional host component (everything up to the first '/').
        let path_part = &rest[rest.find('/')?..];
        let bytes = path_part.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(b) = u8::from_str_radix(&path_part[i + 1..i + 3], 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8(out).ok()
    }

    fn make_pipe() -> Option<(OwnedFd, OwnedFd)> {
        let mut fds = [0i32; 2];
        // O_CLOEXEC so the fds don't leak into mpv's subprocesses.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return None;
        }
        // SAFETY: pipe2 succeeded; both fds are fresh and owned.
        Some(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
    }

    fn set_nonblocking(fd: i32) {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL, 0);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::DndListener;

/// Off-Linux stub: winit's own `DroppedFile` works on macOS/Windows, so the
/// app uses that path there and this listener is a no-op.
#[cfg(not(target_os = "linux"))]
pub struct DndListener;

#[cfg(not(target_os = "linux"))]
impl DndListener {
    pub fn attach(_window: &slint::Window) -> Option<Self> {
        None
    }
    pub fn pump(&mut self) -> Vec<String> {
        Vec::new()
    }
    pub fn read_clipboard_text(&mut self) -> Option<String> {
        None
    }
    pub fn has_selection(&self) -> bool {
        false
    }
}
