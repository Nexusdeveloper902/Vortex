//! Game page: cover header with Play/Stop, secondary actions, details.

use crate::app::AppState;
use crate::game_object::status_label;
use crate::ui::{configure, cover::cover, dialogs, install_wizard, manage, run_view};
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use vortex_core::db::{Game, GameStatus};
use vortex_core::launch;
use vortex_core::paths::HostPath;
use vortex_core::process::{Exit, Outcome, RunEvent, RunHandle};
use vortex_core::winetricks;

/// `launch_args` is a JSON array of strings; shown space-joined.
pub fn format_args(json: &str) -> String {
    serde_json::from_str::<Vec<String>>(json).map_or_else(|_| json.to_string(), |v| v.join(" "))
}

/// `env_vars` is a JSON object of strings; shown as `KEY=value` lines.
pub fn format_env(json: &str) -> String {
    serde_json::from_str::<std::collections::BTreeMap<String, String>>(json).map_or_else(
        |_| json.to_string(),
        |m| {
            m.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
    )
}

fn or_none(s: String) -> String {
    if s.is_empty() { "None".into() } else { s }
}

fn row(title: &str, value: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(glib::markup_escape_text(value))
        .subtitle_selectable(true)
        .build()
}

pub fn folder_row(state: &Rc<AppState>, title: &str, path: Option<&str>) -> adw::ActionRow {
    let r = row(title, path.unwrap_or("Not set"));
    let btn = gtk4::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Open folder")
        .valign(gtk4::Align::Center)
        .css_classes(["flat"])
        .sensitive(path.is_some())
        .build();
    if let Some(p) = path.map(str::to_owned) {
        let state = state.clone();
        btn.connect_clicked(move |_| open_folder(&state, &p));
    }
    r.add_suffix(&btn);
    r
}

fn open_folder(state: &Rc<AppState>, path: &str) {
    if !Path::new(path).is_dir() {
        return state.toast(&format!("Folder does not exist: {path}"));
    }
    let state2 = state.clone();
    gtk4::FileLauncher::new(Some(&gio::File::for_path(path))).launch(
        Some(&state.window),
        None::<&gio::Cancellable>,
        move |res| {
            if let Err(e) = res {
                state2.toast(&format!("Could not open folder: {e}"));
            }
        },
    );
}

pub fn open(state: &Rc<AppState>, id: i64) {
    match build(state, id) {
        Ok(page) => state.nav.push(&page),
        Err(e) => state.toast(&e.to_string()),
    }
}

/// `vortex --launch <id>`: show the game page and press Play. Unknown or not-installed ids only toast.
pub fn open_and_play(state: &Rc<AppState>, id: i64) {
    match state.db().get_game(id) {
        Ok(g) if g.status == GameStatus::Installed => {}
        Ok(g) => return state.toast(&format!("{} is not installed yet", g.name)),
        Err(_) => return state.toast(&format!("There is no game with id {id}")),
    }
    state.nav.pop_to_tag("library");
    open(state, id);
    if let Some(p) = state.nav.visible_page()
        && let Some(b) = find_named(p.upcast_ref(), "play-button").and_downcast::<gtk4::Button>()
        && b.label().as_deref() == Some("Play")
    {
        b.emit_clicked();
    }
}

fn find_named(w: &gtk4::Widget, name: &str) -> Option<gtk4::Widget> {
    if w.widget_name() == name {
        return Some(w.clone());
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        if let Some(f) = find_named(&x, name) {
            return Some(f);
        }
        c = x.next_sibling();
    }
    None
}

fn build(state: &Rc<AppState>, id: i64) -> vortex_core::error::Result<adw::NavigationPage> {
    let g = state.db().get_game(id)?;
    let env = state.db().get_environment(g.environment_id)?;
    let last = state.db().last_launch(id)?;

    let prefs = adw::PreferencesPage::new();

    let info = adw::PreferencesGroup::builder().title("Details").build();
    info.add(&row("Status", status_label(g.status.as_str())));
    info.add(&row("Proton", &env.runtime_path));
    info.add(&folder_row(state, "Prefix", Some(&env.prefix_path)));
    info.add(&folder_row(
        state,
        "Install folder",
        g.install_dir.as_deref(),
    ));
    info.add(&row(
        "Executable",
        g.exe_path.as_deref().unwrap_or("Not set"),
    ));
    prefs.add(&info);

    let launch = adw::PreferencesGroup::builder().title("Launch").build();
    launch.add(&row(
        "Launch arguments",
        &or_none(format_args(&g.launch_args)),
    ));
    launch.add(&row(
        "Environment variables",
        &or_none(format_env(&g.env_vars)),
    ));
    let last_text = last.map_or_else(
        || "Never".to_string(),
        |r| {
            let when = glib::DateTime::from_unix_local(r.started_at)
                .ok()
                .and_then(|d| d.format("%x %H:%M").ok())
                .map_or_else(|| r.started_at.to_string(), |s| s.to_string());
            match r.exit_code {
                Some(c) => format!("{when} (exit code {c})"),
                None => when,
            }
        },
    );
    launch.add(&row("Last launch", &last_text));
    prefs.add(&launch);

    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.pack_end(&overflow(state, &g, &env));
    view.add_top_bar(&header);
    let col = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    col.append(&hero(state, &g, env.id));
    col.append(&secondary(state, &g, &env));
    col.append(&prefs);
    let clamp = adw::Clamp::builder().maximum_size(760).child(&col).build();
    view.set_content(Some(&gtk4::ScrolledWindow::builder().child(&clamp).build()));
    Ok(adw::NavigationPage::new(&view, &g.name))
}

fn pill_button(label: &str, cls: &str) -> gtk4::Button {
    gtk4::Button::builder()
        .label(label)
        .css_classes(["pill", cls])
        .build()
}

/// Large cover, name, status and the Play / Stop pill (or Continue installation).
fn hero(state: &Rc<AppState>, g: &Game, env_id: i64) -> gtk4::Box {
    let hero = gtk4::Box::new(gtk4::Orientation::Horizontal, 24);
    hero.set_margin_top(24);
    hero.set_margin_bottom(12);
    hero.set_margin_start(24);
    hero.set_margin_end(24);
    hero.append(&cover(&g.name, 120, 180));
    let col = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    col.set_valign(gtk4::Align::Center);
    col.append(
        &gtk4::Label::builder()
            .label(&g.name)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["hero-title"])
            .build(),
    );
    let sub = gtk4::Label::builder()
        .label(status_label(g.status.as_str()))
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    col.append(&sub);
    if g.status != GameStatus::Installed {
        let b = pill_button("Continue installation", "suggested-action");
        b.set_halign(gtk4::Align::Start);
        let (state, id) = (state.clone(), g.id);
        b.connect_clicked(move |_| install_wizard::resume(&state, id));
        col.append(&b);
        hero.append(&col);
        return hero;
    }
    let play = pill_button("Play", "suggested-action");
    play.set_widget_name("play-button");
    play.set_halign(gtk4::Align::Start);
    play.set_size_request(140, -1);
    let mark = gtk4::Button::builder()
        .label("Mark as finished")
        .visible(false)
        .halign(gtk4::Align::Start)
        .css_classes(["flat"])
        .build();
    col.append(&play);
    col.append(&mark);
    hero.append(&col);

    let running: Rc<RefCell<Option<RunHandle>>> = Rc::new(RefCell::new(state.runs.active(env_id)));
    let set_running = {
        let (play, sub) = (play.clone(), sub.clone());
        move |on: bool| {
            play.set_label(if on { "Stop" } else { "Play" });
            play.set_sensitive(true);
            play.remove_css_class(if on {
                "suggested-action"
            } else {
                "destructive-action"
            });
            play.add_css_class(if on {
                "destructive-action"
            } else {
                "suggested-action"
            });
            if on {
                sub.set_label("Running");
            }
        }
    };
    if running.borrow().is_some() {
        set_running(true);
    }
    {
        let running = running.clone();
        mark.connect_clicked(move |_| {
            if let Some(h) = running.borrow().as_ref() {
                h.mark_finished();
            }
        });
    }
    let (state, id) = (state.clone(), g.id);
    let installed_label = sub.label().to_string();
    play.connect_clicked(move |b| {
        if let Some(h) = running.borrow().as_ref() {
            h.stop();
            b.set_sensitive(false);
            return;
        }
        b.set_sensitive(false);
        let (state, running, sub, mark, set_running, label) = (
            state.clone(),
            running.clone(),
            sub.clone(),
            mark.clone(),
            set_running.clone(),
            installed_label.clone(),
        );
        glib::spawn_future_local(async move {
            let (runs, db, paths) = (state.runs.clone(), state.db.clone(), state.paths.clone());
            let res =
                crate::app::blocking(move || launch::launch_game(&runs, &db, &paths, id)).await;
            let (h, rx) = match res {
                Some(Ok(v)) => v,
                Some(Err(e)) => {
                    set_running(false);
                    return state.toast(&e.to_string());
                }
                None => return,
            };
            let run_id = h.run_id();
            *running.borrow_mut() = Some(h);
            set_running(true);
            while let Ok(ev) = rx.recv().await {
                match ev {
                    RunEvent::Phase(p) => sub.set_label(run_view::phase_label(p)),
                    RunEvent::Uncertain(_) => {
                        sub.set_label("Background processes are still running");
                        mark.set_visible(true);
                    }
                    RunEvent::Finished(o) => {
                        *running.borrow_mut() = None;
                        mark.set_visible(false);
                        set_running(false);
                        sub.set_label(&label);
                        if !o.stopped && !o.user_forced && o.exit != Some(Exit::Low8(0)) {
                            launch_failed(&state, id, run_id, &o);
                        } else {
                            state.toast(&run_view::outcome_label(&o));
                        }
                    }
                    _ => {}
                }
            }
        });
    });
    hero
}

