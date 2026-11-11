//! M10: launch, run-in-env, winetricks. Default tests use `tests/fake-umu-run` and a fake
//! runtime; `#[ignore]` tests (`it_*`) use real umu-run (see the comments above them).

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use vortex_core::db::{Db, DepState, NewGame, RunKind};
use vortex_core::error::Error;
use vortex_core::launch::{launch_game, run_in_env, run_uninstaller};
use vortex_core::paths::{HostPath, Paths, PrefixDir};
use vortex_core::process::{Exit, Outcome, RunEvent, Runs, Timing, scan_marker};
use vortex_core::runner::{MUTE_AUDIO_OVERRIDES, Tools};
use vortex_core::winetricks;

const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");
const T: Duration = Duration::from_secs(20);

struct Shared {
    /// Fake Proton dir with `protonfixes/winetricks`.
    runtime: tempfile::TempDir,
    /// Fake Proton dir without protonfixes (like Proton Experimental).
    plain: tempfile::TempDir,
    /// umu-run stand-in for winetricks: `list-all` prints the fixture, a verb is appended to
    /// `$WINEPREFIX/winetricks.log`; exits 1 if it is already there (like real umu-run).
    wt: std::path::PathBuf,
    /// fake-umu-run that records to `$PWD/.fake-rec` (runs that take no extra env).
    rec: std::path::PathBuf,
}

/// Built once before anything is spawned (avoids ETXTBSY from forks racing a write).
fn shared() -> &'static Shared {
    static S: OnceLock<Shared> = OnceLock::new();
    S.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let exec = |p: &Path, body: &str| {
            std::fs::write(p, body).unwrap();
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let mk = |fixes: bool| {
            let d = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(d.path().join("files/bin")).unwrap();
            exec(&d.path().join("proton"), "#!/bin/sh\nexit 0\n");
            exec(
                &d.path().join("files/bin/wineserver"),
                "#!/bin/sh\nexit 0\n",
            );
            std::fs::write(d.path().join("toolmanifest.vdf"), "\"manifest\" {}\n").unwrap();
            if fixes {
                std::fs::create_dir_all(d.path().join("protonfixes")).unwrap();
                std::fs::write(d.path().join("protonfixes/winetricks"), "").unwrap();
            }
            d
        };
        let wt = std::env::temp_dir().join(format!("vortex-m10-wt-{}", std::process::id()));
        let fixture = Path::new(MANIFEST).join("tests/fixtures/winetricks/list-all.txt");
        exec(
            &wt,
            &format!(
                r#"#!/bin/sh
[ "$1" = winetricks ] || exit 9
if [ "$2" = list-all ]; then cat '{}'; exit 0; fi
if grep -qx "$2" "$WINEPREFIX/winetricks.log" 2>/dev/null; then exit 1; fi
echo "$2" >> "$WINEPREFIX/winetricks.log"
"#,
                fixture.display()
            ),
        );
        let rec = std::env::temp_dir().join(format!("vortex-m16-rec-{}", std::process::id()));
        exec(
            &rec,
            &format!(
                "#!/bin/sh\nFAKE_UMU_OUT=\"$PWD/.fake-rec\" exec '{MANIFEST}/tests/fake-umu-run' \"$@\"\n"
            ),
        );
        Shared {
            runtime: mk(true),
            plain: mk(false),
            wt,
            rec,
        }
    })
}

