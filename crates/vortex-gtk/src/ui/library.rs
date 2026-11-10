//! Library page: cover grid or compact list, searchable and sortable, filtered by sidebar mode.

use crate::app::AppState;
use crate::game_object::{GameObject, status_label};
use crate::ui::cover::cover;
use gtk4::gio;
use gtk4::prelude::*;
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::rc::Rc;

/// Sidebar sections of the library.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    All,
    Recent,
    Attention,
}

/// Does a game with this status / last-played time belong in `mode`?
pub fn in_mode(mode: Mode, status: &str, last_played: i64) -> bool {
    match mode {
        Mode::All => true,
        Mode::Recent => last_played > 0,
        Mode::Attention => status != "installed",
    }
}

pub fn last_played_text(ts: i64) -> String {
    if ts <= 0 {
        return "Never played".into();
    }
    gtk4::glib::DateTime::from_unix_local(ts)
        .ok()
        .and_then(|d| d.format("%x").ok())
        .map_or_else(String::new, |s| s.to_string())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    LastPlayed,
}

impl SortKey {
    fn from_index(i: u32) -> Self {
        if i == 1 { Self::LastPlayed } else { Self::Name }
    }
}

/// Compares `(name, last_played)` pairs. Last played sorts newest first, never-played last.
pub fn compare(key: SortKey, a: (&str, i64), b: (&str, i64)) -> Ordering {
    let by_name = || a.0.to_lowercase().cmp(&b.0.to_lowercase());
    match key {
        SortKey::Name => by_name(),
        SortKey::LastPlayed => b.1.cmp(&a.1).then_with(by_name),
    }
}

/// Case-insensitive substring match; an empty query matches everything.
pub fn matches(name: &str, query: &str) -> bool {
    name.to_lowercase().contains(&query.trim().to_lowercase())
}

type OpenCb = Rc<RefCell<Option<Box<dyn Fn(i64)>>>>;

pub struct Library {
    pub root: gtk4::Box,
    pub search_bar: gtk4::SearchBar,
    pub sort: gtk4::DropDown,
    pub list_toggle: gtk4::ToggleButton,
    store: gio::ListStore,
    mode: Rc<Cell<Mode>>,
    filter: gtk4::CustomFilter,
    on_open: OpenCb,
}

