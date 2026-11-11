//! M16: the whole lifecycle of a realistic click-through installer for a fictional free game
//! ("Lantern Hollow"), with real umu-run and Proton. Fixture sources and the build script are in
//! `tests/fixtures/wizard/` (built per run with x86_64-w64-mingw32-gcc and the host cc; no
//! binaries are committed). Every test is `#[ignore]` and needs `VORTEX_IT_PROTON`.
//!
//! ```sh
//! VORTEX_IT_PROTON="$HOME/.local/share/Steam/steamapps/common/Proton - Experimental" \
//!   cargo test -p vortex-core --test e2e_wizard -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! The Windows programs are driven by `LH_AUTOCLICK` (they press their own buttons) and log every
//! page shown and button pressed to `LH_TRACE`. Both must be in the environment umu-run inherits,
//! and the test process cannot set its own environment (`unsafe_code = "forbid"`), so each test
//! re-runs itself as a child test process with those variables set (the parent only waits).
//! Everything both create lives in one temp dir (`Paths::under`), removed by the parent.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};
use vortex_core::db::{Db, GameStatus, InstallState, RunKind};
use vortex_core::diagnose::Class;
use vortex_core::error::Error;
use vortex_core::install::{InstallEvent, InstallRequest, Installs, Started};
use vortex_core::paths::{HostPath, Paths, PrefixDir, to_win};
use vortex_core::process::{Exit, Outcome, RunEvent, Runs, scan_marker};
use vortex_core::runner::Tools;
use vortex_core::source::analyze;
use vortex_core::{delete, launch};

const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");
const LONG: Duration = Duration::from_secs(1800);
const GAME: &str = "Lantern Hollow";
const SETUP: &str = "setup_lantern_hollow.exe";
const PART2: &str = "setup_lantern_hollow-2.bin";
const KEY: &str = r"[Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\LanternHollow]";

fn t0() -> Instant {
    static T: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *T.get_or_init(Instant::now)
}

macro_rules! say {
    ($($a:tt)*) => { eprintln!("[{:>7.1}s] {}", t0().elapsed().as_secs_f64(), format!($($a)*)) };
}

fn host_to_z(p: &Path) -> String {
    format!("Z:{}", p.display()).replace('/', "\\")
}

/// In the parent: checks the tools, runs `name` as a child test with `LH_AUTOCLICK=mode` and
/// returns `None`. In the child: returns the shared root.
fn in_child(name: &str, mode: &str) -> Option<PathBuf> {
    if let Ok(root) = std::env::var("LH_E2E_ROOT") {
        assert_eq!(std::env::var("LH_AUTOCLICK").as_deref(), Ok(mode));
        return Some(PathBuf::from(root));
    }
    std::env::var("VORTEX_IT_PROTON")
        .expect("NOT RUN: VORTEX_IT_PROTON must point at a Proton dir for the wizard e2e tests");
    for tool in ["x86_64-w64-mingw32-gcc", "cc", "umu-run"] {
        let ok = std::process::Command::new("sh")
            .args(["-c", &format!("command -v {tool}")])
            .stdout(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "NOT RUN: {tool} is not installed");
    }
    let tmp = tempfile::Builder::new()
        .prefix("vortex-wizard-")
        .tempdir()
        .unwrap();
    let st = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            name,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("LH_E2E_ROOT", tmp.path())
        .env("LH_AUTOCLICK", mode)
        .env("LH_TRACE", host_to_z(&tmp.path().join("trace.txt")))
        .status()
        .unwrap();
    assert!(
        st.success(),
        "{name} failed in its child process (output above)"
    );
    assert!(
        marker_processes().is_empty(),
        "marker processes left: {:?}",
        marker_processes()
    );
    None
}

struct World {
    root: PathBuf,
    src: PathBuf,
    paths: Paths,
    db: Arc<Mutex<Db>>,
    runs: Arc<Runs>,
    inst: Installs,
    proton: HostPath,
}

impl World {
    fn new(root: PathBuf) -> World {
        let paths = Paths::under(&root);
        let db = Arc::new(Mutex::new(Db::open(&paths.db_file()).unwrap()));
        let runs = Arc::new(Runs::new(paths.clone(), Tools::default(), db.clone()));
        let inst = Installs::new(paths.clone(), db.clone(), runs.clone());
        let proton = HostPath::new(std::env::var("VORTEX_IT_PROTON").unwrap()).unwrap();
        say!(
            "root {}, runtime {}",
            root.display(),
            proton.as_path().display()
        );
        World {
            src: root.join("downloads"),
            root,
            paths,
            db,
            runs,
            inst,
            proton,
        }
    }