/// A launch that ended badly: what diagnose found (stored by core before `Finished`), with a way
/// to the logs.
fn launch_failed(state: &Rc<AppState>, game_id: i64, run_id: i64, o: &Outcome) {
    let findings = state
        .db()
        .get_run(run_id)
        .ok()
        .and_then(|r| r.findings)
        .map(|j| vortex_core::diagnose::from_json(&j))
        .unwrap_or_default();
    let body = failure_text(&run_view::outcome_label(o), &findings);
    let d = adw::AlertDialog::new(Some("The game did not run correctly"), Some(&body));
    d.add_response("close", "Close");
    d.add_response("logs", "Show logs");
    d.set_default_response(Some("logs"));
    d.set_close_response("close");
    let s = state.clone();
    d.connect_response(None, move |_, r| {
        if r == "logs" {
            manage::logs_page(&s, game_id);
        }
    });
    d.present(Some(&state.window));
}

pub fn failure_text(outcome: &str, findings: &[vortex_core::diagnose::Finding]) -> String {
    let mut t = outcome.to_string();
    for f in findings {
        t.push_str("\n\n");
        t.push_str(&dialogs::finding_text(f));
    }
    t
}

/// Configure, Run EXE/MSI, Dependencies, Logs, Back up, Restore.
type Action = Box<dyn Fn(&Rc<AppState>)>;

