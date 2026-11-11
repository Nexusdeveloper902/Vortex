//! Management flows on a game's environment: dependencies, backup/restore, logs, uninstall.

use crate::app::{AppState, blocking};
use crate::ui::dialogs::{self, confirm, format_plan, human_size, with_db};
use crate::ui::run_view;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use vortex_core::db::{RunKind, RunState};
use vortex_core::paths::{HostPath, PrefixDir};
use vortex_core::runner::Tools;
use vortex_core::{backup, delete, launch, winetricks};

pub fn run_kind_label(k: RunKind) -> &'static str {
    match k {
        RunKind::PrefixInit => "Prefix setup",
        RunKind::Install => "Installer",
        RunKind::Launch => "Game",
        RunKind::Exec => "Run in environment",
        RunKind::Winetricks => "Dependency",
    }
}

pub fn run_state_label(s: RunState) -> &'static str {
    match s {
        RunState::Running => "Running",
        RunState::Waiting => "Waiting for processes",
        RunState::Finished => "Finished",
        RunState::Stopped => "Stopped",
        RunState::Interrupted => "Interrupted",
    }
}

fn when(ts: i64) -> String {
    glib::DateTime::from_unix_local(ts)
        .ok()
        .and_then(|d| d.format("%x %H:%M").ok())
        .map_or_else(|| ts.to_string(), |s| s.to_string())
}

fn dialog(title: &str, child: &impl IsA<gtk4::Widget>) -> adw::Dialog {
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(child));
    adw::Dialog::builder()
        .title(title)
        .content_width(560)
        .content_height(640)
        .child(&tv)
        .build()
}

fn status(icon: &str, title: &str, desc: &str) -> adw::StatusPage {
    adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .description(desc)
        .build()
}

// ---- run an exe / msi ----

pub fn run_exe(state: &Rc<AppState>, env_id: i64) {
    let s = state.clone();
    dialogs::pick_file(state, "Choose an .exe or .msi", move |p| {
        let s = s.clone();
        glib::spawn_future_local(async move {
            let (runs, db, paths) = (s.runs.clone(), s.db.clone(), s.paths.clone());
            let res = blocking(move || {
                let f = HostPath::new(&p)?;
                launch::run_in_env(&runs, &db, &paths, env_id, &f, vec![], vec![])
            })
            .await;
            match res {
                Some(Ok((h, rx))) => run_view::present(&s, "Run in this environment", h, rx),
                Some(Err(e)) => s.toast(&e.to_string()),
                None => {}
            }
        });
    });
}

// ---- winetricks ----