struct Env {
    _tmp: tempfile::TempDir,
    paths: Paths,
    db: Arc<Mutex<Db>>,
    runs: Runs,
    env_id: i64,
    prefix: PrefixDir,
    runtime: HostPath,
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

fn env_with(tools: Tools, runtime: &Path, timing: Timing) -> Env {
    let tmp = tempfile::tempdir().unwrap();
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
    let dir = HostPath::new(tmp.path().join("game dir")).unwrap();
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

fn fake_tools() -> Tools {
    Tools {
        umu_run: Path::new(MANIFEST).join("tests/fake-umu-run"),
    }
}

/// `None` as root (CI): verifies that launching refuses to spawn, then the test ends.
fn fake_env() -> Option<Env> {
    let e = env_with(fake_tools(), shared().runtime.path(), fast());
    std::fs::write(e.dir.as_path().join("game.exe"), "MZ").unwrap();
    if rustix::process::geteuid().is_root() {
        let id = e.game(&[], &[], None);
        match launch_game(&e.runs, &e.db, &e.paths, id) {
            Err(Error::Validation(m)) if m.contains("root") => {}
            other => panic!("as root, launch must refuse: {:?}", other.map(|_| ())),
        }
        eprintln!("running as root: verified the spawn refusal only");
        return None;
    }
    Some(e)
}

impl Env {
    fn game(&self, args: &[&str], env: &[(&str, &str)], work_dir: Option<&Path>) -> i64 {
        self.game_exe("game.exe", args, env, work_dir)
    }
    fn game_exe(
        &self,
        exe: &str,
        args: &[&str],
        env: &[(&str, &str)],
        work_dir: Option<&Path>,
    ) -> i64 {
        let mut g = NewGame::new("Test Game", self.env_id);
        g.exe_path = Some(self.dir.as_path().join(exe).to_string_lossy().into_owned());
        g.work_dir = work_dir.map(|p| p.to_string_lossy().into_owned());
        g.launch_args = serde_json::to_string(args).unwrap();
        g.env_vars = serde_json::to_string(
            &env.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<std::collections::BTreeMap<_, _>>(),
        )
        .unwrap();
        self.db.lock().unwrap().insert_game(&g).unwrap()
    }
}

fn collect(rx: &async_channel::Receiver<RunEvent>, limit: Duration) -> Outcome {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        match rx.try_recv() {
            Ok(RunEvent::Finished(o)) => return o,
            Ok(_) => {}
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(e) => panic!("channel closed before Finished: {e}"),
        }
    }
    panic!("no Finished within {limit:?}");
}

#[test]
fn launch_passes_args_env_and_exe_dir_cwd() {
    let Some(e) = fake_env() else { return };
    let out = e.dir.as_path().join("rec.out");
    let id = e.game(
        &["a b", "$(touch /tmp/x)", "--q"],
        &[("FAKE_UMU_OUT", &out.to_string_lossy()), ("DXVK_HUD", "1")],
        None,
    );
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, id).unwrap();
    assert_eq!(collect(&rx, T).exit, Some(Exit::Low8(0)));
    let rec = std::fs::read_to_string(&out).unwrap();
    let exe = e.dir.as_path().join("game.exe");
    assert_eq!(
        rec,
        format!(
            "cwd={}\narg={}\narg=a b\narg=$(touch /tmp/x)\narg=--q\n",
            e.dir.as_path().display(),
            exe.display()
        )
    );
    let env = std::fs::read_to_string(format!("{}.env", out.display())).unwrap();
    assert!(env.lines().any(|l| l == "DXVK_HUD=1"));
    assert!(
        env.lines()
            .any(|l| l == format!("WINEPREFIX={}", e.prefix.as_path().display()))
    );
    assert!(
        !env.contains(MUTE_AUDIO_OVERRIDES),
        "games keep their sound"
    );
    // Last launch info.
    let db = e.db.lock().unwrap();
    let last = db.last_launch(id).unwrap().unwrap();
    assert_eq!(
        (last.id, last.kind, last.game_id),
        (h.run_id(), RunKind::Launch, Some(id))
    );
    assert!(last.argv.contains("a b"));
}

