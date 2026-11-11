//! M14 end-to-end demonstration (spec 18/19) with the real umu-run, Proton and mingw/wixl-built
//! installers. Every test is `#[ignore]` and needs `VORTEX_IT_PROTON`; nothing here is faked.
//!
//! ```sh
//! VORTEX_IT_PROTON="$HOME/.local/share/Steam/steamapps/common/Proton - Experimental" \
//!   cargo test -p vortex-core --test e2e -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! - `VORTEX_IT_GE=<dir>` reuses a GE-Proton dir; otherwise the latest release is downloaded with
//!   `runtime_ge` into Vortex's runtimes dir (sha512 verified).
//! - `VORTEX_E2E_ROOT=<dir>` keeps everything in `<dir>` and uses `Paths::from_xdg()`, so the GUI
//!   started with the same `XDG_*` variables shows the result. `XDG_DATA_HOME` must lie inside
//!   `<dir>` (never the user's real data). Without it a temp dir and `Paths::under` are used.
//! - `VORTEX_E2E_BIN=<vortex binary>` is written into the desktop entries.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};
use vortex_core::db::{Db, GameStatus, InstallState, Run, RunKind};
use vortex_core::error::Error;
use vortex_core::install::{InstallEvent, InstallRequest, Installs, Started};
use vortex_core::paths::{HostPath, Paths, PrefixDir, to_win};
use vortex_core::process::{Exit, Outcome, Phase, RunEvent, Runs, scan_marker};
use vortex_core::runner::Tools;
use vortex_core::source::{SourceKind, analyze};
use vortex_core::{backup, delete, launch, runtime, runtime_ge, winetricks};

const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");
const CHILD: &str = "VORTEX_E2E_CHILD_SOURCE";
const LONG: Duration = Duration::from_secs(1800);

fn t0() -> Instant {
    static T: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *T.get_or_init(Instant::now)
}

macro_rules! say {
    ($($a:tt)*) => { eprintln!("[{:>7.1}s] {}", t0().elapsed().as_secs_f64(), format!($($a)*)) };
}

struct World {
    _tmp: Option<tempfile::TempDir>,
    /// Installer sources and build outputs (never inside Vortex's data dir).
    src: PathBuf,
    paths: Paths,
    db: Arc<Mutex<Db>>,
    runs: Arc<Runs>,
    inst: Installs,
    exp: HostPath,
}

impl World {
    fn new() -> World {
        let proton = std::env::var("VORTEX_IT_PROTON")
            .expect("VORTEX_IT_PROTON must point at a Proton dir for the e2e tests");
        let (tmp, root, paths) = match std::env::var("VORTEX_E2E_ROOT") {
            Ok(r) => {
                let data =
                    std::env::var("XDG_DATA_HOME").expect("VORTEX_E2E_ROOT needs XDG_DATA_HOME");
                assert!(
                    Path::new(&data).starts_with(&r),
                    "XDG_DATA_HOME={data} must be inside VORTEX_E2E_ROOT={r}"
                );
                (None, PathBuf::from(r), Paths::from_xdg().unwrap())
            }
            Err(_) => {
                let t = tempfile::Builder::new()
                    .prefix("vortex-e2e-")
                    .tempdir()
                    .unwrap();
                let root = t.path().to_path_buf();
                (Some(t), root.clone(), Paths::under(&root))
            }
        };
        let src = root.join("sources");
        std::fs::create_dir_all(&src).unwrap();
        let db = Arc::new(Mutex::new(Db::open(&paths.db_file()).unwrap()));
        let runs = Arc::new(Runs::new(paths.clone(), Tools::default(), db.clone()));
        let bin =
            std::env::var("VORTEX_E2E_BIN").unwrap_or_else(|_| "/usr/bin/vortex-launcher".into());
        let inst = Installs::new(paths.clone(), db.clone(), runs.clone())
            .with_desktop_entries(PathBuf::from(bin));
        say!("data dir {}", paths.data_dir().display());
        World {
            _tmp: tmp,
            src,
            paths,
            db,
            runs,
            inst,
            exp: HostPath::new(proton).unwrap(),
        }
    }

