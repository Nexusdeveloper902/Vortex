//! Install wizard (adw::Dialog + NavigationView): Source, Choose installer, Name & location,
//! Installing, Choose executable, Done. Also "Add an existing game".

use crate::app::{AppState, blocking};
use crate::ui::dialogs::{self, finding_text, install_state_label, is_resting, runtime_combo};
use crate::ui::{game_page, run_view::RunView};
use async_channel::Receiver;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use vortex_core::db::InstallState;
use vortex_core::diagnose::Finding;
use vortex_core::install::{
    ExeCandidate, InstallEvent, InstallRequest, RegisterEnv, RegisterRequest,
};
use vortex_core::paths::HostPath;
use vortex_core::source::{self, SourceAnalysis};

/// Suggested game name from the picked file or folder.
pub fn guess_name(p: &Path) -> String {
    let n = if p.is_dir() {
        p.file_name()
    } else {
        p.file_stem()
    };
    n.map(|s| s.to_string_lossy().replace(['_', '-'], " "))
        .unwrap_or_default()
}

fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn step(title: &str, content: &impl IsA<gtk4::Widget>, can_pop: bool) -> adw::NavigationPage {
    let clamp = adw::Clamp::builder()
        .maximum_size(520)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(16)
        .margin_end(16)
        .child(content)
        .build();
    let header = adw::HeaderBar::new();
    header.set_show_back_button(can_pop);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&gtk4::ScrolledWindow::builder().child(&clamp).build()));
    let p = adw::NavigationPage::new(&tv, title);
    p.set_can_pop(can_pop);
    p
}

fn column() -> gtk4::Box {
    gtk4::Box::new(gtk4::Orientation::Vertical, 18)
}

fn heading(text: &str, sub: &str) -> gtk4::Box {
    let b = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    b.append(
        &gtk4::Label::builder()
            .label(text)
            .css_classes(["title-2"])
            .wrap(true)
            .xalign(0.0)
            .build(),
    );
    if !sub.is_empty() {
        b.append(
            &gtk4::Label::builder()
                .label(sub)
                .css_classes(["dim-label"])
                .wrap(true)
                .xalign(0.0)
                .build(),
        );
    }
    b
}

fn boxed() -> gtk4::ListBox {
    gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build()
}

fn pill(label: &str, suggested: bool) -> gtk4::Button {
    let b = gtk4::Button::builder()
        .label(label)
        .halign(gtk4::Align::Center)
        .css_classes(["pill"])
        .build();
    if suggested {
        b.add_css_class("suggested-action");
    }
    b
}

/// Radio rows in one group; returns the selected value cell.
struct Radios {
    list: gtk4::ListBox,
    first: RefCell<Option<gtk4::CheckButton>>,
    chosen: Rc<RefCell<Option<HostPath>>>,
    on_change: Rc<dyn Fn()>,
}

impl Radios {
    fn new(on_change: impl Fn() + 'static) -> Self {
        Self {
            list: boxed(),
            first: RefCell::default(),
            chosen: Rc::default(),
            on_change: Rc::new(on_change),
        }
    }
    fn add(&self, title: &str, sub: &str, path: HostPath, active: bool) {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(sub)
            .use_markup(false)
            .activatable(true)
            .build();
        let r = gtk4::CheckButton::new();
        r.set_valign(gtk4::Align::Center);
        r.set_group(self.first.borrow().as_ref());
        self.first.borrow_mut().get_or_insert_with(|| r.clone());
        row.add_prefix(&r);
        row.set_activatable_widget(Some(&r));
        let (chosen, cb) = (self.chosen.clone(), self.on_change.clone());
        r.connect_toggled(move |b| {
            if b.is_active() {
                *chosen.borrow_mut() = Some(path.clone());
                cb();
            }
        });
        self.list.append(&row);
        r.set_active(active);
    }
}

macro_rules! up {
    ($w:ident) => {
        let Some($w) = $w.upgrade() else { return };
    };
}