#[test]
fn finished_launch_is_diagnosed_before_finished() {
    use vortex_core::diagnose::{Class, from_json};
    let Some(e) = fake_env() else { return };
    let ok = e.game(&[], &[("FAKE_UMU_MODE", "exit:0")], None);
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, ok).unwrap();
    assert_eq!(collect(&rx, T).exit, Some(Exit::Low8(0)));
    let f = e.db.lock().unwrap().get_run(h.run_id()).unwrap().findings;
    assert_eq!(f.as_deref(), Some("[]"));

    let bad = e.game(&[], &[("FAKE_UMU_MODE", "exit:42")], None);
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, bad).unwrap();
    assert_eq!(collect(&rx, T).exit, Some(Exit::Low8(42)));
    // Stored before Finished was forwarded.
    let f = e.db.lock().unwrap().get_run(h.run_id()).unwrap().findings;
    let f = from_json(&f.expect("findings stored"));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].class, Class::Unknown);
    assert!(f[0].evidence[0].contains("42"), "{f:?}");
}

#[test]
fn configured_work_dir_wins() {
    let Some(e) = fake_env() else { return };
    let out = e.dir.as_path().join("rec.out");
    let wd = e.dir.as_path().parent().unwrap().to_path_buf();
    let id = e.game(&[], &[("FAKE_UMU_OUT", &out.to_string_lossy())], Some(&wd));
    let (_, rx) = launch_game(&e.runs, &e.db, &e.paths, id).unwrap();
    collect(&rx, T);
    let rec = std::fs::read_to_string(&out).unwrap();
    assert!(rec.starts_with(&format!("cwd={}\n", wd.display())), "{rec}");
}

#[test]
fn missing_exe_is_not_found_and_nothing_spawned() {
    let Some(e) = fake_env() else { return };
    let id = e.game_exe("gone.exe", &[], &[], None);
    let err = launch_game(&e.runs, &e.db, &e.paths, id)
        .map(|_| ())
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
    assert!(e.db.lock().unwrap().runs_for_game(id).unwrap().is_empty());
}

#[test]
fn bad_config_is_rejected() {
    let Some(e) = fake_env() else { return };
    let id = e.game(&[], &[("WINEPREFIX", "/x")], None);
    let r = launch_game(&e.runs, &e.db, &e.paths, id).map(|_| ());
    assert!(matches!(r, Err(Error::Validation(_))));
    let id = e.game(&[], &[], None);
    e.db.lock()
        .unwrap()
        .update_game_settings(
            id,
            "g",
            e.dir.as_path().join("game.exe").to_str(),
            None,
            "a b",
            "{}",
        )
        .unwrap();
    let r = launch_game(&e.runs, &e.db, &e.paths, id).map(|_| ());
    assert!(
        matches!(r, Err(Error::Validation(_))),
        "string args refused"
    );
}

#[test]
fn second_launch_is_busy_and_stop_finishes() {
    let Some(e) = fake_env() else { return };
    let id = e.game(&[], &[("FAKE_UMU_MODE", "sleep:30")], None);
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, id).unwrap();
    let r = launch_game(&e.runs, &e.db, &e.paths, id).map(|_| ());
    assert!(matches!(r, Err(Error::Busy(b)) if b == e.env_id));
    let r = run_in_env(
        &e.runs,
        &e.db,
        &e.paths,
        e.env_id,
        &e.dir.join("game.exe").unwrap(),
        vec![],
        vec![],
    )
    .map(|_| ());
    assert!(matches!(r, Err(Error::Busy(_))), "one active run per env");
    assert!(e.runs.active(e.env_id).is_some());
    h.stop();
    let o = collect(&rx, T);
    assert!(o.stopped);
    assert!(scan_marker(h.marker()).is_empty());
    assert!(
        e.runs.active(e.env_id).is_none()
            || e.runs
                .active(e.env_id)
                .is_some_and(|a| a.run_id() != h.run_id())
    );
}