    fn db(&self) -> MutexGuard<'_, Db> {
        self.db.lock().unwrap()
    }

    fn prefix_of(&self, game_id: i64) -> PrefixDir {
        let db = self.db();
        let env = db
            .get_environment(db.get_game(game_id).unwrap().environment_id)
            .unwrap();
        PrefixDir::new(&env.prefix_path, self.paths.home(), &[]).unwrap()
    }

    /// Runs the install state machine to `SelectExe`, then confirms the top candidate (which
    /// must end with `exe_tail`). Returns the game id and every install event.
    fn install(
        &self,
        name: &str,
        source: &Path,
        runtime: &HostPath,
        exe_tail: &str,
    ) -> (i64, Vec<InstallEvent>, SystemTime) {
        let source = HostPath::new(source).unwrap();
        let a = analyze(&source).unwrap();
        say!(
            "{name}: analyze -> {:?}, ambiguous={}",
            a.candidates
                .iter()
                .map(|c| (c.path.as_path().display().to_string(), c.score))
                .collect::<Vec<_>>(),
            a.ambiguous
        );
        let s = self
            .inst
            .begin(InstallRequest {
                name: name.into(),
                source: source.clone(),
                installer: a.candidates[0].path.clone(),
                runtime: runtime.clone(),
                install_dir: None,
                prefix: None,
            })
            .unwrap();
        let (evs, select_at) = drive(&s, name);
        assert_eq!(
            states(&evs).last(),
            Some(&InstallState::SelectExe),
            "{name}: {evs:?}"
        );
        assert_eq!(
            self.db().get_game(s.game_id).unwrap().status,
            GameStatus::Installing,
            "exit 0 alone must not mark installed"
        );
        let c = self.inst.exe_candidates(s.install_id).unwrap();
        say!(
            "{name}: exe candidates {:?}",
            c.iter().map(|c| c.win.to_string()).collect::<Vec<_>>()
        );
        assert!(c[0].path.as_path().ends_with(exe_tail), "{c:?}");
        assert!(
            !c.iter().any(|c| c
                .path
                .as_path()
                .to_string_lossy()
                .to_lowercase()
                .contains("unins")),
            "{c:?}"
        );
        let r = self
            .inst
            .confirm_exe(s.install_id, &c[0].path, None)
            .unwrap();
        let entry = r.desktop_entry.expect("desktop entries enabled").unwrap();
        let text = std::fs::read_to_string(&entry).unwrap();
        assert!(
            entry.starts_with(self.paths.applications_dir()),
            "{entry:?}"
        );
        assert!(text.contains(&format!("--launch {}", s.game_id)), "{text}");
        say!(
            "{name}: installed as game {} (desktop entry {})",
            s.game_id,
            entry.display()
        );
        assert_eq!(
            self.db().get_game(s.game_id).unwrap().status,
            GameStatus::Installed
        );
        (s.game_id, evs, select_at)
    }

    fn configure(&self, id: i64, args: &[&str], env: &[(&str, &str)]) {
        let g = self.db().get_game(id).unwrap();
        let env: BTreeMap<_, _> = env.iter().copied().collect();
        self.db()
            .update_game_settings(
                id,
                &g.name,
                g.exe_path.as_deref(),
                g.work_dir.as_deref(),
                &serde_json::to_string(args).unwrap(),
                &serde_json::to_string(&env).unwrap(),
            )
            .unwrap();
    }

    /// Launches through `launch::launch_game` and waits for `Finished`.
    fn launch(&self, id: i64) -> (Outcome, Run) {
        let (h, rx) = launch::launch_game(&self.runs, &self.db, &self.paths, id, false).unwrap();
        let o = finish(&rx);
        assert!(
            scan_marker(h.marker()).is_empty(),
            "launch left marker processes"
        );
        let run = self.db().get_run(h.run_id()).unwrap();
        say!("launch game {id}: {o:?}, findings {:?}", run.findings);
        (o, run)
    }
}