pub fn dependencies(state: &Rc<AppState>, env_id: i64) {
    let Ok(env) = state.db().get_environment(env_id) else {
        return;
    };
    let (Ok(rt), Ok(prefix)) = (
        HostPath::new(&env.runtime_path),
        PrefixDir::new(&env.prefix_path, state.paths.home(), &[]),
    ) else {
        return state.toast("The environment paths are invalid");
    };
    let stack = gtk4::Stack::new();
    stack.add_named(&adw::Spinner::new(), Some("loading"));
    let d = dialog("Dependencies", &stack);
    if let Err(e) = winetricks::require_support(&rt) {
        stack.add_named(
            &status(
                "dialog-information-symbolic",
                "Not available",
                &e.to_string(),
            ),
            Some("msg"),
        );
        stack.set_visible_child_name("msg");
        d.present(Some(&state.window));
        return;
    }
    d.present(Some(&state.window));
    let (state, stack, d2) = (state.clone(), stack, d.clone());
    glib::spawn_future_local(async move {
        let (rt2, p2, paths2) = (rt.clone(), prefix.clone(), state.paths.clone());
        let res = blocking(move || {
            let verbs = winetricks::list_verbs(&Tools::default(), &rt2, &paths2)?;
            let inst = winetricks::installed(&p2)?;
            Ok::<_, vortex_core::error::Error>((verbs, inst))
        })
        .await;
        let (verbs, inst) = match res {
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                stack.add_named(
                    &status(
                        "dialog-error-symbolic",
                        "Could not list dependencies",
                        &e.to_string(),
                    ),
                    Some("msg"),
                );
                stack.set_visible_child_name("msg");
                return;
            }
            None => return,
        };
        let verbs = std::sync::Arc::new(verbs);
        let search = gtk4::SearchEntry::builder()
            .placeholder_text("Search dependencies")
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .build();
        let list = gtk4::ListBox::builder()
            .selection_mode(gtk4::SelectionMode::None)
            .css_classes(["boxed-list"])
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .margin_top(12)
            .build();
        for v in verbs.iter() {
            let row = adw::ActionRow::builder()
                .title(&v.name)
                .subtitle(&v.description)
                .use_markup(false)
                .build();
            if inst.contains(&v.name) {
                let i = gtk4::Image::from_icon_name("object-select-symbolic");
                i.set_tooltip_text(Some("Installed"));
                row.add_suffix(&i);
            } else {
                let b = gtk4::Button::builder()
                    .label("Install")
                    .valign(gtk4::Align::Center)
                    .build();
                let (state, verbs, d2, name) =
                    (state.clone(), verbs.clone(), d2.clone(), v.name.clone());
                b.connect_clicked(move |_| {
                    let (state, verbs, d2, name) =
                        (state.clone(), verbs.clone(), d2.clone(), name.clone());
                    glib::spawn_future_local(async move {
                        let (runs, db, paths) =
                            (state.runs.clone(), state.db.clone(), state.paths.clone());
                        let res = blocking(move || {
                            winetricks::install_verb(&runs, &db, &paths, env_id, &name, &verbs)
                        })
                        .await;
                        match res {
                            Some(Ok(Some((h, rx)))) => {
                                d2.close();
                                run_view::present(&state, "Installing dependency", h, rx);
                            }
                            Some(Ok(None)) => state.toast("Already installed"),
                            Some(Err(e)) => state.toast(&e.to_string()),
                            None => {}
                        }
                    });
                });
                row.add_suffix(&b);
            }
            list.append(&row);
        }
        {
            let s2 = search.clone();
            list.set_filter_func(move |r| {
                let q = s2.text().to_lowercase();
                let Some(r) = r.downcast_ref::<adw::ActionRow>() else {
                    return true;
                };
                q.is_empty()
                    || r.title().to_lowercase().contains(&q)
                    || r.subtitle().unwrap_or_default().to_lowercase().contains(&q)
            });
            let list = list.clone();
            search.connect_search_changed(move |_| list.invalidate_filter());
        }
        let col = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        col.append(&search);
        col.append(
            &gtk4::ScrolledWindow::builder()
                .vexpand(true)
                .child(&list)
                .build(),
        );
        stack.add_named(&col, Some("list"));
        stack.set_visible_child_name("list");
    });
}

// ---- backup / restore ----

pub fn backup_now(state: &Rc<AppState>, env_id: i64) {
    // Held for the whole backup so Play cannot start in the prefix being archived.
    let guard = match state.runs.reserve(env_id) {
        Ok(g) => g,
        Err(e) => return state.toast(&e.to_string()),
    };
    state.toast("Backing up...");
    let state = state.clone();
    glib::spawn_future_local(async move {
        let r = with_db(&state, move |db, paths| {
            let _guard = guard;
            backup::backup(db, paths, env_id, &AtomicBool::new(false))
        })
        .await;
        match r {
            Ok(b) => state.toast(&format!(
                "Backup created ({})",
                human_size(b.size_bytes.max(0) as u64)
            )),
            Err(e) => state.toast(&e.to_string()),
        }
    });
}

