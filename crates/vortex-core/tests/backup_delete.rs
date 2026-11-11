use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use vortex_core::backup::{backup, restore, restore_limited};
use vortex_core::db::*;
use vortex_core::delete::*;
use vortex_core::error::Error;
use vortex_core::logs::{LogWriter, Stream};
use vortex_core::paths::Paths;

fn setup() -> (tempfile::TempDir, Paths, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path());
    fs::create_dir_all(paths.prefixes_dir()).unwrap();
    fs::create_dir_all(paths.home()).unwrap();
    let db = Db::open(&paths.db_file()).unwrap();
    (tmp, paths, db)
}

/// Env + game with a populated managed prefix.
fn game(db: &Db, paths: &Paths, name: &str) -> (i64, i64) {
    let env = db.insert_environment("pending", true, "/rt").unwrap();
    let pfx = paths.managed_prefix(env).unwrap();
    let p = pfx.as_path();
    fs::create_dir_all(p.join("drive_c/Games")).unwrap();
    fs::create_dir_all(p.join("dosdevices")).unwrap();
    fs::write(p.join("drive_c/Games/save.dat"), name).unwrap();
    fs::write(p.join("version"), "9.0\n").unwrap();
    symlink("/", p.join("dosdevices/z:")).unwrap();
    // env row path fixed up through a fresh insert: prefix_path must be the managed dir
    db.delete_environment(env).unwrap();
    let env = db
        .insert_environment(p.to_str().unwrap(), true, "/rt")
        .unwrap();
    assert_eq!(paths.managed_prefix(env).unwrap().as_path(), p, "ids align");
    let g = db.insert_game(&NewGame::new(name, env)).unwrap();
    (env, g)
}

#[test]
fn round_trip_keeps_symlink_and_modes() {
    let (_t, paths, db) = setup();
    let (env, _) = game(&db, &paths, "A");
    let p = paths.managed_prefix(env).unwrap();
    let exe = p.as_path().join("drive_c/Games/run.sh");
    fs::write(&exe, "x").unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o750)).unwrap();
    let b = backup(&db, &paths, env, &AtomicBool::new(false)).unwrap();
    assert_eq!(b.prefix_version.as_deref(), Some("9.0"));
    assert!(b.size_bytes < 100_000, "z: -> / was not followed");

    fs::remove_dir_all(p.as_path()).unwrap();
    restore(&db, &paths, b.id, &p, false).unwrap();
    let z = p.as_path().join("dosdevices/z:");
    assert!(fs::symlink_metadata(&z).unwrap().is_symlink());
    assert_eq!(fs::read_link(&z).unwrap(), Path::new("/"));
    assert_eq!(
        fs::read_to_string(p.as_path().join("drive_c/Games/save.dat")).unwrap(),
        "A"
    );
    let mode = fs::metadata(&exe).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o750);
}

#[test]
fn restore_needs_confirmation_to_overwrite() {
    let (_t, paths, db) = setup();
    let (env, _) = game(&db, &paths, "A");
    let p = paths.managed_prefix(env).unwrap();
    let b = backup(&db, &paths, env, &AtomicBool::new(false)).unwrap();
    fs::write(p.as_path().join("marker"), "live").unwrap();
    assert!(matches!(
        restore(&db, &paths, b.id, &p, false),
        Err(Error::Validation(_))
    ));
    assert!(p.as_path().join("marker").exists());
    restore(&db, &paths, b.id, &p, true).unwrap();
    assert!(!p.as_path().join("marker").exists());
}

fn evil_backup(db: &Db, paths: &Paths, entries: &[(&str, bool)], mode: u32, link_to: &str) -> i64 {
    fs::create_dir_all(paths.backups_dir()).unwrap();
    let path = paths.backups_dir().join("evil.tar.gz");
    let gz = flate2::write::GzEncoder::new(
        fs::File::create(&path).unwrap(),
        flate2::Compression::default(),
    );
    let mut raw = Vec::new();
    for (name, link) in entries {
        let mut h = tar::Header::new_gnu();
        if *link {
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_link_name(link_to).unwrap();
        } else if name.ends_with('/') {
            h.set_entry_type(tar::EntryType::Directory);
            h.set_size(0);
        } else {
            h.set_size(1);
        }
        h.set_mode(mode);
        // write the name bytes directly so '..' and '/' survive
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_cksum();
        raw.extend_from_slice(h.as_bytes());
        if !*link && !name.ends_with('/') {
            let mut blk = [0u8; 512];
            blk[0] = b'x';
            raw.extend_from_slice(&blk);
        }
    }
    raw.extend_from_slice(&[0u8; 1024]);
    use std::io::Write;
    let mut gz = gz;
    gz.write_all(&raw).unwrap();
    gz.finish().unwrap();
    db.insert_backup(&NewBackup {
        environment_id: None,
        game_name: "evil".into(),
        archive_path: path.to_str().unwrap().into(),
        runtime_path: "/rt".into(),
        prefix_version: None,
        size_bytes: 1,
    })
    .unwrap()
}