    fn db(&self) -> MutexGuard<'_, Db> {
        self.db.lock().unwrap()
    }

    /// Builds the download folder with `build.sh`.
    fn build(&self, name: &str) -> PathBuf {
        std::fs::create_dir_all(&self.src).unwrap();
        let out = self.src.join(name);
        let st =
            std::process::Command::new(Path::new(MANIFEST).join("tests/fixtures/wizard/build.sh"))
                .arg(&out)
                .status()
                .unwrap();
        assert!(st.success(), "build.sh failed");
        out
    }

    fn prefix_of(&self, game_id: i64) -> PrefixDir {
        let db = self.db();
        let env = db
            .get_environment(db.get_game(game_id).unwrap().environment_id)
            .unwrap();
        PrefixDir::new(&env.prefix_path, self.paths.home(), &[]).unwrap()
    }

    fn begin(&self, source: &Path) -> Started {
        let source = HostPath::new(source).unwrap();
        let a = analyze(&source).unwrap();
        self.inst
            .begin(InstallRequest {
                name: GAME.into(),
                source,
                installer: a.candidates[0].path.clone(),
                runtime: self.proton.clone(),
                install_dir: None,
                prefix: None,
            })
            .unwrap()
    }

    fn trace(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("trace.txt"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim_end().to_string())
            .collect()
    }

    fn no_wine_left(&self) {
        assert!(
            self.db().active_runs().unwrap().is_empty(),
            "runs still active"
        );
        assert!(
            marker_processes().is_empty(),
            "marker processes left: {:?}",
            marker_processes()
        );
    }
}

