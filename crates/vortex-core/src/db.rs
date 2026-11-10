//! SQLite access, schema migrations and startup recovery. The only module with SQL.
//!
//! Host paths are stored as plain text (see ARCHITECTURE section 9) and handed out as `String`
//! columns in the row structs; callers wrap them in `HostPath`/`PrefixDir` at the boundary.
//! New queries from later milestones are appended at the end of this file.

use crate::error::{Error, Result};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_init.sql")];

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// String-backed enum stored in a CHECK-constrained TEXT column.
macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($var),+ }
        impl $name {
            pub fn as_str(self) -> &'static str { match self { $(Self::$var => $s),+ } }
            fn parse(s: &str) -> rusqlite::Result<Self> {
                match s {
                    $($s => Ok(Self::$var),)+
                    _ => Err(rusqlite::Error::FromSqlConversionFailure(
                        0, rusqlite::types::Type::Text, format!("bad {}: {s}", stringify!($name)).into())),
                }
            }
        }
    };
}

str_enum!(GameStatus { Installing = "installing", Installed = "installed", Failed = "failed", Interrupted = "interrupted" });
str_enum!(SourceKind { File = "file", Dir = "dir" });
str_enum!(InstallState {
    Created = "created", PrefixInit = "prefix_init", Running = "running", Waiting = "waiting",
    SelectExe = "select_exe", Done = "done", Failed = "failed", Cancelled = "cancelled",
    Interrupted = "interrupted",
});
str_enum!(RunKind { PrefixInit = "prefix_init", Install = "install", Launch = "launch", Exec = "exec", Winetricks = "winetricks" });
str_enum!(RunState { Running = "running", Waiting = "waiting", Finished = "finished", Stopped = "stopped", Interrupted = "interrupted" });
str_enum!(DepState { Installed = "installed", Failed = "failed" });