impl Library {
    pub fn new(_state: &Rc<AppState>) -> Self {
        let store = gio::ListStore::new::<GameObject>();
        let entry = gtk4::SearchEntry::builder()
            .placeholder_text("Search games")
            .build();
        let search_bar = gtk4::SearchBar::builder().child(&entry).build();
        search_bar.connect_entry(&entry);
        let sort = gtk4::DropDown::from_strings(&["Name", "Last played"]);
        sort.set_tooltip_text(Some("Sort by"));
        let list_toggle = gtk4::ToggleButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text("Compact list view")
            .build();

        let key = Rc::new(Cell::new(SortKey::Name));
        let mode = Rc::new(Cell::new(Mode::All));
        let sorter = gtk4::CustomSorter::new({
            let key = key.clone();
            move |a, b| {
                let (Some(a), Some(b)) = (
                    a.downcast_ref::<GameObject>(),
                    b.downcast_ref::<GameObject>(),
                ) else {
                    return gtk4::Ordering::Equal;
                };
                compare(
                    key.get(),
                    (&a.name(), a.last_played()),
                    (&b.name(), b.last_played()),
                )
                .into()
            }
        });
        let filter = gtk4::CustomFilter::new({
            let (entry, mode) = (entry.clone(), mode.clone());
            move |o| {
                o.downcast_ref::<GameObject>().is_some_and(|g| {
                    matches(&g.name(), &entry.text())
                        && in_mode(mode.get(), &g.status(), g.last_played())
                })
            }
        });
        {
            let (sorter, key) = (sorter.clone(), key.clone());
            sort.connect_selected_notify(move |d| {
                key.set(SortKey::from_index(d.selected()));
                sorter.changed(gtk4::SorterChange::Different);
            });
        }
        {
            let filter = filter.clone();
            entry.connect_search_changed(move |_| filter.changed(gtk4::FilterChange::Different));
        }

        let filtered = gtk4::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let sorted = gtk4::SortListModel::new(Some(filtered.clone()), Some(sorter));
        let selection = gtk4::NoSelection::new(Some(sorted));

        let grid_factory = gtk4::SignalListItemFactory::new();
        grid_factory.connect_setup(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk4::ListItem>() {
                item.set_child(Some(&gtk4::Box::new(gtk4::Orientation::Vertical, 6)));
            }
        });
        grid_factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            if let (Some(g), Some(b)) = (
                item.item().and_downcast::<GameObject>(),
                item.child().and_downcast::<gtk4::Box>(),
            ) {
                fill_tile(&b, &g);
            }
        });
        let list_factory = gtk4::SignalListItemFactory::new();
        list_factory.connect_setup(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk4::ListItem>() {
                item.set_child(Some(&gtk4::Box::new(gtk4::Orientation::Horizontal, 12)));
            }
        });
        list_factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            if let (Some(g), Some(b)) = (
                item.item().and_downcast::<GameObject>(),
                item.child().and_downcast::<gtk4::Box>(),
            ) {
                fill_row(&b, &g);
            }
        });

        let grid = gtk4::GridView::builder()
            .model(&selection)
            .factory(&grid_factory)
            .min_columns(1)
            .max_columns(10)
            .single_click_activate(true)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let list = gtk4::ListView::builder()
            .model(&selection)
            .factory(&list_factory)
            .single_click_activate(true)
            .css_classes(["rich-list"])
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let on_open: OpenCb = Rc::new(RefCell::new(None));
        let activate = {
            let on_open = on_open.clone();
            move |model: Option<gtk4::gio::ListModel>, pos: u32| {
                let id = model
                    .and_then(|m| m.item(pos))
                    .and_downcast::<GameObject>()
                    .map(|o| o.id());
                if let (Some(id), Some(f)) = (id, on_open.borrow().as_ref()) {
                    f(id);
                }
            }
        };
        {
            let a = activate.clone();
            grid.connect_activate(move |g, pos| a(g.model().map(|m| m.upcast()), pos));
        }
        list.connect_activate(move |l, pos| activate(l.model().map(|m| m.upcast()), pos));

        let empty = adw::StatusPage::builder()
            .icon_name("applications-games-symbolic")
            .title("No games here")
            .description("Install a Windows game, or add one that is already installed.")
            .child(
                &gtk4::Button::builder()
                    .label("Install a Game")
                    .halign(gtk4::Align::Center)
                    .action_name("win.install-game")
                    .css_classes(["suggested-action", "pill"])
                    .build(),
            )
            .build();
        let stack = gtk4::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(
            &gtk4::ScrolledWindow::builder()
                .child(&grid)
                .vexpand(true)
                .build(),
            Some("grid"),
        );
        stack.add_named(
            &gtk4::ScrolledWindow::builder()
                .child(&list)
                .vexpand(true)
                .build(),
            Some("list"),
        );
        let show = {
            let (stack, toggle, filtered) = (stack.clone(), list_toggle.clone(), filtered.clone());
            Rc::new(move || {
                let name = if filtered.n_items() == 0 {
                    "empty"
                } else if toggle.is_active() {
                    "list"
                } else {
                    "grid"
                };
                stack.set_visible_child_name(name);
            })
        };
        {
            let show = show.clone();
            filtered.connect_items_changed(move |_, _, _, _| show());
        }
        {
            let show = show.clone();
            list_toggle.connect_toggled(move |t| {
                t.set_icon_name(if t.is_active() {
                    "view-grid-symbolic"
                } else {
                    "view-list-symbolic"
                });
                t.set_tooltip_text(Some(if t.is_active() {
                    "Cover grid view"
                } else {
                    "Compact list view"
                }));
                show();
            });
        }
        stack.set_visible_child_name("empty");

        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        root.append(&search_bar);
        root.append(&stack);
        Self {
            root,
            search_bar,
            sort,
            list_toggle,
            store,
            mode,
            filter,
            on_open,
        }
    }

    pub fn set_mode(&self, m: Mode) {
        self.mode.set(m);
        self.filter.changed(gtk4::FilterChange::Different);
    }

    pub fn connect_open(&self, f: impl Fn(i64) + 'static) {
        *self.on_open.borrow_mut() = Some(Box::new(f));
    }

    /// Reloads all games from the database (tiny queries, safe on the GTK thread).
    pub fn refresh(&self, state: &AppState) {
        let db = state.db();
        let games = match db.list_games() {
            Ok(g) => g,
            Err(e) => {
                drop(db);
                return state.toast(&e.to_string());
            }
        };
        let items: Vec<GameObject> = games
            .iter()
            .map(|g| {
                let last = db.last_launch(g.id).ok().flatten().map(|r| r.started_at);
                let rt = db
                    .get_environment(g.environment_id)
                    .ok()
                    .and_then(|e| {
                        std::path::Path::new(&e.runtime_path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                    })
                    .unwrap_or_default();
                GameObject::new(g, last, &rt)
            })
            .collect();
        self.store.splice(0, self.store.n_items(), &items);
    }
}