pub fn restore_dialog(state: &Rc<AppState>, env_id: i64) {
    let backups: Vec<_> = state
        .db()
        .list_backups()
        .unwrap_or_default()
        .into_iter()
        .filter(|b| b.environment_id == Some(env_id))
        .collect();
    if backups.is_empty() {
        return state.toast("No backups of this game yet");
    }
    let list = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_start(12)
        .margin_end(12)
        .margin_top(12)
        .margin_bottom(12)
        .build();
    let scroll = gtk4::ScrolledWindow::builder().child(&list).build();
    let d = dialog("Restore a backup", &scroll);
    for b in backups {
        let row = adw::ActionRow::builder()
            .title(when(b.created_at))
            .subtitle(human_size(b.size_bytes.max(0) as u64))
            .build();
        let btn = gtk4::Button::builder()
            .label("Restore")
            .valign(gtk4::Align::Center)
            .css_classes(["destructive-action"])
            .build();
        let (state, d) = (state.clone(), d.clone());
        btn.connect_clicked(move |_| {
            let (state2, d) = (state.clone(), d.clone());
            let id = b.id;
            confirm(
                &state,
                "Replace the current prefix?",
                "The game's current Wine prefix (saves and settings inside it) is replaced by this backup.",
                "Restore",
                true,
                move || {
                    d.close();
                    let state = state2.clone();
                    // Held for the whole restore: the prefix is renamed away and replaced.
                    let guard = match state.runs.reserve(env_id) {
                        Ok(g) => g,
                        Err(e) => return state.toast(&e.to_string()),
                    };
                    state.toast("Restoring...");
                    glib::spawn_future_local(async move {
                        let r = with_db(&state, move |db, paths| {
                            let _guard = guard;
                            let env = db.get_environment(env_id)?;
                            let p = PrefixDir::new(&env.prefix_path, paths.home(), &[])?;
                            backup::restore(db, paths, id, &p, true)
                        })
                        .await;
                        state.toast(&match r {
                            Ok(()) => "Backup restored".to_string(),
                            Err(e) => e.to_string(),
                        });
                    });
                },
            );
        });
        row.add_suffix(&btn);
        list.append(&row);
    }
    d.present(Some(&state.window));
}

// ---- logs ----

pub fn logs_page(state: &Rc<AppState>, game_id: i64) {
    let runs = {
        let db = state.db();
        db.get_game(game_id)
            .and_then(|g| db.runs_for_environment(g.environment_id, game_id))
            .unwrap_or_default()
    };
    let list = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_start(16)
        .margin_end(16)
        .margin_top(16)
        .margin_bottom(16)
        .build();
    for r in &runs {
        let code = r
            .exit_code
            .map_or(String::new(), |c| format!(", exit code {c}"));
        let row = adw::ActionRow::builder()
            .title(format!(
                "{} - {}",
                run_kind_label(r.kind),
                when(r.started_at)
            ))
            .subtitle(format!("{}{code}", run_state_label(r.state)))
            .activatable(true)
            .build();
        row.add_suffix(&gtk4::Image::from_icon_name("go-next-symbolic"));
        let (state, path, title) = (state.clone(), r.log_path.clone(), run_kind_label(r.kind));
        row.connect_activated(move |_| log_viewer(&state, &path, title));
        list.append(&row);
    }
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    if runs.is_empty() {
        tv.set_content(Some(&status(
            "document-open-symbolic",
            "No runs yet",
            "Logs appear after the game or an installer runs.",
        )));
    } else {
        let clamp = adw::Clamp::builder().maximum_size(640).child(&list).build();
        tv.set_content(Some(&gtk4::ScrolledWindow::builder().child(&clamp).build()));
    }
    state.nav.push(&adw::NavigationPage::new(&tv, "Logs"));
}

