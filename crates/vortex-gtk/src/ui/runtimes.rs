//! Runtimes page: discovered Proton list, global default, GE-Proton install with progress.

use crate::app::{AppState, blocking};
use crate::ui::settings::{KEY_DEFAULT_RUNTIME, KEY_OFFLINE};
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vortex_core::error::Error;
use vortex_core::runtime::{self, Runtime};
use vortex_core::runtime_ge::{self, Progress};

/// Where a discovered runtime came from, for display.
pub fn source_of(path: &Path, vortex_runtimes: &Path) -> &'static str {
    if path.starts_with(vortex_runtimes) {
        "Vortex"
    } else if path.components().any(|c| c.as_os_str() == "Steam") {
        "Steam"
    } else if path.starts_with("/usr") {
        "System"
    } else {
        "Other"
    }
}

/// Download fraction in 0..=1, or `None` if the total is unknown.
pub fn fraction(done: u64, total: Option<u64>) -> Option<f64> {
    total
        .filter(|t| *t > 0)
        .map(|t| (done as f64 / t as f64).min(1.0))
}

struct Page {
    state: Rc<AppState>,
    list: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
}

impl Page {
    fn refresh(self: &Rc<Self>) {
        let this = self.clone();
        let (home, dir) = (
            self.state.paths.home().to_path_buf(),
            self.state.paths.runtimes_dir(),
        );
        gtk4::glib::spawn_future_local(async move {
            let Some(found) = blocking(move || runtime::discover(&home, &dir)).await else {
                return;
            };
            this.show(&found);
        });
    }

    fn show(self: &Rc<Self>, found: &[Runtime]) {
        for r in self.rows.take() {
            self.list.remove(&r);
        }
        let current = self
            .state
            .db
            .get_setting(KEY_DEFAULT_RUNTIME)
            .ok()
            .flatten();
        let vortex_dir = self.state.paths.runtimes_dir();
        let mut first: Option<gtk4::CheckButton> = None;
        for rt in found {
            let p = rt.path.as_path();
            let row = adw::ActionRow::builder()
                .title(gtk4::glib::markup_escape_text(&rt.label))
                .subtitle(gtk4::glib::markup_escape_text(&format!(
                    "{} - {}",
                    source_of(p, &vortex_dir),
                    p.display()
                )))
                .build();
            let radio = gtk4::CheckButton::builder()
                .valign(gtk4::Align::Center)
                .tooltip_text("Use as default")
                .build();
            radio.set_group(first.as_ref());
            first.get_or_insert_with(|| radio.clone());
            radio.set_active(current.as_deref() == Some(&*p.to_string_lossy()));
            let (state, path) = (self.state.clone(), p.to_string_lossy().into_owned());
            radio.connect_toggled(move |b| {
                if b.is_active()
                    && let Err(e) = state.db.set_setting(KEY_DEFAULT_RUNTIME, &path)
                {
                    state.toast(&e.to_string());
                }
            });
            row.add_prefix(&radio);
            self.list.add(&row);
            self.rows.borrow_mut().push(row);
        }
        if found.is_empty() {
            let row = adw::ActionRow::builder()
                .title("No Proton runtime found")
                .subtitle("Install GE-Proton below, or install Proton through Steam.")
                .build();
            self.list.add(&row);
            self.rows.borrow_mut().push(row);
        }
    }
}