#[test]
fn run_in_env_exe_and_msi() {
    let Some(e) = fake_env() else { return };
    let out = e.dir.as_path().join("rec.out");
    let envv = vec![(
        "FAKE_UMU_OUT".to_string(),
        out.to_string_lossy().into_owned(),
    )];
    let (_, rx) = run_in_env(
        &e.runs,
        &e.db,
        &e.paths,
        e.env_id,
        &e.dir.join("game.exe").unwrap(),
        vec!["/S".into()],
        envv.clone(),
    )
    .unwrap();
    collect(&rx, T);
    let rec = std::fs::read_to_string(&out).unwrap();
    assert!(
        rec.ends_with(&format!(
            "arg={}/game.exe\narg=/S\n",
            e.dir.as_path().display()
        )),
        "{rec}"
    );
    let env = std::fs::read_to_string(format!("{}.env", out.display())).unwrap();
    assert!(!env.contains(MUTE_AUDIO_OVERRIDES), "Run EXE is not muted");

    std::fs::write(e.dir.as_path().join("Setup.MSI"), "x").unwrap();
    let (h, rx) = run_in_env(
        &e.runs,
        &e.db,
        &e.paths,
        e.env_id,
        &e.dir.join("Setup.MSI").unwrap(),
        vec!["/qn".into()],
        envv,
    )
    .unwrap();
    collect(&rx, T);
    let rec = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<_> = rec.lines().collect();
    assert_eq!(lines[1], r"arg=C:\windows\system32\msiexec.exe");
    assert_eq!(lines[2], "arg=/i");
    assert!(
        lines[3].starts_with("arg=Z:\\") && lines[3].ends_with("Setup.MSI"),
        "{}",
        lines[3]
    );
    assert_eq!(lines[4], "arg=/qn");
    assert_eq!(
        e.db.lock().unwrap().get_run(h.run_id()).unwrap().kind,
        RunKind::Exec
    );

    std::fs::write(e.dir.as_path().join("evil.sh"), "").unwrap();
    let r = run_in_env(
        &e.runs,
        &e.db,
        &e.paths,
        e.env_id,
        &e.dir.join("evil.sh").unwrap(),
        vec![],
        vec![],
    )
    .map(|_| ());
    assert!(matches!(r, Err(Error::Validation(_))));
}

#[test]
fn uninstaller_runs_muted_outside_its_folder() {
    if rustix::process::geteuid().is_root() {
        return; // spawning is refused as root (see fake_env)
    }
    let tools = Tools {
        umu_run: shared().rec.clone(),
    };
    let e = env_with(tools, shared().runtime.path(), fast());
    let exe = e.dir.join("unins000.exe").unwrap();
    std::fs::write(exe.as_path(), "MZ").unwrap();
    // No drive_c yet: refused before anything runs.
    let r = run_uninstaller(&e.runs, &e.db, &e.paths, e.env_id, &exe).map(|_| ());
    assert!(matches!(r, Err(Error::NotFound(_))), "{r:?}");
    let c = e.prefix.drive_c();
    std::fs::create_dir_all(&c).unwrap();
    let (h, rx) = run_uninstaller(&e.runs, &e.db, &e.paths, e.env_id, &exe).unwrap();
    assert_eq!(collect(&rx, T).exit, Some(Exit::Low8(0)));
    // The cwd is the prefix's C:\, never the folder the uninstaller deletes.
    let rec = std::fs::read_to_string(c.join(".fake-rec")).unwrap();
    let dir = e.dir.as_path().display();
    assert_eq!(
        rec,
        format!("cwd={}\narg={dir}/unins000.exe\n", c.display())
    );
    let env = std::fs::read_to_string(c.join(".fake-rec.env")).unwrap();
    let overrides = env
        .lines()
        .find_map(|l| l.strip_prefix("WINEDLLOVERRIDES="))
        .unwrap_or_default();
    assert!(overrides.ends_with(MUTE_AUDIO_OVERRIDES), "{overrides:?}");
    assert_eq!(
        e.db.lock().unwrap().get_run(h.run_id()).unwrap().kind,
        RunKind::Exec
    );
    let txt = e.dir.join("readme.txt").unwrap();
    let r = run_uninstaller(&e.runs, &e.db, &e.paths, e.env_id, &txt).map(|_| ());
    assert!(matches!(r, Err(Error::Validation(_))));
}

// ---- winetricks ----

fn fixture_verbs() -> Vec<winetricks::Verb> {
    let text =
        std::fs::read_to_string(Path::new(MANIFEST).join("tests/fixtures/winetricks/list-all.txt"))
            .unwrap();
    winetricks::parse_list_all(&text)
}

