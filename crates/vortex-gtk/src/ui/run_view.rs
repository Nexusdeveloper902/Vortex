//! Live run view: phase, process list, log lines, Stop and "Mark as finished".

use crate::app::AppState;
use async_channel::Receiver;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use vortex_core::logs::Stream;
use vortex_core::process::{Exit, Outcome, Phase, ProcInfo, RunEvent, RunHandle};

const MAX_LINES: i32 = 3000;

pub fn phase_label(p: Phase) -> &'static str {
    match p {
        Phase::Running => "Running",
        Phase::WaitingForChildren => "Main process ended; waiting for background processes",
        Phase::Draining => "Waiting for remaining processes to exit",
    }
}

/// User-visible processes only (Wine infrastructure is hidden).
pub fn proc_summary(ps: &[ProcInfo]) -> String {
    let v: Vec<String> = ps
        .iter()
        .filter(|p| !p.infra)
        .map(|p| format!("{} ({})", p.name, p.pid))
        .collect();
    if v.is_empty() {
        "None".into()
    } else {
        v.join(", ")
    }
}

pub fn outcome_label(o: &Outcome) -> String {
    if o.user_forced {
        return "Marked as finished by you. Background processes may still be running.".into();
    }
    let how = match o.exit {
        Some(Exit::Low8(c)) => format!("exit code {c}"),
        Some(Exit::Signal(s)) => format!("killed by signal {s}"),
        None => "exit status unknown".into(),
    };
    if o.stopped {
        format!("Stopped ({how})")
    } else {
        format!("Ended ({how})")
    }
}

pub struct RunView {
    pub root: gtk4::Box,
    pub stop_btn: gtk4::Button,
    pub mark_btn: gtk4::Button,
    status: adw::ActionRow,
    procs: adw::ActionRow,
    buf: gtk4::TextBuffer,
    view: gtk4::TextView,
    mark: gtk4::TextMark,
    lines: Cell<i32>,
}

impl RunView {
    pub fn new(stop_label: &str) -> Rc<Self> {
        let status = adw::ActionRow::builder()
            .title("Status")
            .subtitle("Starting")
            .use_markup(false)
            .build();
        let procs = adw::ActionRow::builder()
            .title("Processes")
            .subtitle("None")
            .use_markup(false)
            .build();
        let stop_btn = gtk4::Button::builder()
            .label(stop_label)
            .css_classes(["destructive-action", "pill"])
            .build();
        let mark_btn = gtk4::Button::builder()
            .label("Mark as finished")
            .tooltip_text("Stop waiting. Nothing is killed.")
            .css_classes(["pill"])
            .sensitive(false)
            .build();
        let view = gtk4::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk4::WrapMode::WordChar)
            .left_margin(8)
            .right_margin(8)
            .build();
        let buf = view.buffer();
        let mark = buf.create_mark(None, &buf.end_iter(), false);
        let scroll = gtk4::ScrolledWindow::builder()
            .child(&view)
            .min_content_height(220)
            .build();
        let log_expander = adw::ExpanderRow::builder()
            .title("Live log")
            .expanded(true)
            .build();
        let frame = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        frame.append(&scroll);
        log_expander.add_row(&frame);
        let list = gtk4::ListBox::builder()
            .selection_mode(gtk4::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        list.append(&status);
        list.append(&procs);
        list.append(&log_expander);
        let buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
        buttons.set_halign(gtk4::Align::Center);
        buttons.append(&mark_btn);
        buttons.append(&stop_btn);
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 18);
        root.append(&list);
        root.append(&buttons);
        Rc::new(Self {
            root,
            stop_btn,
            mark_btn,
            status,
            procs,
            buf,
            view,
            mark,
            lines: Cell::new(0),
        })
    }

    /// Prepare for a new attempt (Retry).
    pub fn reset(&self) {
        self.buf.set_text("");
        self.lines.set(0);
        self.status.set_subtitle("Starting");
        self.procs.set_subtitle("None");
        self.stop_btn.set_sensitive(true);
        self.mark_btn.set_sensitive(false);
    }

    /// No run is attached (e.g. a resumed install): replaces the initial "Starting".
    pub fn set_idle(&self) {
        self.status.set_subtitle("Not running");
        self.stop_btn.set_sensitive(false);
        self.mark_btn.set_sensitive(false);
    }

    pub fn apply(&self, ev: &RunEvent) {
        match ev {
            RunEvent::Phase(p) => {
                self.status.set_subtitle(phase_label(*p));
                // A new run (e.g. the installer after prefix creation) re-arms Stop; a previous
                // run's Finished had disabled it.
                self.stop_btn.set_sensitive(true);
                self.mark_btn.set_sensitive(*p != Phase::Running);
            }
            RunEvent::Line { stream, text } => self.append(*stream, text),
            RunEvent::Processes(ps) => self.procs.set_subtitle(&proc_summary(ps)),
            RunEvent::Uncertain(ps) => {
                self.status.set_subtitle(
                    "Processes are still running and Vortex cannot tell if the job is done. \
                     Stop them, or mark it as finished if you know it is.",
                );
                self.procs.set_subtitle(&proc_summary(ps));
                self.mark_btn.set_sensitive(true);
            }
            RunEvent::Finished(o) => {
                self.status.set_subtitle(&outcome_label(o));
                self.procs.set_subtitle("None");
                self.stop_btn.set_sensitive(false);
                self.mark_btn.set_sensitive(false);
            }
        }
    }

    fn append(&self, stream: Stream, text: &str) {
        let pre = if stream == Stream::Stderr { "! " } else { "" };
        self.buf
            .insert(&mut self.buf.end_iter(), &format!("{pre}{text}\n"));
        self.lines.set(self.lines.get() + 1);
        let extra = self.lines.get() - MAX_LINES;
        if extra > 0 {
            let mut s = self.buf.start_iter();
            if let Some(mut e) = self.buf.iter_at_line(extra) {
                self.buf.delete(&mut s, &mut e);
                self.lines.set(MAX_LINES);
            }
        }
        self.buf.move_mark(&self.mark, &self.buf.end_iter());
        self.view.scroll_mark_onscreen(&self.mark);
    }
}