struct Wizard {
    state: Rc<AppState>,
    dialog: adw::Dialog,
    nav: adw::NavigationView,
    source: RefCell<Option<PathBuf>>,
    analysis: RefCell<Option<SourceAnalysis>>,
    installer: Rc<RefCell<Option<HostPath>>>,
    install_id: Cell<i64>,
    // progress page
    view: Rc<RunView>,
    status: gtk4::Label,
    findings: gtk4::ListBox,
    retry: gtk4::Button,
    select: gtk4::Button,
}

pub fn open(state: &Rc<AppState>) {
    let w = Wizard::new(state);
    w.nav.add(&w.source_page());
    w.present();
}

/// Continue an interrupted, failed or half-finished install of a game.
pub fn resume(state: &Rc<AppState>, game_id: i64) {
    let install = match state.db().latest_install_for_game(game_id) {
        Ok(Some(i)) => i,
        Ok(None) => return state.toast("This game has no installation to continue"),
        Err(e) => return state.toast(&e.to_string()),
    };
    if state.installs.is_active(install.id)
        || state
            .installs
            .detached()
            .is_ok_and(|d| d.iter().any(|(i, _)| i.id == install.id))
    {
        return state.toast("This installer is still running");
    }
    let w = Wizard::new(state);
    w.install_id.set(install.id);
    w.nav.add(&w.progress_page());
    w.show_state(install.state, &[]);
    w.present();
}

impl Wizard {
    /// Shows the dialog. The wizard is held by the dialog's `closed` handler only until close, and
    /// widget closures hold it weakly, so no reference cycle outlives the dialog.
    fn present(self: &Rc<Self>) {
        let keep = RefCell::new(Some(self.clone()));
        self.dialog.connect_closed(move |_| {
            keep.take();
        });
        self.dialog.present(Some(&self.state.window));
    }

    fn new(state: &Rc<AppState>) -> Rc<Self> {
        let nav = adw::NavigationView::new();
        let dialog = adw::Dialog::builder()
            .title("Install a game")
            .content_width(600)
            .content_height(680)
            .child(&nav)
            .build();
        let findings = boxed();
        findings.set_visible(false);
        Rc::new(Self {
            state: state.clone(),
            dialog,
            nav,
            source: RefCell::default(),
            analysis: RefCell::default(),
            installer: Rc::default(),
            install_id: Cell::new(0),
            view: RunView::new("Cancel install"),
            status: gtk4::Label::builder()
                .css_classes(["title-3"])
                .xalign(0.0)
                .wrap(true)
                .build(),
            findings,
            retry: pill("Retry", true),
            select: pill("The installer finished: choose the executable", false),
        })
    }

