//! M9: install state machine and crash recovery.
//!
//! Default tests drive `tests/fake-umu-run` through a wrapper that reads its mode from
//! `<cwd>/.fake-mode` (cwd = installer dir for the installer, the prefixes dir for wineboot) and a
//! fake runtime whose `wineserver` honours `$WINEPREFIX/fake-wineserver.pid` (as in M7).
//! `#[ignore]` tests (`it_*`) drive the real umu-run + Proton from `VORTEX_IT_PROTON`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use vortex_core::db::{Db, GameStatus, InstallState, RunKind, RunState};
use vortex_core::diagnose::Class;
use vortex_core::error::Error;
use vortex_core::install::{
    InstallEvent, InstallRequest, Installs, RegisterEnv, RegisterRequest, Started, scan_exes,
};
use vortex_core::paths::{HostPath, Paths, PrefixDir};
use vortex_core::process::{Outcome, Phase, RunEvent, Runs, Timing, scan_marker};
use vortex_core::runner::{MUTE_AUDIO_OVERRIDES, Tools};

const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");
const CHILD_ROOT: &str = "VORTEX_M9_CHILD_ROOT";
const CHILD_TOOLS: &str = "VORTEX_M9_CHILD_TOOLS";

fn write_exec(p: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, body).unwrap(); // write + close before anything execs it (ETXTBSY)
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Fake Proton dir and umu wrapper, built once per test binary before anything is spawned.
/// A child test process reuses its parent's (`CHILD_TOOLS`).
/// Shared by concurrent tests; the dir is removed when the last holder drops (a static would
/// never run `TempDir::drop`).
struct FakeTools {
    path: PathBuf,
    _dir: Option<tempfile::TempDir>,
}

fn fake_tools() -> Arc<FakeTools> {
    static T: Mutex<Weak<FakeTools>> = Mutex::new(Weak::new());
    let mut slot = T.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = slot.upgrade() {
        return t;
    }
    let t = Arc::new(if let Ok(p) = std::env::var(CHILD_TOOLS) {
        FakeTools {
            path: PathBuf::from(p),
            _dir: None,
        }
    } else {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let rt = d.join("runtime");
        std::fs::create_dir_all(rt.join("files/bin")).unwrap();
        write_exec(&rt.join("proton"), "#!/bin/sh\nexit 0\n");
        write_exec(
            &rt.join("files/bin/wineserver"),
            r#"#!/bin/sh
pidf="$WINEPREFIX/fake-wineserver.pid"
alive() {
  [ -s "$pidf" ] || return 1
  st=$(cat "/proc/$(cat "$pidf")/stat" 2>/dev/null) || return 1
  case "${st##*) }" in Z*) return 1 ;; esac  # zombie: dead, just unreaped
  return 0
}
case "$1" in
  -w) while alive; do sleep 0.05; done; exit 0 ;;
  -k) if alive; then kill "$(cat "$pidf")"; exit 0; fi; exit 1 ;;
esac
exit 2
"#,
        );
        std::fs::write(rt.join("toolmanifest.vdf"), "\"manifest\" {}\n").unwrap();
        write_exec(
            &d.join("umu-run"),
            &format!(
                "#!/bin/sh\n[ -f .fake-mode ] && FAKE_UMU_MODE=$(cat .fake-mode) && export FAKE_UMU_MODE\n\
                 FAKE_UMU_OUT=\"$PWD/.fake-rec\" exec '{MANIFEST}/tests/fake-umu-run' \"$@\"\n"
            ),
        );
        FakeTools {
            path: d.to_path_buf(),
            _dir: Some(dir),
        }
    });
    *slot = Arc::downgrade(&t);
    t
}

fn fast() -> Timing {
    Timing {
        poll: Duration::from_millis(50),
        scan_every: Duration::from_millis(200),
        drain: Duration::from_millis(600),
        stop_grace: Duration::from_millis(800),
    }
}

struct T {
    _tmp: Option<tempfile::TempDir>,
    root: PathBuf,
    paths: Paths,
    db: Arc<Mutex<Db>>,
    runs: Arc<Runs>,
    inst: Installs,
    runtime: HostPath,
    _tools: Arc<FakeTools>,
}

impl T {
    fn at(root: &Path, tmp: Option<tempfile::TempDir>) -> T {
        let fake = fake_tools();
        let tools = &fake.path;
        let paths = Paths::under(root);
        let db = Arc::new(Mutex::new(Db::open(&paths.db_file()).unwrap()));
        let runs = Arc::new(
            Runs::new(
                paths.clone(),
                Tools {
                    umu_run: tools.join("umu-run"),
                },
                db.clone(),
            )
            .with_timing(fast()),
        );
        let inst = Installs::new(paths.clone(), db.clone(), runs.clone())
            .with_source_check(Duration::from_millis(100))
            .with_desktop_entries(PathBuf::from("/usr/bin/vortex"));
        std::fs::create_dir_all(paths.prefixes_dir()).unwrap();
        T {
            _tmp: tmp,
            root: root.to_path_buf(),
            paths,
            db,
            runs,
            inst,
            runtime: HostPath::new(tools.join("runtime")).unwrap(),
            _tools: fake,
        }
    }

    fn new() -> T {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        T::at(&root, Some(tmp))
    }

    /// Installer dir (with a space) holding setup.exe, data files and a redist.
    fn source(&self, name: &str, mode: &str) -> HostPath {
        let d = self.root.join(name);
        for f in [
            "setup.exe",
            "data1.bin",
            "setup-1.bin",
            "Redist/vcredist_x64.exe",
        ] {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "MZ").unwrap();
        }
        std::fs::write(d.join(".fake-mode"), mode).unwrap();
        HostPath::new(d).unwrap()
    }

    fn init_mode(&self, mode: &str) {
        std::fs::write(self.paths.prefixes_dir().join(".fake-mode"), mode).unwrap();
    }

    fn req(&self, name: &str, source: &HostPath, installer: &str) -> InstallRequest {
        InstallRequest {
            name: name.into(),
            source: source.clone(),
            installer: source.join(installer).unwrap(),
            runtime: self.runtime.clone(),
            install_dir: None,
            prefix: None,
            args: Vec::new(),
        }
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Db> {
        self.db.lock().unwrap()
    }

    fn state(&self, install_id: i64) -> InstallState {
        self.db().get_install(install_id).unwrap().state
    }

    fn prefix(&self, s: &Started) -> PrefixDir {
        self.paths.managed_prefix(s.env_id).unwrap()
    }
}

