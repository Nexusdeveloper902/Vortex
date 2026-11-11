//! Application, main window and shared UI state.

use crate::ui::{
    dialogs, game_page, install_wizard,
    library::{Library, Mode},
    runtimes, settings,
};
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};
use vortex_core::db::Db;
use vortex_core::install::Installs;
use vortex_core::paths::Paths;
use vortex_core::process::Runs;
use vortex_core::runner::Tools;

pub const APP_ID: &str = "io.github.nexusdeveloper902.Vortex";

/// Everything pages need. Lives on the GTK thread. `db`, `runs` and `installs` are shared with the
/// core worker threads; UI code only takes the lock for tiny queries (long jobs open their own
/// connection, see `dialogs::with_db`).
pub struct AppState {
    pub db: Arc<Mutex<Db>>,
    pub runs: Arc<Runs>,
    pub installs: Arc<Installs>,
    pub paths: Paths,
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub nav: adw::NavigationView,
}

impl AppState {
    pub fn db(&self) -> MutexGuard<'_, Db> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

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

fn load_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(include_str!("ui/style.css"));
    if let Some(d) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &d,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.add_main_option(
        "launch",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::Int64,
        "Start the game with this id",
        Some("ID"),
    );
    let slot: Slot = Rc::default();
    {
        let slot = slot.clone();
        app.connect_activate(move |a| activate(a, &slot));
    }
    app.connect_command_line(move |a, cl| {
        let args: Vec<String> = cl
            .arguments()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        a.activate();
        let opt = cl.options_dict().lookup::<i64>("launch").ok().flatten();
        match opt.map(Some).map_or_else(|| parse_launch(&args), Ok) {
            Ok(Some(id)) => {
                if let Some(s) = slot.borrow().clone() {
                    game_page::open_and_play(&s, id);
                }
                glib::ExitCode::SUCCESS
            }
            Ok(None) => glib::ExitCode::SUCCESS,
            Err(msg) => {
                if let Some(s) = slot.borrow().clone() {
                    s.toast(&msg);
                }
                glib::ExitCode::FAILURE
            }
        }
    });
    app.run()
}

type Slot = Rc<std::cell::RefCell<Option<Rc<AppState>>>>;

/// Finds `--launch <id>` / `--launch=<id>` in argv (argv[0] included, ignored).
pub fn parse_launch(args: &[String]) -> Result<Option<i64>, String> {
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let v = if a == "--launch" {
            it.next().map(String::as_str)
        } else {
            a.strip_prefix("--launch=")
        };
        if a == "--launch" || a.starts_with("--launch=") {
            return v
                .and_then(|v| v.parse::<i64>().ok())
                .map(Some)
                .ok_or_else(|| "--launch needs a numeric game id".to_string());
        }
    }
    Ok(None)
}

