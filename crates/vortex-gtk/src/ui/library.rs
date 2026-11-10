//! Library page: searchable, sortable grid of game cards, or an empty state.

use crate::app::AppState;
use crate::game_object::{GameObject, status_label};
use gtk4::gio;
use gtk4::prelude::*;
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::rc::Rc;

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
    store: gio::ListStore,
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

        let key = Rc::new(Cell::new(SortKey::Name));
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
            let entry = entry.clone();
            move |o| {
                o.downcast_ref::<GameObject>()
                    .is_some_and(|g| matches(&g.name(), &entry.text()))
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

        let filtered = gtk4::FilterListModel::new(Some(store.clone()), Some(filter));
        let sorted = gtk4::SortListModel::new(Some(filtered), Some(sorter));
        let selection = gtk4::NoSelection::new(Some(sorted));

        let factory = gtk4::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            item.set_child(Some(&card()));
        });
        factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            let (Some(g), Some(card)) = (
                item.item().and_downcast::<GameObject>(),
                item.child().and_downcast::<gtk4::Box>(),
            ) else {
                return;
            };
            bind_card(&card, &g);
        });
        let grid = gtk4::GridView::builder()
            .model(&selection)
            .factory(&factory)
            .min_columns(1)
            .max_columns(8)
            .single_click_activate(true)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        grid.add_css_class("navigation-sidebar");
        let on_open: OpenCb = Rc::new(RefCell::new(None));
        {
            let on_open = on_open.clone();
            grid.connect_activate(move |g, pos| {
                let id = g
                    .model()
                    .and_then(|m| m.item(pos))
                    .and_downcast::<GameObject>()
                    .map(|o| o.id());
                if let (Some(id), Some(f)) = (id, on_open.borrow().as_ref()) {
                    f(id);
                }
            });
        }

        let empty = adw::StatusPage::builder()
            .icon_name("applications-games-symbolic")
            .title("No games yet")
            .description("Install a Windows game to see it here.")
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
        let update = {
            let stack = stack.clone();
            move |n: u32| stack.set_visible_child_name(if n == 0 { "empty" } else { "grid" })
        };
        store.connect_items_changed(move |s, _, _, _| update(s.n_items()));
        stack.set_visible_child_name("empty");

        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        root.append(&search_bar);
        root.append(&stack);
        Self {
            root,
            search_bar,
            sort,
            store,
            on_open,
        }
    }

    pub fn connect_open(&self, f: impl Fn(i64) + 'static) {
        *self.on_open.borrow_mut() = Some(Box::new(f));
    }

    /// Reloads all games from the database (tiny queries, safe on the GTK thread).
    pub fn refresh(&self, state: &AppState) {
        let games = match state.db.list_games() {
            Ok(g) => g,
            Err(e) => return state.toast(&e.to_string()),
        };
        let items: Vec<GameObject> = games
            .iter()
            .map(|g| {
                let last = state
                    .db
                    .last_launch(g.id)
                    .ok()
                    .flatten()
                    .map(|r| r.started_at);
                GameObject::new(g, last)
            })
            .collect();
        self.store.splice(0, self.store.n_items(), &items);
    }
}

fn card() -> gtk4::Box {
    let b = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    b.set_size_request(150, -1);
    b.set_margin_top(12);
    b.set_margin_bottom(12);
    b.set_margin_start(6);
    b.set_margin_end(6);
    b.append(&adw::Avatar::new(72, None, true));
    let name = gtk4::Label::builder()
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .css_classes(["heading"])
        .build();
    let status = gtk4::Label::builder()
        .css_classes(["dim-label", "caption"])
        .build();
    b.append(&name);
    b.append(&status);
    b
}

fn bind_card(card: &gtk4::Box, g: &GameObject) {
    let avatar = card.first_child().and_downcast::<adw::Avatar>();
    let name = avatar
        .as_ref()
        .and_then(|a| a.next_sibling())
        .and_downcast::<gtk4::Label>();
    let status = name
        .as_ref()
        .and_then(|n| n.next_sibling())
        .and_downcast::<gtk4::Label>();
    if let (Some(a), Some(n), Some(s)) = (avatar, name, status) {
        a.set_text(Some(&g.name()));
        n.set_text(&g.name());
        s.set_text(status_label(&g.status()));
    }
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
    fn filter() {
        assert!(matches("Half-Life", "life"));
        assert!(matches("Half-Life", "  "));
        assert!(!matches("Half-Life", "portal"));
    }
}
