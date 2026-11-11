//! M10: launch, run-in-env, winetricks. Default tests use `tests/fake-umu-run` and a fake
//! runtime; `#[ignore]` tests (`it_*`) use real umu-run (see the comments above them).

use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
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
    /// Owns `wt` and `rec`.
    _scripts: tempfile::TempDir,
}

/// Built while holding the lock, before anything uses it (avoids ETXTBSY from forks racing a
/// write). Shared by all tests running at once; the temp dirs are removed when the last `Env`
/// (or local `Arc`) drops, because a plain static would never run `TempDir::drop`.
fn shared() -> Arc<Shared> {
    static S: Mutex<Weak<Shared>> = Mutex::new(Weak::new());
    let mut slot = S.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = slot.upgrade() {
        return s;
    }
    let s = Arc::new({
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
        let scripts = tempfile::tempdir().unwrap();
        let wt = scripts.path().join("wt");
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
        let rec = scripts.path().join("rec");
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
            _scripts: scripts,
        }
    });
    *slot = Arc::downgrade(&s);
    s
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
    _shared: Arc<Shared>,
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
    let shared = shared();
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
        _shared: shared,
    }
}

fn fake_tools() -> Tools {
    Tools {
        umu_run: Path::new(MANIFEST).join("tests/fake-umu-run"),
    }
}

/// `None` as root (CI): verifies that launching refuses to spawn, then the test ends.
fn fake_env() -> Option<Env> {
    let sh = shared();
    let e = env_with(fake_tools(), sh.runtime.path(), fast());
    std::fs::write(e.dir.as_path().join("game.exe"), "MZ").unwrap();
    if rustix::process::geteuid().is_root() {
        let id = e.game(&[], &[], None);
        match launch_game(&e.runs, &e.db, &e.paths, id, false) {
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
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, id, false).unwrap();
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
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, ok, false).unwrap();
    assert_eq!(collect(&rx, T).exit, Some(Exit::Low8(0)));
    let f = e.db.lock().unwrap().get_run(h.run_id()).unwrap().findings;
    assert_eq!(f.as_deref(), Some("[]"));

    let bad = e.game(&[], &[("FAKE_UMU_MODE", "exit:42")], None);
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, bad, false).unwrap();
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
    let (_, rx) = launch_game(&e.runs, &e.db, &e.paths, id, false).unwrap();
    collect(&rx, T);
    let rec = std::fs::read_to_string(&out).unwrap();
    assert!(rec.starts_with(&format!("cwd={}\n", wd.display())), "{rec}");
}

#[test]
fn missing_exe_is_not_found_and_nothing_spawned() {
    let Some(e) = fake_env() else { return };
    let id = e.game_exe("gone.exe", &[], &[], None);
    let err = launch_game(&e.runs, &e.db, &e.paths, id, false)
        .map(|_| ())
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
    assert!(e.db.lock().unwrap().runs_for_game(id).unwrap().is_empty());
}

#[test]
fn bad_config_is_rejected() {
    let Some(e) = fake_env() else { return };
    let id = e.game(&[], &[("WINEPREFIX", "/x")], None);
    let r = launch_game(&e.runs, &e.db, &e.paths, id, false).map(|_| ());
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
    let r = launch_game(&e.runs, &e.db, &e.paths, id, false).map(|_| ());
    assert!(
        matches!(r, Err(Error::Validation(_))),
        "string args refused"
    );
}

#[test]
fn second_launch_is_busy_and_stop_finishes() {
    let Some(e) = fake_env() else { return };
    let id = e.game(&[], &[("FAKE_UMU_MODE", "sleep:30")], None);
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, id, false).unwrap();
    let r = launch_game(&e.runs, &e.db, &e.paths, id, false).map(|_| ());
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
    let sh = shared();
    let tools = Tools {
        umu_run: sh.rec.clone(),
    };
    let e = env_with(tools, sh.runtime.path(), fast());
    let exe = e.dir.join("unins000.exe").unwrap();
    std::fs::write(exe.as_path(), "MZ").unwrap();
    // No drive_c yet: refused before anything runs.
    let r = run_uninstaller(&e.runs, &e.db, &e.paths, e.env_id, &exe).map(|_| ());
    assert!(matches!(r, Err(Error::NotFound(_))), "{r:?}");
    let c = e.prefix.drive_c();
    std::fs::create_dir_all(&c).unwrap();
    let (h, rx) = run_uninstaller(&e.runs, &e.db, &e.paths, e.env_id, &exe).unwrap();
    assert_eq!(collect(&rx, T).exit, Some(Exit::Low8(0)));
    // Neither umu-run's program nor the cwd is the folder the uninstaller deletes.
    let rec = std::fs::read_to_string(c.join(".fake-rec")).unwrap();
    let win = format!("Z:{}\\unins000.exe", e.dir.as_path().display()).replace('/', "\\");
    assert_eq!(
        rec,
        format!(
            "cwd={}\narg=C:\\windows\\system32\\start.exe\narg=/wait\narg={win}\n",
            c.display()
        )
    );
    // A missing uninstaller is refused before anything runs.
    std::fs::remove_file(exe.as_path()).unwrap();
    let r = run_uninstaller(&e.runs, &e.db, &e.paths, e.env_id, &exe).map(|_| ());
    assert!(matches!(r, Err(Error::NotFound(_))), "{r:?}");
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
    let sh = shared();
    let plain = HostPath::new(sh.plain.path()).unwrap();
    let err = winetricks::list_verbs(&fake_tools(), &plain, &e.paths).unwrap_err();
    assert!(matches!(err, Error::RuntimeInvalid { .. }));
    assert!(err.to_string().contains("GE-Proton or UMU-Proton"), "{err}");
    let e2 = env_with(fake_tools(), sh.plain.path(), fast());
    let err = winetricks::install_verb(
        &e2.runs,
        &e2.db,
        &e2.paths,
        e2.env_id,
        "win10",
        &fixture_verbs(),
        false,
    )
    .map(|_| ())
    .unwrap_err();
    assert!(err.to_string().contains("GE-Proton or UMU-Proton"), "{err}");
}

