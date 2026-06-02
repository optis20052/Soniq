//! In-app on-screen-display notifications (toasts) for player actions.
//!
//! Usage: hold an `Osd` handle (cheaply cloneable) and call
//! `osd.show("icon-name", "message")` from anywhere. Pass "" as the icon for a
//! text-only toast. To notify a new action, add one `osd.show(...)` call - no
//! other wiring needed.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib::{self, ControlFlow};
use gtk::prelude::*;

/// A cheaply-cloneable handle to the OSD widget.
#[derive(Clone)]
pub struct Osd {
    icon: gtk::Image,
    label: gtk::Label,
    revealer: gtk::Revealer,
    /// Debounce token so a new message cancels the previous hide timer.
    token: Rc<Cell<u64>>,
}

impl Osd {
    /// Build the OSD widget (a top-center fading pill). Returns the handle and
    /// the root widget to add into an overlay.
    pub fn new() -> (Self, gtk::Widget) {
        let icon = gtk::Image::new();
        icon.add_css_class("osd-toast-icon");

        let label = gtk::Label::new(None);
        label.add_css_class("osd-toast-label");

        // Icon + text in one pill so heights stay consistent (emoji didn't).
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.add_css_class("osd-toast");
        content.append(&icon);
        content.append(&label);

        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(180)
            .reveal_child(false)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .margin_top(54)
            .child(&content)
            .build();
        revealer.set_can_target(false); // never intercept video clicks

        let osd = Osd {
            icon,
            label,
            revealer: revealer.clone(),
            token: Rc::new(Cell::new(0)),
        };
        (osd, revealer.upcast())
    }

    /// Show a transient message with an optional symbolic icon (pass "" for
    /// none). Auto-hides after a short delay.
    pub fn show(&self, icon_name: &str, message: &str) {
        if icon_name.is_empty() {
            self.icon.set_visible(false);
        } else {
            self.icon.set_icon_name(Some(icon_name));
            self.icon.set_visible(true);
        }
        self.label.set_text(message);
        self.revealer.set_reveal_child(true);

        let token = self.token.get().wrapping_add(1);
        self.token.set(token);
        let revealer = self.revealer.clone();
        let token_cell = self.token.clone();
        glib::timeout_add_local(Duration::from_millis(1400), move || {
            if token_cell.get() == token {
                revealer.set_reveal_child(false);
            }
            ControlFlow::Break
        });
    }
}
