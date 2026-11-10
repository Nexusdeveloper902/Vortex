//! Application, main window and shared UI state.

use crate::ui::{game_page, library::Library, runtimes, settings};
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use std::rc::Rc;
use vortex_core::db::Db;
use vortex_core::paths::Paths;

pub const APP_ID: &str = "io.github.nexusdeveloper902.Vortex";

/// Everything pages need. Lives on the GTK thread; the `Db` is only touched here (queries are tiny).
pub struct AppState {
    pub db: Db,
    pub paths: Paths,
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub nav: adw::NavigationView,
}

impl AppState {
    pub fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }
}

/// Runs `f` on a worker thread and awaits its result without blocking the GTK main loop.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(f());
    });
    rx.recv().await.ok()
}

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(activate);
    app.run()
}

fn activate(app: &adw::Application) {
    if let Some(w) = app.active_window() {
        w.present();
        return;
    }
    let toasts = adw::ToastOverlay::new();
    let nav = adw::NavigationView::new();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Vortex")
        .default_width(960)
        .default_height(640)
        .content(&toasts)
        .build();
    toasts.set_child(Some(&nav));

    let opened = Paths::from_xdg().and_then(|p| Db::open(&p.db_file()).map(|d| (p, d)));
    let (paths, db) = match opened {
        Ok(v) => v,
        Err(e) => {
            let page = adw::StatusPage::builder()
                .icon_name("dialog-error-symbolic")
                .title("Cannot open the Vortex data")
                .description(e.to_string())
                .build();
            toasts.set_child(Some(&page));
            window.present();
            return;
        }
    };
    let state = Rc::new(AppState {
        db,
        paths,
        window: window.clone(),
        toasts,
        nav: nav.clone(),
    });
    install_actions(app, &state);

    let library = Rc::new(Library::new(&state));
    let stack = adw::ViewStack::new();
    stack.add_titled_with_icon(
        &library.root,
        Some("library"),
        "Library",
        "view-grid-symbolic",
    );
    stack.add_titled_with_icon(
        &runtimes::build(&state),
        Some("runtimes"),
        "Runtimes",
        "application-x-executable-symbolic",
    );

    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    let search = gtk4::ToggleButton::builder()
        .icon_name("system-search-symbolic")
        .tooltip_text("Search")
        .build();
    search
        .bind_property("active", &library.search_bar, "search-mode-enabled")
        .bidirectional()
        .sync_create()
        .build();
    let menu = gio::Menu::new();
    menu.append(Some("Preferences"), Some("win.preferences"));
    menu.append(Some("About Vortex"), Some("win.about"));
    let menu_button = gtk4::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .primary(true)
        .tooltip_text("Main menu")
        .build();

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&switcher));
    header.pack_start(&search);
    header.pack_start(&library.sort);
    header.pack_end(&menu_button);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&stack));
    nav.add(&adw::NavigationPage::new(&view, "Vortex"));
    library.search_bar.set_key_capture_widget(Some(&window));

    // Open a game page; refresh the grid when coming back (status may have changed).
    {
        let state = state.clone();
        library.connect_open(move |id| game_page::open(&state, id));
    }
    {
        let library = library.clone();
        let state = state.clone();
        nav.connect_popped(move |_, _| library.refresh(&state));
    }
    library.refresh(&state);
    window.present();
}

fn install_actions(app: &adw::Application, state: &Rc<AppState>) {
    let win = &state.window;
    let add = |name: &str, f: Box<dyn Fn()>| {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| f());
        win.add_action(&a);
    };
    {
        let s = state.clone();
        add("preferences", Box::new(move || settings::present(&s)));
    }
    {
        let w = win.clone();
        add(
            "about",
            Box::new(move || {
                adw::AboutDialog::builder()
                    .application_name("Vortex")
                    .application_icon(APP_ID)
                    .developer_name("NexusDeveloper902")
                    .version(env!("CARGO_PKG_VERSION"))
                    .comments("Install and run Windows games through Proton.")
                    .build()
                    .present(Some(&w));
            }),
        );
    }
    // STUB for M12: connect the install wizard here (replace this handler).
    {
        let s = state.clone();
        add(
            "install-game",
            Box::new(move || s.toast("Not available yet")),
        );
    }
    app.set_accels_for_action("win.preferences", &["<primary>comma"]);
}