#[test]
fn malicious_archives_are_rejected() {
    for entries in [
        vec![("../escape", false)],
        vec![("/abs/file", false)],
        vec![("a/../../escape", false)],
        vec![("lnk", true), ("lnk/pwned", false)],
    ] {
        let (tmp, paths, db) = setup();
        let id = evil_backup(&db, &paths, &entries, 0o644, "/tmp");
        let target = paths.managed_prefix(7).unwrap();
        let r = restore(&db, &paths, id, &target, false);
        assert!(matches!(r, Err(Error::Validation(_))), "{entries:?}: {r:?}");
        assert!(!target.as_path().exists());
        assert!(!tmp.path().join("escape").exists());
        assert!(!Path::new("/tmp/pwned").exists());
        assert!(
            !paths.prefixes_dir().join(".restore-1").exists(),
            "temp cleaned"
        );
    }
}

#[test]
fn plan_refuses_symlinked_prefix_and_keeps_outside_dir() {
    let (tmp, paths, db) = setup();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), "k").unwrap();
    // prefix path itself is a symlink to an outside dir
    let env = db
        .insert_environment(
            paths.prefixes_dir().join("9").to_str().unwrap(),
            true,
            "/rt",
        )
        .unwrap();
    symlink(&outside, paths.prefixes_dir().join("9")).unwrap();
    assert!(plan_environment_removal(&db, &paths, env).is_err());
    assert!(outside.join("keep").exists());

    // symlink inside a prefix pointing outside: plan has only the prefix; outside survives
    let (env2, g2) = game(&db, &paths, "B");
    let p = paths.managed_prefix(env2).unwrap();
    symlink(&outside, p.as_path().join("drive_c/link")).unwrap();
    let plan = plan_game_removal(&db, &paths, g2).unwrap();
    assert_eq!(plan.items.len(), 1);
    assert_eq!(
        plan.items[0].path.as_path(),
        fs::canonicalize(p.as_path()).unwrap()
    );
    execute(&db, &paths, &plan).unwrap();
    assert!(!p.as_path().exists());
    assert!(outside.join("keep").exists());
    assert!(Path::new("/etc").exists());
}

#[test]
fn deleting_a_leaves_b_intact() {
    let (_t, paths, db) = setup();
    let (_ea, ga) = game(&db, &paths, "A");
    let (eb, gb) = game(&db, &paths, "B");
    let plan = plan_game_removal(&db, &paths, ga).unwrap();
    let pb = paths.managed_prefix(eb).unwrap();
    assert!(
        plan.items
            .iter()
            .all(|i| !i.path.as_path().starts_with(pb.as_path()))
    );
    execute(&db, &paths, &plan).unwrap();
    assert!(pb.as_path().join("drive_c/Games/save.dat").exists());
    assert!(db.get_game(gb).is_ok());
    assert!(db.get_game(ga).is_err());
}

/// Writes one run's log through `LogWriter` (as `Runs::start` does) and records the run row.
fn log_run(db: &Db, paths: &Paths, env: i64, game: Option<i64>, text: &str) -> std::path::PathBuf {
    let id = db
        .insert_run(&NewRun {
            marker: format!("m-{text}"),
            kind: RunKind::Launch,
            environment_id: env,
            game_id: game,
            install_id: None,
            argv: "[]".into(),
            log_path: String::new(),
        })
        .unwrap();
    db.finish_run(id, RunState::Finished, Some(0), false).unwrap();
    let mut w = LogWriter::create(paths, game, env, id, RunKind::Launch).unwrap();
    w.line(Stream::Stdout, text).unwrap();
    w.flush().unwrap();
    db.set_run_log_path(id, &w.path().to_string_lossy()).unwrap();
    w.path().to_path_buf()
}

