use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use vortex_core::backup::{backup, restore};
use vortex_core::db::*;
use vortex_core::delete::*;
use vortex_core::error::Error;
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

fn evil_backup(db: &Db, paths: &Paths, entries: &[(&str, bool)]) -> i64 {
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
            h.set_link_name("/tmp").unwrap();
        } else {
            h.set_size(1);
        }
        h.set_mode(0o644);
        // write the name bytes directly so '..' and '/' survive
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_cksum();
        raw.extend_from_slice(h.as_bytes());
        if !*link {
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
        let id = evil_backup(&db, &paths, &entries);
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
    execute(&db, &paths, &plan).unwrap();
    assert!(ext.exists());

    // env still referenced by another game: environment plan refused, game plan keeps prefix
    let (e, g1) = game(&db, &paths, "S1");
    db.insert_game(&NewGame::new("S2", e)).unwrap();
    assert!(plan_environment_removal(&db, &paths, e).is_err());
    let plan = plan_game_removal(&db, &paths, g1).unwrap();
    assert!(!plan.remove_env && plan.items.is_empty());
}

/// Game in a fresh env whose install dir is `dir` (managed).
fn game_with_dir(db: &Db, paths: &Paths, dir: &Path) -> i64 {
    let (env, _) = game(db, paths, "D");
    let mut ng = NewGame::new("D2", env);
    ng.install_dir = Some(dir.to_str().unwrap().into());
    ng.install_dir_managed = true;
    db.insert_game(&ng).unwrap()
}

#[test]
fn install_dir_rules_and_toctou() {
    let (tmp, paths, db) = setup();
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("inst")).unwrap();
    let inst_dir = tmp.path().join("games/A");
    fs::create_dir_all(&inst_dir).unwrap();
    fs::write(inst_dir.join("f"), "1").unwrap();
    let g = game_with_dir(&db, &paths, &inst_dir);
    let plan = plan_game_removal(&db, &paths, g).unwrap();
    assert_eq!(
        plan.items.len(),
        1,
        "install dir only: env still has game D"
    );
    // plan changes between show and execute: abort, nothing deleted
    fs::write(inst_dir.join("g"), "2").unwrap();
    fs::remove_dir_all(&inst_dir).unwrap();
    assert!(execute(&db, &paths, &plan).is_err());
    // dangerous install dirs are refused
    fs::create_dir_all(&inst_dir).unwrap();
    for bad in [
        Path::new("/"),
        paths.home(),
        paths.data_dir(),
        &paths.backups_dir(),
    ] {
        fs::create_dir_all(bad).ok();
        let g = game_with_dir(&db, &paths, bad);
        assert!(plan_game_removal(&db, &paths, g).is_err(), "{bad:?}");
    }
    // install dir containing the installer source is refused
    let g = game_with_dir(&db, &paths, &src);
    db.insert_install(&NewInstall {
        game_id: g,
        source_path: src.join("inst").to_str().unwrap().into(),
        source_kind: SourceKind::Dir,
        installer_path: src.join("inst/setup.exe").to_str().unwrap().into(),
    })
    .unwrap();
    assert!(plan_game_removal(&db, &paths, g).is_err());
    assert!(src.join("inst").exists());
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
