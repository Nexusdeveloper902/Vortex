//! Preferences dialog: global defaults stored in the `settings` table.

use crate::app::{AppState, blocking};
use libadwaita as adw;
use libadwaita::prelude::*;
use std::rc::Rc;
use vortex_core::runtime;

pub const KEY_DEFAULT_RUNTIME: &str = "default_runtime";
pub const KEY_INSTALL_DIR: &str = "default_install_dir";
pub const KEY_PREFIX_DIR: &str = "default_prefix_dir";
pub const KEY_OFFLINE: &str = "offline_mode";

fn dir_row(state: &Rc<AppState>, title: &str, key: &'static str) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title(title)
        .show_apply_button(true)
        .build();
    row.set_text(
        &state
            .db()
            .get_setting(key)
            .ok()
            .flatten()
            .unwrap_or_default(),
    );
    let state = state.clone();
    row.connect_apply(move |r| {
        let v = r.text();
        if !v.is_empty() && !v.starts_with('/') {
            return state.toast("Use an absolute path");
        }
        if let Err(e) = state.db().set_setting(key, &v) {
            state.toast(&e.to_string());
        }
    });
    row
}

pub fn present(state: &Rc<AppState>) {
    let dialog = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::new();

    let proton = adw::PreferencesGroup::builder().title("Proton").build();
    let combo = adw::ComboRow::builder()
        .title("Default Proton")
        .subtitle("Loading")
        .build();
    proton.add(&combo);
    page.add(&proton);

    let dirs = adw::PreferencesGroup::builder()
        .title("Folders")
        .description("Leave empty to use Vortex's own folders.")
        .build();
    dirs.add(&dir_row(state, "Default install folder", KEY_INSTALL_DIR));
    dirs.add(&dir_row(state, "Default prefix folder", KEY_PREFIX_DIR));
    page.add(&dirs);

    let net = adw::PreferencesGroup::builder().title("Network").build();
    let offline = adw::SwitchRow::builder()
        .title("Offline mode")
        .subtitle("Never download anything")
        .active(
            state
                .db()
                .get_setting(KEY_OFFLINE)
                .ok()
                .flatten()
                .as_deref()
                == Some("1"),
        )
        .build();
    {
        let state = state.clone();
        offline.connect_active_notify(move |s| {
            if let Err(e) = state
                .db()
                .set_setting(KEY_OFFLINE, if s.is_active() { "1" } else { "0" })
            {
                state.toast(&e.to_string());
            }
        });
    }
    net.add(&offline);
    page.add(&net);
    dialog.add(&page);

    let (state2, combo2) = (state.clone(), combo.clone());
    gtk4::glib::spawn_future_local(async move {
        let (home, dir) = (
            state2.paths.home().to_path_buf(),
            state2.paths.runtimes_dir(),
        );
        let Some(found) = blocking(move || runtime::discover(&home, &dir)).await else {
            return;
        };
        let paths: Vec<String> = found
            .iter()
            .map(|r| r.path.as_path().to_string_lossy().into_owned())
            .collect();
        let labels: Vec<&str> = found.iter().map(|r| r.label.as_str()).collect();
        combo2.set_model(Some(&gtk4::StringList::new(&labels)));
        combo2.set_subtitle(if found.is_empty() {
            "No Proton runtime found"
        } else {
            "Used for new games"
        });
        let cur = state2.db().get_setting(KEY_DEFAULT_RUNTIME).ok().flatten();
        if let Some(i) = paths.iter().position(|p| Some(p) == cur.as_ref()) {
            combo2.set_selected(i as u32);
        }
        let state3 = state2.clone();
        combo2.connect_selected_notify(move |c| {
            if let Some(p) = paths.get(c.selected() as usize)
                && let Err(e) = state3.db().set_setting(KEY_DEFAULT_RUNTIME, p)
            {
                state3.toast(&e.to_string());
            }
        });
    });
    dialog.present(Some(&state.window));
}