#[test]
fn list_all_fixture_parses_with_sections() {
    let v = fixture_verbs();
    let find = |n: &str| {
        v.iter()
            .find(|x| x.name == n)
            .unwrap_or_else(|| panic!("{n}"))
    };
    assert_eq!(find("vcrun2019").section, "dlls");
    assert_eq!(find("corefonts").section, "fonts");
    assert_eq!(find("7zip").section, "apps");
    assert!(find("win10").description.contains("Windows 10"));
    assert_eq!(v.len(), 7);
}

#[test]
fn verb_validation() {
    let v = fixture_verbs();
    for ok in ["vcrun2019", "win10", "vd=1280x720"] {
        assert_eq!(winetricks::validate_verb(ok, &v).unwrap(), ok);
    }
    for bad in [
        "foo;rm",
        "/tmp/evil.verb",
        "evil.verb",
        "../x",
        "",
        "vcrun2019 corefonts",
        "nosuchverb",
        "vcrun2019;ls",
        "$(id)",
        "vd=1;2",
    ] {
        assert!(
            winetricks::validate_verb(bad, &v).is_err(),
            "{bad:?} must be rejected"
        );
    }
}

#[test]
fn installed_from_log_and_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path());
    let prefix = PrefixDir::new(tmp.path().join("p"), paths.home(), &[]).unwrap();
    std::fs::create_dir_all(prefix.as_path()).unwrap();
    assert!(winetricks::installed(&prefix).unwrap().is_empty());
    std::fs::write(
        prefix.as_path().join("winetricks.log"),
        "vcrun2019\nwin10\n",
    )
    .unwrap();
    let inst = winetricks::installed(&prefix).unwrap();
    assert!(inst.contains("vcrun2019"));
    let todo = winetricks::filter_installed(&["vcrun2019".into(), "corefonts".into()], &inst);
    assert_eq!(todo, ["corefonts"]);
}

#[test]
fn experimental_style_runtime_is_refused_clearly() {
    let Some(e) = fake_env() else { return };
    let plain = HostPath::new(shared().plain.path()).unwrap();
    let err = winetricks::list_verbs(&fake_tools(), &plain, &e.paths).unwrap_err();
    assert!(matches!(err, Error::RuntimeInvalid { .. }));
    assert!(err.to_string().contains("GE-Proton or UMU-Proton"), "{err}");
    let e2 = env_with(fake_tools(), shared().plain.path(), fast());
    let err = winetricks::install_verb(
        &e2.runs,
        &e2.db,
        &e2.paths,
        e2.env_id,
        "win10",
        &fixture_verbs(),
    )
    .map(|_| ())
    .unwrap_err();
    assert!(err.to_string().contains("GE-Proton or UMU-Proton"), "{err}");
}

#[test]
fn list_install_records_dependency_and_skips_installed() {
    let Some(_) = fake_env() else { return };
    let tools = Tools {
        umu_run: shared().wt.clone(),
    };
    let e = env_with(tools.clone(), shared().runtime.path(), fast());
    let known = winetricks::list_verbs(&tools, &e.runtime, &e.paths).unwrap();
    assert_eq!(known.len(), 7);
    let (h, rx) = winetricks::install_verb(&e.runs, &e.db, &e.paths, e.env_id, "vcrun2019", &known)
        .unwrap()
        .unwrap();
    assert_eq!(collect(&rx, T).exit, Some(Exit::Low8(0)));
    let deps = e.db.lock().unwrap().list_dependencies(e.env_id).unwrap();
    assert_eq!(
        (deps[0].verb.as_str(), deps[0].state, deps[0].run_id),
        ("vcrun2019", DepState::Installed, Some(h.run_id()))
    );
    assert_eq!(
        e.db.lock().unwrap().get_run(h.run_id()).unwrap().kind,
        RunKind::Winetricks
    );
    assert!(
        winetricks::installed(&e.prefix)
            .unwrap()
            .contains("vcrun2019")
    );
    // Already installed: nothing runs.
    assert!(
        winetricks::install_verb(&e.runs, &e.db, &e.paths, e.env_id, "vcrun2019", &known)
            .unwrap()
            .is_none()
    );
    // Invalid verbs never spawn.
    assert!(
        winetricks::install_verb(&e.runs, &e.db, &e.paths, e.env_id, "foo;rm", &known).is_err()
    );
    assert_eq!(e.db.lock().unwrap().active_runs().unwrap().len(), 0);
}

