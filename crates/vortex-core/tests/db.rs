use vortex_core::db::*;
use vortex_core::error::Error;
use vortex_core::paths::Paths;

fn open(tmp: &tempfile::TempDir) -> (Paths, Db) {
    let paths = Paths::under(tmp.path());
    let db = Db::open(&paths.db_file()).unwrap();
    (paths, db)
}

fn seed(db: &mut Db) -> (i64, i64, i64) {
    let env = db.insert_environment("/p/1", true, "/rt").unwrap();
    let game = db.insert_game(&NewGame::new("G", env)).unwrap();
    let inst = db
        .insert_install(&NewInstall {
            game_id: game,
            source_path: "/s".into(),
            source_kind: SourceKind::Dir,
            installer_path: "/s/setup.exe".into(),
        })
        .unwrap();
    db.set_install_state(inst, InstallState::Running, None, None)
        .unwrap();
    (env, game, inst)
}

fn new_run(env: i64, game: i64, inst: i64, marker: &str) -> NewRun {
    NewRun {
        marker: marker.into(),
        kind: RunKind::Install,
        environment_id: env,
        game_id: Some(game),
        install_id: Some(inst),
        argv: "[]".into(),
        log_path: "/l".into(),
    }
}

#[test]
fn fresh_db_is_current_with_fk_and_wal() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, db) = open(&tmp);
    assert_eq!(db.user_version().unwrap(), 2);
    // FK enforced: game with unknown environment is rejected.
    assert!(db.insert_game(&NewGame::new("x", 99)).is_err());
}

#[test]
fn newer_schema_refuses_to_open() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path());
    std::fs::create_dir_all(paths.data_dir()).unwrap();
    rusqlite::Connection::open(paths.db_file())
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    assert!(matches!(
        Db::open(&paths.db_file()),
        Err(Error::SchemaTooNew { found: 99, .. })
    ));
}

#[test]
fn reopen_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let (paths, db) = open(&tmp);
    db.set_setting("default_runtime", "/rt").unwrap();
    drop(db);
    let db = Db::open(&paths.db_file()).unwrap();
    assert_eq!(
        db.get_setting("default_runtime").unwrap().as_deref(),
        Some("/rt")
    );
}

#[test]
fn check_rejects_installed_without_exe() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, db) = open(&tmp);
    let env = db.insert_environment("/p/1", true, "/rt").unwrap();
    let mut g = NewGame::new("G", env);
    g.status = GameStatus::Installed;
    assert!(db.insert_game(&g).is_err());
    g.status = GameStatus::Installing;
    let id = db.insert_game(&g).unwrap();
    assert!(db.set_game_status(id, GameStatus::Installed).is_err());
    db.set_game_installed(id, "/g/a.exe", None).unwrap();
    assert_eq!(db.get_game(id).unwrap().status, GameStatus::Installed);
}

#[test]
fn recovery_interrupts_dead_runs_and_installs() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, mut db) = open(&tmp);
    let (env, game, inst) = seed(&mut db);
    let run = db.insert_run(&new_run(env, game, inst, "m1")).unwrap();
    // Dead pid: pid_start can never match.
    db.set_run_pid(run, i64::from(std::process::id()), Some(-1))
        .unwrap();
    let rec = db.recover_on_startup(&|_| false).unwrap();
    assert_eq!(
        rec,
        Recovery {
            runs: 1,
            installs: 1
        }
    );
    assert_eq!(db.get_run(run).unwrap().state, RunState::Interrupted);
    assert!(db.get_run(run).unwrap().ended_at.is_some());
    assert_eq!(
        db.get_install(inst).unwrap().state,
        InstallState::Interrupted
    );
    assert_eq!(db.get_game(game).unwrap().status, GameStatus::Interrupted);
}

#[test]
fn recovery_keeps_live_pid_and_live_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, mut db) = open(&tmp);
    let (env, game, inst) = seed(&mut db);
    let me = i64::from(std::process::id());
    let r1 = db
        .insert_run(&new_run(env, game, inst, "alive-pid"))
        .unwrap();
    db.set_run_pid(r1, me, proc_start_time(me)).unwrap();
    let r2 = db
        .insert_run(&new_run(env, game, inst, "alive-marker"))
        .unwrap();
    let rec = db.recover_on_startup(&|m| m == "alive-marker").unwrap();
    assert_eq!(rec, Recovery::default());
    assert_eq!(db.get_run(r1).unwrap().state, RunState::Running);
    assert_eq!(db.get_run(r2).unwrap().state, RunState::Running);
    assert_eq!(db.get_game(game).unwrap().status, GameStatus::Installing);
}

#[test]
fn install_transition_updates_game_atomically_and_queries_work() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, mut db) = open(&tmp);
    let (env, game, inst) = seed(&mut db);
    db.set_install_state(
        inst,
        InstallState::Failed,
        Some("InstallerFailure"),
        Some(GameStatus::Failed),
    )
    .unwrap();
    assert_eq!(
        db.get_install(inst).unwrap().failure.as_deref(),
        Some("InstallerFailure")
    );
    assert_eq!(db.get_game(game).unwrap().status, GameStatus::Failed);
    // environment still referenced
    assert!(db.delete_environment(env).is_err());
    let run = db
        .insert_run(&NewRun {
            kind: RunKind::Launch,
            ..new_run(env, game, inst, "m")
        })
        .unwrap();
    db.finish_run(run, RunState::Finished, Some(0), false)
        .unwrap();
    assert_eq!(db.last_launch(game).unwrap().unwrap().exit_code, Some(0));
    db.upsert_dependency(env, "vcrun2019", Some(run), DepState::Installed)
        .unwrap();
    db.upsert_dependency(env, "vcrun2019", None, DepState::Failed)
        .unwrap();
    assert_eq!(db.list_dependencies(env).unwrap().len(), 1);
    let b = db
        .insert_backup(&NewBackup {
            environment_id: Some(env),
            game_name: "G".into(),
            archive_path: "/b".into(),
            runtime_path: "/rt".into(),
            prefix_version: None,
            size_bytes: 1,
        })
        .unwrap();
    assert_eq!(db.list_backups().unwrap()[0].id, b);
    db.delete_game(game).unwrap();
    db.delete_environment(env).unwrap();
    assert!(db.get_backup(b).unwrap().environment_id.is_none());
}

#[test]
fn db_and_dirs_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let (paths, db) = open(&tmp);
    db.set_setting("k", "v").unwrap();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(paths.data_dir()), 0o700);
    assert_eq!(mode(&paths.db_file()), 0o600);
}

#[test]
fn runs_for_environment_includes_gameless_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, mut db) = open(&tmp);
    let (env, game, inst) = seed(&mut db);
    db.insert_run(&new_run(env, game, inst, "a")).unwrap();
    let mut loose = new_run(env, game, inst, "b");
    loose.kind = RunKind::Launch;
    loose.game_id = None;
    loose.install_id = None;
    db.insert_run(&loose).unwrap();
    assert_eq!(db.runs_for_game(game).unwrap().len(), 1);
    assert_eq!(db.runs_for_environment(env, game).unwrap().len(), 2);
}