fn activate(app: &adw::Application, slot: &Slot) {
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
    // `nav` must stay unparented here: it becomes the child of the split view's content page
    // below, and a widget with a parent makes `NavigationPage::new` fail (empty content pane).

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
    let db = Arc::new(Mutex::new(db));
    let runs = Arc::new(Runs::new(paths.clone(), Tools::default(), db.clone()));
    let mut installs = Installs::new(paths.clone(), db.clone(), runs.clone());
    if let Ok(exe) = std::env::current_exe() {
        installs = installs.with_desktop_entries(exe);
    }
    // Before any run starts: marks dead runs/installs interrupted, keeps live ones as detached.
    let recovered = installs.recover();
    let state = Rc::new(AppState {
        db,
        runs,
        installs: Arc::new(installs),
        paths,
        window: window.clone(),
        toasts: toasts.clone(),
        nav: nav.clone(),
    });
    *slot.borrow_mut() = Some(state.clone());
    dialogs::quit_guard(&state);
    install_actions(app, &state);
    if let Err(e) = recovered {
        state.toast(&e.to_string());
    }

    load_css();
    let library = Rc::new(Library::new(&state));

    // Content pane: library views or the runtimes page.
    let stack = gtk4::Stack::new();
    stack.add_named(&library.root, Some("library"));
    stack.add_named(&runtimes::build(&state), Some("runtimes"));
    let title = adw::WindowTitle::new("All games", "");
    let search = gtk4::ToggleButton::builder()
        .icon_name("system-search-symbolic")
        .tooltip_text("Search")
        .build();
    search
        .bind_property("active", &library.search_bar, "search-mode-enabled")
        .bidirectional()
        .sync_create()
        .build();
    let content_header = adw::HeaderBar::new();
    content_header.set_title_widget(Some(&title));
    content_header.pack_start(&search);
    content_header.pack_end(&library.list_toggle);
    content_header.pack_end(&library.sort);
    let content_view = adw::ToolbarView::new();
    content_view.add_top_bar(&content_header);
    content_view.set_content(Some(&stack));
    nav.add(&adw::NavigationPage::with_tag(
        &content_view,
        "Vortex",
        "library",
    ));
    library.search_bar.set_key_capture_widget(Some(&window));

    // Sidebar (Bottles-style): sections plus a "+" menu for install / add existing.
    let sections = [
        ("All games", "view-grid-symbolic"),
        ("Recently played", "document-open-recent-symbolic"),
        ("Needs attention", "dialog-warning-symbolic"),
        ("Runtimes", "application-x-executable-symbolic"),
    ];
    let side_list = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .build();
    for (label, icon) in sections {
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
        row.set_margin_top(10);
        row.set_margin_bottom(10);
        row.append(&gtk4::Image::from_icon_name(icon));
        row.append(&gtk4::Label::new(Some(label)));
        side_list.append(&row);
    }
    let add_menu = gio::Menu::new();
    add_menu.append(Some("Install a game..."), Some("win.install-game"));
    add_menu.append(Some("Add an existing game..."), Some("win.add-existing"));
    let add_btn = gtk4::MenuButton::builder()
        .icon_name("list-add-symbolic")
        .menu_model(&add_menu)
        .tooltip_text("Install or add a game")
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
    let side_header = adw::HeaderBar::new();
    side_header.set_title_widget(Some(&adw::WindowTitle::new("Vortex", "")));
    side_header.pack_start(&add_btn);
    side_header.pack_end(&menu_button);
    let side_view = adw::ToolbarView::new();
    side_view.add_top_bar(&side_header);
    side_view.set_content(Some(&side_list));
    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&adw::NavigationPage::new(&side_view, "Vortex")));
    split.set_content(Some(&adw::NavigationPage::new(&nav, "Content")));
    split.set_min_sidebar_width(200.0);
    split.set_max_sidebar_width(260.0);
    {
        let (library, stack, title, nav, split) = (
            library.clone(),
            stack.clone(),
            title.clone(),
            nav.clone(),
            split.clone(),
        );
        let (search, sort, toggle) = (
            search.clone(),
            library.sort.clone(),
            library.list_toggle.clone(),
        );
        side_list.connect_row_selected(move |_, row| {
            let Some(i) = row.map(|r| r.index()) else {
                return;
            };
            nav.pop_to_tag("library");
            title.set_title(sections[i as usize].0);
            let is_lib = i < 3;
            stack.set_visible_child_name(if is_lib { "library" } else { "runtimes" });
            for w in [
                search.upcast_ref::<gtk4::Widget>(),
                sort.upcast_ref(),
                toggle.upcast_ref(),
            ] {
                w.set_visible(is_lib);
            }
            library.set_mode([Mode::All, Mode::Recent, Mode::Attention, Mode::All][i as usize]);
            split.set_show_content(true);
        });
    }
    if let Some(r) = side_list.row_at_index(0) {
        side_list.select_row(Some(&r));
    }
    split.set_show_content(false);
    // Narrow windows: sidebar becomes a page of its own.
    window.set_width_request(360);
    if let Ok(cond) = adw::BreakpointCondition::parse("max-width: 640sp") {
        let bp = adw::Breakpoint::new(cond);
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(bp);
    }
    let banner = dialogs::detached_banner(&state);
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.append(&banner);
    tool_banners(&state, &root, &side_list);
    split.set_vexpand(true);
    root.append(&split);
    toasts.set_child(Some(&root));

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
    {
        let s = state.clone();
        add("install-game", Box::new(move || install_wizard::open(&s)));
    }
    {
        let s = state.clone();
        add(
            "add-existing",
            Box::new(move || install_wizard::open_existing(&s)),
        );
    }
    app.set_accels_for_action("win.preferences", &["<primary>comma"]);
}

/// Startup tool check (M18): banners above the split view, re-checked by "Check again".
/// Nothing is blocked; a missing umu-run just explains itself before the raw spawn error would.
fn tool_banners(state: &Rc<AppState>, root: &gtk4::Box, side_list: &gtk4::ListBox) {
    let mk = |title: &str, button: &str| {
        let b = adw::Banner::new(title);
        b.set_button_label(Some(button));
        b.set_revealed(false);
        root.append(&b);
        b
    };
    let umu = mk(
        "umu-run is required to install and run games. Arch: install umu-launcher. Debian, \
         Ubuntu, Fedora: download it from github.com/Open-Wine-Components/umu-launcher/releases.",
        "Check again",
    );
    let wt = mk(
        "winetricks is not installed. Only the Dependencies feature needs it.",
        "Check again",
    );
    let rt = mk(
        "No Proton runtime found. Open Runtimes to download GE-Proton.",
        "Open Runtimes",
    );
    let run = {
        let (state, umu, wt, rt) = (state.clone(), umu.clone(), wt.clone(), rt.clone());
        Rc::new(move || {
            let (home, dir) = (state.paths.home().to_path_buf(), state.paths.runtimes_dir());
            let (umu, wt, rt) = (umu.clone(), wt.clone(), rt.clone());
            glib::spawn_future_local(async move {
                let Some(c) = blocking(move || {
                    let path = std::env::var_os("PATH").unwrap_or_default();
                    Tools::default().check(&path, &home, &dir)
                })
                .await
                else {
                    return;
                };
                umu.set_revealed(c.umu_run.is_none());
                wt.set_revealed(c.winetricks.is_none());
                rt.set_revealed(c.runtimes.is_empty());
            });
        })
    };
    for b in [&umu, &wt] {
        let run = run.clone();
        b.connect_button_clicked(move |_| run());
    }
    {
        let side_list = side_list.clone();
        rt.connect_button_clicked(move |_| {
            if let Some(r) = side_list.row_at_index(3) {
                side_list.select_row(Some(&r));
            }
        });
    }
    run();
}

#[cfg(test)]
mod tests {
    use super::parse_launch;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn launch_args() {
        assert_eq!(parse_launch(&v(&["vortex"])), Ok(None));
        assert_eq!(parse_launch(&v(&["vortex", "--launch", "7"])), Ok(Some(7)));
        assert_eq!(parse_launch(&v(&["vortex", "--launch=12"])), Ok(Some(12)));
        assert!(parse_launch(&v(&["vortex", "--launch"])).is_err());
        assert!(parse_launch(&v(&["vortex", "--launch", "x"])).is_err());
    }
}