/// Consumes events on the GTK main loop.
pub fn follow(view: &Rc<RunView>, rx: Receiver<RunEvent>, on_done: impl Fn(Outcome) + 'static) {
    let view = view.clone();
    glib::spawn_future_local(async move {
        while let Ok(ev) = rx.recv().await {
            view.apply(&ev);
            if let RunEvent::Finished(o) = ev {
                on_done(o);
            }
        }
    });
}

/// True for a run that ended on its own with a non-zero exit (not stopped, not marked finished).
pub fn failed(o: &Outcome) -> bool {
    !o.stopped && !o.user_forced && o.exit != Some(Exit::Low8(0))
}

/// Pushes a run view page for a run started elsewhere (run exe, uninstaller, winetricks).
/// `retry_verbose` starts the same job again with a verbose log; it is offered once the run
/// has [`failed`], and replaces this page.
pub fn present(
    state: &Rc<AppState>,
    title: &str,
    handle: RunHandle,
    rx: Receiver<RunEvent>,
    retry_verbose: Option<Rc<dyn Fn()>>,
) {
    let view = RunView::new("Stop");
    {
        let h = handle.clone();
        view.stop_btn.connect_clicked(move |b| {
            h.stop();
            b.set_sensitive(false);
        });
        view.mark_btn
            .connect_clicked(move |_| handle.mark_finished());
    }
    if let Some(retry) = retry_verbose {
        let btn = gtk4::Button::builder()
            .label("Retry with verbose log")
            .css_classes(["pill"])
            .visible(false)
            .build();
        let nav = state.nav.clone();
        btn.connect_clicked(move |_| {
            nav.pop();
            retry();
        });
        view.root.append(&btn);
        follow(&view, rx, move |o| btn.set_visible(failed(&o)));
    } else {
        follow(&view, rx, |_| {});
    }
    let clamp = adw::Clamp::builder()
        .maximum_size(640)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(16)
        .margin_end(16)
        .child(&view.root)
        .build();
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&gtk4::ScrolledWindow::builder().child(&clamp).build()));
    state.nav.push(&adw::NavigationPage::new(&tv, title));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes() {
        let o = |exit, stopped, user_forced| Outcome {
            exit,
            stopped,
            user_forced,
        };
        assert_eq!(
            outcome_label(&o(Some(Exit::Low8(3)), false, false)),
            "Ended (exit code 3)"
        );
        assert_eq!(
            outcome_label(&o(Some(Exit::Signal(9)), true, false)),
            "Stopped (killed by signal 9)"
        );
        assert!(outcome_label(&o(None, false, true)).starts_with("Marked as finished"));
        assert!(failed(&o(Some(Exit::Low8(1)), false, false)));
        assert!(!failed(&o(Some(Exit::Low8(0)), false, false)));
        assert!(!failed(&o(Some(Exit::Signal(9)), true, false)));
        assert!(!failed(&o(None, false, true)));
    }

    #[test]
    fn procs() {
        let p = |n: &str, infra| ProcInfo {
            pid: 7,
            name: n.into(),
            infra,
        };
        assert_eq!(proc_summary(&[p("wineserver", true)]), "None");
        assert_eq!(proc_summary(&[p("setup.exe", false)]), "setup.exe (7)");
    }
}
