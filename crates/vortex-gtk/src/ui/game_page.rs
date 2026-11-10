//! Game page: details of one game plus action buttons that M12 will wire.

use crate::app::AppState;
use crate::game_object::status_label;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use std::path::Path;
use std::rc::Rc;

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

fn folder_row(state: &Rc<AppState>, title: &str, path: Option<&str>) -> adw::ActionRow {
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

fn build(state: &Rc<AppState>, id: i64) -> vortex_core::error::Result<adw::NavigationPage> {
    let g = state.db.get_game(id)?;
    let env = state.db.get_environment(g.environment_id)?;
    let last = state.db.last_launch(id)?;

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

    // Placeholders: M12 connects these (find them by widget name).
    let actions = adw::PreferencesGroup::builder().title("Actions").build();
    let bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    bar.set_halign(gtk4::Align::Center);
    for (name, label, cls) in [
        ("play", "Play", "suggested-action"),
        ("install", "Install", ""),
        ("run-exe", "Run exe", ""),
        ("backup", "Back up", ""),
        ("uninstall", "Uninstall", "destructive-action"),
    ] {
        let b = gtk4::Button::builder()
            .label(label)
            .sensitive(false)
            .tooltip_text("Not available yet")
            .build();
        b.set_widget_name(name);
        if !cls.is_empty() {
            b.add_css_class(cls);
        }
        bar.append(&b);
    }
    actions.add(&bar);
    prefs.add(&actions);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&prefs));
    Ok(adw::NavigationPage::new(&view, &g.name))
}

#[cfg(test)]
mod tests {
    use super::*;

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
