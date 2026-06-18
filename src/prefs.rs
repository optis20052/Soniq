//! The Preferences window: lazily created on first open, then wired to every
//! settings callback (shortcuts capture, subtitle style, mouse actions, resume
//! mode) and shown. Pulled out of `main` — it's the single biggest callback
//! installer.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::config::{ResumeMode, SubStyle};
use crate::shortcuts::ACTIONS;
use crate::subs::{apply_sub_style, hex_from_slint, slint_color_from_hex, SUB_POS_MODES};
use crate::video::VideoBridge;
use crate::{App, PrefsWindow, SearchHit, ShortcutItem};

/// Shared state the Preferences window borrows from `main`.
pub struct PrefsDeps {
    pub bridge: Rc<RefCell<Option<VideoBridge>>>,
    pub bindings: Rc<RefCell<Vec<String>>>,
    pub shortcuts_model: Rc<VecModel<ShortcutItem>>,
    pub rebuild_shortcuts: Rc<dyn Fn()>,
    pub persist: Rc<dyn Fn()>,
    pub resume_mode: Rc<Cell<ResumeMode>>,
    pub mouse_single: Rc<Cell<i32>>,
    pub mouse_double: Rc<Cell<i32>>,
    pub mouse_right: Rc<Cell<i32>>,
    pub sub_style: Rc<RefCell<SubStyle>>,
    pub font_families: Rc<Vec<String>>,
}