/// `None` as root (CI containers): then only the spawn refusal is checked (CLAUDE.md rule 2):
/// the install fails at prefix init without anything running, and never reaches `installed`.
fn fake() -> Option<T> {
    let t = T::new();
    if rustix::process::geteuid().is_root() {
        let src = t.source("root src", "exit:0");
        let s = t.inst.begin(t.req("Root Game", &src, "setup.exe")).unwrap();
        let evs = drive(&s.events, Duration::from_secs(20), |_| {});
        assert!(
            evs.iter()
                .any(|e| matches!(e, InstallEvent::Error(m) if m.contains("root"))),
            "{evs:?}"
        );
        assert_eq!(states(&evs).last(), Some(&InstallState::Failed));
        assert_eq!(
            t.db().get_game(s.game_id).unwrap().status,
            GameStatus::Failed
        );
        assert!(t.db().active_runs().unwrap().is_empty());
        eprintln!("running as root: verified the spawn refusal only");
        return None;
    }
    Some(t)
}

/// Collects events until the channel closes (after the final `State`); `on` sees each event.
fn drive(
    rx: &async_channel::Receiver<InstallEvent>,
    limit: Duration,
    mut on: impl FnMut(&InstallEvent),
) -> Vec<InstallEvent> {
    let end = Instant::now() + limit;
    let mut evs = Vec::new();
    loop {
        assert!(
            Instant::now() < end,
            "install did not end in {limit:?}: {evs:?}"
        );
        match rx.try_recv() {
            Ok(ev) => {
                on(&ev);
                evs.push(ev);
            }
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(async_channel::TryRecvError::Closed) => return evs,
        }
    }
}

fn states(evs: &[InstallEvent]) -> Vec<InstallState> {
    evs.iter()
        .filter_map(|e| match e {
            InstallEvent::State(s) => Some(*s),
            _ => None,
        })
        .collect()
}

fn findings(evs: &[InstallEvent]) -> Vec<Class> {
    evs.iter()
        .filter_map(|e| match e {
            InstallEvent::Finding(f) => Some(f.class),
            _ => None,
        })
        .collect()
}

fn run_outcome(evs: &[InstallEvent], kind: RunKind) -> Option<Outcome> {
    evs.iter().find_map(|e| match e {
        InstallEvent::Run {
            kind: k,
            event: RunEvent::Finished(o),
        } if *k == kind => Some(*o),
        _ => None,
    })
}

const LIMIT: Duration = Duration::from_secs(30);

