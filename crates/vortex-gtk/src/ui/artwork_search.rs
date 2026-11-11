//! "Search Steam...": pick the cover of a game by hand (search results, store link or app id).

use crate::app::{AppState, blocking};
use crate::ui::cover;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::rc::Rc;
use vortex_core::artwork::{self, Sources};

/// Opens the dialog for game `id`; nothing is sent until the user presses Enter/Apply.
pub fn open(state: &Rc<AppState>, id: i64) {
    let Ok(game) = state.db().get_game(id) else {
        return;
    };
    let entry = adw::EntryRow::builder()
        .title("Search term, Steam store link or app id")
        .show_apply_button(true)
        .text(artwork::clean_query(&game.name))
        .build();
    let entries = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    entries.append(&entry);
    let results = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(["boxed-list"])
        .visible(false)
        .build();
    let note = gtk4::Label::builder()
        .label("The search term is sent to Steam when you search. Nothing is sent before that. Pasting a store link or an app id downloads that app's artwork directly.")
        .css_classes(["dim-label", "caption"])
        .wrap(true)
        .xalign(0.0)
        .build();
    let col = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    col.set_margin_top(12);
    col.set_margin_bottom(12);
    col.set_margin_start(12);
    col.set_margin_end(12);
    col.append(&note);
    col.append(&entries);
    col.append(
        &gtk4::ScrolledWindow::builder()
            .child(&results)
            .vexpand(true)
            .min_content_height(240)
            .build(),
    );
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&col));
    let dialog = adw::Dialog::builder()
        .title("Search Steam")
        .content_width(520)
        .content_height(520)
        .child(&tv)
        .build();

    let pick = {
        let (state, dialog) = (state.clone(), dialog.clone());
        Rc::new(move |app: i64| {
            let (state, dialog) = (state.clone(), dialog.clone());
            glib::spawn_future_local(async move {
                let (db, paths) = (state.db.clone(), state.paths.clone());
                match blocking(move || artwork::pick(&db, &paths, &Sources::default(), id, app))
                    .await
                {
                    Some(Ok(p)) => {
                        cover::update(id, Some(p.into_path_buf()));
                        state.toast("Cover set from Steam");
                        dialog.close();
                    }
                    Some(Err(e)) => state.toast(&e.to_string()),
                    None => {}
                }
            });
        })
    };
    {
        let (state, results, pick) = (state.clone(), results.clone(), pick.clone());
        entry.connect_apply(move |e| {
            let text = e.text().trim().to_string();
            if let Some(app) = artwork::parse_app_id(&text) {
                return pick(app);
            }
            if text.is_empty() {
                return;
            }
            let (state, results, pick) = (state.clone(), results.clone(), pick.clone());
            glib::spawn_future_local(async move {
                let db = state.db.clone();
                let r =
                    blocking(move || artwork::search_manual(&db, &Sources::default(), &text)).await;
                let items = match r {
                    Some(Ok(i)) => i,
                    Some(Err(e)) => return state.toast(&e.to_string()),
                    None => return,
                };
                results.remove_all();
                results.set_visible(true);
                if items.is_empty() {
                    results.append(&adw::ActionRow::builder().title("No results").build());
                }
                for (app, name) in items {
                    let row = adw::ActionRow::builder()
                        .title(glib::markup_escape_text(&name))
                        .subtitle(format!("Steam app {app}"))
                        .activatable(true)
                        .build();
                    let pick = pick.clone();
                    row.connect_activated(move |_| pick(app));
                    results.append(&row);
                }
            });
        });
    }
    dialog.present(Some(&state.window));
}

/// One-line artwork state for the game page, if there is something to say.
pub fn status_line(db: &vortex_core::db::Db, id: i64) -> Option<String> {
    use vortex_core::artwork::LookupState;
    let n = db.get_artwork(id).ok().flatten()?.candidates;
    match vortex_core::artwork::lookup_state(db, id).ok()? {
        LookupState::NoMatch if n > 0 => Some(format!(
            "{n} possible match{} on Steam, none attached. Use Search Steam to choose.",
            if n == 1 { "" } else { "es" }
        )),
        LookupState::NoMatch => Some("No match found on Steam. Use Search Steam to choose.".into()),
        LookupState::Disabled => Some("Automatic artwork lookup is off for this game.".into()),
        LookupState::NotLookedUp | LookupState::HasImage => None,
    }
}