/// Wire `on_open_prefs`: build the Preferences window on first open and connect
/// all of its controls, then show it.
pub fn install(app: &App, deps: PrefsDeps) {
    let PrefsDeps {
        bridge,
        bindings,
        shortcuts_model,
        rebuild_shortcuts,
        persist,
        resume_mode,
        mouse_single,
        mouse_double,
        mouse_right,
        sub_style,
        font_families,
    } = deps;

    // Preferences window (separate, lazily created)
    let prefs: Rc<RefCell<Option<PrefsWindow>>> = Rc::new(RefCell::new(None));
    {
        let prefs = prefs.clone();
        let bridge = bridge.clone();
        let bindings = bindings.clone();
        let shortcuts_model = shortcuts_model.clone();
        let rebuild_shortcuts = rebuild_shortcuts.clone();
        let persist = persist.clone();
        let resume_mode = resume_mode.clone();
        let mouse_single = mouse_single.clone();
        let mouse_double = mouse_double.clone();
        let mouse_right = mouse_right.clone();
        let sub_style = sub_style.clone();
        let font_families = font_families.clone();
        app.on_open_prefs(move || {
            if prefs.borrow().is_none() {
                if let Ok(w) = PrefsWindow::new() {
                    // macOS gives this window a native fullsize-content titlebar
                    // too; inset the sidebar header clear of the traffic lights.
                    w.set_mac_native(cfg!(target_os = "macos"));
                    w.set_shortcuts(ModelRc::from(shortcuts_model.clone()));
                    {
                        let bindings = bindings.clone();
                        let rebuild = rebuild_shortcuts.clone();
                        let persist = persist.clone();
                        w.on_capture_key(move |idx, name| {
                            let idx = idx as usize;
                            let name = name.to_string();
                            let mut b = bindings.borrow_mut();
                            if idx < b.len() {
                                // a key can map to only one action
                                for x in b.iter_mut() {
                                    if *x == name {
                                        x.clear();
                                    }
                                }
                                b[idx] = name;
                            }
                            drop(b);
                            rebuild();
                            persist();
                        });
                    }
                    {
                        let bindings = bindings.clone();
                        let rebuild = rebuild_shortcuts.clone();
                        let persist = persist.clone();
                        w.on_reset_shortcuts(move || {
                            *bindings.borrow_mut() =
                                ACTIONS.iter().map(|a| a.2.to_string()).collect();
                            rebuild();
                            persist();
                        });
                    }
                    {
                        let bridge = bridge.clone();
                        let sub_style = sub_style.clone();
                        let persist = persist.clone();
                        w.on_set_text_color(move |c| {
                            sub_style.borrow_mut().text_color = hex_from_slint(c);
                            if let Some(b) = bridge.borrow().as_ref() {
                                apply_sub_style(b, &sub_style.borrow());
                            }
                            persist();
                        });
                    }
                    {
                        let bridge = bridge.clone();
                        let sub_style = sub_style.clone();
                        let persist = persist.clone();
                        w.on_set_outline_color(move |c| {
                            sub_style.borrow_mut().outline_color = hex_from_slint(c);
                            if let Some(b) = bridge.borrow().as_ref() {
                                apply_sub_style(b, &sub_style.borrow());
                            }
                            persist();
                        });
                    }
                    {
                        let bridge = bridge.clone();
                        let sub_style = sub_style.clone();
                        let persist = persist.clone();
                        w.on_set_shadow_color(move |c| {
                            sub_style.borrow_mut().shadow_color = hex_from_slint(c);
                            if let Some(b) = bridge.borrow().as_ref() {
                                apply_sub_style(b, &sub_style.borrow());
                            }
                            persist();
                        });
                    }
                    {
                        let bridge = bridge.clone();
                        let sub_style = sub_style.clone();
                        let persist = persist.clone();
                        w.on_set_bg_color(move |c| {
                            sub_style.borrow_mut().bg_color = hex_from_slint(c);
                            if let Some(b) = bridge.borrow().as_ref() {
                                apply_sub_style(b, &sub_style.borrow());
                            }
                            persist();
                        });
                    }
                    {
                        let bridge = bridge.clone();
                        let sub_style = sub_style.clone();
                        let persist = persist.clone();
                        w.on_set_sub_font_name(move |name| {
                            sub_style.borrow_mut().font_name = name.to_string();
                            if let Some(b) = bridge.borrow().as_ref() {
                                b.set_prop_str("sub-font", &name);
                            }
                            persist();
                        });
                    }
                    {
                        // Filter the master font list by the search query and push
                        // the (capped) result back into the picker's list.
                        let families = font_families.clone();
                        let weak = w.as_weak();
                        w.on_sub_font_search(move |q| {
                            let Some(w) = weak.upgrade() else { return };
                            let q = q.to_lowercase();
                            let filtered: Vec<slint::SharedString> = families
                                .iter()
                                .filter(|f| q.is_empty() || f.to_lowercase().contains(&q))
                                .take(300)
                                .map(|f| f.as_str().into())
                                .collect();
                            w.set_sub_font_options(ModelRc::from(filtered.as_slice()));
                        });
                    }
                    {
                        // Settings search (Slint has no string.contains): filter a
                        // flat list of settings by title/category and push matches.
                        let weak = w.as_weak();
                        w.on_search(move |q| {
                            let Some(w) = weak.upgrade() else { return };
                            let q = q.trim().to_lowercase();
                            // (title, category, page index)
                            const SETTINGS: &[(&str, &str, i32)] = &[
                                ("Resume playback", "General · Playback", 0),
                                ("Show network stats", "General · Developer", 0),
                                ("Show FPS", "General · Developer", 0),
                                ("Keyboard shortcuts", "Shortcuts", 1),
                                ("Subtitle font", "Subtitles · Font", 2),
                                ("Subtitle size", "Subtitles · Font", 2),
                                ("Text colour", "Subtitles · Text colour", 2),
                                ("Draw outline", "Subtitles · Outline", 2),
                                ("Outline width", "Subtitles · Outline", 2),
                                ("Outline colour", "Subtitles · Outline", 2),
                                ("Draw shadow", "Subtitles · Drop shadow", 2),
                                ("Shadow offset", "Subtitles · Drop shadow", 2),
                                ("Shadow colour", "Subtitles · Drop shadow", 2),
                                ("Shaded background", "Subtitles · Background box", 2),
                                ("Background colour", "Subtitles · Background box", 2),
                                ("Background opacity", "Subtitles · Background box", 2),
                                ("Subtitle position anchor", "Subtitles · Position", 2),
                                ("Vertical offset", "Subtitles · Position", 2),
                                ("Line spacing", "Subtitles · Position", 2),
                                ("Single click action", "Mouse on video", 3),
                                ("Double click action", "Mouse on video", 3),
                                ("Right click action", "Mouse on video", 3),
                            ];
                            let hits: Vec<SearchHit> = if q.is_empty() {
                                Vec::new()
                            } else {
                                let from_settings = SETTINGS.iter().map(|(t, c, p)| {
                                    (t.to_string(), c.to_string(), *p)
                                });
                                // Each keyboard shortcut is searchable too (e.g.
                                // "mute" → the Mute binding on the Shortcuts page).
                                let from_shortcuts = ACTIONS.iter().map(|a| {
                                    (a.1.to_string(), "Shortcuts".to_string(), 1)
                                });
                                from_settings
                                    .chain(from_shortcuts)
                                    .filter(|(t, c, _)| {
                                        t.to_lowercase().contains(&q) || c.to_lowercase().contains(&q)
                                    })
                                    .map(|(t, c, p)| SearchHit {
                                        title: t.into(),
                                        category: c.into(),
                                        page: p,
                                    })
                                    .collect()
                            };
                            w.set_search_results(ModelRc::from(hits.as_slice()));
                        });
                    }
                    {
                        let bridge = bridge.clone();
                        let sub_style = sub_style.clone();
                        let persist = persist.clone();
                        w.on_adjust(move |key, val| {
                            // All subtitle sliders mutate the style and re-apply
                            // it wholesale (border-style mode is computed in
                            // apply_sub_style, so width/offset/opacity can't be
                            // pushed as standalone mpv props).
                            match key.as_str() {
                                "sub-scale" => sub_style.borrow_mut().scale = val as f64,
                                "sub-pos" => sub_style.borrow_mut().pos = val as f64,
                                "sub-font-size" => {
                                    sub_style.borrow_mut().font_size = val.round() as i64
                                }
                                "sub-border-size" => sub_style.borrow_mut().outline_size = val as f64,
                                "sub-shadow-offset" => {
                                    sub_style.borrow_mut().shadow_offset = val as f64
                                }
                                "sub-bg-opacity" => {
                                    sub_style.borrow_mut().bg_opacity = val.round() as i64
                                }
                                "sub-box-padding" => {
                                    sub_style.borrow_mut().box_padding = val as f64
                                }
                                "sub-line-spacing" => {
                                    sub_style.borrow_mut().line_spacing = val as f64
                                }
                                _ => {}
                            }
                            if let Some(b) = bridge.borrow().as_ref() {
                                apply_sub_style(b, &sub_style.borrow());
                            }
                            persist();
                        });
                    }
                    {
                        let bridge = bridge.clone();
                        let sub_style = sub_style.clone();
                        let persist = persist.clone();
                        w.on_set_sub_pos_mode(move |i| {
                            sub_style.borrow_mut().pos_mode = i;
                            if let Some(b) = bridge.borrow().as_ref() {
                                b.set_prop_str(
                                    "sub-align-y",
                                    SUB_POS_MODES[i.clamp(0, 2) as usize],
                                );
                            }
                            persist();
                        });
                    }
                    {
                        let bridge = bridge.clone();
                        let sub_style = sub_style.clone();
                        let persist = persist.clone();
                        w.on_set_outline(move |on| {
                            sub_style.borrow_mut().outline = on;
                            if let Some(b) = bridge.borrow().as_ref() {
                                apply_sub_style(b, &sub_style.borrow());
                            }
                            persist();
                        });
                    }
                    {
                        let bridge = bridge.clone();
                        let sub_style = sub_style.clone();
                        let persist = persist.clone();
                        w.on_set_shadow(move |on| {
                            sub_style.borrow_mut().shadow = on;
                            if let Some(b) = bridge.borrow().as_ref() {
                                apply_sub_style(b, &sub_style.borrow());
                            }
                            persist();
                        });
                    }
                    {
                        let bridge = bridge.clone();
                        let sub_style = sub_style.clone();
                        let persist = persist.clone();
                        w.on_set_shaded_bg(move |on| {
                            sub_style.borrow_mut().shaded_bg = on;
                            if let Some(b) = bridge.borrow().as_ref() {
                                apply_sub_style(b, &sub_style.borrow());
                            }
                            persist();
                        });
                    }
                    {
                        let resume_mode = resume_mode.clone();
                        let persist = persist.clone();
                        w.on_set_resume(move |i| {
                            resume_mode.set(ResumeMode::from_index(i));
                            persist();
                        });
                    }
                    {
                        let mouse_single = mouse_single.clone();
                        let persist = persist.clone();
                        w.on_set_mouse_single(move |i| {
                            mouse_single.set(i);
                            persist();
                        });
                    }
                    {
                        let mouse_double = mouse_double.clone();
                        let persist = persist.clone();
                        w.on_set_mouse_double(move |i| {
                            mouse_double.set(i);
                            persist();
                        });
                    }
                    {
                        let mouse_right = mouse_right.clone();
                        let persist = persist.clone();
                        w.on_set_mouse_right(move |i| {
                            mouse_right.set(i);
                            persist();
                        });
                    }
                    // Reflect the saved settings in the freshly-built prefs UI.
                    w.set_init_resume(resume_mode.get().to_index());
                    w.set_init_mouse_single(mouse_single.get());
                    w.set_init_mouse_double(mouse_double.get());
                    w.set_init_mouse_right(mouse_right.get());
                    {
                        let s = sub_style.borrow();
                        w.set_init_text_color(slint_color_from_hex(&s.text_color));
                        w.set_init_outline_color(slint_color_from_hex(&s.outline_color));
                        w.set_init_outline_size(s.outline_size as f32);
                        w.set_init_shadow_color(slint_color_from_hex(&s.shadow_color));
                        w.set_init_shadow_offset(s.shadow_offset as f32);
                        w.set_init_bg_color(slint_color_from_hex(&s.bg_color));
                        w.set_init_bg_opacity(s.bg_opacity as f32);
                        w.set_init_box_padding(s.box_padding as f32);
                        w.set_init_line_spacing(s.line_spacing as f32);
                        w.set_init_sub_font_name(s.font_name.as_str().into());
                        w.set_init_sub_font_size(s.font_size as f32);
                        // Seed the picker with the full font list.
                        let all: Vec<slint::SharedString> =
                            font_families.iter().take(300).map(|f| f.as_str().into()).collect();
                        w.set_sub_font_options(ModelRc::from(all.as_slice()));
                        w.set_init_sub_pos_mode(s.pos_mode);
                        w.set_init_sub_pos(s.pos as f32);
                        w.set_init_sub_scale(s.scale as f32);
                        w.set_init_outline(s.outline);
                        w.set_init_shadow(s.shadow);
                        w.set_init_shaded_bg(s.shaded_bg);
                    }
                    w.on_toggle_netstats(move |_on| { /* no debug overlay in the spike */ });
                    w.on_toggle_fps(move |_on| { /* no FPS overlay in the spike */ });
                    {
                        let pw = w.as_weak();
                        w.on_close_prefs(move || {
                            if let Some(w) = pw.upgrade() {
                                let _ = w.hide();
                            }
                        });
                    }
                    *prefs.borrow_mut() = Some(w);
                }
            }
            if let Some(w) = prefs.borrow().as_ref() {
                let _ = w.show();
                // Slint's `min-width`/`min-height` aren't enforced by the
                // compositor on this native-decorated window (you could drag it
                // down to nothing), so clamp the winit window's min size directly.
                // The winit window isn't realized synchronously after show(), so
                // defer it a tick until `with_winit_window` can see it.
                let weak = w.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(100), move || {
                    if let Some(w) = weak.upgrade() {
                        use slint::winit_030::winit::dpi::LogicalSize;
                        use slint::winit_030::WinitWindowAccessor;
                        w.window().with_winit_window(|win| {
                            win.set_min_inner_size(Some(LogicalSize::new(640.0, 480.0)));
                        });
                    }
                });
            }
        });
    }
}
