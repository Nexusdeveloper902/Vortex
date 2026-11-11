//! Shared dialog helpers: confirmations, file pickers, runtime combo, detached-run banner, and the
//! pure formatting functions behind them (unit tested).

use crate::app::{AppState, blocking};
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use vortex_core::db::{Db, InstallState, Run};
use vortex_core::delete::DeletionPlan;
use vortex_core::diagnose::{Class, Confidence, Finding};
use vortex_core::error::{Error, Result};
use vortex_core::install::Installs;
use vortex_core::paths::Paths;
use vortex_core::process::{Runs, program_name};
use vortex_core::runtime::{self, Runtime};

// ---- pure helpers ----

pub fn human_size(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Text of the "Remove completely" confirmation: exactly what will be deleted, then what the user
/// must remove by hand.
pub fn format_plan(plan: &DeletionPlan) -> String {
    let mut out = String::new();
    if plan.items.is_empty() {
        out.push_str("No files will be deleted; only the library entry is removed.\n");
    } else {
        out.push_str("These folders will be deleted permanently:\n");
        let mut total = 0u64;
        for i in &plan.items {
            total += i.bytes;
            out.push_str(&format!(
                "\n\u{2022} {} ({})",
                i.path.as_path().display(),
                human_size(i.bytes)
            ));
        }
        out.push_str(&format!("\n\nTotal: {}\n", human_size(total)));
    }
    if plan.remove_env {
        // With nothing to delete there is no list above; a kept prefix is listed below.
        if !plan.items.is_empty() {
            out.push_str("\nThe game's Wine prefix is deleted only if it is listed above.\n");
        }
    } else {
        out.push_str("\nThe prefix is shared with other games and is kept.\n");
    }
    if !plan.manual.is_empty() {
        out.push_str("\nVortex will NOT delete these folders. Remove them yourself if you want:\n");
        for m in &plan.manual {
            out.push_str(&format!("\n\u{2022} {}", m.as_path().display()));
        }
        out.push('\n');
    }
    out.push_str("\nInstaller files you picked are never touched.");
    out
}

pub fn install_state_label(s: InstallState) -> &'static str {
    match s {
        InstallState::Created => "Preparing",
        InstallState::PrefixInit => "Creating the Wine prefix",
        InstallState::Running => "Running the installer",
        InstallState::Waiting => "Waiting for background processes",
        InstallState::SelectExe => "Choose the game's executable",
        InstallState::Done => "Done",
        InstallState::Failed => "Failed",
        InstallState::Cancelled => "Cancelled",
        InstallState::Interrupted => "Interrupted",
    }
}

pub fn is_resting(s: InstallState) -> bool {
    matches!(
        s,
        InstallState::SelectExe
            | InstallState::Done
            | InstallState::Failed
            | InstallState::Cancelled
            | InstallState::Interrupted
    )
}

/// One-line user text for a diagnostic finding, with its first evidence line.
pub fn finding_text(f: &Finding) -> String {
    let what = match f.class {
        Class::InstallerFailure => "The installer reported a failure.",
        Class::RuntimeInitFailure => "Proton could not initialise the environment.",
        Class::MissingDependency => {
            "A required component is missing. Try Manage dependencies on the game page."
        }
        Class::MissingExecutable => "The executable was not found.",
        Class::AppCrash => "The program crashed.",
        Class::MissingInstallerData => {
            "The installer's source files are missing. Keep the source folder in place."
        }
        Class::WrongWorkingDirectory => "The program was started from the wrong folder.",
        Class::GraphicsPrerequisite => "A graphics (Vulkan) prerequisite seems to be missing.",
        Class::MissingFile => "The program could not find a file it needs.",
        Class::Unknown => "The run failed for an unknown reason. See the log.",
    };
    let conf = match f.confidence {
        Confidence::Confirmed => "confirmed",
        Confidence::Likely => "likely",
    };
    let hint = vortex_core::diagnose::finding_verb(f)
        .map(|v| format!("\nSuggested dependency: {v}"))
        .unwrap_or_default();
    match f.evidence.first() {
        Some(e) => format!("{what} ({conf})\n{e}{hint}"),
        None => format!("{what} ({conf}){hint}"),
    }
}