#[test]
fn list_install_records_dependency_and_skips_installed() {
    let Some(_) = fake_env() else { return };
    let sh = shared();
    let tools = Tools {
        umu_run: sh.wt.clone(),
    };
    let e = env_with(tools.clone(), sh.runtime.path(), fast());
    let known = winetricks::list_verbs(&tools, &e.runtime, &e.paths).unwrap();
    assert_eq!(known.len(), 7);
    let (h, rx) = winetricks::install_verb(
        &e.runs,
        &e.db,
        &e.paths,
        e.env_id,
        "vcrun2019",
        &known,
        false,
    )
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
        winetricks::install_verb(
            &e.runs,
            &e.db,
            &e.paths,
            e.env_id,
            "vcrun2019",
            &known,
            false
        )
        .unwrap()
        .is_none()
    );
    // Invalid verbs never spawn.
    assert!(
        winetricks::install_verb(&e.runs, &e.db, &e.paths, e.env_id, "foo;rm", &known, false)
            .is_err()
    );
    assert_eq!(e.db.lock().unwrap().active_runs().unwrap().len(), 0);
}

#[test]
fn failed_verb_is_recorded_failed() {
    let Some(_) = fake_env() else { return };
    // fake-umu-run exits 0 but never writes winetricks.log: not confirmed, so `failed`.
    let sh = shared();
    let e = env_with(fake_tools(), sh.runtime.path(), fast());
    let (_, rx) = winetricks::install_verb(
        &e.runs,
        &e.db,
        &e.paths,
        e.env_id,
        "win10",
        &fixture_verbs(),
        false,
    )
    .unwrap()
    .unwrap();
    collect(&rx, T);
    let deps = e.db.lock().unwrap().list_dependencies(e.env_id).unwrap();
    assert_eq!(deps[0].state, DepState::Failed);
}