pub fn build(state: &Rc<AppState>) -> gtk4::Widget {
    let list = adw::PreferencesGroup::builder()
        .title("Installed Proton runtimes")
        .description("Select the default used for new games.")
        .build();
    let page = Rc::new(Page {
        state: state.clone(),
        list: list.clone(),
        rows: RefCell::default(),
    });

    let install = gtk4::Button::builder()
        .label("Install latest GE-Proton")
        .halign(gtk4::Align::Start)
        .css_classes(["suggested-action"])
        .build();
    let bar = gtk4::ProgressBar::builder()
        .show_text(true)
        .hexpand(true)
        .valign(gtk4::Align::Center)
        .build();
    let cancel = gtk4::Button::builder()
        .label("Cancel")
        .valign(gtk4::Align::Center)
        .build();
    let progress = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    progress.append(&bar);
    progress.append(&cancel);
    progress.set_visible(false);
    let ge = adw::PreferencesGroup::builder()
        .title("GE-Proton")
        .description("Downloads the newest release from GitHub and verifies its checksum.")
        .build();
    ge.add(&install);
    ge.add(&progress);

    let cancel_flag: Rc<RefCell<Arc<AtomicBool>>> = Rc::default();
    cancel.connect_clicked({
        let flag = cancel_flag.clone();
        move |b| {
            flag.borrow().store(true, Ordering::Relaxed);
            b.set_sensitive(false);
        }
    });
    install.connect_clicked({
        let (page, flag, progress, bar, cancel) =
            (page.clone(), cancel_flag, progress, bar, cancel);
        move |btn| {
            let state = &page.state;
            if state.db.get_setting(KEY_OFFLINE).ok().flatten().as_deref() == Some("1") {
                return state.toast("Offline mode is on. Turn it off in Preferences to download.");
            }
            let cancelled = Arc::new(AtomicBool::new(false));
            *flag.borrow_mut() = cancelled.clone();
            btn.set_sensitive(false);
            cancel.set_sensitive(true);
            bar.set_fraction(0.0);
            bar.set_text(Some("Looking up the latest release"));
            progress.set_visible(true);

            let (ptx, prx) = async_channel::bounded::<Progress>(64);
            let (rtx, rrx) = async_channel::bounded(1);
            let dir = state.paths.runtimes_dir();
            std::thread::spawn(move || {
                let res = runtime_ge::latest_release()
                    .and_then(|rel| runtime_ge::install(&rel, &dir, &ptx, &cancelled))
                    .map(|_| ())
                    .map_err(|e| (matches!(e, Error::Cancelled), e.to_string()));
                drop(ptx);
                let _ = rtx.send_blocking(res);
            });
            let bar2 = bar.clone();
            gtk4::glib::spawn_future_local(async move {
                while let Ok(p) = prx.recv().await {
                    match p {
                        Progress::Downloading { done, total } => {
                            match fraction(done, total) {
                                Some(f) => bar2.set_fraction(f),
                                None => bar2.pulse(),
                            }
                            bar2.set_text(Some(&format!("Downloading {} MiB", done / (1 << 20))));
                        }
                        Progress::Verifying => bar2.set_text(Some("Verifying")),
                        Progress::Extracting => bar2.set_text(Some("Extracting")),
                    }
                }
            });
            let (page, progress, btn) = (page.clone(), progress.clone(), btn.clone());
            gtk4::glib::spawn_future_local(async move {
                let res = rrx.recv().await;
                progress.set_visible(false);
                btn.set_sensitive(true);
                match res {
                    Ok(Ok(())) => page.state.toast("GE-Proton installed"),
                    Ok(Err((true, _))) => page.state.toast("Download cancelled"),
                    Ok(Err((false, msg))) => page.state.toast(&msg),
                    Err(_) => page.state.toast("The download stopped unexpectedly"),
                }
                page.refresh();
            });
        }
    });

    let prefs = adw::PreferencesPage::new();
    prefs.add(&list);
    prefs.add(&ge);
    page.refresh();
    prefs.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources() {
        let v = Path::new("/home/u/.local/share/vortex/runtimes");
        assert_eq!(source_of(&v.join("GE-Proton9"), v), "Vortex");
        assert_eq!(
            source_of(
                Path::new("/home/u/.local/share/Steam/steamapps/common/Proton 9"),
                v
            ),
            "Steam"
        );
        assert_eq!(
            source_of(Path::new("/usr/share/steam/compatibilitytools.d/x"), v),
            "System"
        );
        assert_eq!(source_of(Path::new("/opt/p"), v), "Other");
    }

    #[test]
    fn progress_fraction() {
        assert_eq!(fraction(50, Some(200)), Some(0.25));
        assert_eq!(fraction(300, Some(200)), Some(1.0));
        assert_eq!(fraction(5, None), None);
        assert_eq!(fraction(5, Some(0)), None);
    }
}