fn secondary(state: &Rc<AppState>, g: &Game, env: &vortex_core::db::Environment) -> gtk4::FlowBox {
    let flow = gtk4::FlowBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .max_children_per_line(6)
        .homogeneous(false)
        .margin_start(24)
        .margin_end(24)
        .margin_bottom(12)
        .column_spacing(8)
        .row_spacing(8)
        .build();
    let installed = g.status == GameStatus::Installed;
    let (id, env_id) = (g.id, env.id);
    let add = |icon: &str, label: &str, enabled: bool, tip: Option<String>, f: Action| {
        let b = gtk4::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name(icon)
                    .label(label)
                    .build(),
            )
            .sensitive(enabled)
            .build();
        if let Some(t) = tip {
            b.set_tooltip_text(Some(&t));
        }
        let state = state.clone();
        b.connect_clicked(move |_| f(&state));
        flow.append(&b);
    };
    add(
        "emblem-system-symbolic",
        "Configure",
        true,
        None,
        Box::new(move |s| configure::open(s, id)),
    );
    add(
        "system-run-symbolic",
        "Run EXE/MSI",
        installed,
        None,
        Box::new(move |s| manage::run_exe(s, env_id)),
    );
    let support = HostPath::new(&env.runtime_path)
        .map_err(|e| e.to_string())
        .and_then(|rt| winetricks::require_support(&rt).map_err(|e| e.to_string()));
    add(
        "application-x-addon-symbolic",
        "Dependencies",
        installed && support.is_ok(),
        support.err(),
        Box::new(move |s| manage::dependencies(s, env_id)),
    );
    add(
        "text-x-generic-symbolic",
        "Logs",
        true,
        None,
        Box::new(move |s| manage::logs_page(s, id)),
    );
    add(
        "document-save-symbolic",
        "Back up",
        installed,
        None,
        Box::new(move |s| manage::backup_now(s, env_id)),
    );
    add(
        "document-revert-symbolic",
        "Restore",
        installed,
        None,
        Box::new(move |s| manage::restore_dialog(s, env_id)),
    );
    flow
}

