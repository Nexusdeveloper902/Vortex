//! Configure page: General, Runtime (Proton toggles), Launch, Environment and Danger zone groups.

use crate::app::AppState;
use crate::ui::{dialogs, game_page, manage};
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// (key, Proton env var that DISABLES the feature, title, subtitle).
pub const TOGGLES: [(&str, &str, &str, &str); 4] = [
    (
        "dxvk",
        "PROTON_USE_WINED3D",
        "DXVK",
        "Run Direct3D 9-11 on Vulkan. Off uses Wine's own OpenGL translation (PROTON_USE_WINED3D=1).",
    ),
    (
        "vkd3d",
        "PROTON_NO_D3D12",
        "VKD3D-Proton",
        "Run Direct3D 12 on Vulkan. Off hides DirectX 12 (PROTON_NO_D3D12=1).",
    ),
    (
        "esync",
        "PROTON_NO_ESYNC",
        "Esync",
        "Faster synchronisation through eventfd. Off sets PROTON_NO_ESYNC=1.",
    ),
    (
        "fsync",
        "PROTON_NO_FSYNC",
        "Fsync",
        "Faster synchronisation through futex. Off sets PROTON_NO_FSYNC=1.",
    ),
];

/// Which toggles are on (feature enabled = its disabling variable is absent or not "1").
pub fn toggles_from_env(env: &BTreeMap<String, String>) -> [bool; 4] {
    TOGGLES.map(|(_, var, _, _)| env.get(var).is_none_or(|v| v != "1"))
}

/// Writes the toggle states into `env`; user variables are left alone.
pub fn apply_toggles(env: &mut BTreeMap<String, String>, on: [bool; 4]) {
    for ((_, var, _, _), on) in TOGGLES.iter().zip(on) {
        if on {
            env.remove(*var);
        } else {
            env.insert((*var).into(), "1".into());
        }
    }
}

/// Parses `KEY=value` lines. Rejects empty keys and keys with `=` or spaces.
pub fn parse_env_lines(lines: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut m = BTreeMap::new();
    for l in lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        let (k, v) = l
            .split_once('=')
            .ok_or_else(|| format!("\"{l}\" needs the form KEY=value"))?;
        if k.is_empty() || k.contains(char::is_whitespace) {
            return Err(format!("\"{k}\" is not a valid variable name"));
        }
        m.insert(k.to_string(), v.to_string());
    }
    Ok(m)
}

/// Launch arguments are a JSON array of strings. Older non-JSON values fall back to word splitting.
pub fn args_from_json(json: &str) -> Vec<String> {
    serde_json::from_str(json)
        .unwrap_or_else(|_| json.split_whitespace().map(str::to_owned).collect())
}

/// One argument per entry; blank entries are dropped, spaces inside an argument are kept.
pub fn args_to_json(args: &[String]) -> String {
    let v: Vec<&String> = args.iter().filter(|a| !a.trim().is_empty()).collect();
    serde_json::to_string(&v).unwrap_or_else(|_| "[]".into())
}

fn env_row(list: &adw::PreferencesGroup, rows: &Rc<RefCell<Vec<adw::EntryRow>>>, text: &str) {
    list_row(list, rows, "KEY=value", text);
}

fn list_row(
    list: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::EntryRow>>>,
    title: &str,
    text: &str,
) {
    let row = adw::EntryRow::builder().title(title).build();
    row.set_text(text);
    let rm = gtk4::Button::builder()
        .icon_name("user-trash-symbolic")
        .valign(gtk4::Align::Center)
        .css_classes(["flat"])
        .tooltip_text("Remove")
        .build();
    row.add_suffix(&rm);
    list.add(&row);
    rows.borrow_mut().push(row.clone());
    let (list, rows) = (list.clone(), rows.clone());
    rm.connect_clicked(move |_| {
        list.remove(&row);
        rows.borrow_mut().retain(|r| r != &row);
    });
}