// ---- widgets ----

/// Runs `f` with a private database connection on a worker thread (WAL: does not block the UI's
/// connection). For long jobs: backup, restore, delete.
pub async fn with_db<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&Db, &Paths) -> Result<T> + Send + 'static,
) -> Result<T> {
    let paths = state.paths.clone();
    blocking(move || {
        let db = Db::open(&paths.db_file())?;
        f(&db, &paths)
    })
    .await
    .unwrap_or(Err(Error::Cancelled))
}

pub fn confirm(
    state: &Rc<AppState>,
    heading: &str,
    body: &str,
    action: &str,
    destructive: bool,
    f: impl Fn() + 'static,
) {
    let d = adw::AlertDialog::new(Some(heading), Some(body));
    d.add_response("cancel", "Cancel");
    d.add_response("ok", action);
    d.set_response_appearance(
        "ok",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    d.set_default_response(Some("cancel"));
    d.set_close_response("cancel");
    d.connect_response(None, move |_, id| {
        if id == "ok" {
            f();
        }
    });
    d.present(Some(&state.window));
}

pub fn pick_file(state: &AppState, title: &str, cb: impl FnOnce(PathBuf) + 'static) {
    let d = gtk4::FileDialog::builder().title(title).build();
    d.open(Some(&state.window), None::<&gio::Cancellable>, move |r| {
        if let Some(p) = r.ok().and_then(|f| f.path()) {
            cb(p);
        }
    });
}

pub fn pick_folder(state: &AppState, title: &str, cb: impl FnOnce(PathBuf) + 'static) {
    let d = gtk4::FileDialog::builder().title(title).build();
    d.select_folder(Some(&state.window), None::<&gio::Cancellable>, move |r| {
        if let Some(p) = r.ok().and_then(|f| f.path()) {
            cb(p);
        }
    });
}

pub fn save_file(state: &AppState, title: &str, name: &str, cb: impl FnOnce(PathBuf) + 'static) {
    let d = gtk4::FileDialog::builder()
        .title(title)
        .initial_name(name)
        .build();
    d.save(Some(&state.window), None::<&gio::Cancellable>, move |r| {
        if let Some(p) = r.ok().and_then(|f| f.path()) {
            cb(p);
        }
    });
}

pub type Runtimes = Rc<RefCell<Vec<Runtime>>>;

/// A combo row filled asynchronously with discovered runtimes; `preselect` is a runtime path.
pub fn runtime_combo(state: &Rc<AppState>, preselect: Option<String>) -> (adw::ComboRow, Runtimes) {
    let combo = adw::ComboRow::builder()
        .title("Proton runtime")
        .subtitle("Searching")
        .build();
    let found: Runtimes = Rc::default();
    let want = preselect.or_else(|| {
        state
            .db()
            .get_setting(crate::ui::settings::KEY_DEFAULT_RUNTIME)
            .ok()
            .flatten()
    });
    let (home, dir) = (state.paths.home().to_path_buf(), state.paths.runtimes_dir());
    let (c, f) = (combo.clone(), found.clone());
    glib::spawn_future_local(async move {
        let Some(list) = blocking(move || runtime::discover(&home, &dir)).await else {
            return;
        };
        let labels: Vec<&str> = list.iter().map(|r| r.label.as_str()).collect();
        c.set_model(Some(&gtk4::StringList::new(&labels)));
        c.set_subtitle(if list.is_empty() {
            "No Proton runtime found. Install one on the Runtimes page."
        } else {
            "Used to run this game"
        });
        if let Some(i) = list
            .iter()
            .position(|r| Some(r.path.as_path().to_string_lossy().into_owned()) == want)
        {
            c.set_selected(i as u32);
        }
        *f.borrow_mut() = list;
        c.notify("selected");
    });
    (combo, found)
}

/// Stop sequence for a detached run of any kind; returns how many processes survived. Blocking
/// (up to ~10 s): call on a worker thread. Install runs go through `Installs` so the install
/// row follows.
pub fn stop_detached(installs: &Installs, runs: &Runs, r: &Run) -> Result<usize> {
    match r.install_id {
        Some(i) => installs.stop_detached(i),
        None => runs.stop_detached(r.id),
    }
    .map(|left| left.len())
}

/// Banner for runs (any kind) whose processes survived a previous session: offers Stop or Wait,
/// and polls every one of them until it ends. `on_change` runs whenever the number of detached
/// runs changes (their games' status changed, e.g. installing -> interrupted).
pub fn detached_banner(state: &Rc<AppState>, on_change: impl Fn() + 'static) -> adw::Banner {
    let banner = adw::Banner::builder()
        .button_label("Stop or wait...")
        .revealed(false)
        .build();
    let refresh = {
        let (state, banner) = (state.clone(), banner.clone());
        let last = std::cell::Cell::new(None);
        Rc::new(move || {
            let n = state.runs.detached().map(|v| v.len()).unwrap_or(0);
            if last.replace(Some(n)).is_some_and(|prev| prev != n) {
                on_change();
            }
            banner.set_title(&format!(
                "{n} program(s) from an earlier session are still running"
            ));
            banner.set_revealed(n > 0);
        })
    };
    refresh();
    {
        let (state, refresh) = (state.clone(), refresh.clone());
        banner.connect_button_clicked(move |_| {
            let Ok(list) = state.runs.detached() else {
                return;
            };
            let d = adw::AlertDialog::new(
                Some("Still running"),
                Some(
                    "Vortex can stop these processes, or keep waiting and check again until they end.",
                ),
            );
            d.add_response("wait", "Keep waiting");
            d.add_response("stop", "Stop them");
            d.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
            d.set_close_response("wait");
            let (state2, refresh) = (state.clone(), refresh.clone());
            d.connect_response(None, move |_, id| {
                let state = &state2;
                if id != "stop" {
                    return;
                }
                let (state, refresh, list) = (state.clone(), refresh.clone(), list.clone());
                glib::spawn_future_local(async move {
                    let (installs, runs) = (state.installs.clone(), state.runs.clone());
                    let res = blocking(move || {
                        list.iter()
                            .map(|r| stop_detached(&installs, &runs, r))
                            .collect::<Result<Vec<_>>>()
                    })
                    .await;
                    match res {
                        Some(Ok(left)) if left.iter().any(|n| *n > 0) => state
                            .toast("Some processes did not stop. They are shown as still running."),
                        Some(Ok(_)) => state.toast("Stopped."),
                        Some(Err(e)) => state.toast(&e.to_string()),
                        None => {}
                    }
                    refresh();
                });
            });
            d.present(Some(&state.window));
        });
    }
    // Wait: poll every detached run until its processes are gone.
    {
        let (state, refresh) = (state.clone(), refresh);
        glib::spawn_future_local(async move {
            loop {
                glib::timeout_future_seconds(3).await;
                let (installs, runs) = (state.installs.clone(), state.runs.clone());
                let _ = blocking(move || {
                    for r in runs.detached().unwrap_or_default() {
                        let _ = match r.install_id {
                            Some(i) => installs.check_detached(i),
                            None => runs.check_detached(r.id),
                        };
                    }
                })
                .await;
                refresh();
            }
        });
    }
    banner
}

/// "Alice, Bob and Carol" style list for the quit dialog.
pub fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [a] => a.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Intercepts window close while runs or installs are active (keep running / stop all / cancel).
pub fn quit_guard(state: &Rc<AppState>) {
    let force = Rc::new(std::cell::Cell::new(false));
    let st = state.clone();
    state.window.connect_close_request(move |w| {
        if force.get() {
            return glib::Propagation::Proceed;
        }
        let runs = st.db().active_runs().unwrap_or_default();
        if runs.is_empty() {
            return glib::Propagation::Proceed;
        }
        let mut names: Vec<String> = Vec::new();
        for r in &runs {
            let db = st.db();
            let n = match (r.game_id, r.install_id) {
                (Some(g), _) => db.get_game(g).map(|g| g.name).ok(),
                (None, Some(i)) => db
                    .get_install(i)
                    .and_then(|i| db.get_game(i.game_id))
                    .map(|g| format!("{} (installer)", g.name))
                    .ok(),
                _ => program_name(&r.argv),
            };
            names.push(n.unwrap_or_else(|| "a program".into()));
        }
        names.dedup();
        let d = adw::AlertDialog::new(
            Some("Programs are still running"),
            Some(&format!(
                "{} is still running. Keep it running in the background, or stop everything and quit?",
                join_names(&names)
            )),
        );
        d.add_response("cancel", "Cancel");
        d.add_response("keep", "Keep running in background");
        d.add_response("stop", "Stop all and quit");
        d.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        let (st, force, w2) = (st.clone(), force.clone(), w.clone());
        d.connect_response(None, move |_, id| match id {
            "keep" => {
                force.set(true);
                w2.close();
            }
            "stop" => {
                let (st, force, w2) = (st.clone(), force.clone(), w2.clone());
                glib::spawn_future_local(async move {
                    let (runs, installs, db) = (st.runs.clone(), st.installs.clone(), st.db.clone());
                    let _ = blocking(move || {
                        let active = db.lock().unwrap_or_else(|e| e.into_inner()).active_runs().unwrap_or_default();
                        for r in &active {
                            match (r.install_id, runs.active(r.environment_id)) {
                                (Some(i), _) if installs.is_active(i) => {
                                    let _ = installs.cancel(i);
                                }
                                (_, Some(h)) if h.run_id() == r.id => h.stop(),
                                // From an earlier session: no supervisor to ask, stop it here.
                                _ => {
                                    let _ = stop_detached(&installs, &runs, r);
                                }
                            }
                        }
                        for _ in 0..60 {
                            if active.iter().all(|r| runs.active(r.environment_id).is_none()) {
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(250));
                        }
                    })
                    .await;
                    force.set(true);
                    w2.close();
                });
            }
            _ => {}
        });
        d.present(Some(w));
        glib::Propagation::Stop
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_join() {
        assert_eq!(super::join_names(&[]), "");
        assert_eq!(super::join_names(&["A".into()]), "A");
        assert_eq!(
            super::join_names(&["A".into(), "B".into(), "C".into()]),
            "A, B and C"
        );
    }

    use super::*;
    use vortex_core::delete::DeletionItem;
    use vortex_core::paths::HostPath;

    fn hp(p: &str) -> HostPath {
        HostPath::new(p).unwrap()
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(10), "10 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(3 << 30), "3.0 GiB");
    }

    #[test]
    fn plan_text() {
        let plan = DeletionPlan {
            env_id: 1,
            game_id: Some(2),
            remove_env: true,
            items: vec![DeletionItem {
                path: hp("/d/prefixes/1"),
                bytes: 2048,
            }],
            manual: vec![hp("/home/u/Games/x")],
        };
        let t = format_plan(&plan);
        assert!(t.contains("/d/prefixes/1 (2.0 KiB)"));
        assert!(t.contains("Total: 2.0 KiB"));
        assert!(t.contains("NOT delete"));
        assert!(t.contains("/home/u/Games/x"));
        assert!(t.contains("prefix is deleted only if it is listed above"));

        // nothing to delete, custom prefix kept: no claim that anything is removed (M23 shot 44)
        let plan = DeletionPlan {
            items: vec![],
            manual: vec![hp("/home/u/my-wine")],
            ..plan
        };
        let t = format_plan(&plan);
        assert!(t.contains("No files will be deleted"));
        assert!(t.contains("NOT delete") && t.contains("/home/u/my-wine"));
        assert!(!t.contains("removed too") && !t.contains("deleted only if"), "{t}");
    }

    #[test]
    fn labels_and_findings() {
        assert!(install_state_label(InstallState::Waiting).len() > 5);
        assert!(is_resting(InstallState::Failed) && !is_resting(InstallState::Running));
        let f = Finding {
            class: Class::AppCrash,
            confidence: Confidence::Likely,
            evidence: vec!["boom".into()],
        };
        assert_eq!(finding_text(&f), "The program crashed. (likely)\nboom");
    }
}