#[derive(Debug, Clone, PartialEq)]
pub struct Environment {
    pub id: i64,
    pub prefix_path: String,
    pub prefix_managed: bool,
    pub runtime_path: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewGame {
    pub name: String,
    pub environment_id: i64,
    pub status: GameStatus,
    pub install_dir: Option<String>,
    pub install_dir_managed: bool,
    pub exe_path: Option<String>,
    pub work_dir: Option<String>,
    /// JSON array of strings.
    pub launch_args: String,
    /// JSON object string -> string.
    pub env_vars: String,
}

impl NewGame {
    pub fn new(name: &str, environment_id: i64) -> Self {
        Self {
            name: name.into(),
            environment_id,
            status: GameStatus::Installing,
            install_dir: None,
            install_dir_managed: false,
            exe_path: None,
            work_dir: None,
            launch_args: "[]".into(),
            env_vars: "{}".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Game {
    pub id: i64,
    pub name: String,
    pub environment_id: i64,
    pub status: GameStatus,
    pub install_dir: Option<String>,
    pub install_dir_managed: bool,
    pub exe_path: Option<String>,
    pub work_dir: Option<String>,
    pub launch_args: String,
    pub env_vars: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewInstall {
    pub game_id: i64,
    pub source_path: String,
    pub source_kind: SourceKind,
    pub installer_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Install {
    pub id: i64,
    pub game_id: i64,
    pub source_path: String,
    pub source_kind: SourceKind,
    pub installer_path: String,
    pub state: InstallState,
    pub failure: Option<String>,
    pub started_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewRun {
    pub marker: String,
    pub kind: RunKind,
    pub environment_id: i64,
    pub game_id: Option<i64>,
    pub install_id: Option<i64>,
    /// JSON, already redacted.
    pub argv: String,
    pub log_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub id: i64,
    pub marker: String,
    pub kind: RunKind,
    pub environment_id: i64,
    pub game_id: Option<i64>,
    pub install_id: Option<i64>,
    pub argv: String,
    pub pid: Option<i64>,
    pub pid_start: Option<i64>,
    pub state: RunState,
    pub exit_code: Option<i64>,
    pub user_forced: bool,
    pub findings: Option<String>,
    pub log_path: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dependency {
    pub environment_id: i64,
    pub verb: String,
    pub run_id: Option<i64>,
    pub state: DepState,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewBackup {
    pub environment_id: Option<i64>,
    pub game_name: String,
    pub archive_path: String,
    pub runtime_path: String,
    pub prefix_version: Option<String>,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Backup {
    pub id: i64,
    pub environment_id: Option<i64>,
    pub game_name: String,
    pub archive_path: String,
    pub runtime_path: String,
    pub prefix_version: Option<String>,
    pub size_bytes: i64,
    pub created_at: i64,
}

/// What `recover_on_startup` changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    pub runs: usize,
    pub installs: usize,
}

pub struct Db {
    conn: Connection,
}

impl Db {
    /// Opens (creating if needed) and migrates the database. Refuses a newer schema; copies the
    /// file to `<db>.bak-v<N>` before migrating an existing non-empty one.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(Error::io(dir))?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let mut db = Self { conn };
        db.migrate(path)?;
        Ok(db)
    }

    fn migrate(&mut self, path: &Path) -> Result<()> {
        let found: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        let supported = MIGRATIONS.len() as i64;
        if found > supported {
            return Err(Error::SchemaTooNew { found, supported });
        }
        if found == supported {
            return Ok(());
        }
        if found > 0 {
            self.conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            let mut bak = path.as_os_str().to_owned();
            bak.push(format!(".bak-v{found}"));
            std::fs::copy(path, &bak).map_err(Error::io(path))?;
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", i as i64 + 1)?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn user_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    // ---- settings ----

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2",
            [key, value],
        )?;
        Ok(())
    }

    // ---- environments ----

    pub fn insert_environment(&self, prefix: &str, managed: bool, runtime: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO environments(prefix_path,prefix_managed,runtime_path,created_at) VALUES(?1,?2,?3,?4)",
            params![prefix, managed, runtime, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_environment(&self, id: i64) -> Result<Environment> {
        self.conn
            .query_row("SELECT id,prefix_path,prefix_managed,runtime_path,created_at FROM environments WHERE id=?1", [id], env_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("environment {id}")))
    }

    pub fn list_environments(&self) -> Result<Vec<Environment>> {
        collect(
            &self.conn,
            "SELECT id,prefix_path,prefix_managed,runtime_path,created_at FROM environments ORDER BY id",
            [],
            env_row,
        )
    }

    pub fn set_environment_runtime(&self, id: i64, runtime: &str) -> Result<()> {
        self.expect_one(
            self.conn.execute(
                "UPDATE environments SET runtime_path=?2 WHERE id=?1",
                params![id, runtime],
            )?,
            "environment",
            id,
        )
    }

    /// Fails with a foreign-key error while games still reference the environment.
    pub fn delete_environment(&self, id: i64) -> Result<()> {
        self.expect_one(
            self.conn
                .execute("DELETE FROM environments WHERE id=?1", [id])?,
            "environment",
            id,
        )
    }

    pub fn games_in_environment(&self, id: i64) -> Result<Vec<Game>> {
        collect(
            &self.conn,
            &format!("{GAME_SELECT} WHERE environment_id=?1 ORDER BY id"),
            [id],
            game_row,
        )
    }

    // ---- games ----

    pub fn insert_game(&self, g: &NewGame) -> Result<i64> {
        let t = now();
        self.conn.execute(
            "INSERT INTO games(name,environment_id,status,install_dir,install_dir_managed,exe_path,work_dir,launch_args,env_vars,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
            params![g.name, g.environment_id, g.status.as_str(), g.install_dir, g.install_dir_managed, g.exe_path, g.work_dir, g.launch_args, g.env_vars, t],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_game(&self, id: i64) -> Result<Game> {
        self.conn
            .query_row(&format!("{GAME_SELECT} WHERE id=?1"), [id], game_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("game {id}")))
    }

    pub fn list_games(&self) -> Result<Vec<Game>> {
        collect(
            &self.conn,
            &format!("{GAME_SELECT} ORDER BY name COLLATE NOCASE, id"),
            [],
            game_row,
        )
    }

    /// Fails (CHECK) when setting `installed` on a game without `exe_path`.
    pub fn set_game_status(&self, id: i64, status: GameStatus) -> Result<()> {
        self.expect_one(
            self.conn.execute(
                "UPDATE games SET status=?2,updated_at=?3 WHERE id=?1",
                params![id, status.as_str(), now()],
            )?,
            "game",
            id,
        )
    }

    /// Records the user-confirmed exe and marks the game installed in one statement.
    /// The caller (install::confirm_exe) must have checked that the file exists.
    pub fn set_game_installed(
        &self,
        id: i64,
        exe_path: &str,
        work_dir: Option<&str>,
    ) -> Result<()> {
        self.expect_one(
            self.conn.execute("UPDATE games SET status='installed',exe_path=?2,work_dir=?3,updated_at=?4 WHERE id=?1", params![id, exe_path, work_dir, now()])?,
            "game",
            id,
        )
    }

    pub fn update_game_settings(
        &self,
        id: i64,
        name: &str,
        exe_path: Option<&str>,
        work_dir: Option<&str>,
        launch_args: &str,
        env_vars: &str,
    ) -> Result<()> {
        self.expect_one(
            self.conn.execute(
                "UPDATE games SET name=?2,exe_path=?3,work_dir=?4,launch_args=?5,env_vars=?6,updated_at=?7 WHERE id=?1",
                params![id, name, exe_path, work_dir, launch_args, env_vars, now()],
            )?,
            "game",
            id,
        )
    }

    /// Cascades to the game's installs; runs keep existing with `game_id` NULL.
    pub fn delete_game(&self, id: i64) -> Result<()> {
        self.expect_one(
            self.conn.execute("DELETE FROM games WHERE id=?1", [id])?,
            "game",
            id,
        )
    }

    // ---- installs ----

    pub fn insert_install(&self, i: &NewInstall) -> Result<i64> {
        let t = now();
        self.conn.execute(
            "INSERT INTO installs(game_id,source_path,source_kind,installer_path,state,started_at,updated_at) VALUES(?1,?2,?3,?4,'created',?5,?5)",
            params![i.game_id, i.source_path, i.source_kind.as_str(), i.installer_path, t],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_install(&self, id: i64) -> Result<Install> {
        self.conn
            .query_row(&format!("{INSTALL_SELECT} WHERE id=?1"), [id], install_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("install {id}")))
    }

    /// Latest install of a game, if any.
    pub fn latest_install_for_game(&self, game_id: i64) -> Result<Option<Install>> {
        Ok(self
            .conn
            .query_row(
                &format!("{INSTALL_SELECT} WHERE game_id=?1 ORDER BY id DESC LIMIT 1"),
                [game_id],
                install_row,
            )
            .optional()?)
    }

    /// One transaction: `installs.state` (+ failure) and, when given, `games.status` together.
    pub fn set_install_state(
        &mut self,
        id: i64,
        state: InstallState,
        failure: Option<&str>,
        game_status: Option<GameStatus>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let t = now();
        let n = tx.execute(
            "UPDATE installs SET state=?2,failure=?3,updated_at=?4 WHERE id=?1",
            params![id, state.as_str(), failure, t],
        )?;
        if n == 0 {
            return Err(Error::NotFound(format!("install {id}")));
        }
        if let Some(s) = game_status {
            tx.execute("UPDATE games SET status=?2,updated_at=?3 WHERE id=(SELECT game_id FROM installs WHERE id=?1)", params![id, s.as_str(), t])?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- runs ----

    pub fn insert_run(&self, r: &NewRun) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs(marker,kind,environment_id,game_id,install_id,argv,state,log_path,started_at) VALUES(?1,?2,?3,?4,?5,?6,'running',?7,?8)",
            params![r.marker, r.kind.as_str(), r.environment_id, r.game_id, r.install_id, r.argv, r.log_path, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_run(&self, id: i64) -> Result<Run> {
        self.conn
            .query_row(&format!("{RUN_SELECT} WHERE id=?1"), [id], run_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("run {id}")))
    }

    pub fn set_run_pid(&self, id: i64, pid: i64, pid_start: Option<i64>) -> Result<()> {
        self.expect_one(
            self.conn.execute(
                "UPDATE runs SET pid=?2,pid_start=?3 WHERE id=?1",
                params![id, pid, pid_start],
            )?,
            "run",
            id,
        )
    }

    pub fn set_run_state(&self, id: i64, state: RunState) -> Result<()> {
        self.expect_one(
            self.conn.execute(
                "UPDATE runs SET state=?2 WHERE id=?1",
                params![id, state.as_str()],
            )?,
            "run",
            id,
        )
    }

    /// Terminal state: sets `ended_at` now.
    pub fn finish_run(
        &self,
        id: i64,
        state: RunState,
        exit_code: Option<i64>,
        user_forced: bool,
    ) -> Result<()> {
        self.expect_one(
            self.conn.execute(
                "UPDATE runs SET state=?2,exit_code=?3,user_forced=?4,ended_at=?5 WHERE id=?1",
                params![id, state.as_str(), exit_code, user_forced, now()],
            )?,
            "run",
            id,
        )
    }

    /// `findings` is the JSON produced by diagnose.
    pub fn set_run_findings(&self, id: i64, findings: &str) -> Result<()> {
        self.expect_one(
            self.conn.execute(
                "UPDATE runs SET findings=?2 WHERE id=?1",
                params![id, findings],
            )?,
            "run",
            id,
        )
    }

    pub fn active_runs(&self) -> Result<Vec<Run>> {
        collect(
            &self.conn,
            &format!("{RUN_SELECT} WHERE state IN ('running','waiting') ORDER BY id"),
            [],
            run_row,
        )
    }

    pub fn active_run_for_environment(&self, env: i64) -> Result<Option<Run>> {
        Ok(self.conn.query_row(&format!("{RUN_SELECT} WHERE environment_id=?1 AND state IN ('running','waiting') LIMIT 1"), [env], run_row).optional()?)
    }

    pub fn runs_for_game(&self, game_id: i64) -> Result<Vec<Run>> {
        collect(
            &self.conn,
            &format!("{RUN_SELECT} WHERE game_id=?1 ORDER BY started_at DESC, id DESC"),
            [game_id],
            run_row,
        )
    }

    /// "Last launch information": newest `launch` run of the game.
    pub fn last_launch(&self, game_id: i64) -> Result<Option<Run>> {
        Ok(self.conn.query_row(&format!("{RUN_SELECT} WHERE game_id=?1 AND kind='launch' ORDER BY started_at DESC, id DESC LIMIT 1"), [game_id], run_row).optional()?)
    }

    // ---- dependencies ----

    pub fn upsert_dependency(
        &self,
        env: i64,
        verb: &str,
        run_id: Option<i64>,
        state: DepState,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dependencies(environment_id,verb,run_id,state,at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(environment_id,verb) DO UPDATE SET run_id=?3,state=?4,at=?5",
            params![env, verb, run_id, state.as_str(), now()],
        )?;
        Ok(())
    }

    pub fn list_dependencies(&self, env: i64) -> Result<Vec<Dependency>> {
        collect(
            &self.conn,
            "SELECT environment_id,verb,run_id,state,at FROM dependencies WHERE environment_id=?1 ORDER BY verb",
            [env],
            |r| {
                Ok(Dependency {
                    environment_id: r.get(0)?,
                    verb: r.get(1)?,
                    run_id: r.get(2)?,
                    state: DepState::parse(&r.get::<_, String>(3)?)?,
                    at: r.get(4)?,
                })
            },
        )
    }

    // ---- backups ----

    pub fn insert_backup(&self, b: &NewBackup) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO backups(environment_id,game_name,archive_path,runtime_path,prefix_version,size_bytes,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![b.environment_id, b.game_name, b.archive_path, b.runtime_path, b.prefix_version, b.size_bytes, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_backup(&self, id: i64) -> Result<Backup> {
        self.conn
            .query_row(&format!("{BACKUP_SELECT} WHERE id=?1"), [id], backup_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("backup {id}")))
    }

    pub fn list_backups(&self) -> Result<Vec<Backup>> {
        collect(
            &self.conn,
            &format!("{BACKUP_SELECT} ORDER BY created_at DESC, id DESC"),
            [],
            backup_row,
        )
    }

    pub fn delete_backup(&self, id: i64) -> Result<()> {
        self.expect_one(
            self.conn.execute("DELETE FROM backups WHERE id=?1", [id])?,
            "backup",
            id,
        )
    }

    // ---- recovery ----

    /// Run once at startup. Active runs whose process is gone (pid + start time mismatch) and
    /// whose `marker_alive(marker)` is false become `interrupted`; installs in
    /// `prefix_init|running|waiting` left without an active run become `interrupted`, as do
    /// their `installing` games. `marker_alive` is `process::scan_marker(..)` non-empty in the app.
    pub fn recover_on_startup(&mut self, marker_alive: &dyn Fn(&str) -> bool) -> Result<Recovery> {
        let mut rec = Recovery::default();
        let tx = self.conn.transaction()?;
        let t = now();
        let active: Vec<Run> = collect(
            &tx,
            &format!("{RUN_SELECT} WHERE state IN ('running','waiting')"),
            [],
            run_row,
        )?;
        for r in active {
            let alive = match (r.pid, r.pid_start) {
                (Some(p), Some(s)) => proc_start_time(p) == Some(s),
                _ => false,
            };
            if !alive && !marker_alive(&r.marker) {
                tx.execute(
                    "UPDATE runs SET state='interrupted',ended_at=?2 WHERE id=?1",
                    params![r.id, t],
                )?;
                rec.runs += 1;
            }
        }
        let stale = "state IN ('prefix_init','running','waiting')
                     AND NOT EXISTS (SELECT 1 FROM runs WHERE runs.install_id=installs.id AND runs.state IN ('running','waiting'))";
        tx.execute(
            &format!(
                "UPDATE games SET status='interrupted',updated_at=?1 WHERE status='installing'
                      AND id IN (SELECT game_id FROM installs WHERE {stale})"
            ),
            [t],
        )?;
        rec.installs = tx.execute(
            &format!("UPDATE installs SET state='interrupted',updated_at=?1 WHERE {stale}"),
            [t],
        )?;
        tx.commit()?;
        Ok(rec)
    }

    fn expect_one(&self, n: usize, what: &str, id: i64) -> Result<()> {
        if n == 0 {
            Err(Error::NotFound(format!("{what} {id}")))
        } else {
            Ok(())
        }
    }
}

/// Start time (clock ticks since boot, `/proc/<pid>/stat` field 22) or `None` if the pid is gone.
/// Use it to fill `runs.pid_start` right after spawning.
pub fn proc_start_time(pid: i64) -> Option<i64> {
    parse_start_time(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

fn parse_start_time(stat: &str) -> Option<i64> {
    // comm (field 2) may contain spaces and parens: split after the last ')'.
    // After it, field 3 is first, so field 22 is index 19.
    stat.get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

const GAME_SELECT: &str = "SELECT id,name,environment_id,status,install_dir,install_dir_managed,exe_path,work_dir,launch_args,env_vars,created_at,updated_at FROM games";
const INSTALL_SELECT: &str = "SELECT id,game_id,source_path,source_kind,installer_path,state,failure,started_at,updated_at FROM installs";
const RUN_SELECT: &str = "SELECT id,marker,kind,environment_id,game_id,install_id,argv,pid,pid_start,state,exit_code,user_forced,findings,log_path,started_at,ended_at FROM runs";
const BACKUP_SELECT: &str = "SELECT id,environment_id,game_name,archive_path,runtime_path,prefix_version,size_bytes,created_at FROM backups";

fn collect<T, P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    p: P,
    f: impl FnMut(&Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    Ok(conn
        .prepare(sql)?
        .query_map(p, f)?
        .collect::<rusqlite::Result<_>>()?)
}

fn env_row(r: &Row) -> rusqlite::Result<Environment> {
    Ok(Environment {
        id: r.get(0)?,
        prefix_path: r.get(1)?,
        prefix_managed: r.get(2)?,
        runtime_path: r.get(3)?,
        created_at: r.get(4)?,
    })
}

fn game_row(r: &Row) -> rusqlite::Result<Game> {
    Ok(Game {
        id: r.get(0)?,
        name: r.get(1)?,
        environment_id: r.get(2)?,
        status: GameStatus::parse(&r.get::<_, String>(3)?)?,
        install_dir: r.get(4)?,
        install_dir_managed: r.get(5)?,
        exe_path: r.get(6)?,
        work_dir: r.get(7)?,
        launch_args: r.get(8)?,
        env_vars: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

fn install_row(r: &Row) -> rusqlite::Result<Install> {
    Ok(Install {
        id: r.get(0)?,
        game_id: r.get(1)?,
        source_path: r.get(2)?,
        source_kind: SourceKind::parse(&r.get::<_, String>(3)?)?,
        installer_path: r.get(4)?,
        state: InstallState::parse(&r.get::<_, String>(5)?)?,
        failure: r.get(6)?,
        started_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn run_row(r: &Row) -> rusqlite::Result<Run> {
    Ok(Run {
        id: r.get(0)?,
        marker: r.get(1)?,
        kind: RunKind::parse(&r.get::<_, String>(2)?)?,
        environment_id: r.get(3)?,
        game_id: r.get(4)?,
        install_id: r.get(5)?,
        argv: r.get(6)?,
        pid: r.get(7)?,
        pid_start: r.get(8)?,
        state: RunState::parse(&r.get::<_, String>(9)?)?,
        exit_code: r.get(10)?,
        user_forced: r.get(11)?,
        findings: r.get(12)?,
        log_path: r.get(13)?,
        started_at: r.get(14)?,
        ended_at: r.get(15)?,
    })
}

fn backup_row(r: &Row) -> rusqlite::Result<Backup> {
    Ok(Backup {
        id: r.get(0)?,
        environment_id: r.get(1)?,
        game_name: r.get(2)?,
        archive_path: r.get(3)?,
        runtime_path: r.get(4)?,
        prefix_version: r.get(5)?,
        size_bytes: r.get(6)?,
        created_at: r.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_parsing_handles_odd_comm() {
        let stat = "123 (we ird) name)) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 4242 20";
        assert_eq!(parse_start_time(stat), Some(4242));
        assert!(proc_start_time(i64::from(std::process::id())).is_some());
    }
}