    // ---- 1. Source ----
    fn source_page(self: &Rc<Self>) -> adw::NavigationPage {
        let col = column();
        col.append(&heading("Where is the installer?", "Pick the setup file (.exe or .msi) or the folder that holds the installer. Your files are never modified."));
        let file = pill("Choose a file...", true);
        let dir = pill("Choose a folder...", false);
        for (b, folder) in [(&file, false), (&dir, true)] {
            let w = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                up!(w);
                let w2 = Rc::downgrade(&w);
                let cb = move |p: PathBuf| {
                    if let Some(w) = w2.upgrade() {
                        w.analyze(p)
                    }
                };
                if folder {
                    dialogs::pick_folder(&w.state, "Installer folder", cb);
                } else {
                    dialogs::pick_file(&w.state, "Installer file", cb);
                }
            });
        }
        col.append(&file);
        col.append(&dir);
        let p = step("Source", &col, false);
        p.set_tag(Some("source"));
        p
    }

    fn analyze(self: &Rc<Self>, p: PathBuf) {
        let w = self.clone();
        glib::spawn_future_local(async move {
            let q = p.clone();
            let res = blocking(move || HostPath::new(&q).and_then(|h| source::analyze(&h))).await;
            match res {
                Some(Ok(a)) if a.candidates.is_empty() => {
                    w.state.toast("No installer was found there")
                }
                Some(Ok(a)) => {
                    *w.source.borrow_mut() = Some(p);
                    *w.analysis.borrow_mut() = Some(a);
                    w.nav.push(&w.choose_page());
                }
                Some(Err(e)) => w.state.toast(&e.to_string()),
                None => {}
            }
        });
    }

    // ---- 2. Choose installer ----
    fn choose_page(self: &Rc<Self>) -> adw::NavigationPage {
        let a = self.analysis.borrow();
        let Some(a) = a.as_ref() else {
            return step("Installer", &column(), true);
        };
        let next = pill("Next", true);
        next.set_sensitive(false);
        let n2 = next.clone();
        let radios = Radios::new(move || n2.set_sensitive(true));
        let col = column();
        col.append(&heading(
            if a.ambiguous {
                "Several installers found"
            } else {
                "Installer"
            },
            if a.ambiguous {
                "Choose the one that starts the installation. Vortex never runs them all."
            } else {
                "This is the installer Vortex will run."
            },
        ));
        for (i, c) in a.candidates.iter().enumerate() {
            let sub = if c.reasons.is_empty() {
                "Candidate".to_string()
            } else {
                c.reasons.join(", ")
            };
            radios.add(
                &file_name(c.path.as_path()),
                &sub,
                c.path.clone(),
                !a.ambiguous && i == 0,
            );
        }
        col.append(&radios.list);
        col.append(&next);
        let chosen = radios.chosen.clone();
        let w = Rc::downgrade(self);
        next.connect_clicked(move |_| {
            up!(w);
            *w.installer.borrow_mut() = chosen.borrow().clone();
            w.nav.push(&w.name_page());
        });
        step("Installer", &col, true)
    }

    // ---- 3. Name & location ----
    fn name_page(self: &Rc<Self>) -> adw::NavigationPage {
        let col = column();
        col.append(&heading("Name and location", ""));
        let name = adw::EntryRow::builder().title("Game name").build();
        name.set_text(
            &self
                .source
                .borrow()
                .as_deref()
                .map(guess_name)
                .unwrap_or_default(),
        );
        let (combo, runtimes) = runtime_combo(&self.state, None);
        let loc = adw::EntryRow::builder()
            .title("Install folder (optional, must exist)")
            .build();
        let browse = gtk4::Button::builder()
            .icon_name("folder-open-symbolic")
            .valign(gtk4::Align::Center)
            .css_classes(["flat"])
            .tooltip_text("Choose folder")
            .build();
        {
            let (w, loc) = (Rc::downgrade(self), loc.clone());
            browse.connect_clicked(move |_| {
                up!(w);
                let loc = loc.clone();
                dialogs::pick_folder(&w.state, "Install folder", move |p| {
                    loc.set_text(&p.to_string_lossy())
                });
            });
        }
        loc.add_suffix(&browse);
        let list = boxed();
        list.append(&name);
        list.append(&combo);
        list.append(&loc);
        col.append(&list);
        col.append(&gtk4::Label::builder().label("Leave the folder empty to let the installer use its default location inside the game's prefix.").css_classes(["dim-label", "caption"]).wrap(true).xalign(0.0).build());
        let go = pill("Install", true);
        col.append(&go);
        let w = Rc::downgrade(self);
        go.connect_clicked(move |b| {
            up!(w);
            let Some(rt) = runtimes
                .borrow()
                .get(combo.selected() as usize)
                .map(|r| r.path.clone())
            else {
                return w.state.toast("Choose a Proton runtime first");
            };
            let (Some(src), Some(inst)) = (w.source.borrow().clone(), w.installer.borrow().clone())
            else {
                return;
            };
            let loc_text = loc.text().trim().to_string();
            let req = (|| {
                Ok::<_, vortex_core::error::Error>(InstallRequest {
                    name: name.text().to_string(),
                    source: HostPath::new(&src)?,
                    installer: inst,
                    runtime: rt,
                    install_dir: if loc_text.is_empty() {
                        None
                    } else {
                        Some(HostPath::new(&loc_text)?)
                    },
                })
            })();
            let req = match req {
                Ok(r) => r,
                Err(e) => return w.state.toast(&e.to_string()),
            };
            b.set_sensitive(false);
            let (w, b) = (w.clone(), b.clone());
            glib::spawn_future_local(async move {
                let installs = w.state.installs.clone();
                let res = blocking(move || installs.begin(req)).await;
                b.set_sensitive(true);
                match res {
                    Some(Ok(s)) => {
                        w.install_id.set(s.install_id);
                        w.nav.push(&w.progress_page());
                        w.consume(s.events);
                    }
                    Some(Err(e)) => w.state.toast(&e.to_string()),
                    None => {}
                }
            });
        });
        step("Name and location", &col, true)
    }

    // ---- 4. Installing ----
    fn progress_page(self: &Rc<Self>) -> adw::NavigationPage {
        let col = column();
        col.append(&self.status);
        col.append(&self.findings);
        col.append(&self.view.root);
        self.retry.set_visible(false);
        self.select.set_visible(false);
        col.append(&self.retry);
        col.append(&self.select);
        {
            let w = Rc::downgrade(self);
            self.view.stop_btn.connect_clicked(move |b| {
                up!(w);
                b.set_sensitive(false);
                if let Err(e) = w.state.installs.cancel(w.install_id.get()) {
                    w.state.toast(&e.to_string());
                }
            });
            let w = Rc::downgrade(self);
            self.view.mark_btn.connect_clicked(move |_| {
                up!(w);
                if let Err(e) = w.state.installs.mark_run_finished(w.install_id.get()) {
                    w.state.toast(&e.to_string());
                }
            });
            let w = Rc::downgrade(self);
            self.retry.connect_clicked(move |_| {
                up!(w);
                match w.state.installs.retry(w.install_id.get()) {
                    Ok(rx) => {
                        w.view.reset();
                        while let Some(c) = w.findings.first_child() {
                            w.findings.remove(&c);
                        }
                        w.findings.set_visible(false);
                        w.retry.set_visible(false);
                        w.select.set_visible(false);
                        w.consume(rx);
                    }
                    Err(e) => w.state.toast(&e.to_string()),
                }
            });
            let w = Rc::downgrade(self);
            self.select.connect_clicked(move |_| {
                up!(w);
                glib::spawn_future_local(async move {
                    let (installs, id) = (w.state.installs.clone(), w.install_id.get());
                    match blocking(move || installs.mark_installer_finished(id)).await {
                        Some(Ok(c)) => w.nav.push(&w.exe_page(c)),
                        Some(Err(e)) => w.state.toast(&e.to_string()),
                        None => {}
                    }
                });
            });
        }
        let p = step("Installing", &col, false);
        p.set_tag(Some("progress"));
        p
    }

    fn show_state(self: &Rc<Self>, s: InstallState, cands: &[ExeCandidate]) {
        self.status.set_text(install_state_label(s));
        let resting = is_resting(s);
        self.dialog.set_can_close(resting);
        let again = matches!(
            s,
            InstallState::Failed | InstallState::Cancelled | InstallState::Interrupted
        );
        self.retry.set_visible(again);
        self.select.set_visible(again);
        if resting {
            self.view.stop_btn.set_sensitive(false);
            self.view.mark_btn.set_sensitive(false);
        }
        if s == InstallState::SelectExe {
            let (w, cands) = (self.clone(), cands.to_vec());
            glib::spawn_future_local(async move {
                let c = if cands.is_empty() {
                    let (installs, id) = (w.state.installs.clone(), w.install_id.get());
                    blocking(move || installs.exe_candidates(id).unwrap_or_default())
                        .await
                        .unwrap_or_default()
                } else {
                    cands
                };
                w.nav.push(&w.exe_page(c));
            });
        }
    }

    fn add_finding(&self, f: &Finding) {
        let row = adw::ActionRow::builder()
            .title(finding_text(f))
            .use_markup(false)
            .build();
        row.add_prefix(&gtk4::Image::from_icon_name("dialog-warning-symbolic"));
        self.findings.append(&row);
        self.findings.set_visible(true);
    }

    fn consume(self: &Rc<Self>, rx: Receiver<InstallEvent>) {
        let w = self.clone();
        glib::spawn_future_local(async move {
            let mut cands = Vec::new();
            while let Ok(ev) = rx.recv().await {
                match ev {
                    InstallEvent::State(s) => w.show_state(s, &cands),
                    InstallEvent::Run { event, .. } => w.view.apply(&event),
                    InstallEvent::Finding(f) => w.add_finding(&f),
                    InstallEvent::Candidates(c) => cands = c,
                    InstallEvent::Error(e) => w.state.toast(&e),
                }
            }
        });
    }

    // ---- 5. Choose executable ----
    fn exe_page(self: &Rc<Self>, cands: Vec<ExeCandidate>) -> adw::NavigationPage {
        let col = column();
        col.append(&heading("Which file starts the game?", "Vortex never assumes the install worked. Pick the game's executable, or browse for it."));
        let confirm = pill("Confirm and add to library", true);
        confirm.set_sensitive(false);
        let c2 = confirm.clone();
        let radios = Rc::new(Radios::new(move || c2.set_sensitive(true)));
        for (i, c) in cands.iter().enumerate() {
            let sub = format!("{} - {}", c.reasons.join(", "), dialogs::human_size(c.size));
            radios.add(c.win.as_str(), &sub, c.path.clone(), i == 0);
        }
        col.append(&radios.list);
        let browse = pill("Browse...", false);
        {
            let (w, radios) = (Rc::downgrade(self), radios.clone());
            browse.connect_clicked(move |_| {
                up!(w);
                let radios = radios.clone();
                dialogs::pick_file(&w.state, "Game executable", move |p| {
                    if let Ok(h) = HostPath::new(&p) {
                        radios.add(&file_name(&p), &p.to_string_lossy(), h, true);
                    }
                });
            });
        }
        col.append(&browse);
        col.append(&confirm);
        let failed = gtk4::Button::builder()
            .label("The installer failed")
            .css_classes(["flat"])
            .halign(gtk4::Align::Center)
            .build();
        col.append(&failed);
        {
            let w = Rc::downgrade(self);
            failed.connect_clicked(move |_| {
                up!(w);
                if let Err(e) = w.state.installs.mark_failed(w.install_id.get()) {
                    return w.state.toast(&e.to_string());
                }
                w.nav.pop();
                w.show_state(InstallState::Failed, &[]);
            });
            let w = Rc::downgrade(self);
            confirm.connect_clicked(move |b| {
                up!(w);
                let Some(exe) = radios.chosen.borrow().clone() else {
                    return;
                };
                b.set_sensitive(false);
                let b = b.clone();
                glib::spawn_future_local(async move {
                    let (installs, id) = (w.state.installs.clone(), w.install_id.get());
                    let res = blocking(move || installs.confirm_exe(id, &exe, None)).await;
                    b.set_sensitive(true);
                    match res {
                        Some(Ok(reg)) => {
                            if let Some(Err(e)) = &reg.desktop_entry {
                                w.state
                                    .toast(&format!("Added, but the desktop entry failed: {e}"));
                            }
                            w.nav.push(&w.done_page(reg.game_id));
                        }
                        Some(Err(e)) => w.state.toast(&e.to_string()),
                        None => {}
                    }
                });
            });
        }
        step("Executable", &col, true)
    }

    // ---- 6. Done ----
    fn done_page(self: &Rc<Self>, game_id: i64) -> adw::NavigationPage {
        let open = pill("Open game", true);
        let sp = adw::StatusPage::builder()
            .icon_name("emblem-ok-symbolic")
            .title("Game added")
            .description("It is now in your library.")
            .child(&open)
            .build();
        let w = Rc::downgrade(self);
        open.connect_clicked(move |_| {
            up!(w);
            w.dialog.close();
            w.state.nav.pop_to_tag("library");
            game_page::open(&w.state, game_id);
        });
        step("Done", &sp, false)
    }
}