/// `KEY=value` lines of a fake-umu-run env dump that are about verbose logging, sorted.
fn verbose_env(dump: &Path) -> Vec<String> {
    std::fs::read_to_string(dump)
        .unwrap()
        .lines()
        .filter(|l| {
            ["UMU_LOG=", "PROTON_LOG=", "PROTON_LOG_DIR=", "WINEDEBUG="]
                .iter()
                .any(|k| l.starts_with(k))
        })
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[test]
fn verbose_launch_writes_a_redacted_proton_log_and_names_the_missing_file() {
    let Some(e) = fake_env() else { return };
    let out = e.dir.as_path().join("rec.out");
    let dump = Path::new(&format!("{}.env", out.display())).to_path_buf();
    let id = e.game(
        &[],
        &[
            ("FAKE_UMU_OUT", &out.to_string_lossy()),
            ("FAKE_UMU_MODE", "protonlog,exit:3"),
        ],
        None,
    );
    let run_of =
        |h: &vortex_core::process::RunHandle| e.db.lock().unwrap().get_run(h.run_id()).unwrap();

    // Not verbose: nothing about verbose logging is set (a WINEDEBUG inherited by the test
    // process passes through unchanged), no Proton log, no missing-file finding.
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, id, false).unwrap();
    assert_eq!(collect(&rx, T).exit, Some(Exit::Low8(3)));
    let inherited = std::env::var("WINEDEBUG")
        .ok()
        .map(|v| format!("WINEDEBUG={v}"));
    assert_eq!(
        verbose_env(&dump),
        inherited.into_iter().collect::<Vec<_>>()
    );
    let run = run_of(&h);
    assert!(!vortex_core::logs::proton_log_dir(Path::new(&run.log_path)).exists());
    let f = vortex_core::diagnose::from_json(&run.findings.unwrap());
    assert!(
        f.iter()
            .all(|f| f.class != vortex_core::diagnose::Class::MissingFile),
        "{f:?}"
    );

    // Verbose: the env contract, the Proton log in the run's managed log dir, redacted.
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, id, true).unwrap();
    assert_eq!(collect(&rx, T).exit, Some(Exit::Low8(3)));
    let run = run_of(&h);
    let dir = vortex_core::logs::proton_log_dir(Path::new(&run.log_path));
    assert!(dir.starts_with(e.paths.logs_dir()), "{}", dir.display());
    assert_eq!(
        verbose_env(&dump),
        [
            "PROTON_LOG=1".to_string(),
            format!("PROTON_LOG_DIR={}", dir.display()),
            "UMU_LOG=debug".to_string(),
            format!("WINEDEBUG={}", vortex_core::runner::VERBOSE_WINEDEBUG),
        ]
    );
    let plog = vortex_core::logs::proton_log(Path::new(&run.log_path)).unwrap();
    let text = std::fs::read_to_string(&plog).unwrap();
    assert!(
        !text.contains("ghp_") && text.contains("GITHUB_TOKEN=<redacted>"),
        "{text}"
    );
    let header = std::fs::read_to_string(&run.log_path).unwrap();
    assert!(header.contains(&format!("proton_log_dir: {}", dir.display())));
    let f = vortex_core::diagnose::from_json(&run.findings.unwrap());
    let m = f
        .iter()
        .find(|f| f.class == vortex_core::diagnose::Class::MissingFile)
        .unwrap_or_else(|| panic!("{f:?}"));
    assert!(m.evidence[0].ends_with(r"game dir\data3.bin"), "{m:?}");
}