#[test]
fn happy_path_select_exe_then_confirm() {
    let Some(t) = fake() else { return };
    let src = t.source("My Installer", "sleep:1");
    let s = t.inst.begin(t.req("Foo Game", &src, "setup.exe")).unwrap();
    let pfx = t.prefix(&s);
    let game_dir = pfx.drive_c().join("Program Files/Foo Game");
    let mut status_at_select = None;
    let evs = drive(&s.events, LIMIT, |ev| match ev {
        // The "installer" writes the game while it runs.
        InstallEvent::State(InstallState::Running) => {
            std::fs::create_dir_all(&game_dir).unwrap();
            std::fs::write(game_dir.join("FooGame.exe"), vec![0u8; 4096]).unwrap();
            std::fs::write(game_dir.join("unins000.exe"), vec![0u8; 9000]).unwrap();
            std::fs::write(game_dir.join("CrashReporter.exe"), "MZ").unwrap();
            std::fs::write(game_dir.join("tool.exe"), "MZ").unwrap();
        }
        InstallEvent::State(InstallState::SelectExe) => {
            status_at_select = Some(t.db().get_game(s.game_id).unwrap().status);
        }
        _ => {}
    });
    assert_eq!(
        states(&evs),
        [
            InstallState::PrefixInit,
            InstallState::Running,
            InstallState::Waiting,
            InstallState::SelectExe
        ]
    );
    // Exit code 0 alone never sets installed.
    assert_eq!(status_at_select, Some(GameStatus::Installing));
    assert_eq!(t.state(s.install_id), InstallState::SelectExe);

    let cands = evs
        .iter()
        .find_map(|e| match e {
            InstallEvent::Candidates(c) => Some(c.clone()),
            _ => None,
        })
        .unwrap();
    let names: Vec<_> = cands
        .iter()
        .map(|c| {
            c.path
                .as_path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, ["FooGame.exe", "tool.exe"], "{cands:?}");
    assert_eq!(
        cands[0].win.as_str(),
        r"C:\Program Files\Foo Game\FooGame.exe"
    );
    assert_eq!(t.inst.exe_candidates(s.install_id).unwrap(), cands);

    // The prefix init and installer invocations (fake records argv and cwd).
    let init = std::fs::read_to_string(t.paths.prefixes_dir().join(".fake-rec")).unwrap();
    assert!(
        init.contains("arg=C:\\windows\\system32\\wineboot.exe\narg=-u\n"),
        "{init}"
    );
    let rec = std::fs::read_to_string(src.as_path().join(".fake-rec")).unwrap();
    let dir = src.as_path().display();
    assert_eq!(rec, format!("cwd={dir}\narg={dir}/setup.exe\n"));
    let envs = std::fs::read_to_string(src.as_path().join(".fake-rec.env")).unwrap();
    assert!(envs.contains(&format!("WINEPREFIX={}\n", pfx.as_path().display())));
    // Installers run muted (the audio drivers are disabled for that run only).
    let overrides = envs
        .lines()
        .find_map(|l| l.strip_prefix("WINEDLLOVERRIDES="))
        .unwrap_or_default();
    assert!(overrides.ends_with(MUTE_AUDIO_OVERRIDES), "{overrides:?}");

    // confirm_exe rejects a missing exe and non-programs; state stays SelectExe.
    let missing = HostPath::new(game_dir.join("Nope.exe")).unwrap();
    assert!(matches!(
        t.inst.confirm_exe(s.install_id, &missing, None),
        Err(Error::NotFound(_))
    ));
    std::fs::write(game_dir.join("readme.txt"), "x").unwrap();
    let txt = HostPath::new(game_dir.join("readme.txt")).unwrap();
    assert!(matches!(
        t.inst.confirm_exe(s.install_id, &txt, None),
        Err(Error::Validation(_))
    ));
    assert_eq!(t.state(s.install_id), InstallState::SelectExe);
    assert_eq!(
        t.db().get_game(s.game_id).unwrap().status,
        GameStatus::Installing
    );

    let r = t
        .inst
        .confirm_exe(s.install_id, &cands[0].path, None)
        .unwrap();
    assert_eq!(r.game_id, s.game_id);
    let entry = r.desktop_entry.unwrap().unwrap();
    assert!(entry.starts_with(t.paths.applications_dir()) && entry.is_file());
    let g = t.db().get_game(s.game_id).unwrap();
    assert_eq!(g.status, GameStatus::Installed);
    assert_eq!(
        g.exe_path.as_deref(),
        Some(&*cands[0].path.as_path().to_string_lossy())
    );
    assert_eq!(t.state(s.install_id), InstallState::Done);

    // Done is terminal.
    for res in [
        t.inst
            .confirm_exe(s.install_id, &cands[0].path, None)
            .map(drop),
        t.inst.retry(s.install_id, false).map(drop),
        t.inst.mark_failed(s.install_id),
        t.inst.mark_installer_finished(s.install_id).map(drop),
        t.inst.cancel(s.install_id),
    ] {
        assert!(matches!(res, Err(Error::Validation(_))), "{res:?}");
    }
    // The source is referenced, never copied or changed.
    let i = t.db().get_install(s.install_id).unwrap();
    assert_eq!(i.source_path, src.as_path().to_string_lossy());
    assert!(src.as_path().join("data1.bin").is_file());
}

#[test]
fn installer_exit_nonzero_fails_and_reboot_code_does_not() {
    let Some(t) = fake() else { return };
    let src = t.source("bad src", "exit:5");
    let s = t.inst.begin(t.req("Bad", &src, "setup.exe")).unwrap();
    let evs = drive(&s.events, LIMIT, |_| {});
    assert_eq!(states(&evs).last(), Some(&InstallState::Failed));
    assert!(findings(&evs).contains(&Class::InstallerFailure), "{evs:?}");
    let i = t.db().get_install(s.install_id).unwrap();
    assert_eq!(i.failure.as_deref(), Some("InstallerFailure"));
    assert_eq!(
        t.db().get_game(s.game_id).unwrap().status,
        GameStatus::Failed
    );
    let run = t.db().runs_for_game(s.game_id).unwrap()[0].clone();
    assert!(run.findings.unwrap().contains("InstallerFailure"));
    // Illegal from Failed; the user may still say it finished, then pick the exe.
    let exe = src.join("setup.exe").unwrap();
    assert!(matches!(
        t.inst.confirm_exe(s.install_id, &exe, None),
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        t.inst.mark_failed(s.install_id),
        Err(Error::Validation(_))
    ));
    t.inst.mark_installer_finished(s.install_id).unwrap();
    assert_eq!(t.state(s.install_id), InstallState::SelectExe);
    t.inst.mark_failed(s.install_id).unwrap();
    assert_eq!(t.state(s.install_id), InstallState::Failed);

    // 3010 arrives as 194 and means "reboot required", not failure.
    let src = t.source("reboot src", "exit:3010");
    let s = t.inst.begin(t.req("Reboot", &src, "setup.exe")).unwrap();
    let evs = drive(&s.events, LIMIT, |_| {});
    assert_eq!(
        states(&evs).last(),
        Some(&InstallState::SelectExe),
        "{evs:?}"
    );
    assert!(!findings(&evs).contains(&Class::InstallerFailure));
    assert_ne!(
        t.db().get_game(s.game_id).unwrap().status,
        GameStatus::Installed
    );
}

#[test]
fn source_removed_mid_install_is_missing_installer_data() {
    let Some(t) = fake() else { return };
    let src = t.source("vanishing src", "sleep:2");
    let s = t.inst.begin(t.req("Gone", &src, "setup.exe")).unwrap();
    let mut finding_while_running = false;
    let mut finished = false;
    let evs = drive(&s.events, LIMIT, |ev| match ev {
        // Remove only once the installer process is up: removing at State(Running) can beat
        // the spawn (its cwd would be gone), which is a spawn error, not a lost source.
        InstallEvent::Run {
            event: RunEvent::Phase(Phase::Running),
            kind: RunKind::Install,
        } => {
            // The fake umu-run may still be writing its .fake-rec files into the folder (cwd):
            // remove_dir_all can then fail with ENOTEMPTY, so retry until it is gone.
            for _ in 0..50 {
                if std::fs::remove_dir_all(src.as_path()).is_ok() || !src.as_path().exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(!src.as_path().exists());
        }
        InstallEvent::Finding(f) if f.class == Class::MissingInstallerData && !finished => {
            finding_while_running = true;
        }
        InstallEvent::Run {
            event: RunEvent::Finished(_),
            kind: RunKind::Install,
        } => finished = true,
        _ => {}
    });
    assert!(finding_while_running, "{evs:?}");
    assert_eq!(states(&evs).last(), Some(&InstallState::Failed));
    let i = t.db().get_install(s.install_id).unwrap();
    assert_eq!(i.failure.as_deref(), Some("MissingInstallerData"));
    assert_ne!(
        t.db().get_game(s.game_id).unwrap().status,
        GameStatus::Installed
    );

    // Retrying without the source fails at once, without running anything.
    let before = t.db().runs_for_game(s.game_id).unwrap().len();
    let evs = drive(&t.inst.retry(s.install_id, false).unwrap(), LIMIT, |_| {});
    assert_eq!(states(&evs).last(), Some(&InstallState::Failed));
    assert!(findings(&evs).contains(&Class::MissingInstallerData));
    assert_eq!(t.db().runs_for_game(s.game_id).unwrap().len(), before);
}

#[test]
fn source_file_removed_mid_install_is_named() {
    let Some(t) = fake() else { return };
    let src = t.source("lost file src", "sleep:2,exit:3");
    let s = t.inst.begin(t.req("Lost File", &src, "setup.exe")).unwrap();
    let evs = drive(&s.events, LIMIT, |ev| {
        if let InstallEvent::Run {
            event: RunEvent::Phase(Phase::Running),
            kind: RunKind::Install,
        } = ev
        {
            std::fs::remove_file(src.as_path().join("data1.bin")).unwrap();
        }
    });
    assert_eq!(states(&evs).last(), Some(&InstallState::Failed));
    let named = evs.iter().any(|e| {
        matches!(e, InstallEvent::Finding(f) if f.class == Class::MissingInstallerData
            && f.confidence == vortex_core::diagnose::Confidence::Confirmed
            && f.evidence.iter().any(|l| l.contains("data1.bin")))
    });
    assert!(named, "{evs:?}");
}

#[test]
fn cancel_mid_install_then_retry() {
    let Some(t) = fake() else { return };
    let src = t.source("cancel src", "sleep:30");
    let s = t.inst.begin(t.req("Cancel Me", &src, "setup.exe")).unwrap();
    assert!(t.inst.is_active(s.install_id));
    // While active, user transitions are refused.
    assert!(matches!(
        t.inst.retry(s.install_id, false),
        Err(Error::Busy(_))
    ));
    let evs = drive(&s.events, LIMIT, |ev| {
        if *ev == InstallEvent::State(InstallState::Running) {
            std::thread::sleep(Duration::from_millis(200));
            t.inst.cancel(s.install_id).unwrap();
        }
    });
    assert_eq!(states(&evs).last(), Some(&InstallState::Cancelled));
    assert!(run_outcome(&evs, RunKind::Install).unwrap().stopped);
    assert!(!t.inst.is_active(s.install_id));
    assert_eq!(
        t.db().get_game(s.game_id).unwrap().status,
        GameStatus::Failed
    );
    for r in t.db().runs_for_game(s.game_id).unwrap() {
        assert!(scan_marker(&r.marker).is_empty());
    }

    std::fs::write(src.as_path().join(".fake-mode"), "exit:0").unwrap();
    let evs = drive(&t.inst.retry(s.install_id, false).unwrap(), LIMIT, |_| {});
    assert_eq!(
        states(&evs).last(),
        Some(&InstallState::SelectExe),
        "{evs:?}"
    );
}

#[test]
fn mark_run_finished_while_waiting_for_children() {
    let Some(t) = fake() else { return };
    let src = t.source("orphan src", "orphan:30");
    let s = t.inst.begin(t.req("Orphan", &src, "setup.exe")).unwrap();
    let evs = drive(&s.events, LIMIT, |ev| {
        if *ev == InstallEvent::State(InstallState::Waiting) {
            assert_eq!(t.state(s.install_id), InstallState::Waiting);
            t.inst.mark_run_finished(s.install_id).unwrap();
        }
    });
    assert_eq!(states(&evs).last(), Some(&InstallState::SelectExe));
    let o = run_outcome(&evs, RunKind::Install).unwrap();
    assert!(o.user_forced && !o.stopped);
    // Forced kills nothing: clean the orphan up with the detached stop.
    let run = t
        .db()
        .runs_for_game(s.game_id)
        .unwrap()
        .into_iter()
        .find(|r| r.kind == RunKind::Install)
        .unwrap();
    assert!(!scan_marker(&run.marker).is_empty());
    assert!(t.runs.stop_detached(run.id).unwrap().is_empty());
    assert!(scan_marker(&run.marker).is_empty());
}

#[test]
fn msi_runs_through_msiexec_with_windows_path() {
    let Some(t) = fake() else { return };
    let dir = t.root.join("msi src");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Game.msi"), "x").unwrap();
    let msi = HostPath::new(dir.join("Game.msi")).unwrap();
    let mut req = t.req("Msi", &msi, "unused");
    req.installer = msi.clone();
    let s = t.inst.begin(req).unwrap();
    let evs = drive(&s.events, LIMIT, |_| {});
    assert_eq!(states(&evs).last(), Some(&InstallState::SelectExe));
    let rec = std::fs::read_to_string(dir.join(".fake-rec")).unwrap();
    let win = format!("Z:{}", msi.as_path().display()).replace('/', "\\");
    assert_eq!(
        rec,
        format!(
            "cwd={}\narg=C:\\windows\\system32\\msiexec.exe\narg=/i\narg={win}\n",
            dir.display()
        )
    );
    assert_eq!(
        t.db().get_install(s.install_id).unwrap().source_kind,
        vortex_core::db::SourceKind::File
    );
}

#[test]
fn msi_args_follow_the_file_exact_argv() {
    let Some(t) = fake() else { return };
    let dir = t.root.join("msi args");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Game.msi"), "x").unwrap();
    let msi = HostPath::new(dir.join("Game.msi")).unwrap();
    let mut req = t.req("MsiArgs", &msi, "unused");
    req.installer = msi.clone();
    req.args = vortex_core::install::split_args("/qn INSTALLDIR=\"C:\\My Games\"").unwrap();
    let s = t.inst.begin(req).unwrap();
    drive(&s.events, LIMIT, |_| {});
    let rec = std::fs::read_to_string(dir.join(".fake-rec")).unwrap();
    let win = format!("Z:{}", msi.as_path().display()).replace('/', "\\");
    assert_eq!(
        rec,
        format!(
            "cwd={}\narg=C:\\windows\\system32\\msiexec.exe\narg=/i\narg={win}\narg=/qn\narg=INSTALLDIR=C:\\My Games\n",
            dir.display()
        )
    );
}

#[test]
fn exe_args_are_stored_and_reused_by_retry() {
    let Some(t) = fake() else { return };
    let src = t.source("args src", "exit:3");
    let mut req = t.req("ExeArgs", &src, "setup.exe");
    req.args = vec!["/S".into(), "/D=a b".into()];
    let s = t.inst.begin(req).unwrap();
    drive(&s.events, LIMIT, |_| {});
    let dir = src.as_path().display();
    let want = format!("cwd={dir}\narg={dir}/setup.exe\narg=/S\narg=/D=a b\n");
    let rec = || std::fs::read_to_string(src.as_path().join(".fake-rec")).unwrap();
    assert_eq!(rec(), want);
    assert_eq!(
        t.db().get_install(s.install_id).unwrap().installer_args,
        r#"["/S","/D=a b"]"#
    );
    std::fs::remove_file(src.as_path().join(".fake-rec")).unwrap();
    drive(&t.inst.retry(s.install_id, false).unwrap(), LIMIT, |_| {});
    assert_eq!(rec(), want);
}

#[test]
fn prefix_init_failure_never_runs_the_installer() {
    let Some(t) = fake() else { return };
    t.init_mode("exit:1");
    let src = t.source("init src", "exit:0");
    let s = t.inst.begin(t.req("NoInit", &src, "setup.exe")).unwrap();
    let evs = drive(&s.events, LIMIT, |_| {});
    assert_eq!(
        states(&evs),
        [InstallState::PrefixInit, InstallState::Failed]
    );
    assert!(findings(&evs).contains(&Class::RuntimeInitFailure));
    let i = t.db().get_install(s.install_id).unwrap();
    assert_eq!(i.failure.as_deref(), Some("RuntimeInitFailure"));
    let runs = t.db().runs_for_game(s.game_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].kind, RunKind::PrefixInit);
    assert!(!src.as_path().join(".fake-rec").exists());
}

#[test]
fn begin_validates_before_writing_anything() {
    let t = T::new();
    let src = t.source("val src", "exit:0");
    let mut bad = vec![
        t.req("  ", &src, "setup.exe"),
        t.req("X", &src, "data1.bin"),
        t.req("X", &src, "nope.exe"),
    ];
    let mut r = t.req("X", &src, "setup.exe");
    r.runtime = src.clone();
    bad.push(r);
    let mut r = t.req("X", &src, "setup.exe");
    r.install_dir = Some(HostPath::new(t.root.join("missing/no such dir")).unwrap());
    bad.push(r);
    let full = t.root.join("full");
    std::fs::create_dir_all(full.join("x")).unwrap();
    for p in [
        full.clone(),
        t.root.join("missing/pfx"),
        t.paths.data_dir().join("mine"),
        t.paths.home().to_path_buf(),
    ] {
        let mut r = t.req("X", &src, "setup.exe");
        r.prefix = Some(HostPath::new(&p).unwrap());
        bad.push(r);
    }
    for req in bad {
        assert!(t.inst.begin(req.clone()).is_err(), "{req:?}");
    }
    assert!(t.db().list_games().unwrap().is_empty());
    assert!(t.db().list_environments().unwrap().is_empty());

    // An install dir under games_dir is Vortex's own and gets created.
    let mut r = t.req("Managed Dir", &src, "setup.exe");
    let gd = HostPath::new(t.paths.games_dir().join("Managed Dir")).unwrap();
    r.install_dir = Some(gd.clone());
    let s = t.inst.begin(r).unwrap();
    drive(&s.events, LIMIT, |_| {});
    let g = t.db().get_game(s.game_id).unwrap();
    assert!(g.install_dir_managed && gd.as_path().is_dir());
    let env = t.db().get_environment(s.env_id).unwrap();
    assert!(env.prefix_managed);
    assert_eq!(
        Path::new(&env.prefix_path),
        t.paths.managed_prefix(s.env_id).unwrap().as_path()
    );
}

#[test]
fn custom_prefix_location_and_new_install_dir() {
    let Some(t) = fake() else { return };
    let src = t.source("custom src", "exit:0");
    let root = std::fs::canonicalize(&t.root).unwrap();
    std::fs::create_dir_all(root.join("My Prefixes")).unwrap();
    std::fs::create_dir_all(root.join("Games")).unwrap();
    let pfx = root.join("My Prefixes/Foo");
    let dir = root.join("Games/Foo");
    let mut r = t.req("Foo", &src, "setup.exe");
    r.prefix = Some(HostPath::new(&pfx).unwrap());
    r.install_dir = Some(HostPath::new(&dir).unwrap());
    let s = t.inst.begin(r).unwrap();
    let evs = drive(&s.events, LIMIT, |_| {});
    assert_eq!(
        states(&evs).last(),
        Some(&InstallState::SelectExe),
        "{evs:?}"
    );
    let env = t.db().get_environment(s.env_id).unwrap();
    assert_eq!(Path::new(&env.prefix_path), pfx);
    assert!(
        !env.prefix_managed,
        "custom location is never Vortex's to delete"
    );
    assert!(pfx.is_dir() && !t.paths.managed_prefix(s.env_id).unwrap().as_path().exists());
    let g = t.db().get_game(s.game_id).unwrap();
    assert_eq!(g.install_dir.as_deref().map(Path::new), Some(dir.as_path()));
    assert!(!g.install_dir_managed && dir.is_dir());

    // inside (or equal to) a known prefix: refused, nothing written
    let n = t.db().list_games().unwrap().len();
    for p in [pfx.join("drive_c/x"), pfx.clone()] {
        let mut r = t.req("Bar", &src, "setup.exe");
        r.prefix = Some(HostPath::new(&p).unwrap());
        assert!(t.inst.begin(r).is_err(), "{p:?}");
    }
    assert_eq!(t.db().list_games().unwrap().len(), n);

    // deletion: only run logs planned (M28), both folders shown as manual, both survive
    std::fs::write(pfx.join("system.reg"), "x").unwrap();
    let plan = vortex_core::delete::plan_game_removal(&t.db(), &t.paths, s.game_id).unwrap();
    let logs = std::fs::canonicalize(t.paths.logs_dir()).unwrap();
    assert!(
        plan.remove_env
            && plan
                .items
                .iter()
                .all(|i| i.path.as_path().starts_with(&logs)),
        "{plan:?}"
    );
    let manual: Vec<_> = plan.manual.iter().map(|m| m.as_path()).collect();
    assert!(
        manual.contains(&pfx.as_path()) && manual.contains(&dir.as_path()),
        "{plan:?}"
    );
    vortex_core::delete::execute(&t.db(), &t.paths, &plan).unwrap();
    assert!(pfx.join("system.reg").is_file() && dir.is_dir());
    assert!(t.db().list_environments().unwrap().is_empty());
}

#[test]
fn exe_candidate_ranking() {
    let tmp = tempfile::tempdir().unwrap();
    let pfx = PrefixDir::new(tmp.path().join("pfx"), Path::new("/nonexistent"), &[]).unwrap();
    let pf = pfx.drive_c().join("Program Files");
    let w = |rel: &str, size: usize| {
        let p = pfx.drive_c().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; size]).unwrap();
    };
    // Present before the installer started: ignored by ctime.
    w("Program Files/Old Game/OldGame.exe", 10);
    w("Program Files/Internet Explorer/iexplore.exe", 10);
    std::thread::sleep(Duration::from_millis(1100));
    let since = vortex_core::db::now();
    for (rel, size) in [
        ("Program Files/Space Quest/SpaceQuest.exe", 100),
        ("Program Files/Space Quest/sq_launcher.exe", 5000),
        ("Program Files/Space Quest/unins000.exe", 9000),
        ("Program Files/Space Quest/UnityCrashHandler64.exe", 9000),
        ("Program Files/Space Quest/setup.exe", 9000),
        (
            "Program Files/Space Quest/_CommonRedist/vcredist_x64.exe",
            9000,
        ),
        ("Program Files/Space Quest/Redist/DXSETUP.exe", 9000),
        ("Program Files/Space Quest/dotNetFx40.exe", 9000),
        ("Program Files/Space Quest/readme.txt", 9000),
        ("Program Files (x86)/Other/editor.exe", 7000),
        ("Program Files/Steam/steam.exe", 9000),
        ("Program Files/Common Files/x.exe", 9000),
        ("windows/system32/notepad.exe", 9000),
    ] {
        w(rel, size);
    }
    std::os::unix::fs::symlink(
        pf.join("Space Quest/SpaceQuest.exe"),
        pf.join("Space Quest/link.exe"),
    )
    .unwrap();
    let own = tmp.path().join("chosen dir");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join("Bonus.exe"), "MZ").unwrap();

    let got = scan_exes("Space Quest", None, &pfx, since);
    let names: Vec<_> = got
        .iter()
        .map(|c| {
            c.path
                .as_path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    // Exact name first, then same-folder by size, then anything else new.
    assert_eq!(
        names,
        ["SpaceQuest.exe", "sq_launcher.exe", "editor.exe"],
        "{got:?}"
    );
    assert!(got[0].score > got[1].score && got[1].score > got[2].score);

    // The chosen install folder is scanned regardless of age and ranked up.
    let own = HostPath::new(&own).unwrap();
    let got = scan_exes("Bonus", Some(&own), &pfx, i64::MAX);
    assert_eq!(got.len(), 1);
    assert!(got[0].reasons.iter().any(|r| r.contains("install folder")));
    assert!(got[0].win.as_str().starts_with("Z:\\"));
}

#[test]
fn manual_registration() {
    let t = T::new();
    let exe_dir = t.root.join("Portable Game");
    std::fs::create_dir_all(&exe_dir).unwrap();
    std::fs::write(exe_dir.join("game.exe"), "MZ").unwrap();
    let exe = HostPath::new(exe_dir.join("game.exe")).unwrap();
    let req = |env| RegisterRequest {
        name: "Portable".into(),
        exe: exe.clone(),
        work_dir: None,
        env,
    };

    // Missing exe, bad runtime: nothing written.
    let mut r = req(RegisterEnv::New {
        runtime: t.runtime.clone(),
    });
    r.exe = HostPath::new(exe_dir.join("missing.exe")).unwrap();
    assert!(matches!(
        t.inst.register_existing(r),
        Err(Error::NotFound(_))
    ));
    let r = req(RegisterEnv::New {
        runtime: exe.clone(),
    });
    assert!(matches!(
        t.inst.register_existing(r),
        Err(Error::RuntimeInvalid { .. })
    ));
    assert!(t.db().list_games().unwrap().is_empty());
    assert!(t.db().list_environments().unwrap().is_empty());

    // New managed prefix.
    let a = t
        .inst
        .register_existing(req(RegisterEnv::New {
            runtime: t.runtime.clone(),
        }))
        .unwrap();
    let g = t.db().get_game(a.game_id).unwrap();
    assert_eq!(g.status, GameStatus::Installed);
    assert_eq!(
        g.exe_path.as_deref(),
        Some(&*exe.as_path().to_string_lossy())
    );
    let env = t.db().get_environment(a.env_id).unwrap();
    assert!(env.prefix_managed);
    assert!(Path::new(&env.prefix_path).is_dir());
    assert!(a.desktop_entry.unwrap().unwrap().is_file());
    assert!(t.db().latest_install_for_game(a.game_id).unwrap().is_none());

    // Shared environment.
    let b = t
        .inst
        .register_existing(req(RegisterEnv::Existing(a.env_id)))
        .unwrap();
    assert_eq!(b.env_id, a.env_id);
    assert!(
        t.inst
            .register_existing(req(RegisterEnv::Existing(999)))
            .is_err()
    );

    // Adopted prefix: never managed; must look like a prefix and not be a known one.
    let old = t.root.join("old wine prefix");
    let adopt = |p: &Path| {
        req(RegisterEnv::Adopt {
            prefix: HostPath::new(p).unwrap(),
            runtime: t.runtime.clone(),
        })
    };
    std::fs::create_dir_all(&old).unwrap();
    assert!(matches!(
        t.inst.register_existing(adopt(&old)),
        Err(Error::Validation(_))
    ));
    std::fs::create_dir_all(old.join("drive_c")).unwrap();
    let c = t.inst.register_existing(adopt(&old)).unwrap();
    assert!(!t.db().get_environment(c.env_id).unwrap().prefix_managed);
    assert!(t.inst.register_existing(adopt(&old)).is_err());
    assert!(
        t.inst
            .register_existing(adopt(Path::new(&env.prefix_path)))
            .is_err()
    );
}

// ---- crash recovery: the app (a child test process) is SIGKILLed mid-install ----

/// Child half: begins an install and blocks until killed. A no-op in a normal test run.
#[test]
fn child_begins_install_and_blocks() {
    let Ok(root) = std::env::var(CHILD_ROOT) else {
        return;
    };
    let t = T::at(Path::new(&root), None);
    let src = HostPath::new(Path::new(&root).join("crash src")).unwrap();
    let _s = t.inst.begin(t.req("Crash", &src, "setup.exe")).unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Starts the child, waits until its installer run is live, SIGKILLs it, returns the marker.
fn kill_app_mid_install(t: &T) -> String {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "child_begins_install_and_blocks",
            "--exact",
            "--test-threads=1",
        ])
        .env(CHILD_ROOT, &t.root)
        .env(CHILD_TOOLS, &fake_tools().path)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + LIMIT;
    let marker = loop {
        assert!(Instant::now() < end, "child install never started running");
        let run = t
            .db()
            .active_runs()
            .unwrap()
            .into_iter()
            .find(|r| r.kind == RunKind::Install && r.pid.is_some());
        // The pid is recorded at spawn, before exec: the marker env is only visible once the
        // fake umu-run has exec'd, so wait for a live marked process before killing the app.
        if let Some(r) = run
            && !scan_marker(&r.marker).is_empty()
        {
            break r.marker;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    child.kill().unwrap();
    child.wait().unwrap();
    marker
}

#[test]
fn app_killed_mid_install_recovers_as_interrupted() {
    let Some(t) = fake() else { return };
    t.source("crash src", "sleep:1");
    let marker = kill_app_mid_install(&t);
    // The orphaned fake umu-run finishes on its own; nobody records its exit.
    let end = Instant::now() + LIMIT;
    while !scan_marker(&marker).is_empty() {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(50));
    }
    let install = t.db().list_installs().unwrap()[0].clone();
    assert!(matches!(
        install.state,
        InstallState::Running | InstallState::Waiting
    ));

    // "Next start": a fresh Installs on the same database.
    let t2 = T::at(&t.root, None);
    let rec = t2.inst.recover().unwrap();
    assert_eq!((rec.runs, rec.installs), (1, 1));
    assert_eq!(t2.state(install.id), InstallState::Interrupted);
    let g = t2.db().get_game(install.game_id).unwrap();
    assert_eq!(g.status, GameStatus::Interrupted);
    assert!(g.exe_path.is_none());
    let runs = t2.db().runs_for_game(g.id).unwrap();
    assert!(runs.iter().any(|r| r.state == RunState::Interrupted));
    assert!(t2.inst.detached().unwrap().is_empty());
    // From interrupted: the user may say it finished (or retry).
    t2.inst.mark_installer_finished(install.id).unwrap();
    assert_eq!(t2.state(install.id), InstallState::SelectExe);
}

#[test]
fn live_leftover_run_is_detached_until_stopped() {
    let Some(t) = fake() else { return };
    t.source("crash src", "sleep:30");
    let marker = kill_app_mid_install(&t);
    assert!(!scan_marker(&marker).is_empty());

    let t2 = T::at(&t.root, None);
    let rec = t2.inst.recover().unwrap();
    assert_eq!((rec.runs, rec.installs), (0, 0));
    let det = t2.inst.detached().unwrap();
    assert_eq!(det.len(), 1, "{det:?}");
    let (install, run) = &det[0];
    assert_eq!(run.marker, marker);
    assert_eq!(install.state, InstallState::Running);
    assert!(t2.inst.check_detached(install.id).unwrap());
    // A live leftover blocks a retry in the same environment.
    assert!(t2.inst.retry(install.id, false).is_err());

    assert!(t2.inst.stop_detached(install.id).unwrap().is_empty());
    assert!(scan_marker(&marker).is_empty());
    assert_eq!(t2.state(install.id), InstallState::Interrupted);
    let g = t2.db().get_game(install.game_id).unwrap();
    assert_eq!(g.status, GameStatus::Interrupted);
    assert!(t2.inst.detached().unwrap().is_empty());
}

// ---------------------------------------------------------------------------------------------
// Real umu-run + Proton. Run with:
//   VORTEX_IT_PROTON="$HOME/.local/share/Steam/steamapps/common/Proton - Experimental" \
//     cargo test -p vortex-core --test install -- --ignored --test-threads=1

/// setup.exe: needs `data1.bin` relative to its cwd (exit 3 otherwise), copies the "game" into
/// Program Files with an uninstaller next to it, starts `Redist\prereq.exe` detached, exits 0.
const SETUP_C: &str = r#"
#include <windows.h>
#include <stdio.h>
#include <string.h>
int main(void) {
    char mod[MAX_PATH], src[MAX_PATH], cmd[2 * MAX_PATH];
    GetModuleFileNameA(NULL, mod, MAX_PATH);
    *strrchr(mod, '\\') = 0;
    FILE *d = fopen("data1.bin", "rb");
    if (!d) return 3;
    fclose(d);
    CreateDirectoryA("C:\\Program Files\\Vortex Test Game", NULL);
    snprintf(src, MAX_PATH, "%s\\payload\\game.dat", mod);
    if (!CopyFileA(src, "C:\\Program Files\\Vortex Test Game\\VortexTestGame.exe", FALSE)) return 4;
    if (!CopyFileA(src, "C:\\Program Files\\Vortex Test Game\\unins000.exe", FALSE)) return 5;
    snprintf(cmd, sizeof cmd, "\"%s\\Redist\\prereq.exe\"", mod);
    STARTUPINFOA si = { sizeof si };
    PROCESS_INFORMATION pi;
    if (!CreateProcessA(NULL, cmd, NULL, NULL, FALSE, DETACHED_PROCESS, NULL, NULL, &si, &pi)) return 6;
    return 0;
}
"#;

/// prereq.exe: outlives setup.exe by 8 s, then writes `C:\vortex-prereq.done`.
const PREREQ_C: &str = r#"
#include <windows.h>
#include <stdio.h>
int main(void) {
    Sleep(8000);
    FILE *f = fopen("C:\\vortex-prereq.done", "w");
    fputs("done\n", f);
    fclose(f);
    return 0;
}
"#;

fn mingw(src: &str, out: &Path) {
    let c = out.with_extension("c");
    std::fs::write(&c, src).unwrap();
    let st = std::process::Command::new("x86_64-w64-mingw32-gcc")
        .args(["-O2", "-o"])
        .arg(out)
        .arg(&c)
        .status()
        .unwrap_or_else(|e| panic!("x86_64-w64-mingw32-gcc not available ({e}): NOT RUN"));
    assert!(st.success(), "mingw build of {} failed", out.display());
    std::fs::remove_file(c).unwrap();
}

#[test]
#[ignore = "needs real umu-run, VORTEX_IT_PROTON and mingw-w64-gcc"]
fn it_multi_file_installer_full_state_machine() {
    let proton = std::env::var("VORTEX_IT_PROTON")
        .expect("VORTEX_IT_PROTON must point at a Proton dir for --ignored tests");
    let tmp = tempfile::Builder::new()
        .prefix("vortex-m9-it-")
        .tempdir()
        .unwrap();
    let paths = Paths::under(tmp.path());
    let db = Arc::new(Mutex::new(Db::open(&paths.db_file()).unwrap()));
    let runs = Arc::new(Runs::new(paths.clone(), Tools::default(), db.clone()));
    let inst = Installs::new(paths.clone(), db.clone(), runs);

    // Multi-file source with a space in its name.
    let src = tmp.path().join("Vortex Test Installer");
    std::fs::create_dir_all(src.join("Redist")).unwrap();
    std::fs::create_dir_all(src.join("payload")).unwrap();
    mingw(SETUP_C, &src.join("setup.exe"));
    mingw(PREREQ_C, &src.join("Redist/prereq.exe"));
    mingw(
        &std::fs::read_to_string(
            Path::new(MANIFEST).join("tests/fixtures/installers/src/exitcode.c"),
        )
        .unwrap(),
        &src.join("payload/game.dat"),
    );
    std::fs::write(src.join("data1.bin"), "DATA1-CONTENT-OK\r\n").unwrap();
    std::fs::write(src.join("setup-1.bin"), "x").unwrap();
    let source = HostPath::new(&src).unwrap();
    let analysis = vortex_core::source::analyze(&source).unwrap();
    assert!(analysis.candidates[0].path.as_path().ends_with("setup.exe"));

    let t0 = Instant::now();
    let s = inst
        .begin(InstallRequest {
            name: "Vortex Test Game".into(),
            source: source.clone(),
            installer: analysis.candidates[0].path.clone(),
            runtime: HostPath::new(&proton).unwrap(),
            install_dir: None,
            prefix: None,
            args: Vec::new(),
        })
        .unwrap();
    let pfx = paths.managed_prefix(s.env_id).unwrap();
    let done = pfx.drive_c().join("vortex-prereq.done");
    let mut prereq_done_at_select = None;
    let evs = drive(&s.events, Duration::from_secs(900), |ev| {
        if let InstallEvent::State(st) = ev {
            eprintln!("{:>7.2}s state {st:?}", t0.elapsed().as_secs_f64());
            if *st == InstallState::SelectExe {
                prereq_done_at_select = Some(done.is_file());
            }
        }
        if let InstallEvent::Run { kind, event } = ev
            && !matches!(event, RunEvent::Line { .. } | RunEvent::Processes(_))
        {
            eprintln!("{:>7.2}s {kind:?} {event:?}", t0.elapsed().as_secs_f64());
        }
        if let InstallEvent::Finding(f) = ev {
            eprintln!("finding {f:?}");
        }
    });
    eprintln!("states: {:?}", states(&evs));
    assert_eq!(
        states(&evs),
        [
            InstallState::PrefixInit,
            InstallState::Running,
            InstallState::Waiting,
            InstallState::SelectExe
        ],
        "{evs:?}"
    );
    assert_eq!(
        prereq_done_at_select,
        Some(true),
        "prereq not done at SelectExe"
    );
    let cands = inst.exe_candidates(s.install_id).unwrap();
    eprintln!("candidates: {cands:#?}");
    assert!(
        cands[0]
            .path
            .as_path()
            .ends_with("Program Files/Vortex Test Game/VortexTestGame.exe")
    );
    assert!(
        !cands
            .iter()
            .any(|c| c.path.as_path().to_string_lossy().contains("unins"))
    );
    assert_eq!(
        db.lock().unwrap().get_game(s.game_id).unwrap().status,
        GameStatus::Installing,
        "exit 0 alone must not mark installed"
    );
    inst.confirm_exe(s.install_id, &cands[0].path, None)
        .unwrap();
    let g = db.lock().unwrap().get_game(s.game_id).unwrap();
    assert_eq!(g.status, GameStatus::Installed);
    eprintln!("installed {:?} after {:?}", g.exe_path, t0.elapsed());

    for r in db.lock().unwrap().runs_for_game(s.game_id).unwrap() {
        let left = scan_marker(&r.marker);
        assert!(left.is_empty(), "{:?} run left processes: {left:?}", r.kind);
        eprintln!(
            "{:?} run: {:?} exit {:?} findings {:?}",
            r.kind, r.state, r.exit_code, r.findings
        );
    }
}

#[test]
fn verbose_retry_names_the_file_the_installer_could_not_find() {
    let Some(t) = fake() else { return };
    let src = t.source("verbose src", "protonlog,exit:3");
    let s = t.inst.begin(t.req("Verbose", &src, "setup.exe")).unwrap();
    let missing = |evs: &[InstallEvent]| {
        evs.iter().any(|e| {
            matches!(e, InstallEvent::Finding(f) if f.class == Class::MissingInstallerData
                && f.evidence[0].ends_with(r"verbose src\data3.bin"))
        })
    };
    let rec = src.as_path().join(".fake-rec.env");
    let has = |k: &str| {
        std::fs::read_to_string(&rec)
            .unwrap()
            .lines()
            .any(|l| l.starts_with(k))
    };

    // The first attempt is not verbose: no Proton log, nothing named.
    let evs = drive(&s.events, LIMIT, |_| {});
    assert_eq!(states(&evs).last(), Some(&InstallState::Failed));
    assert!(!missing(&evs), "{evs:?}");
    assert!(!has("PROTON_LOG=") && !has("UMU_LOG="));

    let evs = drive(&t.inst.retry(s.install_id, true).unwrap(), LIMIT, |_| {});
    assert_eq!(states(&evs).last(), Some(&InstallState::Failed));
    assert!(missing(&evs), "{evs:?}");
    assert!(has("PROTON_LOG=1") && has("UMU_LOG=debug") && has("PROTON_LOG_DIR="));
    let run = t
        .db()
        .runs_for_game(s.game_id)
        .unwrap()
        .into_iter()
        .find(|r| {
            r.kind == RunKind::Install
                && r.findings
                    .as_deref()
                    .is_some_and(|f| f.contains("data3.bin"))
        })
        .unwrap();
    let plog = vortex_core::logs::proton_log(Path::new(&run.log_path)).unwrap();
    assert!(plog.starts_with(t.paths.logs_dir()));
    assert!(!std::fs::read_to_string(plog).unwrap().contains("ghp_"));
    // Still never installed.
    assert_ne!(
        t.db().get_game(s.game_id).unwrap().status,
        GameStatus::Installed
    );
}