#[test]
fn failed_verb_is_recorded_failed() {
    let Some(_) = fake_env() else { return };
    // fake-umu-run exits 0 but never writes winetricks.log: not confirmed, so `failed`.
    let e = env_with(fake_tools(), shared().runtime.path(), fast());
    let (_, rx) = winetricks::install_verb(
        &e.runs,
        &e.db,
        &e.paths,
        e.env_id,
        "win10",
        &fixture_verbs(),
    )
    .unwrap()
    .unwrap();
    collect(&rx, T);
    let deps = e.db.lock().unwrap().list_dependencies(e.env_id).unwrap();
    assert_eq!(deps[0].state, DepState::Failed);
}

// ---------------------------------------------------------------------------------------------
// Real umu-run. Run with:
//   VORTEX_IT_PROTON="$HOME/.local/share/Steam/steamapps/common/Proton - Experimental" \
//     cargo test -p vortex-core --test launch -- --ignored --test-threads=1 --nocapture
// `it_winetricks_*` downloads GE-Proton (or uses VORTEX_IT_GE=<dir>) into a throwaway dir.

fn init_prefix(e: &Env) {
    use vortex_core::process::RunSpec;
    use vortex_core::runner::{Invocation, Program, SystemExe};
    let inv = Invocation {
        program: Program::System(SystemExe::Wineboot),
        args: vec!["-u".into()],
        cwd: HostPath::new(e.prefix.as_path().parent().unwrap()).unwrap(),
        prefix: e.prefix.clone(),
        runtime: e.runtime.clone(),
        env: vec![],
        verbose_log_dir: None,
        mute_audio: false,
    };
    let (h, rx) = e
        .runs
        .start(RunSpec {
            kind: RunKind::PrefixInit,
            env_id: e.env_id,
            game_id: None,
            install_id: None,
            invocation: inv,
        })
        .unwrap();
    let o = collect(&rx, Duration::from_secs(900));
    assert_eq!(o.exit, Some(Exit::Low8(0)), "prefix init");
    assert!(scan_marker(h.marker()).is_empty());
}