fn clear(b: &gtk4::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

fn fill_tile(b: &gtk4::Box, g: &GameObject) {
    clear(b);
    b.add_css_class("game-tile");
    b.set_halign(gtk4::Align::Center);
    b.append(&cover(&g.name(), 150, 225));
    b.append(
        &gtk4::Label::builder()
            .label(g.name())
            .max_width_chars(16)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    b.append(
        &gtk4::Label::builder()
            .label(status_label(&g.status()))
            .css_classes(["dim-label", "caption"])
            .build(),
    );
}

fn fill_row(b: &gtk4::Box, g: &GameObject) {
    clear(b);
    b.set_margin_top(6);
    b.set_margin_bottom(6);
    b.set_margin_start(6);
    b.set_margin_end(6);
    b.append(&cover(&g.name(), 40, 56));
    let col = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    col.set_hexpand(true);
    col.set_valign(gtk4::Align::Center);
    col.append(
        &gtk4::Label::builder()
            .label(g.name())
            .xalign(0.0)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    col.append(
        &gtk4::Label::builder()
            .label(format!(
                "{} - {}",
                g.runtime(),
                last_played_text(g.last_played())
            ))
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    b.append(&col);
    b.append(
        &gtk4::Label::builder()
            .label(status_label(&g.status()))
            .css_classes(["dim-label"])
            .build(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_by_name_ignores_case() {
        assert_eq!(
            compare(SortKey::Name, ("alpha", 0), ("Beta", 9)),
            Ordering::Less
        );
    }

    #[test]
    fn sort_last_played_newest_first_then_name() {
        let k = SortKey::LastPlayed;
        assert_eq!(compare(k, ("a", 5), ("b", 9)), Ordering::Greater);
        assert_eq!(compare(k, ("a", 0), ("b", 9)), Ordering::Greater); // never played last
        assert_eq!(compare(k, ("a", 3), ("b", 3)), Ordering::Less);
    }

    #[test]
    fn modes() {
        assert!(in_mode(Mode::Attention, "failed", 0));
        assert!(!in_mode(Mode::Attention, "installed", 0));
        assert!(in_mode(Mode::Recent, "installed", 5));
        assert!(!in_mode(Mode::Recent, "installed", 0));
    }

    #[test]
    fn filter() {
        assert!(matches("Half-Life", "life"));
        assert!(matches("Half-Life", "  "));
        assert!(!matches("Half-Life", "portal"));
    }
}