pub fn open(state: &Rc<AppState>, game_id: i64) {
    let (g, env) = match (|| {
        let db = state.db();
        let g = db.get_game(game_id)?;
        let env = db.get_environment(g.environment_id)?;
        Ok::<_, vortex_core::error::Error>((g, env))
    })() {
        Ok(v) => v,
        Err(e) => return state.toast(&e.to_string()),
    };
    let prefs = adw::PreferencesPage::new();

    // General
    let general = adw::PreferencesGroup::builder().title("General").build();
    let name = adw::EntryRow::builder().title("Name").build();
    name.set_text(&g.name);
    let exe = adw::EntryRow::builder().title("Executable").build();
    exe.set_text(g.exe_path.as_deref().unwrap_or(""));
    let wd = adw::EntryRow::builder()
        .title("Working directory (empty: the exe's folder)")
        .build();
    wd.set_text(g.work_dir.as_deref().unwrap_or(""));
    for (row, folder) in [(&exe, false), (&wd, true)] {
        let b = gtk4::Button::builder()
            .icon_name(if folder {
                "folder-open-symbolic"
            } else {
                "document-open-symbolic"
            })
            .valign(gtk4::Align::Center)
            .css_classes(["flat"])
            .build();
        row.add_suffix(&b);
        let (state, row) = (state.clone(), row.clone());
        b.connect_clicked(move |_| {
            let row = row.clone();
            let cb = move |p: std::path::PathBuf| row.set_text(&p.to_string_lossy());
            if folder {
                dialogs::pick_folder(&state, "Working directory", cb);
            } else {
                dialogs::pick_file(&state, "Executable", cb);
            }
        });
    }
    general.add(&name);
    general.add(&exe);
    general.add(&wd);
    prefs.add(&general);

    // Runtime
    let user_env: BTreeMap<String, String> = serde_json::from_str(&g.env_vars).unwrap_or_default();
    let on = toggles_from_env(&user_env);
    let rt = adw::PreferencesGroup::builder()
        .title("Runtime")
        .description("Changing the Proton version affects every game that shares this prefix.")
        .build();
    let (combo, runtimes) = dialogs::runtime_combo(state, Some(env.runtime_path.clone()));
    rt.add(&combo);
    let switches: Vec<adw::SwitchRow> = TOGGLES
        .iter()
        .zip(on)
        .map(|((_, _, t, s), on)| {
            let sw = adw::SwitchRow::builder()
                .title(*t)
                .subtitle(*s)
                .active(on)
                .build();
            rt.add(&sw);
            sw
        })
        .collect();
    prefs.add(&rt);

    // Launch
    let launch = adw::PreferencesGroup::builder().title("Launch").build();
    let arg_rows: Rc<RefCell<Vec<adw::EntryRow>>> = Rc::default();
    let add_arg = adw::ButtonRow::builder()
        .title("Add argument")
        .start_icon_name("list-add-symbolic")
        .build();
    launch.set_description(Some(
        "One argument per row. Spaces inside a row stay part of that argument.",
    ));
    launch.add(&add_arg);
    for a in args_from_json(&g.launch_args) {
        list_row(&launch, &arg_rows, "Argument", &a);
    }
    {
        let (launch, arg_rows) = (launch.clone(), arg_rows.clone());
        add_arg.connect_activated(move |_| list_row(&launch, &arg_rows, "Argument", ""));
    }
    prefs.add(&launch);
    let vars = adw::PreferencesGroup::builder()
        .title("Environment variables")
        .build();
    let rows: Rc<RefCell<Vec<adw::EntryRow>>> = Rc::default();
    let managed: Vec<&str> = TOGGLES.iter().map(|t| t.1).collect();
    for (k, v) in user_env
        .iter()
        .filter(|(k, _)| !managed.contains(&k.as_str()))
    {
        env_row(&vars, &rows, &format!("{k}={v}"));
    }
    let add = adw::ButtonRow::builder()
        .title("Add variable")
        .start_icon_name("list-add-symbolic")
        .build();
    {
        let (vars, rows) = (vars.clone(), rows.clone());
        add.connect_activated(move |_| env_row(&vars, &rows, ""));
    }
    vars.add(&add);
    prefs.add(&vars);

    // Environment
    let envg = adw::PreferencesGroup::builder()
        .title("Environment")
        .build();
    envg.add(&game_page::folder_row(
        state,
        "Prefix",
        Some(&env.prefix_path),
    ));
    let deps = adw::ExpanderRow::builder()
        .title("Installed dependencies")
        .build();
    let list = state.db().list_dependencies(env.id).unwrap_or_default();
    if list.is_empty() {
        deps.set_subtitle("None recorded");
    }
    for d in list {
        deps.add_row(
            &adw::ActionRow::builder()
                .title(&d.verb)
                .subtitle(d.state.as_str())
                .build(),
        );
    }
    envg.add(&deps);
    let entry_path = state
        .paths
        .applications_dir()
        .join(format!("{}.game-{game_id}.desktop", crate::app::APP_ID));
    let desktop = adw::SwitchRow::builder()
        .title("Add to applications menu")
        .subtitle("Creates a launcher that starts this game directly (vortex --launch)")
        .active(entry_path.exists())
        .build();
    let was_on = desktop.is_active();
    envg.add(&desktop);
    prefs.add(&envg);

    // Danger zone
    let danger = adw::PreferencesGroup::builder()
        .title("Danger zone")
        .build();
    let rm = adw::ButtonRow::builder()
        .title("Remove completely...")
        .css_classes(["destructive-action"])
        .build();
    {
        let state = state.clone();
        rm.connect_activated(move |_| manage::remove_completely(&state, game_id));
    }
    danger.add(&rm);
    prefs.add(&danger);

    let save = gtk4::Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .build();
    {
        let state = state.clone();
        let old_rt = env.runtime_path.clone();
        let env_id = env.id;
        save.connect_clicked(move |_| {
            let lines: Vec<String> = rows.borrow().iter().map(|r| r.text().to_string()).collect();
            let mut vars = match parse_env_lines(&lines) {
                Ok(v) => v,
                Err(e) => return state.toast(&e),
            };
            apply_toggles(&mut vars, std::array::from_fn(|i| switches[i].is_active()));
            let argv: Vec<String> = arg_rows
                .borrow()
                .iter()
                .map(|r| r.text().to_string())
                .collect();
            let nz = |s: gtk4::glib::GString| {
                if s.trim().is_empty() {
                    None
                } else {
                    Some(s.trim().to_string())
                }
            };
            if state.runs.active(env_id).is_some() {
                return state.toast("Stop the running game before saving changes");
            }
            for (label, v) in [
                ("executable", nz(exe.text())),
                ("working folder", nz(wd.text())),
            ] {
                let Some(v) = v else { continue };
                let p = std::path::Path::new(&v);
                let ok = if label == "executable" {
                    p.is_file()
                        && p.extension().is_some_and(|e| {
                            ["exe", "bat", "cmd", "com"]
                                .iter()
                                .any(|x| e.eq_ignore_ascii_case(x))
                        })
                } else {
                    p.is_dir()
                };
                if !ok {
                    return state.toast(&format!("The {label} is not valid: {v}"));
                }
            }
            if let Some(r) = runtimes.borrow().get(combo.selected() as usize)
                && vortex_core::runtime::validate(r.path.as_path()).is_err()
            {
                return state.toast("The selected Proton runtime is not valid");
            }
            let res = (|| {
                let db = state.db();
                db.update_game_settings(
                    game_id,
                    name.text().trim(),
                    nz(exe.text()).as_deref(),
                    nz(wd.text()).as_deref(),
                    &args_to_json(&argv),
                    &serde_json::to_string(&vars).unwrap_or_else(|_| "{}".into()),
                )?;
                if let Some(r) = runtimes.borrow().get(combo.selected() as usize) {
                    let p = r.path.as_path().to_string_lossy().into_owned();
                    if p != old_rt {
                        db.set_environment_runtime(env.id, &p)?;
                    }
                }
                drop(db);
                if desktop.is_active() != was_on {
                    if desktop.is_active() {
                        let bin = std::env::current_exe()
                            .map_err(vortex_core::error::Error::io("vortex"))?;
                        vortex_core::desktop::write_entry(
                            &state.paths,
                            &bin,
                            &game_id.to_string(),
                            name.text().trim(),
                        )?;
                    } else {
                        vortex_core::desktop::remove_entry(&state.paths, &game_id.to_string())?;
                    }
                }
                Ok::<_, vortex_core::error::Error>(())
            })();
            match res {
                Ok(()) => {
                    state.toast("Saved");
                    // The game page below was built from the old values: rebuild it.
                    state.nav.pop_to_tag("library");
                    crate::ui::game_page::open(&state, game_id);
                }
                Err(e) => state.toast(&e.to_string()),
            }
        });
    }
    let header = adw::HeaderBar::new();
    header.pack_end(&save);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&prefs));
    state.nav.push(&adw::NavigationPage::new(&tv, "Configure"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_roundtrip() {
        let mut env = BTreeMap::new();
        assert_eq!(toggles_from_env(&env), [true; 4]);
        apply_toggles(&mut env, [false, true, true, false]);
        assert_eq!(env.get("PROTON_USE_WINED3D").map(String::as_str), Some("1"));
        assert_eq!(env.get("PROTON_NO_FSYNC").map(String::as_str), Some("1"));
        assert_eq!(toggles_from_env(&env), [false, true, true, false]);
        apply_toggles(&mut env, [true; 4]);
        assert!(env.is_empty());
    }

    #[test]
    fn args_roundtrip() {
        let a = vec!["-w".to_string(), "My Dir/x y".to_string(), " ".to_string()];
        let json = args_to_json(&a);
        assert_eq!(json, r#"["-w","My Dir/x y"]"#);
        assert_eq!(args_from_json(&json), vec!["-w", "My Dir/x y"]);
        assert_eq!(args_from_json("-a -b"), vec!["-a", "-b"]);
        assert_eq!(args_from_json("[]"), Vec::<String>::new());
    }

    #[test]
    fn env_lines() {
        let ok = parse_env_lines(&["A=1".into(), "".into(), "B=x=y".into()]).unwrap();
        assert_eq!(ok["B"], "x=y");
        assert!(parse_env_lines(&["novalue".into()]).is_err());
        assert!(parse_env_lines(&["=v".into()]).is_err());
        assert!(parse_env_lines(&["a b=1".into()]).is_err());
    }
}
