//! M7: runner + process supervisor.
//!
//! Default tests drive `tests/fake-umu-run` and a fake runtime whose `files/bin/wineserver` is a
//! sh script (`-w` blocks while `$WINEPREFIX/fake-wineserver.pid` lives, `-k` kills it).
//! `#[ignore]` tests (`it_*`) drive the real umu-run + Proton from `VORTEX_IT_PROTON`.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use vortex_core::db::{Db, RunKind, RunState};
use vortex_core::error::Error;
use vortex_core::logs::Stream;
use vortex_core::paths::{HostPath, Paths, PrefixDir, to_win};
use vortex_core::process::{
    Exit, Outcome, Phase, RunEvent, RunHandle, RunSpec, Runs, Timing, scan_marker,
};
use vortex_core::runner::{Invocation, Program, SystemExe, Tools};

const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");

/// Fake Proton dir, built once per test binary before anything is spawned (avoids ETXTBSY).
fn fake_runtime() -> &'static Path {
    static RT: OnceLock<tempfile::TempDir> = OnceLock::new();
    RT.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("files/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let ws = r#"#!/bin/sh
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
"#;
        for (p, body) in [
            (d.path().join("proton"), "#!/bin/sh\nexit 0\n"),
            (bin.join("wineserver"), ws),
        ] {
            std::fs::write(&p, body).unwrap(); // write + close before anything execs it
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(d.path().join("toolmanifest.vdf"), "\"manifest\" {}\n").unwrap();
        d
    })
    .path()
}

struct Env {
    _tmp: tempfile::TempDir,
    paths: Paths,
    db: Arc<Mutex<Db>>,
    runs: Runs,
    env_id: i64,
    prefix: PrefixDir,
    runtime: HostPath,
    /// Installer dir with a space in its name.
    dir: HostPath,
}

fn fast() -> Timing {
    Timing {
        poll: Duration::from_millis(50),
        scan_every: Duration::from_millis(200),
        drain: Duration::from_millis(600),
        stop_grace: Duration::from_millis(800),
    }
}

fn env_with(tools: Tools, runtime: &Path, timing: Timing, tmp: tempfile::TempDir) -> Env {
    let paths = Paths::under(tmp.path());
    let db = Db::open(&paths.db_file()).unwrap();
    let prefix = PrefixDir::new(tmp.path().join("pfx"), paths.home(), &[]).unwrap();
    std::fs::create_dir_all(prefix.as_path()).unwrap();
    let env_id = db
        .insert_environment(
            &prefix.as_path().to_string_lossy(),
            true,
            &runtime.to_string_lossy(),
        )
        .unwrap();
    let dir = HostPath::new(tmp.path().join("inst dir")).unwrap();
    std::fs::create_dir_all(dir.as_path()).unwrap();
    let db = Arc::new(Mutex::new(db));
    let runs = Runs::new(paths.clone(), tools, db.clone()).with_timing(timing);
    Env {
        _tmp: tmp,
        paths,
        db,
        runs,
        env_id,
        prefix,
        runtime: HostPath::new(runtime).unwrap(),
        dir,
    }
}

/// `None` when running as root (e.g. CI containers): then the only thing to verify is that the
/// runner refuses to spawn (CLAUDE.md rule 2), which this asserts before returning.
fn fake_env() -> Option<Env> {
    let rt = fake_runtime();
    let tools = Tools {
        umu_run: Path::new(MANIFEST).join("tests/fake-umu-run"),
    };
    let e = env_with(tools, rt, fast(), tempfile::tempdir().unwrap());
    std::fs::write(e.dir.as_path().join("setup.exe"), "MZ").unwrap();
    if rustix::process::geteuid().is_root() {
        match e.fake("exit:0") {
            Err(Error::Validation(m)) if m.contains("root") => {}
            other => panic!("as root, start must refuse: {:?}", other.map(|_| ())),
        }
        assert!(e.db.lock().unwrap().active_runs().unwrap().is_empty());
        eprintln!("running as root: verified the spawn refusal only");
        return None;
    }
    Some(e)
}