#[test]
fn game_logs_are_planned_and_a_reused_id_starts_with_an_empty_log_dir() {
    let (_t, paths, db) = setup();
    let (env, g) = game(&db, &paths, "Old");
    let old = log_run(&db, &paths, env, Some(g), "old game");
    let old_env = log_run(&db, &paths, env, None, "old env");
    let plan = plan_game_removal(&db, &paths, g).unwrap();
    for l in [&old, &old_env] {
        let dir = fs::canonicalize(l.parent().unwrap()).unwrap();
        assert!(
            plan.items.iter().any(|i| i.path.as_path() == dir),
            "{} not in plan",
            dir.display()
        );
    }
    execute(&db, &paths, &plan).unwrap();
    assert!(!old.exists() && !old_env.exists());

    // SQLite hands out the deleted ids again (INTEGER PRIMARY KEY without AUTOINCREMENT)
    let (env2, g2) = game(&db, &paths, "New");
    assert_eq!((env2, g2), (env, g), "ids are reused");
    let new = log_run(&db, &paths, env2, Some(g2), "new game");
    let dir: Vec<_> = fs::read_dir(new.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(dir, [new.clone()], "only the new run's log");
    assert_eq!(fs::read_to_string(&new).unwrap(), "O|new game\n");

    // a game sharing its environment: only the game's logs go, the environment's stay
    let other = db.insert_game(&NewGame::new("Other", env2)).unwrap();
    let env_log = log_run(&db, &paths, env2, None, "shared env");
    let plan = plan_game_removal(&db, &paths, g2).unwrap();
    assert!(!plan.remove_env);
    execute(&db, &paths, &plan).unwrap();
    assert!(!new.exists() && env_log.exists());
    assert!(db.get_game(other).is_ok());
}

#[test]
fn removing_a_game_removes_only_its_desktop_entry() {
    let (_t, paths, db) = setup();
    let (_ea, ga) = game(&db, &paths, "A");
    let (_eb, gb) = game(&db, &paths, "B");
    let bin = Path::new("/usr/bin/vortex");
    let ea = vortex_core::desktop::write_entry(&paths, bin, &ga.to_string(), "A").unwrap();
    let eb = vortex_core::desktop::write_entry(&paths, bin, &gb.to_string(), "B").unwrap();
    let plan = plan_game_removal(&db, &paths, ga).unwrap();
    execute(&db, &paths, &plan).unwrap();
    assert!(!ea.exists(), "stale desktop entry {}", ea.display());
    assert!(eb.exists());
}

#[test]
fn unmanaged_prefix_is_never_planned_and_shared_env_is_kept() {
    let (tmp, paths, db) = setup();
    let ext = tmp.path().join("my-wine");
    fs::create_dir(&ext).unwrap();
    let env = db
        .insert_environment(ext.to_str().unwrap(), false, "/rt")
        .unwrap();
    let g = db.insert_game(&NewGame::new("U", env)).unwrap();
    let plan = plan_game_removal(&db, &paths, g).unwrap();
    assert!(plan.items.is_empty());
    let shown = fs::canonicalize(&ext).unwrap();
    assert_eq!(
        plan.manual.iter().map(|m| m.as_path()).collect::<Vec<_>>(),
        [shown.as_path()]
    );
    execute(&db, &paths, &plan).unwrap();
    assert!(ext.exists());

    // env still referenced by another game: environment plan refused, game plan keeps prefix
    let (e, g1) = game(&db, &paths, "S1");
    db.insert_game(&NewGame::new("S2", e)).unwrap();
    assert!(plan_environment_removal(&db, &paths, e).is_err());
    let plan = plan_game_removal(&db, &paths, g1).unwrap();
    assert!(!plan.remove_env && plan.items.is_empty());
}

/// Game in a fresh env whose install dir is `dir` (flagged managed).
fn game_with_dir(db: &Db, paths: &Paths, dir: &Path) -> i64 {
    let (env, _) = game(db, paths, "D");
    let mut ng = NewGame::new("D2", env);
    ng.install_dir = Some(dir.to_str().unwrap().into());
    ng.install_dir_managed = true;
    db.insert_game(&ng).unwrap()
}

#[test]
fn only_dirs_under_games_root_are_deletable() {
    let (tmp, paths, db) = setup();
    let mine = paths.games_dir().join("A");
    fs::create_dir_all(&mine).unwrap();
    let g = game_with_dir(&db, &paths, &mine);
    let plan = plan_game_removal(&db, &paths, g).unwrap();
    assert_eq!(plan.items.len(), 1);
    assert!(plan.manual.is_empty());
    // everything else, even flagged managed, is manual-only and never planned
    let docs = tmp.path().join("home/Documents");
    let child = tmp.path().join("home/Games");
    let mnt = tmp.path().join("mnt/data");
    for d in [
        docs.as_path(),
        child.as_path(),
        mnt.as_path(),
        paths.home(),
        paths.data_dir(),
        &paths.backups_dir(),
        &paths.games_dir(),
        Path::new("/etc"),
        Path::new("/"),
    ] {
        fs::create_dir_all(d).ok();
        let g = game_with_dir(&db, &paths, d);
        let plan = plan_game_removal(&db, &paths, g).unwrap();
        assert!(plan.items.is_empty(), "{d:?}");
        assert_eq!(plan.manual.len(), 1, "{d:?}");
        execute(&db, &paths, &plan).unwrap();
        assert!(d.exists(), "{d:?} survived");
    }
}

#[test]
fn install_dir_inside_prefix_is_manual_and_rerun_is_safe() {
    let (_t, paths, db) = setup();
    let (env, g) = game(&db, &paths, "N");
    let p = paths.managed_prefix(env).unwrap();
    // install dir under games root, plus a symlink-free nested layout is impossible, so nest by
    // pointing the install dir inside the prefix is refused as not under games: manual list.
    let inner = p.as_path().join("drive_c/Games");
    let mut ng = db.get_game(g).unwrap();
    ng.install_dir = Some(inner.to_str().unwrap().into());
    drop(ng);
    let plan = plan_game_removal(&db, &paths, g).unwrap();
    execute(&db, &paths, &plan).unwrap();
    // second execution (retry) of the same plan is idempotent
    assert!(
        execute(&db, &paths, &plan).is_err(),
        "rows gone: game not found"
    );
    assert!(!p.as_path().exists());
}

#[test]
fn plan_changed_before_execute_aborts() {
    let (tmp, paths, db) = setup();
    let (env, g) = game(&db, &paths, "T");
    let plan = plan_game_removal(&db, &paths, g).unwrap();
    let p = paths.managed_prefix(env).unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), "k").unwrap();
    fs::remove_dir_all(p.as_path()).unwrap();
    symlink(&outside, p.as_path()).unwrap();
    assert!(execute(&db, &paths, &plan).is_err());
    assert!(outside.join("keep").exists());
    assert!(db.get_game(g).is_ok());
}

