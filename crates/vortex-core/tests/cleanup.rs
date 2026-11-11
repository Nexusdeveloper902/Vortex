//! M25: backup/runtime removal plans and log retention (tempdirs only).
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use vortex_core::db::*;
use vortex_core::delete::*;
use vortex_core::logs::{self, LogWriter};
use vortex_core::paths::Paths;

fn setup() -> (tempfile::TempDir, Paths, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path());
    fs::create_dir_all(paths.home()).unwrap();
    fs::create_dir_all(paths.backups_dir()).unwrap();
    fs::create_dir_all(paths.runtimes_dir()).unwrap();
    let db = Db::open(&paths.db_file()).unwrap();
    (tmp, paths, db)
}

fn backup_row(db: &Db, archive: &Path) -> i64 {
    db.insert_backup(&NewBackup {
        environment_id: None,
        game_name: "g".into(),
        archive_path: archive.to_string_lossy().into(),
        runtime_path: "/rt".into(),
        prefix_version: None,
        size_bytes: 3,
    })
    .unwrap()
}

#[test]
fn delete_backup_removes_file_and_row() {
    let (_t, paths, db) = setup();
    let f = paths.backups_dir().join("a.tar.gz");
    fs::write(&f, "abc").unwrap();
    let id = backup_row(&db, &f);
    let plan = plan_backup_removal(&db, &paths, id).unwrap();
    assert_eq!(plan.item.bytes, 3);
    execute_cleanup(&db, &paths, &plan).unwrap();
    assert!(!f.exists());
    assert!(db.get_backup(id).is_err());
}

#[test]
fn backup_outside_backups_dir_refused() {
    let (t, paths, db) = setup();
    let f = t.path().join("elsewhere.tar.gz");
    fs::write(&f, "abc").unwrap();
    let id = backup_row(&db, &f);
    assert!(plan_backup_removal(&db, &paths, id).is_err());
    assert!(f.exists());
}

#[test]
fn backup_symlink_removed_as_link() {
    let (t, paths, db) = setup();
    let target = t.path().join("precious");
    fs::write(&target, "keep").unwrap();
    let f = paths.backups_dir().join("l.tar.gz");
    symlink(&target, &f).unwrap();
    let id = backup_row(&db, &f);
    let plan = plan_backup_removal(&db, &paths, id).unwrap();
    execute_cleanup(&db, &paths, &plan).unwrap();
    assert!(fs::symlink_metadata(&f).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
}

#[test]
fn runtime_rules() {
    let (t, paths, db) = setup();
    let rt = paths.runtimes_dir().join("GE-Proton9");
    fs::create_dir_all(rt.join("files")).unwrap();
    let outside = t.path().join("precious");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("f"), "keep").unwrap();
    symlink(&outside, rt.join("files/link")).unwrap();

    // outside the managed dir (Steam's Proton, anything else): refused
    let steam = t.path().join("Steam/Proton 9");
    fs::create_dir_all(&steam).unwrap();
    assert!(plan_runtime_removal(&db, &paths, &steam).is_err());
    assert!(plan_runtime_removal(&db, &paths, &paths.runtimes_dir()).is_err());

    // in use: refused, message names the game
    let rt_s = fs::canonicalize(&rt).unwrap();
    let env = db
        .insert_environment("/p", true, rt_s.to_str().unwrap())
        .unwrap();
    db.insert_game(&NewGame::new("Cool Game", env)).unwrap();
    let e = plan_runtime_removal(&db, &paths, &rt).unwrap_err();
    assert!(e.to_string().contains("Cool Game"), "{e}");
    assert!(rt.exists());

    // default runtime: refused
    db.set_setting("default_runtime", rt_s.to_str().unwrap())
        .unwrap();
    let e = plan_runtime_removal(&db, &paths, &rt).unwrap_err();
    assert!(e.to_string().contains("default"), "{e}");
    db.set_setting("default_runtime", "/x").unwrap();

    // unused: removed, symlink target untouched
    db.set_environment_runtime(env, "/other").unwrap();
    let plan = plan_runtime_removal(&db, &paths, &rt).unwrap();
    execute_cleanup(&db, &paths, &plan).unwrap();
    assert!(!rt.exists());
    assert_eq!(fs::read_to_string(outside.join("f")).unwrap(), "keep");
}

#[test]
fn log_prune_keeps_newest_and_running() {
    let (_t, paths, db) = setup();
    let env = db.insert_environment("/p", true, "/rt").unwrap();
    let mut ids = Vec::new();
    for i in 0..6 {
        let path = LogWriter::log_path(&paths, 1, 1000 + i, RunKind::Launch);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "x").unwrap();
        fs::create_dir_all(logs::proton_log_dir(&path)).unwrap();
        let id = db
            .insert_run(&NewRun {
                marker: format!("m{i}"),
                kind: RunKind::Launch,
                environment_id: env,
                game_id: None,
                install_id: None,
                argv: "[]".into(),
                log_path: path.to_string_lossy().into(),
            })
            .unwrap();
        // run 0 (oldest) stays running; others finished
        if i > 0 {
            db.finish_run(id, RunState::Finished, Some(0), false)
                .unwrap();
        }
        ids.push(path);
    }
    // keep 2 newest (4, 5) + running oldest (0); prune 1, 2, 3
    assert_eq!(logs::prune(&db, &paths, 2).unwrap(), 3);
    for (i, p) in ids.iter().enumerate() {
        let kept = matches!(i, 0 | 4 | 5);
        assert_eq!(p.exists(), kept, "log {i}");
        assert_eq!(logs::proton_log_dir(p).exists(), kept, "proton {i}");
    }
}