impl Env {
    fn inv(&self, program: Program, args: &[&str], env: &[(&str, &str)]) -> Invocation {
        Invocation {
            program,
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: self.dir.clone(),
            prefix: self.prefix.clone(),
            runtime: self.runtime.clone(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            verbose_log_dir: None,
        }
    }
    fn setup_exe(&self) -> Program {
        Program::Exe(self.dir.join("setup.exe").unwrap())
    }
    fn spec(&self, inv: Invocation) -> RunSpec {
        RunSpec {
            kind: RunKind::Install,
            env_id: self.env_id,
            game_id: None,
            install_id: None,
            invocation: inv,
        }
    }
    fn fake(
        &self,
        mode: &str,
    ) -> vortex_core::error::Result<(RunHandle, async_channel::Receiver<RunEvent>)> {
        let inv = self.inv(self.setup_exe(), &[], &[("FAKE_UMU_MODE", mode)]);
        self.runs.start(self.spec(inv))
    }
}

/// Collects events until `Finished` (panics after `limit`).
fn collect(rx: &async_channel::Receiver<RunEvent>, limit: Duration) -> (Vec<RunEvent>, Outcome) {
    let end = Instant::now() + limit;
    let mut evs = Vec::new();
    while Instant::now() < end {
        match rx.try_recv() {
            Ok(RunEvent::Finished(o)) => return (evs, o),
            Ok(ev) => evs.push(ev),
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(e) => panic!("channel closed before Finished: {e}"),
        }
    }
    panic!("no Finished within {limit:?}; events: {evs:?}");
}

fn phases(evs: &[RunEvent]) -> Vec<Phase> {
    evs.iter()
        .filter_map(|e| match e {
            RunEvent::Phase(p) => Some(*p),
            _ => None,
        })
        .collect()
}

const T: Duration = Duration::from_secs(20);

#[test]
fn records_argv_env_cwd_with_spaces_no_shell() {
    let Some(e) = fake_env() else { return };
    let out = e.dir.as_path().join("rec.out");
    let inv = e.inv(
        e.setup_exe(),
        &["a b", "$(touch /tmp/x)", "--q"],
        &[("FAKE_UMU_OUT", &out.to_string_lossy()), ("DXVK_HUD", "1")],
    );
    let (h, rx) = e.runs.start(e.spec(inv)).unwrap();
    let (_, o) = collect(&rx, T);
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    let rec = std::fs::read_to_string(&out).unwrap();
    let dir = e.dir.as_path().to_string_lossy();
    assert_eq!(
        rec,
        format!("cwd={dir}\narg={dir}/setup.exe\narg=a b\narg=$(touch /tmp/x)\narg=--q\n")
    );
    let envs = std::fs::read_to_string(format!("{}.env", out.display())).unwrap();
    let has = |kv: String| assert!(envs.lines().any(|l| l == kv), "missing {kv}");
    has(format!("WINEPREFIX={}", e.prefix.as_path().display()));
    has(format!("PROTONPATH={}", e.runtime.as_path().display()));
    has(format!("VORTEX_RUN_ID={}", h.marker()));
    has(format!("STEAM_COMPAT_INSTALL_PATH={dir}"));
    has("GAMEID=0".into());
    has("DXVK_HUD=1".into());
}

#[test]
fn reserved_env_and_missing_exe_rejected_before_anything_runs() {
    let Some(e) = fake_env() else { return };
    for k in [
        "WINEPREFIX",
        "STEAM_COMPAT_DATA_PATH",
        "PRESSURE_VESSEL_X",
        "VORTEX_RUN_ID",
    ] {
        let inv = e.inv(e.setup_exe(), &[], &[(k, "x")]);
        assert!(
            matches!(e.runs.start(e.spec(inv)), Err(Error::Validation(_))),
            "{k}"
        );
    }
    let inv = e.inv(Program::Exe(e.dir.join("nope.exe").unwrap()), &[], &[]);
    assert!(matches!(e.runs.start(e.spec(inv)), Err(Error::NotFound(_))));
    let mut inv = e.inv(e.setup_exe(), &[], &[]);
    inv.runtime = e.dir.clone();
    assert!(matches!(
        e.runs.start(e.spec(inv)),
        Err(Error::RuntimeInvalid { .. })
    ));
    assert!(e.db.lock().unwrap().active_runs().unwrap().is_empty());
    assert!(
        e.db.lock()
            .unwrap()
            .active_run_for_environment(e.env_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn lines_in_order_redacted_in_ui_and_log() {
    let Some(e) = fake_env() else { return };
    let (h, rx) = e.fake("lines:200,secret").unwrap();
    let (evs, _) = collect(&rx, T);
    let lines: Vec<&str> = evs
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::Line {
                stream: Stream::Stdout,
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let want: Vec<String> = (1..=200).map(|i| format!("line {i}")).collect();
    assert_eq!(&lines[..200], want.as_slice());
    assert!(!lines[200].contains("ghp_"), "{}", lines[200]);
    assert!(lines[200].contains("<redacted>"), "{}", lines[200]);

    let run = e.db.lock().unwrap().get_run(h.run_id()).unwrap();
    let log = std::fs::read_to_string(&run.log_path).unwrap();
    assert!(
        run.log_path
            .starts_with(&*e.paths.logs_dir().to_string_lossy())
    );
    assert!(log.contains("O|line 1\nO|line 2\n"));
    assert!(log.contains("O|line 200\n"));
    assert!(log.contains(&format!("marker: {}", h.marker())));
    assert!(!log.contains("ghp_"));
}

#[test]
fn exit_code_propagates_as_low_8_bits() {
    let Some(e) = fake_env() else { return };
    let (h, rx) = e.fake("exit:7").unwrap();
    let (_, o) = collect(&rx, T);
    assert_eq!(
        o,
        Outcome {
            exit: Some(Exit::Low8(7)),
            stopped: false,
            user_forced: false
        }
    );
    assert_eq!(
        e.db.lock().unwrap().get_run(h.run_id()).unwrap().exit_code,
        Some(7)
    );

    let (_, rx) = e.fake("exit:3010").unwrap();
    let (_, o) = collect(&rx, T);
    assert_eq!(o.exit, Some(Exit::Low8(194)));
    assert!(o.exit.unwrap().matches(3010));
}

#[test]
fn orphan_holds_finished_until_wineserver_barrier_ends() {
    let Some(e) = fake_env() else { return };
    let t0 = Instant::now();
    let (h, rx) = e.fake("orphan:2").unwrap();
    let mut saw_waiting_row = false;
    let end = Instant::now() + T;
    let mut evs = Vec::new();
    let o = loop {
        assert!(Instant::now() < end, "timeout: {evs:?}");
        match rx.try_recv() {
            Ok(RunEvent::Finished(o)) => break o,
            Ok(ev) => {
                if ev == RunEvent::Phase(Phase::WaitingForChildren) {
                    let st = e.db.lock().unwrap().get_run(h.run_id()).unwrap().state;
                    saw_waiting_row = st == RunState::Waiting;
                }
                evs.push(ev)
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    assert!(
        t0.elapsed() >= Duration::from_millis(1800),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(
        phases(&evs),
        [Phase::Running, Phase::WaitingForChildren, Phase::Draining]
    );
    assert!(!evs.iter().any(|e| matches!(e, RunEvent::Uncertain(_))));
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    assert!(
        saw_waiting_row,
        "run row was not 'waiting' during the barrier"
    );
    assert!(scan_marker(h.marker()).is_empty());
    let run = e.db.lock().unwrap().get_run(h.run_id()).unwrap();
    assert_eq!(run.state, RunState::Finished);
    assert!(run.pid.is_some() && run.pid_start.is_some() && run.ended_at.is_some());
}

#[test]
fn stray_marker_process_makes_run_uncertain_then_finishes() {
    let Some(e) = fake_env() else { return };
    let (h, rx) = e.fake("stray:2").unwrap();
    let (evs, o) = collect(&rx, T);
    let left = evs
        .iter()
        .find_map(|e| match e {
            RunEvent::Uncertain(l) => Some(l.clone()),
            _ => None,
        })
        .expect("Uncertain");
    assert!(
        left.iter().any(|p| p.name.starts_with("sleep 2")),
        "{left:?}"
    );
    assert!(!o.stopped && !o.user_forced);
    assert!(scan_marker(h.marker()).is_empty());
}

#[test]
fn stop_with_sigterm_ignored_leaves_no_marker_process() {
    let Some(e) = fake_env() else { return };
    let (h, rx) = e.fake("trap").unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(!scan_marker(h.marker()).is_empty());
    let t = Instant::now();
    h.stop();
    assert!(
        t.elapsed() < Duration::from_millis(50),
        "stop() must not block"
    );
    let (_, o) = collect(&rx, T);
    assert!(o.stopped);
    assert_eq!(o.exit, Some(Exit::Signal(9)));
    assert!(scan_marker(h.marker()).is_empty());
    assert_eq!(
        e.db.lock().unwrap().get_run(h.run_id()).unwrap().state,
        RunState::Stopped
    );
}

#[test]
fn second_run_in_same_environment_is_busy() {
    let Some(e) = fake_env() else { return };
    let (h, rx) = e.fake("sleep:5").unwrap();
    assert!(matches!(e.fake("exit:0"), Err(Error::Busy(id)) if id == e.env_id));
    assert!(e.runs.active(e.env_id).is_some());
    h.stop();
    let (_, o) = collect(&rx, T);
    assert!(o.stopped);
    assert!(e.runs.active(e.env_id).is_none());
    let (_, rx) = e.fake("exit:0").unwrap();
    collect(&rx, T);
}

#[test]
fn mark_finished_then_stop_detached() {
    let Some(e) = fake_env() else { return };
    let (h, rx) = e.fake("sleep:30").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    h.mark_finished();
    let (_, o) = collect(&rx, T);
    assert_eq!(
        o,
        Outcome {
            exit: None,
            stopped: false,
            user_forced: true
        }
    );
    let run = e.db.lock().unwrap().get_run(h.run_id()).unwrap();
    assert!(run.user_forced && run.state == RunState::Finished);
    assert!(!scan_marker(h.marker()).is_empty(), "forced kills nothing");
    let left = e.runs.stop_detached(h.run_id()).unwrap();
    assert!(left.is_empty(), "{left:?}");
    assert!(scan_marker(h.marker()).is_empty());
}

/// F5: after a stop that leaves marker processes, the run stays Uncertain (env busy) and keeps
/// watching, without stopping again on its own, until they are gone.
#[test]
fn stop_with_survivors_stays_uncertain_until_they_are_gone() {
    let Some(e) = fake_env() else { return };
    let t0 = Instant::now();
    let (h, rx) = e.fake("respawn:5,sleep:30").unwrap();
    std::thread::sleep(Duration::from_millis(400));
    h.stop();
    let mut uncertain_at = None;
    let mut stops = 0;
    let end = Instant::now() + T;
    let o = loop {
        assert!(Instant::now() < end, "no Finished");
        match rx.try_recv() {
            Ok(RunEvent::Finished(o)) => break o,
            Ok(RunEvent::Uncertain(_)) if uncertain_at.is_none() => {
                uncertain_at = Some(t0.elapsed());
                assert!(
                    e.runs.active(e.env_id).is_some(),
                    "env unlocked while Uncertain"
                );
                assert!(matches!(e.fake("exit:0"), Err(Error::Busy(_))));
            }
            Ok(RunEvent::Line { text, .. }) if text == "vortex: stop requested" => stops += 1,
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    assert!(uncertain_at.is_some(), "stop should have left survivors");
    assert_eq!(stops, 1, "one Stop runs the sequence once");
    assert!(
        t0.elapsed() >= Duration::from_secs(4),
        "Finished at {:?} while the respawner (4-5 s) was alive",
        t0.elapsed()
    );
    assert!(o.stopped && !o.user_forced);
    assert!(scan_marker(h.marker()).is_empty());
    assert_eq!(
        e.db.lock().unwrap().get_run(h.run_id()).unwrap().state,
        RunState::Stopped
    );
}

/// Records a `running` row for a marked process this session does not supervise, like one left
/// by a previous Vortex session.
fn detached_row(e: &Env, kind: RunKind, child: &std::process::Child, marker: &str) -> i64 {
    let db = e.db.lock().unwrap();
    let id = db
        .insert_run(&vortex_core::db::NewRun {
            marker: marker.into(),
            kind,
            environment_id: e.env_id,
            game_id: None,
            install_id: None,
            argv: r#"["umu-run","/g/game.exe"]"#.into(),
            log_path: String::new(),
        })
        .unwrap();
    let pid = i64::from(child.id());
    db.set_run_pid(id, pid, vortex_core::db::proc_start_time(pid))
        .unwrap();
    id
}

fn marked(marker: &str, secs: &str) -> std::process::Child {
    std::process::Command::new("sleep")
        .arg(secs)
        .env("VORTEX_RUN_ID", marker)
        .spawn()
        .unwrap()
}

/// F1: a detached launch is listed, blocks the env, and finishes as `interrupted` (exit unknown)
/// once its processes are gone.
#[test]
fn detached_launch_is_watched_and_finished() {
    let Some(e) = fake_env() else { return };
    let marker = format!("det-launch-{}", std::process::id());
    let mut child = marked(&marker, "1");
    let id = detached_row(&e, RunKind::Launch, &child, &marker);
    let det = e.runs.detached().unwrap();
    assert_eq!(det.iter().map(|r| r.id).collect::<Vec<_>>(), [id]);
    assert!(e.runs.check_detached(id).unwrap());
    assert!(matches!(e.fake("exit:0"), Err(Error::Busy(_))));
    child.wait().unwrap();
    assert!(!e.runs.check_detached(id).unwrap());
    let run = e.db.lock().unwrap().get_run(id).unwrap();
    assert_eq!((run.state, run.exit_code), (RunState::Interrupted, None));
    assert!(e.runs.detached().unwrap().is_empty());
    let (_, rx) = e.fake("exit:0").unwrap();
    collect(&rx, T);
    // A run supervised in this session is never listed as detached.
    let (h, rx) = e.fake("sleep:1").unwrap();
    assert!(e.runs.detached().unwrap().is_empty());
    assert!(e.runs.check_detached(h.run_id()).unwrap());
    collect(&rx, T);
}

/// F1: a detached exec run is stoppable without a supervisor.
#[test]
fn detached_exec_is_stoppable() {
    let Some(e) = fake_env() else { return };
    let marker = format!("det-exec-{}", std::process::id());
    let mut child = marked(&marker, "30");
    let id = detached_row(&e, RunKind::Exec, &child, &marker);
    assert!(e.runs.stop_detached(id).unwrap().is_empty());
    assert!(scan_marker(&marker).is_empty());
    assert_eq!(
        e.db.lock().unwrap().get_run(id).unwrap().state,
        RunState::Stopped
    );
    child.wait().unwrap();
    assert!(e.runs.detached().unwrap().is_empty());
}

/// F2: a reservation makes `start` Busy until it is dropped, and cannot be taken while a run
/// (or another reservation) holds the environment.
#[test]
fn reservation_holds_the_environment() {
    let Some(e) = fake_env() else { return };
    let g = e.runs.reserve(e.env_id).unwrap();
    assert!(matches!(e.fake("exit:0"), Err(Error::Busy(id)) if id == e.env_id));
    assert!(matches!(e.runs.reserve(e.env_id), Err(Error::Busy(_))));
    assert!(
        e.runs.active(e.env_id).is_none(),
        "a reservation is not a run"
    );
    drop(g);
    let (h, rx) = e.fake("sleep:1").unwrap();
    assert!(matches!(e.runs.reserve(e.env_id), Err(Error::Busy(_))));
    collect(&rx, T);
    drop(h);
    drop(e.runs.reserve(e.env_id).unwrap());
    // A detached row blocks it too.
    let marker = format!("det-res-{}", std::process::id());
    let mut child = marked(&marker, "30");
    let id = detached_row(&e, RunKind::Launch, &child, &marker);
    assert!(matches!(e.runs.reserve(e.env_id), Err(Error::Busy(_))));
    assert!(e.runs.stop_detached(id).unwrap().is_empty());
    child.wait().unwrap();
}

/// F12: process names (cmdlines) are redacted before they reach events and notes.
#[test]
fn process_names_and_notes_are_redacted() {
    let Some(e) = fake_env() else { return };
    let inv = e.inv(
        e.setup_exe(),
        &["GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123"],
        &[("FAKE_UMU_MODE", "sleep:1")],
    );
    let (h, rx) = e.runs.start(e.spec(inv)).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let names = scan_marker(h.marker());
    assert!(
        names.iter().any(|p| p.name.contains("<redacted>")),
        "{names:?}"
    );
    let (evs, _) = collect(&rx, T);
    for ev in &evs {
        let text = match ev {
            RunEvent::Line { text, .. } => text.clone(),
            RunEvent::Processes(l) | RunEvent::Uncertain(l) => format!("{l:?}"),
            _ => continue,
        };
        assert!(!text.contains("ghp_"), "{text}");
    }
    assert!(names.iter().all(|p| !p.name.contains("ghp_")), "{names:?}");
}

// ---------------------------------------------------------------------------------------------
// Real umu-run + Proton. Run with:
//   VORTEX_IT_PROTON="$HOME/.local/share/Steam/steamapps/common/Proton - Experimental" \
//     cargo test -p vortex-core --test process -- --ignored --test-threads=1

fn it_env() -> Env {
    let proton = std::env::var("VORTEX_IT_PROTON")
        .expect("VORTEX_IT_PROTON must point at a Proton dir for --ignored tests");
    let tmp = tempfile::Builder::new()
        .prefix("vortex-m7-it-")
        .tempdir()
        .unwrap();
    let e = env_with(Tools::default(), Path::new(&proton), Timing::default(), tmp);
    let inv = Invocation {
        cwd: HostPath::new(e.prefix.as_path().parent().unwrap()).unwrap(),
        ..e.inv(Program::System(SystemExe::Wineboot), &["-u"], &[])
    };
    let (h, rx) = e.runs.start(e.spec_kind(inv, RunKind::PrefixInit)).unwrap();
    let (_, o) = collect(&rx, Duration::from_secs(600));
    assert_eq!(o.exit, Some(Exit::Low8(0)), "prefix init");
    assert!(e.prefix.as_path().join("system.reg").is_file());
    assert_no_markers(h.marker());
    e
}

impl Env {
    fn spec_kind(&self, inv: Invocation, kind: RunKind) -> RunSpec {
        RunSpec {
            kind,
            ..self.spec(inv)
        }
    }
    fn copy_fixture(&self, name: &str) {
        copy_dir(
            &Path::new(MANIFEST)
                .join("tests/fixtures/installers")
                .join(name),
            self.dir.as_path(),
        );
    }
    fn run_bat(&self, bat: &str) -> (RunHandle, async_channel::Receiver<RunEvent>) {
        let win = to_win(&self.prefix, &self.dir.join(bat).unwrap());
        let inv = self.inv(Program::System(SystemExe::Cmd), &["/c", win.as_str()], &[]);
        self.runs.start(self.spec(inv)).unwrap()
    }
    fn log(&self, h: &RunHandle) -> String {
        let run = self.db.lock().unwrap().get_run(h.run_id()).unwrap();
        std::fs::read_to_string(run.log_path).unwrap()
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for ent in std::fs::read_dir(from).unwrap() {
        let ent = ent.unwrap();
        let dst = to.join(ent.file_name());
        if ent.file_type().unwrap().is_dir() {
            copy_dir(&ent.path(), &dst);
        } else {
            std::fs::copy(ent.path(), dst).unwrap();
        }
    }
}

fn assert_no_markers(marker: &str) {
    let left = scan_marker(marker);
    assert!(left.is_empty(), "marker processes left behind: {left:?}");
}

fn build(tool: &str, args: &[&str], cwd: &Path) {
    let st = std::process::Command::new(tool)
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("{tool} not available ({e}): NOT RUN"));
    assert!(st.success(), "{tool} {args:?} failed");
}

const IT: Duration = Duration::from_secs(300);

#[test]
#[ignore = "needs real umu-run and VORTEX_IT_PROTON"]
fn it_nested_prereq_finishes_before_finished() {
    let e = it_env();
    e.copy_fixture("nested");
    let t0 = Instant::now();
    let (h, rx) = e.run_bat("setup.bat");
    let (evs, o) = collect(&rx, IT);
    let done = e.dir.as_path().join("Redist/prereq.done");
    eprintln!(
        "nested: {o:?} after {:?}; phases {:?}",
        t0.elapsed(),
        phases(&evs)
    );
    assert!(done.is_file(), "prereq.done missing at Finished");
    assert!(t0.elapsed() >= Duration::from_secs(13));
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    assert_no_markers(h.marker());
    eprintln!("nested log:\n{}", e.log(&h));
}

#[test]
#[ignore = "needs real umu-run and VORTEX_IT_PROTON"]
fn it_delayed_second_stage_finishes_before_finished() {
    let e = it_env();
    e.copy_fixture("delayed");
    let (h, rx) = e.run_bat("setup.bat");
    let (_, o) = collect(&rx, IT);
    assert!(
        e.dir.as_path().join("stage2.done").is_file(),
        "stage2.done missing"
    );
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    assert_no_markers(h.marker());
}

#[test]
#[ignore = "needs real umu-run and VORTEX_IT_PROTON"]
fn it_multi_file_installer_sees_its_dir_as_cwd() {
    let e = it_env();
    e.copy_fixture("multi");
    let (h, rx) = e.run_bat("setup.bat");
    let (_, o) = collect(&rx, IT);
    let res = std::fs::read_to_string(e.dir.as_path().join("result.txt")).unwrap();
    eprintln!("multi: {o:?}\n{res}");
    let cwd_line = res.lines().next().unwrap().trim_end();
    assert!(
        cwd_line.eq_ignore_ascii_case(&format!("cwd={}", to_win(&e.prefix, &e.dir))),
        "{cwd_line}"
    );
    assert!(res.contains("data1_rel=found"));
    assert!(res.contains("DATA1-CONTENT-OK"));
    assert!(res.contains("setup1_rel=found"));
    assert!(res.contains("prereq_rel_call=ok"));
    assert!(res.contains(&format!("marker={}", h.marker())));
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    assert_no_markers(h.marker());
    eprintln!("multi log:\n{}", e.log(&h));
}

#[test]
#[ignore = "needs real umu-run, VORTEX_IT_PROTON and mingw-w64-gcc"]
fn it_mingw_exe_exit_code_low_8_bits() {
    let e = it_env();
    let src = Path::new(MANIFEST).join("tests/fixtures/installers/src/exitcode.c");
    let exe = e.dir.as_path().join("exitcode.exe");
    build(
        "x86_64-w64-mingw32-gcc",
        &["-O2", "-o", &exe.to_string_lossy(), &src.to_string_lossy()],
        e.dir.as_path(),
    );
    for (arg, want) in [("7", 7u8), ("3010", 194), ("0", 0)] {
        let inv = e.inv(Program::Exe(HostPath::new(&exe).unwrap()), &[arg], &[]);
        let (h, rx) = e.runs.start(e.spec(inv)).unwrap();
        let (_, o) = collect(&rx, IT);
        eprintln!("exitcode {arg}: {o:?}");
        assert_eq!(o.exit, Some(Exit::Low8(want)), "exit {arg}");
        assert_no_markers(h.marker());
    }
}

#[test]
#[ignore = "needs real umu-run, VORTEX_IT_PROTON and msitools (wixl)"]
fn it_msi_via_msiexec_full_path() {
    let e = it_env();
    let src = Path::new(MANIFEST).join("tests/fixtures/installers/src");
    for f in ["vtest.wxs", "payload.txt"] {
        std::fs::copy(src.join(f), e.dir.as_path().join(f)).unwrap();
    }
    build(
        "wixl",
        &["-a", "x64", "-o", "vtest.msi", "vtest.wxs"],
        e.dir.as_path(),
    );
    let msi = to_win(&e.prefix, &e.dir.join("vtest.msi").unwrap());
    let inv = e.inv(
        Program::System(SystemExe::Msiexec),
        &["/i", msi.as_str(), "/qn"],
        &[],
    );
    let (h, rx) = e.runs.start(e.spec(inv)).unwrap();
    let (_, o) = collect(&rx, IT);
    eprintln!("msi: {o:?}");
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    let payload = e
        .prefix
        .drive_c()
        .join("Program Files/VortexSpike/payload.txt");
    assert!(
        std::fs::read_to_string(payload)
            .unwrap()
            .contains("VTEST-MSI-PAYLOAD")
    );
    assert_no_markers(h.marker());
    eprintln!("msi log:\n{}", e.log(&h));

    // A missing MSI must give a nonzero code: proves the code is not swallowed (bare `msiexec`
    // always gave 0 in M1).
    let missing = to_win(&e.prefix, &e.dir.join("missing.msi").unwrap());
    let inv = e.inv(
        Program::System(SystemExe::Msiexec),
        &["/i", missing.as_str(), "/qn"],
        &[],
    );
    let (h, rx) = e.runs.start(e.spec(inv)).unwrap();
    let (_, o) = collect(&rx, IT);
    eprintln!("missing msi: {o:?}");
    assert!(matches!(o.exit, Some(Exit::Low8(c)) if c != 0), "{o:?}");
    assert_no_markers(h.marker());
}

#[test]
#[ignore = "needs real umu-run and VORTEX_IT_PROTON"]
fn it_stop_mid_run_leaves_no_marker_process() {
    let e = it_env();
    e.copy_fixture("nested");
    std::fs::write(
        e.dir.as_path().join("Redist/prereq.bat"),
        "@echo off\r\nping -n 120 127.0.0.1 > nul\r\necho done> \"%~dp0prereq.done\"\r\n",
    )
    .unwrap();
    std::fs::write(
        e.dir.as_path().join("setup.bat"),
        "@echo off\r\nstart \"\" cmd /c Redist\\prereq.bat\r\nping -n 120 127.0.0.1 > nul\r\n",
    )
    .unwrap();
    let (h, rx) = e.run_bat("setup.bat");
    std::thread::sleep(Duration::from_secs(10));
    let before = scan_marker(h.marker());
    eprintln!(
        "stop: {} marker processes before stop, {} non-infra: {:?}",
        before.len(),
        before.iter().filter(|p| !p.infra).count(),
        before
            .iter()
            .filter(|p| !p.infra)
            .map(|p| &p.name)
            .collect::<Vec<_>>()
    );
    assert!(before.iter().any(
        |p| !p.infra && p.name.contains("PING") || p.name.to_ascii_lowercase().contains("ping")
    ));
    let t = Instant::now();
    h.stop();
    let (_, o) = collect(&rx, Duration::from_secs(60));
    eprintln!("stop: {o:?} after {:?}", t.elapsed());
    assert!(o.stopped);
    assert!(!e.dir.as_path().join("Redist/prereq.done").exists());
    assert_no_markers(h.marker());
    assert_eq!(
        e.db.lock().unwrap().get_run(h.run_id()).unwrap().state,
        RunState::Stopped
    );
}

#[test]
fn zombie_with_marker_is_not_listed() {
    let marker = format!("zombie-test-{}", std::process::id());
    let mut c = std::process::Command::new("true");
    c.env("VORTEX_RUN_ID", &marker);
    let child = c.spawn().unwrap();
    let pid = child.id();
    let is_zombie = || {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|s| s.rsplit(") ").next().is_some_and(|r| r.starts_with('Z')))
    };
    let end = Instant::now() + Duration::from_secs(5);
    while !is_zombie() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(is_zombie(), "child should be an unreaped zombie");
    assert!(scan_marker(&marker).is_empty());
    drop(child);
}