fn log_viewer(state: &Rc<AppState>, path: &str, title: &str) {
    let view = gtk4::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .left_margin(8)
        .build();
    let export = gtk4::Button::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text("Export log")
        .build();
    let header = adw::HeaderBar::new();
    header.pack_end(&export);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&gtk4::ScrolledWindow::builder().child(&view).build()));
    state.nav.push(&adw::NavigationPage::new(&tv, title));
    let p = path.to_string();
    {
        let (state, p) = (state.clone(), p.clone());
        export.connect_clicked(move |_| {
            let (s2, p) = (state.clone(), p.clone());
            let name = std::path::Path::new(&p)
                .file_name()
                .map_or("vortex.log".into(), |n| n.to_string_lossy().into_owned());
            dialogs::save_file(&state, "Export log", &name, move |dest| {
                glib::spawn_future_local(async move {
                    let r = blocking(move || std::fs::copy(&p, &dest).map(|_| ())).await;
                    s2.toast(&match r {
                        Some(Ok(())) => "Log exported".to_string(),
                        Some(Err(e)) => format!("Could not export: {e}"),
                        None => return,
                    });
                });
            });
        });
    }
    let buf = view.buffer();
    glib::spawn_future_local(async move {
        // The log is already redacted by core. Show at most the last 1 MiB.
        let text = blocking(move || {
            std::fs::read(&p).map(|b| {
                let s = b.len().saturating_sub(1 << 20);
                String::from_utf8_lossy(&b[s..]).into_owned()
            })
        })
        .await;
        buf.set_text(&match text {
            Some(Ok(t)) => t,
            Some(Err(e)) => format!("Could not read the log: {e}"),
            None => return,
        });
    });
}

// ---- uninstall ----

/// Runs the game's own uninstaller (unins*.exe) in its environment.
pub fn run_uninstaller(state: &Rc<AppState>, game_id: i64, exe: std::path::PathBuf) {
    let Ok(g) = state.db().get_game(game_id) else {
        return;
    };
    let s = state.clone();
    confirm(
        state,
        "Run the uninstaller?",
        &format!(
            "{}\n\nThis runs the game's own uninstaller inside its environment.",
            exe.display()
        ),
        "Run uninstaller",
        false,
        move || {
            let (s, exe) = (s.clone(), exe.clone());
            glib::spawn_future_local(async move {
                let (runs, db, paths) = (s.runs.clone(), s.db.clone(), s.paths.clone());
                let env = g.environment_id;
                let res = blocking(move || {
                    launch::run_uninstaller(&runs, &db, &paths, env, &HostPath::new(&exe)?)
                })
                .await;
                match res {
                    Some(Ok((h, rx))) => run_view::present(&s, "Uninstaller", h, rx),
                    Some(Err(e)) => s.toast(&e.to_string()),
                    None => {}
                }
            });
        },
    );
}

/// "Remove completely": shows the exact deletion plan, then executes it after confirmation.
pub fn remove_completely(state: &Rc<AppState>, game_id: i64) {
    let state = state.clone();
    glib::spawn_future_local(async move {
        let plan = with_db(&state, move |db, paths| {
            delete::plan_game_removal(db, paths, game_id)
        })
        .await;
        let plan = match plan {
            Ok(p) => p,
            Err(e) => return state.toast(&e.to_string()),
        };
        let name = state
            .db()
            .get_game(game_id)
            .map(|g| g.name)
            .unwrap_or_default();
        let body = format_plan(&plan);
        let s = state.clone();
        confirm(
            &state,
            &format!("Remove \"{name}\" completely?"),
            &body,
            "Remove",
            true,
            move || {
                let (s, plan) = (s.clone(), plan.clone());
                // Held while files and rows are removed so nothing starts in the environment.
                let guard = match s.runs.reserve(plan.env_id) {
                    Ok(g) => g,
                    Err(e) => return s.toast(&e.to_string()),
                };
                glib::spawn_future_local(async move {
                    let r = with_db(&s, move |db, paths| {
                        let _guard = guard;
                        delete::execute(db, paths, &plan)
                    })
                    .await;
                    match r {
                        Ok(()) => {
                            s.nav.pop_to_tag("library");
                            s.toast("Removed");
                        }
                        Err(e) => s.toast(&e.to_string()),
                    }
                });
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(run_kind_label(RunKind::Winetricks), "Dependency");
        assert_eq!(run_state_label(RunState::Interrupted), "Interrupted");
    }
}
