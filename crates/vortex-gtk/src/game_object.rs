//! `GameObject`: the GObject row stored in the library `gio::ListStore`.

use gtk4::glib;
use gtk4::glib::prelude::*;
use gtk4::subclass::prelude::*;
use vortex_core::db::Game;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(glib::Properties, Default)]
    #[properties(wrapper_type = super::GameObject)]
    pub struct GameObject {
        #[property(get, set)]
        id: Cell<i64>,
        #[property(get, set)]
        name: RefCell<String>,
        /// `db::GameStatus::as_str()` value.
        #[property(get, set)]
        status: RefCell<String>,
        /// Unix seconds of the newest launch run, 0 if never.
        #[property(get, set)]
        last_played: Cell<i64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GameObject {
        const NAME: &'static str = "VortexGameObject";
        type Type = super::GameObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for GameObject {}
}

glib::wrapper! {
    pub struct GameObject(ObjectSubclass<imp::GameObject>);
}

impl GameObject {
    pub fn new(g: &Game, last_played: Option<i64>) -> Self {
        glib::Object::builder()
            .property("id", g.id)
            .property("name", &g.name)
            .property("status", g.status.as_str())
            .property("last-played", last_played.unwrap_or(0))
            .build()
    }
}

/// User-facing text for a stored status value.
pub fn status_label(status: &str) -> &'static str {
    match status {
        "installing" => "Installing",
        "installed" => "Installed",
        "failed" => "Install failed",
        "interrupted" => "Interrupted",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(status_label("installed"), "Installed");
        assert_eq!(status_label("failed"), "Install failed");
        assert_eq!(status_label("bogus"), "Unknown");
    }
}