/// All events of one install attempt, and when its last state arrived.
fn drive(s: &Started, name: &str) -> (Vec<InstallEvent>, SystemTime) {
    let end = Instant::now() + LONG;
    let mut evs = Vec::new();
    let mut last = SystemTime::now();
    loop {
        assert!(Instant::now() < end, "{name}: install did not end: {evs:?}");
        match s.events.try_recv() {
            Ok(ev) => {
                match &ev {
                    InstallEvent::State(st) => {
                        last = SystemTime::now();
                        say!("{name}: state {st:?}")
                    }
                    InstallEvent::Run { kind, event }
                        if !matches!(event, RunEvent::Line { .. } | RunEvent::Processes(_)) =>
                    {
                        say!("{name}: {kind:?} {event:?}")
                    }
                    InstallEvent::Run {
                        event: RunEvent::Processes(p),
                        ..
                    } if !p.is_empty() => {
                        say!(
                            "{name}: processes {:?}",
                            p.iter().map(|p| p.name.clone()).collect::<Vec<_>>()
                        )
                    }
                    InstallEvent::Finding(f) => say!("{name}: finding {f:?}"),
                    InstallEvent::Error(e) => say!("{name}: error {e}"),
                    _ => {}
                }
                evs.push(ev);
            }
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(async_channel::TryRecvError::Closed) => return (evs, last),
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

fn finish(rx: &async_channel::Receiver<RunEvent>) -> Outcome {
    let end = Instant::now() + LONG;
    loop {
        assert!(Instant::now() < end, "run did not finish");
        match rx.try_recv() {
            Ok(RunEvent::Finished(o)) => return o,
            Ok(_) => {}
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(async_channel::TryRecvError::Closed) => {
                panic!("run channel closed without Finished")
            }
        }
    }
}

fn mingw(src: &str, out: &Path, defines: &[&str]) {
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let st = std::process::Command::new("x86_64-w64-mingw32-gcc")
        .args(["-O2", "-static"])
        .args(defines)
        .arg("-o")
        .arg(out)
        .arg(Path::new(MANIFEST).join("tests/fixtures/e2e").join(src))
        .status()
        .unwrap_or_else(|e| panic!("x86_64-w64-mingw32-gcc not available ({e}): NOT RUN"));
    assert!(st.success(), "mingw build of {} failed", out.display());
}

/// The spec 5.2 directory: setup.exe + data1.bin, data2.bin, setup-1.bin + Redist\prereq.exe +
/// payload\Game.dat (the game exe, packed).
fn multi_source(dir: &Path, defines: &[&str]) {
    mingw("setup.c", &dir.join("setup.exe"), defines);
    mingw("prereq.c", &dir.join("Redist/prereq.exe"), &[]);
    mingw("game.c", &dir.join("payload/Game.dat"), &[]);
    std::fs::write(dir.join("data1.bin"), "DATA1-CONTENT-OK\r\n").unwrap();
    std::fs::write(dir.join("data2.bin"), vec![0xa5u8; 64 * 1024]).unwrap();
    std::fs::write(dir.join("setup-1.bin"), vec![0x5au8; 128 * 1024]).unwrap();
}

/// Hash of a tree: relative path, type, and file contents or symlink target; never follows links.
fn tree_hash(root: &Path) -> String {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut ents: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        ents.sort();
        for p in ents {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let m = std::fs::symlink_metadata(&p).unwrap();
            if m.is_symlink() {
                out.push((
                    format!("L {rel}"),
                    std::fs::read_link(&p)
                        .unwrap()
                        .into_os_string()
                        .into_encoded_bytes(),
                ));
            } else if m.is_dir() {
                out.push((format!("D {rel}"), vec![]));
                walk(&p, root, out);
            } else {
                out.push((
                    format!("F {rel}"),
                    Sha256::digest(std::fs::read(&p).unwrap()).to_vec(),
                ));
            }
        }
    }
    let mut v = Vec::new();
    walk(root, root, &mut v);
    let mut h = Sha256::new();
    for (k, d) in &v {
        h.update(k.as_bytes());
        h.update([0]);
        h.update(d);
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex} ({} entries)", v.len())
}

fn report(prefix: &PrefixDir, exe: &str) -> String {
    let p = prefix.drive_c().join(format!("vortex-report-{exe}.txt"));
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .replace("\r\n", "\n")
}

fn err<T>(r: vortex_core::error::Result<T>) -> Error {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}

fn ge_runtime(paths: &Paths) -> HostPath {
    if let Ok(d) = std::env::var("VORTEX_IT_GE") {
        return HostPath::new(d).unwrap();
    }
    if let Ok(rd) = std::fs::read_dir(paths.runtimes_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("GE-Proton")
                && runtime::validate(&p).is_ok()
            {
                say!("reusing {}", p.display());
                return HostPath::new(p).unwrap();
            }
        }
    }
    let rel = runtime_ge::latest_release().expect("latest GE-Proton release (network)");
    say!("downloading {} from {}", rel.tag, rel.asset_url);
    let (tx, rx) = async_channel::unbounded();
    let ge =
        runtime_ge::install(&rel, &paths.runtimes_dir(), &tx, &AtomicBool::new(false)).unwrap();
    drop(tx);
    let stages: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|p| !matches!(p, runtime_ge::Progress::Downloading { .. }))
        .collect();
    say!(
        "GE-Proton installed at {} (stages {stages:?})",
        ge.as_path().display()
    );
    runtime::validate(ge.as_path()).unwrap();
    ge
}