/// F6: execute deletes the rebuilt items and refuses a plan whose items differ from them, so an
/// item added to the shown plan is never deleted.
#[test]
fn plan_with_extra_item_is_refused() {
    let (tmp, paths, db) = setup();
    let (env, g) = game(&db, &paths, "T");
    let mut plan = plan_game_removal(&db, &paths, g).unwrap();
    let extra = tmp.path().join("not planned");
    fs::create_dir(&extra).unwrap();
    plan.items.push(DeletionItem {
        path: vortex_core::paths::HostPath::new(&extra).unwrap(),
        bytes: 0,
    });
    assert!(matches!(
        execute(&db, &paths, &plan),
        Err(Error::Validation(_))
    ));
    assert!(extra.exists());
    assert!(paths.managed_prefix(env).unwrap().as_path().exists());
    assert!(db.get_game(g).is_ok());
    plan.items.pop();
    execute(&db, &paths, &plan).unwrap();
    assert!(extra.exists());
    assert!(!paths.managed_prefix(env).unwrap().as_path().exists());
}

#[test]
fn every_install_source_is_protected() {
    let (tmp, paths, db) = setup();
    let other_src = tmp.path().join("src2");
    fs::create_dir_all(&other_src).unwrap();
    let (_e, other) = game(&db, &paths, "O");
    db.insert_install(&NewInstall {
        game_id: other,
        source_path: other_src.to_str().unwrap().into(),
        source_kind: SourceKind::Dir,
        installer_path: other_src.join("setup.exe").to_str().unwrap().into(),
        installer_args: "[]".into(),
    })
    .unwrap();
    // a games-root install dir that contains another game's installer source
    let d = paths.games_dir().join("X");
    fs::create_dir_all(d.join("inst")).unwrap();
    db.insert_install(&NewInstall {
        game_id: other,
        source_path: d.join("inst").to_str().unwrap().into(),
        source_kind: SourceKind::Dir,
        installer_path: d.join("inst/setup.exe").to_str().unwrap().into(),
        installer_args: "[]".into(),
    })
    .unwrap();
    let g = game_with_dir(&db, &paths, &d);
    assert!(plan_game_removal(&db, &paths, g).is_err());
    assert!(d.join("inst").exists());
}

