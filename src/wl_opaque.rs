//! Wayland opaque-region management for the frameless (CSD) window.
//!
//! GTK's client-side-decorated windows mark everything except the rounded
//! corners as an *opaque region* (`wl_surface.set_opaque_region`). The
//! compositor then uses its fast opaque path for the window body and only
//! alpha-blends the corners. Without this (winit never sets the region), the
//! whole surface is alpha-blended, which on NVIDIA + Mutter intermittently
//! presents bad frames while the content animates — the "screen blinks every
//! time the panels fade" bug. This module replicates GTK's behaviour by
//! talking to the compositor directly over winit's existing connection.

#[cfg(target_os = "linux")]
mod imp {
    use slint::winit_030::WinitWindowAccessor;
    use wayland_client::protocol::{wl_compositor, wl_region, wl_registry, wl_surface};
    use wayland_client::{Connection, Proxy};

    struct NoEvents;
    impl wayland_client::Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents>
        for NoEvents
    {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &wayland_client::globals::GlobalListContents,
            _: &Connection,
            _: &wayland_client::QueueHandle<Self>,
        ) {
        }
    }
    impl wayland_client::Dispatch<wl_compositor::WlCompositor, ()> for NoEvents {
        fn event(
            _: &mut Self,
            _: &wl_compositor::WlCompositor,
            _: wl_compositor::Event,
            _: &(),
            _: &Connection,
            _: &wayland_client::QueueHandle<Self>,
        ) {
        }
    }
    impl wayland_client::Dispatch<wl_region::WlRegion, ()> for NoEvents {
        fn event(
            _: &mut Self,
            _: &wl_region::WlRegion,
            _: wl_region::Event,
            _: &(),
            _: &Connection,
            _: &wayland_client::QueueHandle<Self>,
        ) {
        }
    }

    pub struct OpaqueRegion {
        conn: Connection,
        compositor: wl_compositor::WlCompositor,
        qh: wayland_client::QueueHandle<NoEvents>,
        // Kept alive for the lifetime of the proxies created on it.
        _queue: wayland_client::EventQueue<NoEvents>,
        surface_ptr: *mut std::ffi::c_void,
    }

    impl OpaqueRegion {
        /// Hook onto winit's Wayland connection. Returns None off-Wayland.
        pub fn attach(window: &slint::Window) -> Option<Self> {
            window
                .with_winit_window(|w| {
                    use slint::winit_030::winit::raw_window_handle::{
                        HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
                    };
                    let display = match w.display_handle().ok()?.as_raw() {
                        RawDisplayHandle::Wayland(d) => d.display.as_ptr(),
                        _ => return None,
                    };
                    let surface_ptr = match w.window_handle().ok()?.as_raw() {
                        RawWindowHandle::Wayland(s) => s.surface.as_ptr(),
                        _ => return None,
                    };
                    let backend = unsafe {
                        wayland_backend::client::Backend::from_foreign_display(display.cast())
                    };
                    let conn = Connection::from_backend(backend);
                    let (globals, queue) =
                        wayland_client::globals::registry_queue_init::<NoEvents>(&conn).ok()?;
                    let qh = queue.handle();
                    let compositor: wl_compositor::WlCompositor =
                        globals.bind(&qh, 1..=4, ()).ok()?;
                    Some(OpaqueRegion { conn, compositor, qh, _queue: queue, surface_ptr })
                })
                .flatten()
        }

        /// Mark the window body opaque, leaving `corner`-sized squares at the four
        /// corners alpha-blended (where the rounded arcs are carved). All sizes in
        /// *logical* (surface-local) coordinates. `corner == 0` = fully opaque.
        pub fn set(&self, w: i32, h: i32, corner: i32) {
            if w <= 0 || h <= 0 {
                return;
            }
            let surface_id = match unsafe {
                wayland_client::backend::ObjectId::from_ptr(
                    wl_surface::WlSurface::interface(),
                    self.surface_ptr.cast(),
                )
            } {
                Ok(id) => id,
                Err(_) => return,
            };
            let Ok(surface) = wl_surface::WlSurface::from_id(&self.conn, surface_id) else {
                return;
            };
            let region = self.compositor.create_region(&self.qh, ());
            region.add(0, 0, w, h);
            if corner > 0 {
                region.subtract(0, 0, corner, corner);
                region.subtract(w - corner, 0, corner, corner);
                region.subtract(0, h - corner, corner, corner);
                region.subtract(w - corner, h - corner, corner, corner);
            }
            // Double-buffered state: applied by the next commit (every frame swap).
            surface.set_opaque_region(Some(&region));
            region.destroy();
            let _ = self.conn.flush();
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::OpaqueRegion;

/// No-op stub off-Linux: macOS / Windows compositors handle transparent
/// windows + rounded corners without opaque-region hints.
#[cfg(not(target_os = "linux"))]
pub struct OpaqueRegion;

#[cfg(not(target_os = "linux"))]
impl OpaqueRegion {
    pub fn attach(_window: &slint::Window) -> Option<Self> {
        None
    }
    pub fn set(&self, _w: i32, _h: i32, _corner: i32) {}
}