/// Every process on the machine that carries a `VORTEX_RUN_ID` (any run, any test).
fn marker_processes() -> Vec<String> {
    let mut v = Vec::new();
    for e in std::fs::read_dir("/proc").unwrap().flatten() {
        if let Ok(env) = std::fs::read(e.path().join("environ"))
            && env
                .split(|b| *b == 0)
                .any(|kv| kv.starts_with(b"VORTEX_RUN_ID="))
        {
            v.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    v
}

fn no_wine_left(w: &World) {
    assert!(
        w.db().active_runs().unwrap().is_empty(),
        "runs still active"
    );
    assert!(
        marker_processes().is_empty(),
        "marker processes left: {:?}",
        marker_processes()
    );
}

// ---------------------------------------------------------------------------------------------

/// Scenarios 1-5, 7 and 8 (launch failures): multi-file install to launch, standalone EXE on
/// GE-Proton, MSI, winetricks isolation, run another exe/msi, backup/restore, failed launches,
/// complete removal, persistence.
#[test]
#[ignore = "needs real umu-run, VORTEX_IT_PROTON, mingw-w64-gcc, wixl and GE-Proton (network or VORTEX_IT_GE)"]
fn it_e2e_install_to_launch() {
    let w = World::new();
    let ge = ge_runtime(&w.paths);

    // ---- build the installer sources ----
    let multi = w.src.join("E2E Multi Installer");
    multi_source(&multi, &[]);
    let standalone = w.src.join("standalone/StandaloneSetup.exe");
    mingw("game.c", &standalone, &[]);
    let build = w.src.join(".build");
    mingw("game.c", &build.join("MsiGame.exe"), &[]);
    std::fs::copy(
        Path::new(MANIFEST).join("tests/fixtures/e2e/game.wxs"),
        build.join("game.wxs"),
    )
    .unwrap();
    std::fs::create_dir_all(w.src.join("msi")).unwrap();
    let msi = w.src.join("msi/E2E MSI Game.msi");
    let st = std::process::Command::new("wixl")
        .args(["-a", "x64", "-o"])
        .arg(&msi)
        .arg("game.wxs")
        .current_dir(&build)
        .status()
        .unwrap_or_else(|e| panic!("wixl (msitools) not available ({e}): NOT RUN"));
    assert!(st.success());
    let tool = w.src.join("tools dir/E2E Tool.exe");
    mingw("game.c", &tool, &[]);
    std::fs::remove_dir_all(&build).unwrap();
    let sources = [
        multi.clone(),
        w.src.join("standalone"),
        w.src.join("msi"),
        w.src.join("tools dir"),
    ];
    let hashes: Vec<String> = sources.iter().map(|d| tree_hash(d)).collect();
    for (d, h) in sources.iter().zip(&hashes) {
        say!("source {} sha256 {h}", d.display());
    }

    // ---- 1. multi-file directory installer on Proton Experimental ----
    let a = analyze(&HostPath::new(&multi).unwrap()).unwrap();
    assert_eq!(a.kind, SourceKind::Dir);
    assert!(
        a.candidates[0].path.as_path().ends_with("setup.exe"),
        "{a:?}"
    );
    assert!(!a.ambiguous, "{a:?}");
    assert!(
        a.candidates
            .iter()
            .skip(1)
            .all(|c| c.score < a.candidates[0].score)
    );
    say!("multi: reasons {:?}", a.candidates[0].reasons);
    let (m, evs, select_at) = w.install(
        "E2E Multi Game",
        &multi,
        &w.exp,
        "Program Files/E2E Multi Game/Game.exe",
    );
    assert_eq!(
        states(&evs),
        [
            InstallState::PrefixInit,
            InstallState::Running,
            InstallState::Waiting,
            InstallState::SelectExe
        ]
    );
    let phases: Vec<Phase> = evs
        .iter()
        .filter_map(|e| match e {
            InstallEvent::Run {
                kind: RunKind::Install,
                event: RunEvent::Phase(p),
            } => Some(*p),
            _ => None,
        })
        .collect();
    say!("multi: install run phases {phases:?}");
    let pm = w.prefix_of(m);
    let done = pm.drive_c().join("vortex-prereq.done");
    // The prereq finished before Vortex left Waiting: SelectExe is the last state and the file
    // exists; prereq.exe sleeps 5 s after setup.exe exited.
    assert!(done.is_file(), "prereq did not finish before SelectExe");
    let mtime = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
    let setup_at = mtime(&pm.drive_c().join("vortex-setup.txt"));
    let done_at = mtime(&done);
    let secs = |a: SystemTime, b: SystemTime| {
        a.duration_since(b)
            .map_or_else(|e| -e.duration().as_secs_f64(), |d| d.as_secs_f64())
    };
    say!(
        "multi: prereq.done written {:.2} s after setup.exe started, {:.2} s before SelectExe",
        secs(done_at, setup_at),
        secs(select_at, done_at)
    );
    assert!(
        secs(done_at, setup_at) >= 4.5,
        "prereq did not outlive setup.exe"
    );
    assert!(
        secs(select_at, done_at) >= 0.0,
        "SelectExe before the prereq finished"
    );
    let setup = std::fs::read_to_string(pm.drive_c().join("vortex-setup.txt"))
        .unwrap()
        .replace("\r\n", "\n");
    say!("multi: setup.exe saw\n{setup}");
    let src_win = to_win(&pm, &HostPath::new(&multi).unwrap());
    assert!(
        setup.contains(&format!("cwd={src_win}\n")),
        "installer cwd: {setup}"
    );
    let game_dir = pm.drive_c().join("Program Files/E2E Multi Game");
    assert_eq!(
        std::fs::read(game_dir.join("data/data1.bin")).unwrap(),
        b"DATA1-CONTENT-OK\r\n"
    );

    // Launch with args (one with spaces) and an env var.
    w.configure(
        m,
        &["--mode", "two words", "$HOME"],
        &[("VORTEX_E2E_VAR", "hello e2e")],
    );
    let (o, run) = w.launch(m);
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    let rep = report(&pm, "Game.exe");
    say!("multi: Game.exe wrote\n{rep}");
    let exe_win = to_win(&pm, &HostPath::new(game_dir.join("Game.exe")).unwrap());
    let dir_win = to_win(&pm, &HostPath::new(&game_dir).unwrap());
    assert_eq!(
        rep,
        format!(
            "module={exe_win}\ncwd={dir_win}\narg=--mode\narg=two words\narg=$HOME\nenv=hello e2e\ndata1=DATA1-CONTENT-OK\n"
        )
    );
    let log = std::fs::read_to_string(&run.log_path).unwrap();
    assert!(log.contains("two words"), "launch log lacks the args");
    say!(
        "multi: launch log header:\n{}",
        log.lines()
            .take_while(|l| *l != "--")
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        log.contains(&format!("runtime: {}", w.exp.as_path().display())),
        "{log}"
    );
    assert!(
        log.contains(&format!("prefix: {}", pm.as_path().display())),
        "{log}"
    );
    assert_eq!(tree_hash(&multi), hashes[0], "the installer source changed");
    assert!(
        pm.as_path().join("system.reg").is_file(),
        "prefix did not persist"
    );

    // ---- 2. standalone EXE (GE-Proton, scenario 3) and MSI (Experimental) ----
    let (s, _, _) = w.install(
        "E2E Standalone",
        &standalone,
        &ge,
        "Program Files/E2E Standalone/Standalone.exe",
    );
    let ps = w.prefix_of(s);
    assert_ne!(ps.as_path(), pm.as_path());
    w.configure(s, &["standalone arg"], &[("VORTEX_E2E_VAR", "ge")]);
    let (o, run_s) = w.launch(s);
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    assert!(report(&ps, "Standalone.exe").contains("arg=standalone arg\nenv=ge\n"));
    let log_s = std::fs::read_to_string(&run_s.log_path).unwrap();
    assert!(
        log_s.contains(&format!("runtime: {}", ge.as_path().display())),
        "{log_s}"
    );
    assert!(log_s.contains(&format!("prefix: {}", ps.as_path().display())));
    say!(
        "standalone runs on {} in {}",
        ge.as_path().display(),
        ps.as_path().display()
    );

    let (i, _, _) = w.install(
        "E2E MSI Game",
        &msi,
        &w.exp,
        "Program Files/E2E MSI Game/MsiGame.exe",
    );
    let pi = w.prefix_of(i);
    let (o, _) = w.launch(i);
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    assert!(report(&pi, "MsiGame.exe").contains("env=(unset)\n"));

    // ---- 3b. winetricks win10 into the GE game; the other prefixes do not change ----
    let before_m = tree_hash(pm.as_path());
    let before_i = tree_hash(pi.as_path());
    let env_s = w.db().get_game(s).unwrap().environment_id;
    let known = winetricks::list_verbs(&Tools::default(), &ge, &w.paths).unwrap();
    say!("winetricks: {} verbs", known.len());
    let (h, rx) = winetricks::install_verb(&w.runs, &w.db, &w.paths, env_s, "win10", &known, false)
        .unwrap()
        .unwrap();
    let o = finish(&rx);
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    assert!(scan_marker(h.marker()).is_empty());
    assert!(winetricks::installed(&ps).unwrap().contains("win10"));
    let deps = w.db().list_dependencies(env_s).unwrap();
    say!("winetricks: dependencies of env {env_s}: {deps:?}");
    assert!(winetricks::installed(&pm).unwrap().is_empty());
    assert_eq!(
        tree_hash(pm.as_path()),
        before_m,
        "winetricks changed another game's prefix"
    );
    assert_eq!(
        tree_hash(pi.as_path()),
        before_i,
        "winetricks changed another game's prefix"
    );
    // Experimental cannot run winetricks: refused with a clear error, nothing runs.
    let env_m = w.db().get_game(m).unwrap().environment_id;
    let e = err(winetricks::install_verb(
        &w.runs, &w.db, &w.paths, env_m, "win10", &known, false,
    ));
    say!("winetricks on Experimental: {e}");
    assert!(matches!(e, Error::RuntimeInvalid { .. }));

    // ---- 4. run another exe and an msi in the multi game's prefix ----
    let (h, rx) = launch::run_in_env(
        &w.runs,
        &w.db,
        &w.paths,
        env_m,
        &HostPath::new(&tool).unwrap(),
        vec!["x y".into()],
        vec![("VORTEX_E2E_VAR".into(), "tool".into())],
    )
    .unwrap();
    assert_eq!(finish(&rx).exit, Some(Exit::Low8(0)));
    assert!(scan_marker(h.marker()).is_empty());
    let rep = report(&pm, "E2E Tool.exe");
    say!("tool in multi prefix wrote\n{rep}");
    let tool_dir = to_win(&pm, &HostPath::new(tool.parent().unwrap()).unwrap());
    assert!(
        rep.contains(&format!("cwd={tool_dir}\narg=x y\nenv=tool\n")),
        "{rep}"
    );
    let (h, rx) = launch::run_in_env(
        &w.runs,
        &w.db,
        &w.paths,
        env_m,
        &HostPath::new(&msi).unwrap(),
        vec!["/qn".into()],
        vec![],
    )
    .unwrap();
    assert_eq!(finish(&rx).exit, Some(Exit::Low8(0)));
    assert!(scan_marker(h.marker()).is_empty());
    assert!(
        pm.drive_c()
            .join("Program Files/E2E MSI Game/MsiGame.exe")
            .is_file()
    );

    // ---- 5. backup, damage, restore, launch again (and 8a: missing exe) ----
    let b = backup::backup(&w.db(), &w.paths, env_m, &AtomicBool::new(false)).unwrap();
    say!(
        "backup {} ({} bytes) at {}",
        b.id,
        b.size_bytes,
        b.archive_path
    );
    std::fs::remove_file(game_dir.join("Game.exe")).unwrap();
    let e = err(launch::launch_game(&w.runs, &w.db, &w.paths, m, false));
    say!("launch with the exe deleted: {e}");
    assert!(matches!(e, Error::NotFound(_)), "{e:?}");
    backup::restore(&w.db(), &w.paths, b.id, &pm, true).unwrap();
    assert!(game_dir.join("Game.exe").is_file());
    std::fs::remove_file(pm.drive_c().join("vortex-report-Game.exe.txt")).unwrap();
    let (o, _) = w.launch(m);
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    assert!(report(&pm, "Game.exe").contains("arg=two words\n"));

    // ---- 8. failed launches: nonzero exit, crash, broken runtime are distinguishable ----
    w.configure(m, &[], &[("VORTEX_E2E_MODE", "exit:42")]);
    let (o, run) = w.launch(m);
    assert_eq!(o.exit, Some(Exit::Low8(42)));
    let exit_findings = run
        .findings
        .clone()
        .expect("a failed launch must be diagnosed");
    assert!(
        exit_findings.contains("Unknown") && !exit_findings.contains("AppCrash"),
        "{exit_findings}"
    );
    w.configure(m, &[], &[("VORTEX_E2E_MODE", "crash")]);
    let (o, run) = w.launch(m);
    assert_ne!(o.exit, Some(Exit::Low8(0)));
    let crash_findings = run.findings.clone().expect("a crash must be diagnosed");
    let crash_log = std::fs::read_to_string(&run.log_path).unwrap();
    say!(
        "crash log excerpt: {:?}",
        crash_log
            .lines()
            .filter(|l| l.to_lowercase().contains("unhandled") || l.contains("page fault"))
            .collect::<Vec<_>>()
    );
    assert!(crash_findings.contains("AppCrash"), "{crash_findings}");
    w.configure(
        m,
        &["--mode", "two words", "$HOME"],
        &[("VORTEX_E2E_VAR", "hello e2e")],
    );
    let env_i = w.db().get_game(i).unwrap().environment_id;
    let bogus = w.src.join("no-such-proton");
    w.db()
        .set_environment_runtime(env_i, &bogus.to_string_lossy())
        .unwrap();
    let e = err(launch::launch_game(&w.runs, &w.db, &w.paths, i, false));
    say!("launch with a broken PROTONPATH: {e}");
    assert!(matches!(e, Error::RuntimeInvalid { .. }), "{e:?}");
    w.db()
        .set_environment_runtime(env_i, &w.exp.as_path().to_string_lossy())
        .unwrap();

    // ---- 7. remove the GE game completely ----
    let before_m = tree_hash(pm.as_path());
    let plan = delete::plan_game_removal(&w.db(), &w.paths, s).unwrap();
    say!(
        "deletion plan: {:?}",
        plan.items
            .iter()
            .map(|i| (i.path.as_path().display().to_string(), i.bytes))
            .collect::<Vec<_>>()
    );
    for it in &plan.items {
        assert!(
            !it.path.as_path().starts_with(&w.src),
            "plan touches a source: {it:?}"
        );
        assert!(
            !pm.as_path().starts_with(it.path.as_path())
                && !it.path.as_path().starts_with(pm.as_path())
        );
    }
    assert_eq!(plan.items.len(), 1, "{:?}", plan.items);
    assert!(
        plan.items
            .iter()
            .any(|it| it.path.as_path() == ps.as_path())
    );
    delete::execute(&w.db(), &w.paths, &plan).unwrap();
    assert!(!ps.as_path().exists());
    assert!(w.db().get_game(s).is_err());
    let entry = w.paths.applications_dir().join(format!(
        "io.github.nexusdeveloper902.Vortex.game-{s}.desktop"
    ));
    assert!(
        !entry.exists(),
        "removed game left its desktop entry {}",
        entry.display()
    );
    assert_eq!(
        tree_hash(pm.as_path()),
        before_m,
        "removing one game changed another's prefix"
    );
    for (d, h) in sources.iter().zip(&hashes) {
        assert_eq!(&tree_hash(d), h, "source {} changed", d.display());
    }
    let (o, _) = w.launch(m);
    assert_eq!(o.exit, Some(Exit::Low8(0)));

    // ---- persistence: a fresh database connection sees the same library ----
    no_wine_left(&w);
    let db2 = Db::open(&w.paths.db_file()).unwrap();
    let lib: Vec<_> = db2
        .list_games()
        .unwrap()
        .into_iter()
        .map(|g| (g.id, g.name, g.status))
        .collect();
    say!("library after reopen: {lib:?}");
    assert!(lib.contains(&(m, "E2E Multi Game".into(), GameStatus::Installed)));
    assert!(lib.contains(&(i, "E2E MSI Game".into(), GameStatus::Installed)));
    say!("it_e2e_install_to_launch: all assertions passed");
}

/// Scenario 8b: setup.exe needs data3.bin, which is absent.
#[test]
#[ignore = "needs real umu-run, VORTEX_IT_PROTON and mingw-w64-gcc"]
fn it_e2e_missing_installer_data() {
    let w = World::new();
    let dir = w.src.join("E2E Broken Installer");
    multi_source(&dir, &["-DNEED_DATA3"]);
    let source = HostPath::new(&dir).unwrap();
    let a = analyze(&source).unwrap();
    let s = w
        .inst
        .begin(InstallRequest {
            name: "E2E Broken Installer".into(),
            source,
            installer: a.candidates[0].path.clone(),
            runtime: w.exp.clone(),
            install_dir: None,
            prefix: None,
        })
        .unwrap();
    let (evs, _) = drive(&s, "broken");
    assert_eq!(states(&evs).last(), Some(&InstallState::Failed), "{evs:?}");
    let classes: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            InstallEvent::Finding(f) => Some(f.class),
            _ => None,
        })
        .collect();
    say!("broken: findings {classes:?}");
    assert!(
        classes.contains(&vortex_core::diagnose::Class::InstallerFailure),
        "{evs:?}"
    );
    let g = w.db().get_game(s.game_id).unwrap();
    assert_eq!(g.status, GameStatus::Failed);
    let run = w
        .db()
        .runs_for_game(s.game_id)
        .unwrap()
        .into_iter()
        .find(|r| r.kind == RunKind::Install)
        .unwrap();
    assert_eq!(run.exit_code, Some(3));
    let log = std::fs::read_to_string(&run.log_path).unwrap();
    say!(
        "broken: install log {} ({} lines), findings {:?}",
        run.log_path,
        log.lines().count(),
        run.findings
    );
    assert!(log.contains("kind: install"));
    assert!(run.findings.unwrap().contains("InstallerFailure"));
    no_wine_left(&w);
}