#[test]
fn crashed_restore_is_recovered_not_deleted() {
    let (_t, paths, db) = setup();
    let (env, _) = game(&db, &paths, "R");
    let p = paths.managed_prefix(env).unwrap();
    let b = backup(&db, &paths, env, &AtomicBool::new(false)).unwrap();
    fs::write(p.as_path().join("mine"), "precious").unwrap();
    // simulate a crash after the first rename
    let old = paths.prefixes_dir().join(format!(".old-{}", env));
    fs::rename(p.as_path(), &old).unwrap();
    assert!(matches!(
        restore(&db, &paths, b.id, &p, false),
        Err(Error::Validation(_))
    ));
    assert_eq!(
        fs::read_to_string(p.as_path().join("mine")).unwrap(),
        "precious"
    );
    restore(&db, &paths, b.id, &p, true).unwrap();
    assert!(!old.exists());
}

#[test]
fn hostile_modes_are_neutralised_and_size_capped() {
    let (_t, paths, db) = setup();
    let id = evil_backup(&db, &paths, &[("f", false)], 0o4000, "/tmp");
    let target = paths.managed_prefix(3).unwrap();
    restore(&db, &paths, id, &target, false).unwrap();
    let m = fs::metadata(target.as_path().join("f"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(m & 0o7000, 0);
    assert_eq!(m & 0o600, 0o600);
    let t2 = paths.managed_prefix(4).unwrap();
    let r = restore_limited(&db, &paths, id, &t2, false, 0);
    assert!(matches!(r, Err(Error::Validation(_))));
    assert!(!t2.as_path().exists());
}

#[test]
fn backup_skips_fifos_and_restore_path_must_not_escape() {
    let (tmp, paths, db) = setup();
    let (env, _) = game(&db, &paths, "F");
    let p = paths.managed_prefix(env).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(p.as_path().join("pipe"))
            .status()
            .unwrap()
            .success()
    );
    let b = backup(&db, &paths, env, &AtomicBool::new(false)).unwrap();
    let t = paths.managed_prefix(50).unwrap();
    restore(&db, &paths, b.id, &t, false).unwrap();
    assert!(!t.as_path().join("pipe").exists());
    assert!(t.as_path().join("version").exists());
    // archive path with '..' that lexically starts under backups/
    let evil = paths.backups_dir().join("../../x.tar.gz");
    fs::copy(&b.archive_path, tmp.path().join("x.tar.gz")).unwrap();
    let id = db
        .insert_backup(&NewBackup {
            environment_id: None,
            game_name: "e".into(),
            archive_path: evil.to_str().unwrap().into(),
            runtime_path: "/rt".into(),
            prefix_version: None,
            size_bytes: 1,
        })
        .unwrap();
    let t2 = paths.managed_prefix(51).unwrap();
    assert!(matches!(
        restore(&db, &paths, id, &t2, false),
        Err(Error::InvalidPath(_))
    ));
}

#[test]
fn busy_environment_is_refused() {
    let (_t, paths, db) = setup();
    let (env, g) = game(&db, &paths, "A");
    db.insert_run(&NewRun {
        marker: "m".into(),
        kind: RunKind::Launch,
        environment_id: env,
        game_id: Some(g),
        install_id: None,
        argv: "[]".into(),
        log_path: "/l".into(),
    })
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(
        backup(&db, &paths, env, &AtomicBool::new(false)),
        Err(Error::Busy(_))
    ));
    assert!(matches!(
        plan_game_removal(&db, &paths, g),
        Err(Error::Busy(_))
    ));
}

#[test]
fn symlink_then_same_path_entry_never_touches_outside_file() {
    use std::os::unix::fs::PermissionsExt;
    for second in [("x", false), ("x/", false)] {
        let (tmp, paths, db) = setup();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, "secret").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o640)).unwrap();
        let id = evil_backup(
            &db,
            &paths,
            &[("x", true), second],
            0o4777,
            outside.to_str().unwrap(),
        );
        let target = paths.managed_prefix(5).unwrap();
        let r = restore(&db, &paths, id, &target, false);
        assert!(matches!(r, Err(Error::Validation(_))), "{second:?}: {r:?}");
        assert!(!target.as_path().exists());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "secret");
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode() & 0o7777,
            0o640
        );
    }
}