#[test]
#[ignore = "needs real umu-run, VORTEX_IT_PROTON and mingw-w64-gcc"]
fn it_launch_delivers_args_and_env() {
    let proton = std::env::var("VORTEX_IT_PROTON").expect("VORTEX_IT_PROTON must be set");
    let e = env_with(Tools::default(), Path::new(&proton), Timing::default());
    init_prefix(&e);
    let src = Path::new(MANIFEST).join("tests/fixtures/launch/echoargs.c");
    let exe = e.dir.as_path().join("echoargs.exe");
    let st = std::process::Command::new("x86_64-w64-mingw32-gcc")
        .args(["-O2", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .unwrap_or_else(|er| panic!("mingw-w64-gcc not available ({er}): NOT RUN"));
    assert!(st.success());
    let id = e.game_exe(
        "echoargs.exe",
        &["one two", "--flag=3", "$HOME"],
        &[("VORTEX_TEST_VAR", "hello world")],
        None,
    );
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, id).unwrap();
    assert_eq!(
        collect(&rx, Duration::from_secs(300)).exit,
        Some(Exit::Low8(0))
    );
    let got = std::fs::read_to_string(e.dir.as_path().join("args.txt"))
        .expect("exe ran in its own dir")
        .replace("\r\n", "\n");
    assert_eq!(
        got,
        "arg=one two\narg=--flag=3\narg=$HOME\nenv=hello world\n"
    );
    assert!(scan_marker(h.marker()).is_empty());
}

#[test]
#[ignore = "needs real umu-run, network (GE-Proton download) or VORTEX_IT_GE"]
fn it_winetricks_with_ge_proton() {
    let tmp = tempfile::tempdir().unwrap();
    let ge = match std::env::var("VORTEX_IT_GE") {
        Ok(d) => HostPath::new(d).unwrap(),
        Err(_) => {
            let rel = vortex_core::runtime_ge::latest_release().expect("latest GE-Proton release");
            eprintln!("installing {}", rel.tag);
            let (tx, _rx) = async_channel::unbounded();
            vortex_core::runtime_ge::install(
                &rel,
                &tmp.path().join("runtimes"),
                &tx,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap()
        }
    };
    let e = env_with(Tools::default(), ge.as_path(), Timing::default());
    init_prefix(&e);
    // Prefix B must stay untouched.
    let b = tmp.path().join("prefix-b");
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(b.join("winetricks.log"), "vcrun2019\n").unwrap();

    let known = winetricks::list_verbs(&Tools::default(), &e.runtime, &e.paths).unwrap();
    eprintln!("{} verbs", known.len());
    assert!(
        known
            .iter()
            .any(|v| v.name == "vcrun2005" && v.section == "dlls")
    );
    let (h, rx) = winetricks::install_verb(&e.runs, &e.db, &e.paths, e.env_id, "win10", &known)
        .unwrap()
        .unwrap();
    let o = collect(&rx, Duration::from_secs(1800));
    eprintln!(
        "log: {}",
        std::fs::read_to_string(e.db.lock().unwrap().get_run(h.run_id()).unwrap().log_path)
            .unwrap()
    );
    assert_eq!(o.exit, Some(Exit::Low8(0)));
    assert!(winetricks::installed(&e.prefix).unwrap().contains("win10"));
    let deps = e.db.lock().unwrap().list_dependencies(e.env_id).unwrap();
    assert_eq!(
        (deps[0].verb.as_str(), deps[0].state),
        ("win10", DepState::Installed)
    );
    assert!(scan_marker(h.marker()).is_empty());
    assert!(
        winetricks::install_verb(&e.runs, &e.db, &e.paths, e.env_id, "win10", &known)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        std::fs::read_to_string(b.join("winetricks.log")).unwrap(),
        "vcrun2019\n"
    );
}

#[test]
fn list_verbs_runs_in_scratch_prefix_never_a_game_prefix() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    // Fresh runtime path so the per-runtime cache cannot skip the call.
    let rt = t.path().join("rt");
    std::fs::create_dir_all(rt.join("files/bin")).unwrap();
    std::fs::create_dir_all(rt.join("protonfixes")).unwrap();
    std::fs::write(rt.join("protonfixes/winetricks"), "").unwrap();
    std::fs::write(rt.join("files/bin/wineserver"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        rt.join("files/bin/wineserver"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let rec = t.path().join("wineprefix.txt");
    let umu = t.path().join("umu");
    let fixture = Path::new(MANIFEST).join("tests/fixtures/winetricks/list-all.txt");
    std::fs::write(
        &umu,
        format!(
            "#!/bin/sh\nprintf %s \"$WINEPREFIX\" > '{}'\ncat '{}'\n",
            rec.display(),
            fixture.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&umu, std::fs::Permissions::from_mode(0o755)).unwrap();
    let paths = Paths::under(t.path());
    let verbs = winetricks::list_verbs(
        &Tools { umu_run: umu },
        &HostPath::new(&rt).unwrap(),
        &paths,
    )
    .unwrap();
    assert!(!verbs.is_empty());
    let used = std::fs::read_to_string(rec).unwrap();
    let scratch = paths.scratch_prefix().unwrap();
    assert_eq!(Path::new(&used), scratch.as_path());
    assert!(!Path::new(&used).starts_with(paths.prefixes_dir()));
}
