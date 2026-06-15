//! Scripted, env-var-gated diagnostics that reproduce hard-to-trigger UI bugs
//! headlessly (fullscreen round-trips, resizes, the blink sequence). None run
//! unless their `SONIQ_*` variable is set, so this is inert in normal use.
//!
//! (The render-loop diagnostics — SONIQ_PERF / DUMPWIN / CORNERTEST / BLINKDET
//! — live inside the rendering notifier where the GL framebuffer is available.)

use std::rc::Rc;
use std::time::Duration;

use slint::ComponentHandle;

use crate::App;

pub fn install(app: &App, set_fullscreen: &Rc<dyn Fn(bool)>) {
    // Enter fullscreen at 1.5s, exit at 3.5s — combined with SONIQ_CORNERTEST
    // this reproduces the fullscreen-roundtrip corruption headlessly (corner
    // alphas + window state are printed).
    if std::env::var("SONIQ_FSTEST").is_ok() {
        let weak = app.as_weak();
        let t1 = slint::Timer::default();
        t1.start(slint::TimerMode::SingleShot, Duration::from_millis(1500), {
            let set_fullscreen = set_fullscreen.clone();
            move || {
                eprintln!("[fstest] entering fullscreen");
                set_fullscreen(true);
            }
        });
        let tp = slint::Timer::default();
        tp.start(slint::TimerMode::SingleShot, Duration::from_millis(2500), {
            let weak = weak.clone();
            move || {
                if let Some(a) = weak.upgrade() {
                    eprintln!("[fstest] pausing");
                    a.invoke_toggle_pause();
                }
            }
        });
        std::mem::forget(tp);
        let t2 = slint::Timer::default();
        t2.start(slint::TimerMode::SingleShot, Duration::from_millis(3500), {
            let set_fullscreen = set_fullscreen.clone();
            move || {
                eprintln!("[fstest] exiting fullscreen");
                set_fullscreen(false);
            }
        });
        let t3 = slint::Timer::default();
        t3.start(slint::TimerMode::Repeated, Duration::from_millis(1000), {
            let weak = weak.clone();
            move || {
                if let Some(a) = weak.upgrade() {
                    let s = a.window().size();
                    eprintln!(
                        "[fstest] fs={} max={} size={}x{} win_square={}",
                        a.window().is_fullscreen(),
                        a.window().is_maximized(),
                        s.width,
                        s.height,
                        a.get_win_square()
                    );
                }
            }
        });
        std::mem::forget(t1);
        std::mem::forget(t2);
        std::mem::forget(t3);
    }
    // Replay the user's blink sequence — fullscreen on/off, resize, pause, then
    // chrome fade toggles (use with SONIQ_BLINKDET).
    if std::env::var("SONIQ_UXTEST").is_ok() {
        let weak = app.as_weak();
        let sf = set_fullscreen.clone();
        let mk = |ms: u64, f: Box<dyn Fn()>| {
            let t = slint::Timer::default();
            t.start(slint::TimerMode::SingleShot, Duration::from_millis(ms), move || f());
            std::mem::forget(t);
        };
        {
            let sf2 = sf.clone();
            mk(1500, Box::new(move || { eprintln!("[ux] fs on"); sf2(true); }));
        }
        {
            let sf2 = sf.clone();
            mk(3000, Box::new(move || { eprintln!("[ux] fs off"); sf2(false); }));
        }
        {
            let weak2 = weak.clone();
            mk(4500, Box::new(move || {
                if let Some(a) = weak2.upgrade() {
                    eprintln!("[ux] resize");
                    let s = a.window().size();
                    a.window().set_size(slint::PhysicalSize::new(s.width + 200, s.height + 100));
                }
            }));
        }
        {
            let weak2 = weak.clone();
            mk(5500, Box::new(move || {
                if let Some(a) = weak2.upgrade() {
                    eprintln!("[ux] pause");
                    a.invoke_toggle_pause();
                }
            }));
        }
        for (i, ms) in [6500u64, 7300, 8100, 8900].iter().enumerate() {
            let weak2 = weak.clone();
            mk(*ms, Box::new(move || {
                if let Some(a) = weak2.upgrade() {
                    eprintln!("[ux] chrome toggle {i}");
                    a.set_chrome_shown(!a.get_chrome_shown());
                }
            }));
        }
    }
    // Programmatic window resize at 2s (use with SONIQ_CORNERTEST), followed by
    // a 1px jiggle to test whether a second resize heals the surface.
    if std::env::var("SONIQ_RZTEST").is_ok() {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_millis(2000), move || {
            if let Some(a) = weak.upgrade() {
                let s = a.window().size();
                eprintln!("[rztest] resizing {}x{} -> +300x+200", s.width, s.height);
                a.window()
                    .set_size(slint::PhysicalSize::new(s.width + 300, s.height + 200));
                let weak2 = weak.clone();
                slint::Timer::single_shot(Duration::from_millis(400), move || {
                    if let Some(a) = weak2.upgrade() {
                        let s = a.window().size();
                        eprintln!("[rztest] jiggle +1");
                        a.window().set_size(slint::PhysicalSize::new(s.width, s.height + 1));
                    }
                });
            }
        });
        std::mem::forget(t);
    }
    if std::env::var("SONIQ_SHOT_FS").is_ok() {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_millis(800), move || {
            if let Some(a) = weak.upgrade() {
                a.window().set_fullscreen(true);
            }
        });
        std::mem::forget(t);
    }
}
