//! A logo widget with a diagonal "shine" streak that sweeps corner-to-corner
//! across the glyph. The streak is a moving gradient *masked to the logo's
//! alpha*, so the light only travels over the shape itself (not the surrounding
//! background) — which plain CSS can't do for an icon.

use std::cell::{Cell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

/// Period of one shine cycle (sweep + idle pause), seconds.
const CYCLE: f64 = 3.8;
/// Fraction of the cycle spent sweeping; the rest is an idle pause.
const SWEEP_FRAC: f64 = 0.55;
const LOGO_SIZE: i32 = 112;

mod imp {
    use super::*;

    pub struct ShineLogo {
        pub paintable: RefCell<Option<gdk::Paintable>>,
        pub phase: Cell<f64>,
    }

    impl Default for ShineLogo {
        fn default() -> Self {
            Self {
                paintable: RefCell::new(lookup_logo()),
                phase: Cell::new(0.0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ShineLogo {
        const NAME: &'static str = "SoniqShineLogo";
        type Type = super::ShineLogo;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ShineLogo {
        fn constructed(&self) {
            self.parent_constructed();
            // Drive the sweep off the frame clock (monotonic — no wall clock).
            self.obj().add_tick_callback(|obj, clock| {
                let t = clock.frame_time() as f64 / 1_000_000.0;
                obj.imp().phase.set((t % CYCLE) / CYCLE);
                obj.queue_draw();
                glib::ControlFlow::Continue
            });
        }
    }

    impl WidgetImpl for ShineLogo {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (LOGO_SIZE, LOGO_SIZE, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f64, obj.height() as f64);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let Some(paintable) = self.paintable.borrow().clone() else {
                return;
            };

            // Base logo, always visible.
            paintable.snapshot(snapshot, w, h);

            let c = band_center(self.phase.get());
            if c < -0.2 || c > 1.2 {
                return; // band off-screen (idle pause / sweep ends)
            }

            // Shine: a diagonal gradient band, masked to the logo's alpha so it
            // only lights up the glyph.
            snapshot.push_mask(gsk::MaskMode::Alpha);
            paintable.snapshot(snapshot, w, h); // mask = logo shape
            snapshot.pop(); // -> record the source
            let bounds = graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
            let start = graphene::Point::new(0.0, 0.0);
            let end = graphene::Point::new(w as f32, h as f32);
            snapshot.append_linear_gradient(&bounds, &start, &end, &band_stops(c));
            snapshot.pop(); // apply mask
        }
    }

    /// Band centre along the top-left → bottom-right diagonal (0..1), or an
    /// off-screen value during the idle pause.
    fn band_center(phase: f64) -> f64 {
        if phase >= SWEEP_FRAC {
            return 2.0;
        }
        let t = phase / SWEEP_FRAC;
        let eased = t * t * (3.0 - 2.0 * t); // smoothstep
        -0.25 + eased * 1.5
    }

    fn band_stops(c: f64) -> Vec<gsk::ColorStop> {
        let hw = 0.14; // half-width of the bright band
        let transp = gdk::RGBA::new(1.0, 1.0, 1.0, 0.0);
        let bright = gdk::RGBA::new(1.0, 1.0, 1.0, 0.6);
        let a = (c - hw).clamp(0.0, 1.0) as f32;
        let b = c.clamp(0.0, 1.0) as f32;
        let d = (c + hw).clamp(0.0, 1.0) as f32;
        vec![
            gsk::ColorStop::new(0.0, transp),
            gsk::ColorStop::new(a, transp),
            gsk::ColorStop::new(b, bright),
            gsk::ColorStop::new(d, transp),
            gsk::ColorStop::new(1.0, transp),
        ]
    }

    fn lookup_logo() -> Option<gdk::Paintable> {
        let display = gdk::Display::default()?;
        let icon = gtk::IconTheme::for_display(&display).lookup_icon(
            crate::WORDMARK_ICON,
            &[],
            LOGO_SIZE,
            1,
            gtk::TextDirection::None,
            gtk::IconLookupFlags::empty(),
        );
        Some(icon.upcast())
    }
}

glib::wrapper! {
    pub struct ShineLogo(ObjectSubclass<imp::ShineLogo>) @extends gtk::Widget;
}

impl ShineLogo {
    pub fn new() -> Self {
        glib::Object::new()
    }
}

impl Default for ShineLogo {
    fn default() -> Self {
        Self::new()
    }
}