#[test]
fn verbose_winetricks_run_gets_the_proton_log_env() {
    let Some(_) = fake_env() else { return };
    let sh = shared();
    let tools = Tools {
        umu_run: sh.rec.clone(),
    };
    let e = env_with(tools, sh.runtime.path(), fast());
    let dump = e.prefix.as_path().join(".fake-rec.env");
    let run = |verbose| {
        let (h, rx) = winetricks::install_verb(
            &e.runs,
            &e.db,
            &e.paths,
            e.env_id,
            "win10",
            &fixture_verbs(),
            verbose,
        )
        .unwrap()
        .unwrap();
        collect(&rx, T);
        e.db.lock().unwrap().get_run(h.run_id()).unwrap()
    };
    run(false);
    assert!(
        verbose_env(&dump)
            .iter()
            .all(|l| l.starts_with("WINEDEBUG=")),
        "{:?}",
        verbose_env(&dump)
    );
    let r = run(true);
    let dir = vortex_core::logs::proton_log_dir(Path::new(&r.log_path));
    let v = verbose_env(&dump);
    assert!(v.contains(&"PROTON_LOG=1".to_string()), "{v:?}");
    assert!(v.contains(&"UMU_LOG=debug".to_string()), "{v:?}");
    assert!(
        v.contains(&format!("PROTON_LOG_DIR={}", dir.display())),
        "{v:?}"
    );
    assert!(dir.is_dir());
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
            verbose: false,
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
    let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, id, false).unwrap();
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
#[ignore = "needs real umu-run, VORTEX_IT_PROTON and mingw-w64-gcc"]
fn it_verbose_launch_names_the_missing_data_file() {
    let proton = std::env::var("VORTEX_IT_PROTON").expect("VORTEX_IT_PROTON must be set");
    let e = env_with(Tools::default(), Path::new(&proton), Timing::default());
    init_prefix(&e);
    let src = Path::new(MANIFEST).join("tests/fixtures/launch/needsdata.c");
    let exe = e.dir.as_path().join("needsdata.exe");
    let st = std::process::Command::new("x86_64-w64-mingw32-gcc")
        .args(["-O2", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .unwrap_or_else(|er| panic!("mingw-w64-gcc not available ({er}): NOT RUN"));
    assert!(st.success());
    let id = e.game_exe("needsdata.exe", &[], &[], None);
    let launch = |verbose| {
        let (h, rx) = launch_game(&e.runs, &e.db, &e.paths, id, verbose).unwrap();
        let o = collect(&rx, Duration::from_secs(300));
        assert_eq!(o.exit, Some(Exit::Low8(3)));
        assert!(scan_marker(h.marker()).is_empty());
        e.db.lock().unwrap().get_run(h.run_id()).unwrap()
    };
    let missing = |r: &vortex_core::db::Run| {
        vortex_core::diagnose::from_json(r.findings.as_deref().unwrap_or("[]"))
            .into_iter()
            .find(|f| f.class == vortex_core::diagnose::Class::MissingFile)
    };
    let r = launch(false);
    assert!(vortex_core::logs::proton_log(Path::new(&r.log_path)).is_none());
    assert!(missing(&r).is_none());

    let r = launch(true);
    let plog = vortex_core::logs::proton_log(Path::new(&r.log_path)).expect("Proton log");
    assert!(plog.starts_with(e.paths.logs_dir()));
    let size = std::fs::metadata(&plog).unwrap().len();
    eprintln!("proton log {} ({size} bytes)", plog.display());
    let f = missing(&r).unwrap_or_else(|| panic!("no MissingFile finding: {:?}", r.findings));
    eprintln!("finding: {f:?}");
    assert!(f.evidence[0].ends_with(r"game dir\data3.bin"), "{f:?}");
    assert!(!f.evidence[0].contains(".dll") && !f.evidence[0].contains(".manifest"));
}

/// `VORTEX_IT_GE`, or the latest GE-Proton installed by `runtime_ge` under `tmp`.
fn ge_runtime(tmp: &Path) -> HostPath {
    match std::env::var("VORTEX_IT_GE") {
        Ok(d) => HostPath::new(d).unwrap(),
        Err(_) => {
            let rel = vortex_core::runtime_ge::latest_release().expect("latest GE-Proton release");
            eprintln!("installing {}", rel.tag);
            let (tx, _rx) = async_channel::unbounded();
            vortex_core::runtime_ge::install(
                &rel,
                &tmp.join("runtimes"),
                &tx,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap()
        }
    }
}

#[test]
#[ignore = "needs real umu-run, network (GE-Proton download) or VORTEX_IT_GE"]
fn it_winetricks_with_ge_proton() {
    let tmp = tempfile::tempdir().unwrap();
    let ge = ge_runtime(tmp.path());
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
    let (h, rx) =
        winetricks::install_verb(&e.runs, &e.db, &e.paths, e.env_id, "win10", &known, false)
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
        winetricks::install_verb(&e.runs, &e.db, &e.paths, e.env_id, "win10", &known, false)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        std::fs::read_to_string(b.join("winetricks.log")).unwrap(),
        "vcrun2019\n"
    );
}

/// Real redistributable verbs (M28), each in its own fresh prefix. Needs network: winetricks
/// downloads the installers. `VORTEX_IT_VERBS=a,b` overrides the list. Every verb runs; the test
/// fails at the end if any did not install.
#[test]
#[ignore = "needs real umu-run, network (redistributables, GE-Proton or VORTEX_IT_GE)"]
fn it_winetricks_real_verbs() {
    let tmp = tempfile::tempdir().unwrap();
    let ge = ge_runtime(tmp.path());
    let verbs =
        std::env::var("VORTEX_IT_VERBS").unwrap_or_else(|_| "vcrun2022,d3dx9,corefonts".into());
    let verbose = std::env::var_os("VORTEX_IT_VERBOSE").is_some();
    let mut failed = Vec::new();
    for verb in verbs.split(',') {
        let e = env_with(Tools::default(), ge.as_path(), Timing::default());
        let t0 = std::time::Instant::now();
        init_prefix(&e);
        let init = t0.elapsed();
        let known = winetricks::list_verbs(&Tools::default(), &e.runtime, &e.paths).unwrap();
        let t1 = std::time::Instant::now();
        let (h, rx) =
            winetricks::install_verb(&e.runs, &e.db, &e.paths, e.env_id, verb, &known, verbose)
                .unwrap()
                .unwrap();
        let o = collect(&rx, Duration::from_secs(3600));
        let took = t1.elapsed();
        let log =
            std::fs::read_to_string(e.prefix.as_path().join("winetricks.log")).unwrap_or_default();
        let listed = winetricks::installed(&e.prefix).unwrap().contains(verb);
        let dep = e.db.lock().unwrap().list_dependencies(e.env_id).unwrap();
        let ok = o.exit == Some(Exit::Low8(0))
            && listed
            && dep
                .iter()
                .any(|d| d.verb == verb && d.state == DepState::Installed)
            && scan_marker(h.marker()).is_empty();
        eprintln!(
            "VERB {verb}: {} exit {:?}, prefix init {:.0?}, verb {:.0?}, winetricks.log: {:?}",
            if ok { "PASS" } else { "FAIL" },
            o.exit,
            init,
            took,
            log.lines().collect::<Vec<_>>()
        );
        if !ok {
            let run = e.db.lock().unwrap().get_run(h.run_id()).unwrap();
            let l = std::fs::read_to_string(&run.log_path).unwrap_or_default();
            let tail: Vec<_> = l.lines().rev().take(40).collect();
            eprintln!("run log tail (newest first):\n{}", tail.join("\n"));
            failed.push(verb.to_string());
        }
    }
    assert!(failed.is_empty(), "verbs not installed: {failed:?}");
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