/// "Add an existing game": register an exe that is already installed.
pub fn open_existing(state: &Rc<AppState>) {
    let col = column();
    col.append(&heading(
        "Add an existing game",
        "Register a game that is already installed. Vortex will not run anything now.",
    ));
    let name = adw::EntryRow::builder().title("Game name").build();
    let exe_row = adw::ActionRow::builder()
        .title("Executable")
        .subtitle("Not chosen")
        .use_markup(false)
        .build();
    let exe: Rc<RefCell<Option<PathBuf>>> = Rc::default();
    let pick = gtk4::Button::builder()
        .label("Choose...")
        .valign(gtk4::Align::Center)
        .build();
    exe_row.add_suffix(&pick);
    {
        let (state, exe, exe_row, name) =
            (state.clone(), exe.clone(), exe_row.clone(), name.clone());
        pick.connect_clicked(move |_| {
            let (exe, exe_row, name) = (exe.clone(), exe_row.clone(), name.clone());
            dialogs::pick_file(&state, "Game executable", move |p| {
                exe_row.set_subtitle(&p.to_string_lossy());
                if name.text().is_empty() {
                    name.set_text(&guess_name(&p));
                }
                *exe.borrow_mut() = Some(p);
            });
        });
    }
    let (combo, runtimes) = runtime_combo(state, None);
    let prefix = adw::EntryRow::builder()
        .title("Existing Wine prefix (optional)")
        .build();
    let pbtn = gtk4::Button::builder()
        .icon_name("folder-open-symbolic")
        .valign(gtk4::Align::Center)
        .css_classes(["flat"])
        .build();
    prefix.add_suffix(&pbtn);
    {
        let (state, prefix) = (state.clone(), prefix.clone());
        pbtn.connect_clicked(move |_| {
            let prefix = prefix.clone();
            dialogs::pick_folder(&state, "Wine prefix", move |p| {
                prefix.set_text(&p.to_string_lossy())
            });
        });
    }
    let list = boxed();
    for w in [
        name.upcast_ref::<gtk4::Widget>(),
        exe_row.upcast_ref(),
        combo.upcast_ref(),
        prefix.upcast_ref(),
    ] {
        list.append(w);
    }
    col.append(&list);
    col.append(&gtk4::Label::builder().label("Leave the prefix empty to create a new one. An existing prefix is used as is and Vortex never deletes it.").css_classes(["dim-label", "caption"]).wrap(true).xalign(0.0).build());
    let add = pill("Add to library", true);
    col.append(&add);
    let nav = adw::NavigationView::new();
    nav.add(&step("Add an existing game", &col, false));
    let dialog = adw::Dialog::builder()
        .title("Add an existing game")
        .content_width(600)
        .content_height(640)
        .child(&nav)
        .build();
    {
        let (state, dialog) = (state.clone(), dialog.clone());
        add.connect_clicked(move |_| {
            let Some(exe) = exe.borrow().clone() else {
                return state.toast("Choose the game's executable");
            };
            let Some(rt) = runtimes
                .borrow()
                .get(combo.selected() as usize)
                .map(|r| r.path.clone())
            else {
                return state.toast("Choose a Proton runtime first");
            };
            let pfx = prefix.text().trim().to_string();
            let req = (|| {
                Ok::<_, vortex_core::error::Error>(RegisterRequest {
                    name: name.text().to_string(),
                    exe: HostPath::new(&exe)?,
                    work_dir: None,
                    env: if pfx.is_empty() {
                        RegisterEnv::New { runtime: rt }
                    } else {
                        RegisterEnv::Adopt {
                            prefix: HostPath::new(&pfx)?,
                            runtime: rt,
                        }
                    },
                })
            })();
            let req = match req {
                Ok(r) => r,
                Err(e) => return state.toast(&e.to_string()),
            };
            let (state, dialog) = (state.clone(), dialog.clone());
            glib::spawn_future_local(async move {
                let installs = state.installs.clone();
                match blocking(move || installs.register_existing(req)).await {
                    Some(Ok(r)) => {
                        dialog.close();
                        state.nav.pop_to_tag("library");
                        game_page::open(&state, r.game_id);
                    }
                    Some(Err(e)) => state.toast(&e.to_string()),
                    None => {}
                }
            });
        });
    }
    dialog.present(Some(&state.window));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            guess_name(Path::new("/x/My_Game-Setup.exe")),
            "My Game Setup"
        );
    }
}