/// All events of one install attempt, and when its last state arrived.
fn drive(s: &Started) -> (Vec<InstallEvent>, SystemTime) {
    let end = Instant::now() + LONG;
    let mut evs = Vec::new();
    let mut last = SystemTime::now();
    loop {
        assert!(Instant::now() < end, "install did not end: {evs:?}");
        match s.events.try_recv() {
            Ok(ev) => {
                match &ev {
                    InstallEvent::State(st) => {
                        last = SystemTime::now();
                        say!("install: state {st:?}")
                    }
                    InstallEvent::Run {
                        event: RunEvent::Processes(p),
                        ..
                    } if !p.is_empty() => say!(
                        "install: processes {:?}",
                        p.iter()
                            .filter(|p| !p.infra)
                            .map(|p| p.name.chars().take(120).collect::<String>())
                            .collect::<Vec<_>>()
                    ),
                    InstallEvent::Run { kind, event }
                        if !matches!(event, RunEvent::Line { .. } | RunEvent::Processes(_)) =>
                    {
                        say!("install: {kind:?} {event:?}")
                    }
                    InstallEvent::Finding(f) => say!("install: finding {f:?}"),
                    InstallEvent::Error(e) => say!("install: error {e}"),
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

/// Waits for `Finished`; returns it and every non-infrastructure process name seen on the way.
fn finish(rx: &async_channel::Receiver<RunEvent>) -> (Outcome, Vec<String>) {
    let end = Instant::now() + LONG;
    let mut seen: Vec<String> = Vec::new();
    loop {
        assert!(Instant::now() < end, "run did not finish");
        match rx.try_recv() {
            Ok(RunEvent::Finished(o)) => return (o, seen),
            Ok(RunEvent::Processes(p)) => {
                for p in p.into_iter().filter(|p| !p.infra) {
                    if !seen.contains(&p.name) {
                        say!("run: process {}", p.name);
                        seen.push(p.name);
                    }
                }
            }
            Ok(RunEvent::Phase(p)) => say!("run: phase {p:?}"),
            Ok(_) => {}
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(async_channel::TryRecvError::Closed) => panic!("run channel closed"),
        }
    }
}

/// Hash of a tree: relative path, type, and file contents or symlink target; never follows links.
fn tree_hash(root: &Path) -> String {
    fn walk(dir: &Path, root: &Path, h: &mut Sha256, n: &mut usize) {
        let mut ents: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        ents.sort();
        for p in ents {
            *n += 1;
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let m = std::fs::symlink_metadata(&p).unwrap();
            h.update(rel.as_bytes());
            h.update([0]);
            if m.is_symlink() {
                h.update(
                    std::fs::read_link(&p)
                        .unwrap()
                        .into_os_string()
                        .into_encoded_bytes(),
                );
            } else if m.is_dir() {
                h.update(b"D");
                walk(&p, root, h, n);
            } else {
                h.update(Sha256::digest(std::fs::read(&p).unwrap()));
            }
        }
    }
    let (mut h, mut n) = (Sha256::new(), 0);
    walk(root, root, &mut h, &mut n);
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex} ({n} entries)")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (s, d) = (e.path(), to.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            copy_tree(&s, &d);
        } else {
            std::fs::copy(&s, &d).unwrap();
        }
    }
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

/// Asserts that `want` appear in `lines` in this order (other lines may come between).
fn in_order(lines: &[String], want: &[&str]) {
    let mut it = lines.iter();
    for w in want {
        assert!(
            it.any(|l| l.starts_with(w)),
            "trace lacks {w:?} (in order); trace:\n{}",
            lines.join("\n")
        );
    }
}

/// `%APPDATA%\LanternHollow` of the prefix's (only) Wine user.
fn appdata(prefix: &PrefixDir) -> PathBuf {
    let users = prefix.drive_c().join("users");
    for e in std::fs::read_dir(&users).unwrap().flatten() {
        let p = e.path().join("AppData/Roaming/LanternHollow");
        if p.is_dir() {
            return p;
        }
    }
    panic!("no AppData/Roaming/LanternHollow under {}", users.display())
}

fn kv(text: &str, key: &str) -> String {
    text.lines()
        .find_map(|l| l.trim_end().strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("{key} missing in {text:?}"))
        .to_string()
}

/// Whether the host has any audio output at all (so a game with no device is not Vortex's doing).
fn host_audio() -> Option<String> {
    let out = std::process::Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

// ---------------------------------------------------------------------------------------------

/// Scenarios a-d: detection, click-through install with a waited and a detached prerequisite,
/// launch, the game's own two-phase uninstaller, then Vortex's removal of the environment.
#[test]
#[ignore = "needs real umu-run, VORTEX_IT_PROTON, mingw-w64-gcc and a host cc"]
fn it_wizard_lifecycle() {
    let Some(root) = in_child("it_wizard_lifecycle", "1") else {
        return;
    };
    let w = World::new(root);
    let src = w.build("Lantern Hollow 1.0");
    let src_hash = tree_hash(&src);
    say!("source {} sha256 {src_hash}", src.display());

    // ---- a. source detection ----
    let a = analyze(&HostPath::new(&src).unwrap()).unwrap();
    say!(
        "analyze: {:?}, ambiguous={}",
        a.candidates
            .iter()
            .map(|c| (c.path.as_path().display().to_string(), c.score))
            .collect::<Vec<_>>(),
        a.ambiguous
    );
    assert!(a.candidates[0].path.as_path().ends_with(SETUP), "{a:?}");
    assert!(!a.ambiguous, "{a:?}");
    assert!(
        a.candidates
            .iter()
            .skip(1)
            .all(|c| c.score < a.candidates[0].score)
    );
    say!("a: reasons {:?}", a.candidates[0].reasons);

    // ---- b. install with every wizard page clicked through ----
    let s = w.begin(&src);
    let (evs, select_at) = drive(&s);
    let st = states(&evs);
    assert_eq!(st.last(), Some(&InstallState::SelectExe), "{evs:?}");
    assert!(
        st.starts_with(&[InstallState::PrefixInit, InstallState::Running]),
        "{st:?}"
    );
    assert_eq!(
        w.db().get_game(s.game_id).unwrap().status,
        GameStatus::Installing,
        "exit 0 alone must not mark installed"
    );
    let pfx = w.prefix_of(s.game_id);
    let trace = w.trace();
    say!("b: trace\n{}", trace.join("\n"));
    let src_win = to_win(&pfx, &HostPath::new(&src).unwrap());
    in_order(
        &trace,
        &[
            &format!("start cwd={src_win} autoclick=1"),
            "page welcome",
            "press welcome next",
            "page license",
            "license next_enabled_before_accept=0",
            "press license accept-on",
            "press license next",
            "page destination",
            "press destination install",
            "page installing",
            "prereq start /passive",
            "prereq passive done",
            "page finish",
            "press finish finish",
            "exit 0",
            "prereq finalize done",
        ],
    );
    // Started by the wizard while it installed (logged by the prereq, so no fixed order).
    in_order(&trace, &["prereq start /finalize", "prereq finalize done"]);
    // Next did nothing on the License page until the box was ticked.
    let accept = trace
        .iter()
        .position(|l| l == "press license accept-on")
        .unwrap();
    assert!(
        !trace[..accept].iter().any(|l| l == "press license next"),
        "Next worked before the license was accepted"
    );
    // The detached prerequisite outlived the wizard, and Vortex waited for it.
    let rt = pfx.drive_c().join("ProgramData/LanternHollow Runtime");
    assert!(rt.join("installed.txt").is_file());
    let fin = rt.join("finalize.txt");
    assert!(
        fin.is_file(),
        "SelectExe before the detached prerequisite finished"
    );
    let mtime = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
    assert!(mtime(&fin) <= select_at);
    say!(
        "b: finalize.txt written {:.2} s before SelectExe",
        select_at.duration_since(mtime(&fin)).unwrap().as_secs_f64()
    );
    // Installer audio: muted, so it saw no render device.
    let game_dir = pfx.drive_c().join("Games/Lantern Hollow");
    let ad = appdata(&pfx);
    let setup_audio = std::fs::read_to_string(ad.join("setup-audio.txt")).unwrap();
    say!("b: installer audio\n{setup_audio}");
    assert_eq!(kv(&setup_audio, "waveout_devices"), "0", "{setup_audio}");
    assert_eq!(kv(&setup_audio, "default_render"), "no", "{setup_audio}");
    // What the installer laid down.
    for f in [
        "LanternHollow.exe",
        "unins000.exe",
        "unins000.dat",
        "data/world.dat",
        "data/manifest.txt",
        "readme.txt",
        "manual/index.html",
        "manual/wiki/story.html",
        "manual/wiki/controls.html",
        "manual/wiki/lanterns.html",
        "manual/img/lantern.svg",
        "manual/img/map.svg",
    ] {
        assert!(game_dir.join(f).is_file(), "{f} not installed");
    }
    assert_eq!(
        std::fs::read(game_dir.join("manual/index.html")).unwrap(),
        std::fs::read(src.join("manual/index.html")).unwrap()
    );
    let sys = std::fs::read_to_string(pfx.as_path().join("system.reg")).unwrap();
    assert!(sys.contains(KEY), "Uninstall key not in system.reg");
    assert!(sys.contains(r#""InstallLocation"="C:\\Games\\Lantern Hollow""#));
    // Exe candidates and confirmation.
    let c = w.inst.exe_candidates(s.install_id).unwrap();
    say!(
        "b: exe candidates {:?}",
        c.iter().map(|c| c.win.to_string()).collect::<Vec<_>>()
    );
    assert!(
        c[0].path
            .as_path()
            .ends_with("Games/Lantern Hollow/LanternHollow.exe"),
        "{c:?}"
    );
    assert!(
        !c.iter()
            .any(|c| c.win.as_str().to_lowercase().contains("unins")),
        "{c:?}"
    );
    assert!(
        !c.iter()
            .any(|c| c.win.as_str().to_lowercase().contains("prereq")),
        "{c:?}"
    );
    w.inst.confirm_exe(s.install_id, &c[0].path, None).unwrap();
    assert_eq!(
        w.db().get_game(s.game_id).unwrap().status,
        GameStatus::Installed
    );
    assert_eq!(tree_hash(&src), src_hash, "the installer source changed");

    // ---- c. launch ----
    let (h, rx) = launch::launch_game(&w.runs, &w.db, &w.paths, s.game_id, false).unwrap();
    let (o, _) = finish(&rx);
    assert!(scan_marker(h.marker()).is_empty());
    assert_eq!(o.exit, Some(Exit::Low8(0)), "{o:?}");
    let last = std::fs::read_to_string(ad.join("last_run.txt")).unwrap();
    say!("c: last_run.txt\n{last}");
    assert_eq!(kv(&last, "cwd"), r"C:\Games\Lantern Hollow");
    assert_eq!(kv(&last, "verified"), "3/3");
    assert_eq!(kv(&last, "status"), "OK");
    let save = std::fs::read_to_string(ad.join("save.dat")).unwrap();
    assert_eq!(save.trim_end(), "LHSAVE1 lanterns_lit=1");
    let game_audio = std::fs::read_to_string(ad.join("game-audio.txt")).unwrap();
    say!("c: game audio\n{game_audio}");
    match host_audio() {
        Some(sinks) => {
            say!("c: host sinks: {sinks}");
            assert_eq!(
                kv(&game_audio, "default_render"),
                "yes",
                "the game lost its sound"
            );
            assert_ne!(
                kv(&game_audio, "waveout_devices"),
                "0",
                "the game lost its sound"
            );
        }
        None => {
            say!("c: game audio half NOT RUN: the host has no audio output (pactl lists no sink)")
        }
    }

    // ---- d. the game's own uninstaller (what the GUI's "Run the game's uninstaller" calls) ----
    let unins = HostPath::new(game_dir.join("unins000.exe")).unwrap();
    let env = w.db().get_game(s.game_id).unwrap().environment_id;
    let (h, rx) = launch::run_uninstaller(&w.runs, &w.db, &w.paths, env, &unins).unwrap();
    let (o, seen) = finish(&rx);
    say!("d: uninstaller {o:?}, processes seen {seen:?}");
    assert!(scan_marker(h.marker()).is_empty());
    assert_eq!(o.exit, Some(Exit::Low8(0)), "{o:?}");
    assert!(!o.stopped && !o.user_forced);
    // Finished only after the relaunched %TEMP% copy (phase 2) was done.
    let trace = w.trace();
    in_order(
        &trace,
        &[
            "unins start",
            "unins phase1 started",
            "unins start /SECONDPHASE",
            "unins phase2 dir=C:\\Games\\Lantern Hollow",
            "unins phase2 answer=yes",
            "unins phase2 removed",
            "unins phase2 exit",
        ],
    );
    assert!(
        seen.iter()
            .any(|n| n.to_lowercase().contains("_lh") && n.contains(".tmp")),
        "the %TEMP% copy was never listed: {seen:?}"
    );
    assert!(
        !game_dir.exists(),
        "install dir left: {}",
        game_dir.display()
    );
    let sys = std::fs::read_to_string(pfx.as_path().join("system.reg")).unwrap();
    assert!(!sys.contains(KEY), "Uninstall key still in system.reg");
    assert!(
        ad.join("save.dat").is_file(),
        "the uninstaller removed the save"
    );
    let run = w.db().get_run(h.run_id()).unwrap();
    assert_eq!((run.kind, run.game_id), (RunKind::Exec, None));
    // Vortex does not track what an uninstaller removes (docs/USER.md): the game stays in the
    // library as Installed, and Play then fails cleanly because the exe is gone.
    let g = w.db().get_game(s.game_id).unwrap();
    say!(
        "d: game after the uninstaller: status {:?}, exe {:?}",
        g.status,
        g.exe_path
    );
    assert_eq!(g.status, GameStatus::Installed);
    let e = launch::launch_game(&w.runs, &w.db, &w.paths, s.game_id, false).map(|_| ());
    say!("d: Play after uninstall: {e:?}");
    assert!(matches!(e, Err(Error::NotFound(_))), "{e:?}");

    // ---- removal of what is left, through the deletion plan ----
    let plan = delete::plan_game_removal(&w.db(), &w.paths, s.game_id).unwrap();
    say!(
        "deletion plan: {:?}",
        plan.items
            .iter()
            .map(|i| (i.path.as_path().display().to_string(), i.bytes))
            .collect::<Vec<_>>()
    );
    assert_eq!(plan.items.len(), 1, "{:?}", plan.items);
    assert_eq!(plan.items[0].path.as_path(), pfx.as_path());
    assert!(
        plan.items
            .iter()
            .all(|i| !i.path.as_path().starts_with(&w.src))
    );
    delete::execute(&w.db(), &w.paths, &plan).unwrap();
    assert!(!pfx.as_path().exists());
    assert!(w.db().get_game(s.game_id).is_err());
    assert_eq!(tree_hash(&src), src_hash, "the installer source changed");
    w.no_wine_left();
    say!("it_wizard_lifecycle: all assertions passed");
}

/// Scenario e: the second data part is missing from a copy of the download.
#[test]
#[ignore = "needs real umu-run, VORTEX_IT_PROTON, mingw-w64-gcc and a host cc"]
fn it_wizard_missing_part() {
    let Some(root) = in_child("it_wizard_missing_part", "1") else {
        return;
    };
    let w = World::new(root);
    let full = w.build("full");
    let src = w.src.join("Lantern Hollow (incomplete)");
    copy_tree(&full, &src);
    std::fs::remove_file(src.join(PART2)).unwrap();
    let src_hash = tree_hash(&src);
    let s = w.begin(&src);
    let (evs, _) = drive(&s);
    assert_eq!(states(&evs).last(), Some(&InstallState::Failed), "{evs:?}");
    let findings: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            InstallEvent::Finding(f) => Some(f.clone()),
            _ => None,
        })
        .collect();
    say!("e: findings {findings:?}");
    assert!(
        findings.iter().any(|f| f.class == Class::InstallerFailure),
        "{findings:?}"
    );
    let named = findings
        .iter()
        .any(|f| f.evidence.iter().any(|e| e.contains(PART2)));
    say!("e: a finding names {PART2}: {named}");
    let g = w.db().get_game(s.game_id).unwrap();
    assert_ne!(g.status, GameStatus::Installed);
    assert!(g.exe_path.is_none());
    let run = w
        .db()
        .runs_for_game(s.game_id)
        .unwrap()
        .into_iter()
        .find(|r| r.kind == RunKind::Install)
        .unwrap();
    assert_eq!(run.exit_code, Some(3));
    // The installer itself named the missing part in its message box.
    let trace = w.trace();
    say!("e: trace\n{}", trace.join("\n"));
    assert!(
        trace
            .iter()
            .any(|l| l.starts_with("error ") && l.contains(PART2)),
        "{trace:?}"
    );
    in_order(&trace, &["page installing", "error ", "exit 3"]);

    // M19 (F4): "Retry with verbose log" names the missing part from Proton's own log.
    let retry = Started {
        events: w.inst.retry(s.install_id, true).unwrap(),
        ..s
    };
    let (evs, _) = drive(&retry);
    assert_eq!(states(&evs).last(), Some(&InstallState::Failed), "{evs:?}");
    let named: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            InstallEvent::Finding(f) if f.class == Class::MissingInstallerData => Some(f),
            _ => None,
        })
        .collect();
    say!("e: verbose retry findings {named:?}");
    assert!(
        named.iter().any(|f| f.evidence[0].contains(PART2)),
        "{named:?}"
    );
    assert_ne!(
        w.db().get_game(s.game_id).unwrap().status,
        GameStatus::Installed
    );
    assert_eq!(tree_hash(&src), src_hash);
    w.no_wine_left();
}

/// Scenario f: Cancel on the License page.
#[test]
#[ignore = "needs real umu-run, VORTEX_IT_PROTON, mingw-w64-gcc and a host cc"]
fn it_wizard_cancel() {
    let Some(root) = in_child("it_wizard_cancel", "cancel") else {
        return;
    };
    let w = World::new(root);
    let src = w.build("Lantern Hollow 1.0");
    let s = w.begin(&src);
    let (evs, _) = drive(&s);
    assert_eq!(states(&evs).last(), Some(&InstallState::Failed), "{evs:?}");
    let g = w.db().get_game(s.game_id).unwrap();
    assert_ne!(g.status, GameStatus::Installed);
    assert!(g.exe_path.is_none());
    let run = w
        .db()
        .runs_for_game(s.game_id)
        .unwrap()
        .into_iter()
        .find(|r| r.kind == RunKind::Install)
        .unwrap();
    assert_eq!(run.exit_code, Some(2));
    let trace = w.trace();
    say!("f: trace\n{}", trace.join("\n"));
    in_order(
        &trace,
        &[
            "page welcome",
            "press welcome next",
            "page license",
            "press license cancel",
            "exit 2",
        ],
    );
    assert!(!trace.iter().any(|l| l == "page destination"), "{trace:?}");
    assert!(
        !w.prefix_of(s.game_id)
            .drive_c()
            .join("Games/Lantern Hollow")
            .exists()
    );
    w.no_wine_left();
}