/// Overflow menu: run the game's own uninstaller, or remove everything.
fn overflow(
    state: &Rc<AppState>,
    g: &Game,
    env: &vortex_core::db::Environment,
) -> gtk4::MenuButton {
    let roots: Vec<std::path::PathBuf> = match g.install_dir.as_deref() {
        Some(d) => vec![d.into()],
        None => ["Program Files", "Program Files (x86)", "GOG Games", "Games"]
            .iter()
            .map(|r| {
                std::path::Path::new(&env.prefix_path)
                    .join("drive_c")
                    .join(r)
            })
            .collect(),
    };
    let found = dialogs::find_uninstallers(&roots, 3);
    let group = gio::SimpleActionGroup::new();
    let un = gio::SimpleAction::new("uninstaller", None);
    un.set_enabled(g.status == GameStatus::Installed && !found.is_empty());
    {
        let (state, id) = (state.clone(), g.id);
        un.connect_activate(move |_, _| {
            if let Some(f) = found.first() {
                manage::run_uninstaller(&state, id, f.clone());
            }
        });
    }
    let rm = gio::SimpleAction::new("remove", None);
    {
        let (state, id) = (state.clone(), g.id);
        rm.connect_activate(move |_, _| manage::remove_completely(&state, id));
    }
    group.add_action(&un);
    group.add_action(&rm);
    let menu = gio::Menu::new();
    menu.append(Some("Run the game's uninstaller"), Some("game.uninstaller"));
    menu.append(Some("Remove completely..."), Some("game.remove"));
    let b = gtk4::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .tooltip_text("Uninstall or remove")
        .build();
    b.insert_action_group("game", Some(&group));
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_text_lists_findings() {
        use vortex_core::diagnose::{Class, Confidence, Finding};
        let f = Finding {
            class: Class::AppCrash,
            confidence: Confidence::Confirmed,
            evidence: vec!["wine: Unhandled page fault".into()],
        };
        let t = failure_text("Ended (exit code 5)", &[f]);
        assert!(t.starts_with("Ended (exit code 5)\n\nThe program crashed. (confirmed)\n"));
        assert_eq!(
            failure_text("Ended (exit code 1)", &[]),
            "Ended (exit code 1)"
        );
    }

    #[test]
    fn args_and_env() {
        assert_eq!(
            format_args(r#"["-windowed","-w","800"]"#),
            "-windowed -w 800"
        );
        assert_eq!(format_args("[]"), "");
        assert_eq!(format_args("not json"), "not json");
        assert_eq!(format_env(r#"{"B":"2","A":"1"}"#), "A=1\nB=2");
        assert_eq!(format_env("{}"), "");
    }
}