/// Child half of scenario 6: begins a real install and blocks until it is SIGKILLed.
#[test]
#[ignore = "child process of it_e2e_app_killed_mid_install"]
fn e2e_child_begins_install() {
    // A helper, not a test: it must be started by it_e2e_app_killed_mid_install.
    let src = std::env::var(CHILD)
        .expect("helper only: run via it_e2e_app_killed_mid_install, which sets CHILD");
    let w = World::new();
    let source = HostPath::new(&src).unwrap();
    let a = analyze(&source).unwrap();
    let _s = w
        .inst
        .begin(InstallRequest {
            name: "E2E Killed Install".into(),
            source,
            installer: a.candidates[0].path.clone(),
            runtime: w.exp.clone(),
            install_dir: None,
            prefix: None,
        })
        .unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Scenario 6: the "app" (a child test process) is SIGKILLed while its installer runs.
#[test]
#[ignore = "needs real umu-run, VORTEX_IT_PROTON and mingw-w64-gcc"]
fn it_e2e_app_killed_mid_install() {
    assert!(
        std::env::var("VORTEX_E2E_ROOT").is_ok(),
        "set VORTEX_E2E_ROOT (with XDG_DATA_HOME inside it) so the child shares the data dir"
    );
    let w = World::new();
    let dir = w.src.join("E2E Slow Installer");
    mingw("prereq.c", &dir.join("setup.exe"), &["-DSLEEP_MS=60000"]);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "e2e_child_begins_install",
            "--exact",
            "--ignored",
            "--test-threads=1",
        ])
        .env(CHILD, &dir)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + LONG;
    let run = loop {
        assert!(Instant::now() < end, "child install never started running");
        let r = w
            .db()
            .active_runs()
            .unwrap()
            .into_iter()
            .find(|r| r.kind == RunKind::Install && r.pid.is_some());
        if let Some(r) = r {
            break r;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    std::thread::sleep(Duration::from_secs(3));
    say!(
        "killing the app (pid {}) while install run {} is live",
        child.id(),
        run.id
    );
    child.kill().unwrap(); // SIGKILL
    child.wait().unwrap();
    assert!(
        !scan_marker(&run.marker).is_empty(),
        "umu-run should survive the app (S6)"
    );

    // "Next start": a fresh Installs on the same database.
    let w2 = World::new();
    let rec = w2.inst.recover().unwrap();
    say!("recovery: {rec:?}");
    let det = w2.inst.detached().unwrap();
    assert_eq!(det.len(), 1, "{det:?}");
    let (install, _) = &det[0];
    assert_ne!(
        w2.db().get_game(install.game_id).unwrap().status,
        GameStatus::Installed
    );
    let left = w2.inst.stop_detached(install.id).unwrap();
    assert!(left.is_empty(), "{left:?}");
    assert!(scan_marker(&run.marker).is_empty());
    let i = w2.db().get_install(install.id).unwrap();
    let g = w2.db().get_game(install.game_id).unwrap();
    say!(
        "after stop: install {:?}, game {:?}, exe {:?}",
        i.state,
        g.status,
        g.exe_path
    );
    assert_eq!(i.state, InstallState::Interrupted);
    assert_eq!(g.status, GameStatus::Interrupted);
    assert!(g.exe_path.is_none());
}
