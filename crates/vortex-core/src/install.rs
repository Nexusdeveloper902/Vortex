//! Install state machine and crash recovery (ARCHITECTURE section 7, spec 5.3 and 15).
//!
//! ```text
//! begin: Created -> PrefixInit -> Running -> Waiting -> SelectExe -> (confirm_exe) Done
//!                       | fail       | fail/stop          | user: "installer failed"
//!                       v            v                    v
//!                     Failed     Failed / Cancelled     Failed
//! startup recovery: Created | PrefixInit | Running | Waiting without a live run -> Interrupted
//! retry:                   Failed | Cancelled | Interrupted -> (PrefixInit ->) Running
//! mark_installer_finished: Failed | Interrupted -> SelectExe
//! ```
//!
//! Every state is persisted before its action starts. `games.status = 'installed'` is set only by
//! [`Installs::confirm_exe`] (a user-chosen exe that exists) or [`Installs::register_existing`];
//! no exit code ever marks an install as done. The installer source is referenced in place,
//! never copied. Events for the UI are plain data over `async-channel`.

use crate::db::{
    Db, GameStatus, Install, InstallState, NewEnv, NewGame, NewInstall, Recovery, Run, RunKind,
    SourceKind,
};
use crate::diagnose::{self, Class, Confidence, Finding, Preflight};
use crate::error::{EnvId, Error, Result};
use crate::paths::{HostPath, Paths, PrefixDir, WinPath, create_private_dir, to_win};
use crate::process::{
    Exit, Outcome, Phase, ProcInfo, RunEvent, RunHandle, RunSpec, Runs, scan_marker,
};
use crate::runner::{Invocation, Program, SystemExe};
use async_channel::{Receiver, Sender};
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// What the install wizard collected (spec 5.3 steps 1-6).
#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub name: String,
    /// What the user picked: an installer file or an installer directory. Never modified.
    pub source: HostPath,
    /// The primary installer: one of `source::analyze(&source).candidates`.
    pub installer: HostPath,
    /// Proton dir (`PROTONPATH`).
    pub runtime: HostPath,
    /// Where the user wants the game. Under `Paths::games_dir()` it is created and owned by
    /// Vortex; anywhere else it must already exist. Scanned for exes after the install.
    pub install_dir: Option<HostPath>,
}

/// A possible game exe found after the installer finished. The user always confirms or browses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExeCandidate {
    pub path: HostPath,
    /// Windows view for display (`C:\...` inside the prefix).
    pub win: WinPath,
    pub score: i32,
    pub size: u64,
    pub reasons: Vec<String>,
}

/// Plain-data events of one install attempt. The last event is `State` with a resting state
/// (`SelectExe`, `Failed` or `Cancelled`); after it the channel closes.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallEvent {
    /// A transition that has been persisted.
    State(InstallState),
    /// Forwarded from the run supervisor (`PrefixInit` or `Install` run).
    Run { kind: RunKind, event: RunEvent },
    /// A diagnostic finding. `MissingInstallerData` arrives live while the installer runs when
    /// the source disappears (the UI offers Stop); the rest after a run ends.
    Finding(Finding),
    /// Sent right before `State(SelectExe)`.
    Candidates(Vec<ExeCandidate>),
    /// Something prevented a step (spawn refused, database error). `State(Failed)` follows.
    Error(String),
}

/// Result of [`Installs::begin`].
pub struct Started {
    pub install_id: i64,
    pub game_id: i64,
    pub env_id: EnvId,
    pub events: Receiver<InstallEvent>,
}

/// Where a manually registered game runs.
#[derive(Debug, Clone)]
pub enum RegisterEnv {
    /// A new Vortex-managed prefix (created empty; Proton initialises it on first launch).
    New { runtime: HostPath },
    /// Share an existing environment.
    Existing(EnvId),
    /// Use a prefix Vortex did not create (`prefix_managed = 0`: never deleted by Vortex).
    Adopt { prefix: HostPath, runtime: HostPath },
}

/// Manual registration of an already-installed exe (spec 18 "register an installed game").
#[derive(Debug, Clone)]
pub struct RegisterRequest {
    pub name: String,
    pub exe: HostPath,
    pub work_dir: Option<HostPath>,
    pub env: RegisterEnv,
}

/// A game that is now `installed`.
#[derive(Debug)]
pub struct Registered {
    pub game_id: i64,
    pub env_id: EnvId,
    /// `None` unless [`Installs::with_desktop_entries`] was set. An error here does not undo
    /// the registration.
    pub desktop_entry: Option<Result<PathBuf>>,
}

#[derive(Default)]
struct Active {
    cancel: AtomicBool,
    run: Mutex<Option<RunHandle>>,
}

type ActiveMap = Arc<Mutex<HashMap<i64, Arc<Active>>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
struct Ctx {
    paths: Paths,
    db: Arc<Mutex<Db>>,
    runs: Arc<Runs>,
    active: ActiveMap,
    source_check: Duration,
}

/// Drives installs. Share one `Runs` with launch so "one run per environment" holds globally.
pub struct Installs {
    ctx: Ctx,
    vortex_bin: Option<PathBuf>,
}

const GAME_ROOTS: &[&str] = &["Program Files", "Program Files (x86)", "GOG Games", "Games"];
/// Wine/Proton's own folders directly under the roots; never game exes.
const SKIP_DIRS: &[&str] = &[
    "common files",
    "internet explorer",
    "windows media player",
    "windows nt",
    "windows defender",
    "steam",
];
const REDIST_DIRS: &[&str] = &[
    "redist",
    "_commonredist",
    "directx",
    "vcredist",
    "dotnet",
    "physx",
    "support",
];
const MAX_DEPTH: usize = 8;

impl Installs {
    pub fn new(paths: Paths, db: Arc<Mutex<Db>>, runs: Arc<Runs>) -> Self {
        Self {
            ctx: Ctx {
                paths,
                db,
                runs,
                active: Arc::default(),
                source_check: Duration::from_secs(5),
            },
            vortex_bin: None,
        }
    }

    /// How often the source is checked while the installer runs (default 5 s).
    pub fn with_source_check(mut self, every: Duration) -> Self {
        self.ctx.source_check = every;
        self
    }

    /// Write a desktop entry launching `<vortex_bin> --launch <id>` for every installed game.
    pub fn with_desktop_entries(mut self, vortex_bin: PathBuf) -> Self {
        self.vortex_bin = Some(vortex_bin);
        self
    }

    /// Call once at startup, before starting any run. Runs whose process is gone become
    /// `interrupted`, and so do their installs (never `installed`); runs that are still alive stay
    /// `running` and show up in [`Installs::detached`]. Installs left in `created` (the app died
    /// before the first step) become `interrupted` too.
    pub fn recover(&self) -> Result<Recovery> {
        let mut db = lock(&self.ctx.db);
        let mut rec = db.recover_on_startup(&|m| !scan_marker(m).is_empty())?;
        for i in db.list_installs()? {
            if i.state == InstallState::Created {
                db.set_install_state(
                    i.id,
                    InstallState::Interrupted,
                    None,
                    Some(GameStatus::Interrupted),
                )?;
                rec.installs += 1;
            }
        }
        Ok(rec)
    }

    /// Creates the environment (managed prefix), game and install rows in one transaction
    /// (`created`), then starts prefix init and the installer on a worker thread.
    pub fn begin(&self, req: InstallRequest) -> Result<Started> {
        let name = req.name.trim();
        if name.is_empty() {
            return Err(Error::Validation("the game needs a name".into()));
        }
        let analysis = crate::source::analyze(&req.source)?;
        let source = HostPath::new(
            std::fs::canonicalize(&req.source).map_err(Error::io(req.source.as_path()))?,
        )?;
        let installer = std::fs::canonicalize(&req.installer)
            .ok()
            .and_then(|p| {
                analysis
                    .candidates
                    .iter()
                    .find(|c| c.path.as_path() == p)
                    .map(|c| c.path.clone())
            })
            .ok_or_else(|| {
                Error::Validation(format!(
                    "{} is not an installer in {}",
                    req.installer.as_path().display(),
                    source.as_path().display()
                ))
            })?;
        let runtime = crate::runtime::validate(req.runtime.as_path())?.path;
        let (install_dir, managed_dir) = match &req.install_dir {
            None => (None, false),
            Some(d) => {
                let managed = d.as_path().starts_with(self.ctx.paths.games_dir())
                    && d.as_path() != self.ctx.paths.games_dir();
                if managed {
                    std::fs::create_dir_all(d).map_err(Error::io(d.as_path()))?;
                } else if !d.as_path().is_dir() {
                    return Err(Error::NotFound(format!(
                        "install folder {}",
                        d.as_path().display()
                    )));
                }
                (Some(str_of(d)), managed)
            }
        };
        let kind = match analysis.kind {
            crate::source::SourceKind::File => SourceKind::File,
            crate::source::SourceKind::Dir => SourceKind::Dir,
        };
        let mut game = NewGame::new(name, 0);
        game.install_dir = install_dir;
        game.install_dir_managed = managed_dir;
        let install = NewInstall {
            game_id: 0,
            source_path: str_of(&source),
            source_kind: kind,
            installer_path: str_of(&installer),
        };
        let (env_id, game_id, install_id) = self.create_managed(&game, Some(&install), &runtime)?;
        let install_id = install_id.ok_or_else(|| Error::NotFound("install row".into()))?;
        let events = self.spawn(install_id);
        Ok(Started {
            install_id,
            game_id,
            env_id,
            events,
        })
    }

    /// Re-runs the installer in the same prefix (prefix init first if it never completed).
    pub fn retry(&self, install_id: i64) -> Result<Receiver<InstallEvent>> {
        let i = self.check(
            install_id,
            &[
                InstallState::Failed,
                InstallState::Cancelled,
                InstallState::Interrupted,
            ],
        )?;
        let db = lock(&self.ctx.db);
        let env = db.get_game(i.game_id)?.environment_id;
        if db.active_run_for_environment(env)?.is_some() {
            return Err(Error::Busy(env));
        }
        drop(db);
        Ok(self.spawn(install_id))
    }

    /// Cancel an install running in this session: stops the current run (ARCHITECTURE 6.3).
    /// Returns at once; the events end with `State(Cancelled)`.
    pub fn cancel(&self, install_id: i64) -> Result<()> {
        let a = self.active(install_id)?;
        a.cancel.store(true, Ordering::SeqCst);
        if let Some(h) = lock(&a.run).as_ref() {
            h.stop();
        }
        Ok(())
    }

    /// "Mark as finished" while the run waits for background processes: stop waiting, kill
    /// nothing, continue to `SelectExe`.
    pub fn mark_run_finished(&self, install_id: i64) -> Result<()> {
        let a = self.active(install_id)?;
        match lock(&a.run).as_ref() {
            Some(h) => {
                h.mark_finished();
                Ok(())
            }
            None => Err(Error::Validation(
                "no run is active for this install".into(),
            )),
        }
    }

    /// The user says the installer did finish (after `Interrupted` or `Failed`): `SelectExe`.
    pub fn mark_installer_finished(&self, install_id: i64) -> Result<Vec<ExeCandidate>> {
        self.check(
            install_id,
            &[InstallState::Failed, InstallState::Interrupted],
        )?;
        lock(&self.ctx.db).set_install_state(
            install_id,
            InstallState::SelectExe,
            None,
            Some(GameStatus::Installing),
        )?;
        self.exe_candidates(install_id)
    }

    /// The user says the installer failed (from `SelectExe`).
    pub fn mark_failed(&self, install_id: i64) -> Result<()> {
        self.check(install_id, &[InstallState::SelectExe])?;
        lock(&self.ctx.db).set_install_state(
            install_id,
            InstallState::Failed,
            Some("InstallerFailure"),
            Some(GameStatus::Failed),
        )
    }

    /// Ranked exes created since the installer first started (see [`scan_exes`]).
    pub fn exe_candidates(&self, install_id: i64) -> Result<Vec<ExeCandidate>> {
        let (game, prefix, since) = {
            let db = lock(&self.ctx.db);
            let i = db.get_install(install_id)?;
            let game = db.get_game(i.game_id)?;
            let env = db.get_environment(game.environment_id)?;
            let since = db
                .runs_for_game(game.id)?
                .iter()
                .filter(|r| r.install_id == Some(install_id) && r.kind == RunKind::Install)
                .map(|r| r.started_at)
                .min()
                .unwrap_or(i.started_at);
            let prefix = PrefixDir::new(&env.prefix_path, self.ctx.paths.home(), &[])?;
            (game, prefix, since)
        };
        let dir = game.install_dir.as_deref().map(HostPath::new).transpose()?;
        Ok(scan_exes(&game.name, dir.as_ref(), &prefix, since))
    }

    /// `SelectExe -> Done`: records the user-chosen exe and marks the game installed in one
    /// transaction. The exe must be an existing regular file (`.exe`, `.bat`, `.cmd`, `.com`).
    pub fn confirm_exe(
        &self,
        install_id: i64,
        exe: &HostPath,
        work_dir: Option<&HostPath>,
    ) -> Result<Registered> {
        let i = self.check(install_id, &[InstallState::SelectExe])?;
        check_exe(exe, work_dir)?;
        let game = {
            let mut db = lock(&self.ctx.db);
            db.complete_install(install_id, &str_of(exe), work_dir.map(str_of).as_deref())?;
            db.get_game(i.game_id)?
        };
        Ok(self.registered(game.id, &game.name, game.environment_id))
    }

    /// Registers an exe that is already installed: environment (if new) and an `installed`
    /// game in one transaction. Call only after the user confirmed the exe.
    pub fn register_existing(&self, req: RegisterRequest) -> Result<Registered> {
        let name = req.name.trim();
        if name.is_empty() {
            return Err(Error::Validation("the game needs a name".into()));
        }
        check_exe(&req.exe, req.work_dir.as_ref())?;
        let mut game = NewGame::new(name, 0);
        game.status = GameStatus::Installed;
        game.exe_path = Some(str_of(&req.exe));
        game.work_dir = req.work_dir.as_ref().map(str_of);
        let (env_id, game_id) = match &req.env {
            RegisterEnv::New { runtime } => {
                let rt = crate::runtime::validate(runtime.as_path())?.path;
                let (e, g, _) = self.create_managed(&game, None, &rt)?;
                (e, g)
            }
            RegisterEnv::Existing(id) => {
                let mut db = lock(&self.ctx.db);
                db.get_environment(*id)?;
                let (e, g, _) = db.create_game(NewEnv::Existing(*id), &game, None)?;
                (e, g)
            }
            RegisterEnv::Adopt { prefix, runtime } => {
                let rt = crate::runtime::validate(runtime.as_path())?.path;
                let mut db = lock(&self.ctx.db);
                let known: Vec<PrefixDir> = db
                    .list_environments()?
                    .iter()
                    .filter_map(|e| PrefixDir::new(&e.prefix_path, self.ctx.paths.home(), &[]).ok())
                    .collect();
                if known.iter().any(|k| k.as_path() == prefix.as_path()) {
                    return Err(Error::Validation(format!(
                        "{} is already a Vortex environment; share it instead",
                        prefix.as_path().display()
                    )));
                }
                let p = PrefixDir::new(prefix, self.ctx.paths.home(), &known)?;
                if !p.drive_c().is_dir() {
                    return Err(Error::Validation(format!(
                        "{} is not a Wine prefix (no drive_c)",
                        p.as_path().display()
                    )));
                }
                let (e, g, _) = db.create_game(
                    NewEnv::Unmanaged {
                        prefix: &str_of(p.as_host()),
                        runtime: &str_of(&rt),
                    },
                    &game,
                    None,
                )?;
                (e, g)
            }
        };
        Ok(self.registered(game_id, name, env_id))
    }

    /// True while this session drives the install (its worker thread is alive).
    pub fn is_active(&self, install_id: i64) -> bool {
        lock(&self.ctx.active).contains_key(&install_id)
    }

    /// Installs whose run survived a previous Vortex session ("running (detached)"). The UI offers
    /// [`Installs::stop_detached`] and polls [`Installs::check_detached`].
    pub fn detached(&self) -> Result<Vec<(Install, Run)>> {
        let runs = self.ctx.runs.detached()?;
        let db = lock(&self.ctx.db);
        let active = lock(&self.ctx.active);
        let mut out = Vec::new();
        for r in runs {
            if let Some(id) = r.install_id
                && !active.contains_key(&id)
            {
                out.push((db.get_install(id)?, r));
            }
        }
        Ok(out)
    }

    /// For a detached install: true while its processes live ([`Runs::check_detached`]). Once
    /// they are gone the run and the install become `interrupted` (the user can then retry or
    /// mark the installer finished).
    pub fn check_detached(&self, install_id: i64) -> Result<bool> {
        let run = self.detached_run(install_id)?;
        if self.ctx.runs.check_detached(run.id)? {
            return Ok(true);
        }
        lock(&self.ctx.db).set_install_state(
            install_id,
            InstallState::Interrupted,
            None,
            Some(GameStatus::Interrupted),
        )?;
        Ok(false)
    }

    /// Stop sequence for a detached install (blocking, up to ~10 s: call off the UI thread).
    /// Returns survivors; when there are none the install becomes `interrupted`.
    pub fn stop_detached(&self, install_id: i64) -> Result<Vec<ProcInfo>> {
        let run = self.detached_run(install_id)?;
        let left = self.ctx.runs.stop_detached(run.id)?;
        if left.is_empty() {
            lock(&self.ctx.db).set_install_state(
                install_id,
                InstallState::Interrupted,
                None,
                Some(GameStatus::Interrupted),
            )?;
        }
        Ok(left)
    }

    // ---- internals ----

    fn detached_run(&self, install_id: i64) -> Result<Run> {
        if self.is_active(install_id) {
            return Err(Error::Validation(
                "this install is running in this session".into(),
            ));
        }
        lock(&self.ctx.db)
            .active_runs()?
            .into_iter()
            .find(|r| r.install_id == Some(install_id))
            .ok_or_else(|| Error::NotFound(format!("detached run of install {install_id}")))
    }

    /// Rows for a new managed environment, then its prefix dir. A leftover non-empty dir at the
    /// new prefix path (ids are reused after deletion) is refused and the new rows are removed.
    fn create_managed(
        &self,
        game: &NewGame,
        install: Option<&NewInstall>,
        runtime: &HostPath,
    ) -> Result<(EnvId, i64, Option<i64>)> {
        let mut db = lock(&self.ctx.db);
        let prefixes = self.ctx.paths.prefixes_dir();
        let ids = db.create_game(
            NewEnv::Managed {
                prefixes_dir: &prefixes,
                runtime: &str_of(runtime),
            },
            game,
            install,
        )?;
        let made = self.ctx.paths.managed_prefix(ids.0).and_then(|prefix| {
            let leftover =
                std::fs::read_dir(prefix.as_path()).is_ok_and(|mut d| d.next().is_some());
            if leftover {
                Err(Error::Validation(format!(
                    "{} already exists and is not empty; move it away and try again",
                    prefix.as_path().display()
                )))
            } else {
                create_private_dir(prefix.as_path())
            }
        });
        if let Err(e) = made {
            db.delete_game(ids.1)?;
            db.delete_environment(ids.0)?;
            return Err(e);
        }
        Ok(ids)
    }

    fn registered(&self, game_id: i64, name: &str, env_id: EnvId) -> Registered {
        let desktop_entry = self.vortex_bin.as_ref().map(|bin| {
            crate::desktop::write_entry(&self.ctx.paths, bin, &game_id.to_string(), name)
        });
        Registered {
            game_id,
            env_id,
            desktop_entry,
        }
    }

    fn active(&self, install_id: i64) -> Result<Arc<Active>> {
        lock(&self.ctx.active)
            .get(&install_id)
            .cloned()
            .ok_or_else(|| Error::Validation("this install is not running".into()))
    }

    /// The install exists, is not driven right now, and is in one of `from`.
    fn check(&self, install_id: i64, from: &[InstallState]) -> Result<Install> {
        let db = lock(&self.ctx.db);
        let i = db.get_install(install_id)?;
        if self.is_active(install_id) {
            return Err(Error::Busy(db.get_game(i.game_id)?.environment_id));
        }
        if !from.contains(&i.state) {
            return Err(Error::Validation(format!(
                "not possible while the install is {}",
                i.state.as_str()
            )));
        }
        Ok(i)
    }

    fn spawn(&self, install_id: i64) -> Receiver<InstallEvent> {
        let (tx, rx) = async_channel::bounded(1024);
        let act = Arc::new(Active::default());
        lock(&self.ctx.active).insert(install_id, act.clone());
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            Worker {
                ctx,
                id: install_id,
                act,
                tx,
            }
            .run()
        });
        rx
    }
}

fn str_of(p: &HostPath) -> String {
    p.as_path().to_string_lossy().into_owned()
}

fn check_exe(exe: &HostPath, work_dir: Option<&HostPath>) -> Result<()> {
    let ext = exe
        .as_path()
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if !matches!(ext.as_deref(), Some("exe" | "bat" | "cmd" | "com")) {
        return Err(Error::Validation(format!(
            "{} is not a Windows program (.exe, .bat, .cmd, .com)",
            exe.as_path().display()
        )));
    }
    if !exe.as_path().is_file() {
        return Err(Error::NotFound(format!(
            "executable {}",
            exe.as_path().display()
        )));
    }
    if let Some(d) = work_dir
        && !d.as_path().is_dir()
    {
        return Err(Error::NotFound(format!(
            "working directory {}",
            d.as_path().display()
        )));
    }
    Ok(())
}

/// Game status shown while an install is in `s` (`installed` only via `complete_install`).
fn game_status(s: InstallState) -> GameStatus {
    match s {
        InstallState::Failed | InstallState::Cancelled => GameStatus::Failed,
        InstallState::Interrupted => GameStatus::Interrupted,
        _ => GameStatus::Installing,
    }
}

fn class_name(c: Class) -> String {
    format!("{c:?}")
}

fn source_missing() -> Finding {
    Finding {
        class: Class::MissingInstallerData,
        confidence: Confidence::Confirmed,
        evidence: vec!["the installer source is no longer available".into()],
    }
}

struct Worker {
    ctx: Ctx,
    id: i64,
    act: Arc<Active>,
    tx: Sender<InstallEvent>,
}

/// Where an attempt comes to rest.
enum End {
    Failed(String),
    Cancelled,
    SelectExe,
}

struct Ran {
    run_id: i64,
    outcome: Option<Outcome>,
    source_lost: bool,
}

impl Worker {
    fn send(&self, ev: InstallEvent) {
        let _ = self.tx.send_blocking(ev);
    }

    fn set(&self, s: InstallState) -> Result<()> {
        lock(&self.ctx.db).set_install_state(self.id, s, None, Some(game_status(s)))?;
        self.send(InstallEvent::State(s));
        Ok(())
    }

    fn cancelled(&self) -> bool {
        self.act.cancel.load(Ordering::SeqCst)
    }

    fn run(self) {
        let end = self.steps().unwrap_or_else(|e| {
            self.send(InstallEvent::Error(e.to_string()));
            let class = match e {
                Error::RuntimeInvalid { .. } => Class::RuntimeInitFailure,
                Error::NotFound(_) => Class::MissingInstallerData,
                _ => Class::Unknown,
            };
            End::Failed(class_name(class))
        });
        let (state, failure) = match &end {
            End::Failed(c) => (InstallState::Failed, Some(c.as_str())),
            End::Cancelled => (InstallState::Cancelled, None),
            End::SelectExe => (InstallState::SelectExe, None),
        };
        // Persist, release the install, then tell the UI (it may act on the event at once).
        let saved =
            lock(&self.ctx.db).set_install_state(self.id, state, failure, Some(game_status(state)));
        lock(&self.ctx.active).remove(&self.id);
        match saved {
            Ok(()) => self.send(InstallEvent::State(state)),
            Err(e) => self.send(InstallEvent::Error(format!(
                "cannot save the install state: {e}"
            ))),
        }
    }

    fn steps(&self) -> Result<End> {
        let (inst, game, prefix, runtime) = {
            let db = lock(&self.ctx.db);
            let inst = db.get_install(self.id)?;
            let game = db.get_game(inst.game_id)?;
            let env = db.get_environment(game.environment_id)?;
            let prefix = PrefixDir::new(&env.prefix_path, self.ctx.paths.home(), &[])?;
            (inst, game, prefix, HostPath::new(&env.runtime_path)?)
        };
        let spec = |kind, invocation| RunSpec {
            kind,
            env_id: game.environment_id,
            game_id: Some(game.id),
            install_id: Some(self.id),
            invocation,
        };
        let inv = |program, args: Vec<String>, cwd: HostPath| Invocation {
            program,
            args,
            cwd,
            prefix: prefix.clone(),
            runtime: runtime.clone(),
            env: Vec::new(),
            verbose_log_dir: None,
        };

        let installer = HostPath::new(&inst.installer_path)?;
        let source = HostPath::new(&inst.source_path)?;
        let source_ok = || installer.as_path().is_file() && source.as_path().exists();
        if !source_ok() {
            self.send(InstallEvent::Finding(source_missing()));
            return Ok(End::Failed(class_name(Class::MissingInstallerData)));
        }

        // Step 7: prefix init (skipped on retry once the prefix exists).
        if !prefix.as_path().join("system.reg").is_file() {
            self.set(InstallState::PrefixInit)?;
            create_private_dir(prefix.as_path())?;
            let cwd = prefix
                .as_host()
                .parent()
                .unwrap_or_else(|| prefix.as_host().clone());
            let wineboot = inv(Program::System(SystemExe::Wineboot), vec!["-u".into()], cwd);
            let ran = self.run_one(spec(RunKind::PrefixInit, wineboot), None)?;
            let o = ran.outcome;
            if self.cancelled() || o.is_some_and(|o| o.stopped) {
                return Ok(End::Cancelled);
            }
            if o.and_then(|o| o.exit) != Some(Exit::Low8(0)) {
                let mut f = self.classify(
                    ran.run_id,
                    diagnose::RunKind::Other,
                    o,
                    Preflight::default(),
                );
                f.push(Finding {
                    class: Class::RuntimeInitFailure,
                    confidence: Confidence::Confirmed,
                    evidence: vec![format!("prefix init (wineboot -u) ended with {o:?}")],
                });
                self.report(ran.run_id, &f);
                return Ok(End::Failed(class_name(Class::RuntimeInitFailure)));
            }
        }
        if self.cancelled() {
            return Ok(End::Cancelled);
        }

        // Steps 8-10: the installer, from its own folder, with the source watched.
        if !source_ok() {
            self.send(InstallEvent::Finding(source_missing()));
            return Ok(End::Failed(class_name(Class::MissingInstallerData)));
        }
        self.set(InstallState::Running)?;
        let cwd = installer
            .parent()
            .ok_or_else(|| Error::InvalidPath(inst.installer_path.clone()))?;
        let is_msi = installer
            .as_path()
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("msi"));
        let invocation = if is_msi {
            let msi = to_win(&prefix, &installer).as_str().to_string();
            inv(
                Program::System(SystemExe::Msiexec),
                vec!["/i".into(), msi],
                cwd,
            )
        } else {
            inv(Program::Exe(installer.clone()), Vec::new(), cwd)
        };
        let ran = self.run_one(spec(RunKind::Install, invocation), Some(&source_ok))?;
        let o = ran.outcome;
        if self.cancelled() || o.is_some_and(|o| o.stopped) {
            return Ok(End::Cancelled);
        }
        let lost = ran.source_lost || !source_ok();
        let pre = Preflight {
            source_exists: Some(!lost),
            ..Preflight::default()
        };
        let findings = self.classify(ran.run_id, diagnose::RunKind::Install, o, pre);
        self.report(ran.run_id, &findings);
        if lost {
            return Ok(End::Failed(class_name(Class::MissingInstallerData)));
        }
        let failed = match o {
            // The supervisor ended without `Finished`: never guess success.
            None => true,
            Some(o) => match o.exit {
                Some(Exit::Signal(_)) => true,
                Some(x @ Exit::Low8(c)) => c != 0 && !x.matches(3010) && !x.matches(1641),
                // "Mark as finished" before umu-run exited: the user decides at SelectExe.
                None => false,
            },
        };
        if failed {
            return Ok(End::Failed(class_name(Class::InstallerFailure)));
        }

        // Steps 12-13: offer candidates; the user confirms with confirm_exe.
        let dir = game.install_dir.as_deref().map(HostPath::new).transpose()?;
        let since = lock(&self.ctx.db).get_run(ran.run_id)?.started_at;
        self.send(InstallEvent::Candidates(scan_exes(
            &game.name,
            dir.as_ref(),
            &prefix,
            since,
        )));
        Ok(End::SelectExe)
    }

    /// Starts one run and forwards its events until `Finished`.
    fn run_one(&self, spec: RunSpec, watch: Option<&dyn Fn() -> bool>) -> Result<Ran> {
        let kind = spec.kind;
        let (h, rx) = self.ctx.runs.start(spec)?;
        *lock(&self.act.run) = Some(h.clone());
        if self.cancelled() {
            h.stop();
        }
        let mut ran = Ran {
            run_id: h.run_id(),
            outcome: None,
            source_lost: false,
        };
        let mut next_check = Instant::now() + self.ctx.source_check;
        loop {
            match rx.try_recv() {
                Ok(RunEvent::Finished(o)) => {
                    ran.outcome = Some(o);
                    self.send(InstallEvent::Run {
                        kind,
                        event: RunEvent::Finished(o),
                    });
                    break;
                }
                Ok(event) => {
                    if kind == RunKind::Install
                        && event == RunEvent::Phase(Phase::WaitingForChildren)
                    {
                        // The run row is already `waiting`; mirror it on the install.
                        self.set(InstallState::Waiting)?;
                    }
                    let droppable = matches!(event, RunEvent::Line { .. } | RunEvent::Processes(_));
                    let ev = InstallEvent::Run { kind, event };
                    if droppable {
                        let _ = self.tx.try_send(ev); // drop on full: the log file has it all
                    } else {
                        self.send(ev);
                    }
                }
                Err(async_channel::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(async_channel::TryRecvError::Closed) => break,
            }
            if let Some(ok) = watch
                && !ran.source_lost
                && Instant::now() >= next_check
            {
                next_check = Instant::now() + self.ctx.source_check;
                if !ok() {
                    ran.source_lost = true;
                    self.send(InstallEvent::Finding(source_missing()));
                }
            }
        }
        *lock(&self.act.run) = None;
        Ok(ran)
    }

    /// Classifies a finished run from its whole log file (the UI channel may drop lines).
    fn classify(
        &self,
        run_id: i64,
        kind: diagnose::RunKind,
        o: Option<Outcome>,
        pre: Preflight,
    ) -> Vec<Finding> {
        let log = lock(&self.ctx.db)
            .get_run(run_id)
            .ok()
            .and_then(|r| std::fs::read(r.log_path).ok())
            .unwrap_or_default();
        let lines = diagnose::log_lines(&String::from_utf8_lossy(&log));
        diagnose::classify(
            kind,
            diagnose::exit_of(o.and_then(|o| o.exit)),
            &lines,
            &pre,
        )
    }

    fn report(&self, run_id: i64, findings: &[Finding]) {
        if let Err(e) = lock(&self.ctx.db).set_run_findings(run_id, &diagnose::to_json(findings)) {
            self.send(InstallEvent::Error(format!("cannot save findings: {e}")));
        }
        for f in findings {
            self.send(InstallEvent::Finding(f.clone()));
        }
    }
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn excluded_name(stem: &str) -> bool {
    ["unins", "vcredist", "vc_redist", "dxsetup", "dotnet"]
        .iter()
        .any(|p| stem.starts_with(p))
        || ["setup", "crash", "redist", "uninst"]
            .iter()
            .any(|p| stem.contains(p))
}

/// Ranked game exe candidates: `*.exe` under `install_dir` (any age) and under
/// `drive_c/{Program Files,Program Files (x86),GOG Games,Games}` whose inode changed at or after
/// `since` (ctime: installers often keep archive mtimes, but cannot set ctime). Uninstallers,
/// setups, redistributables, crash handlers and Wine's own folders are left out. Ranked by name
/// similarity to the game, then size. Symlinks are never followed.
pub fn scan_exes(
    name: &str,
    install_dir: Option<&HostPath>,
    prefix: &PrefixDir,
    since: i64,
) -> Vec<ExeCandidate> {
    let mut roots: Vec<(PathBuf, bool)> = install_dir
        .map(|d| (d.as_path().to_path_buf(), true))
        .into_iter()
        .collect();
    let dc = prefix.drive_c();
    roots.extend(GAME_ROOTS.iter().map(|r| (dc.join(r), false)));
    // path -> (size, in install dir, root it was found under)
    let mut found: HashMap<PathBuf, (u64, bool, PathBuf)> = HashMap::new();
    for (root, own) in &roots {
        walk(root, 0, &mut |p, size, ctime| {
            if *own || ctime >= since {
                found
                    .entry(p.to_path_buf())
                    .or_insert((size, *own, root.clone()));
            }
        });
    }
    let want = norm(name);
    let tokens: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|t| t.len() >= 3)
        .collect();
    let mut out: Vec<ExeCandidate> = found
        .into_iter()
        .filter_map(|(p, (size, own, root))| {
            let stem = p.file_stem()?.to_string_lossy().to_ascii_lowercase();
            let dirs: Vec<String> = p
                .strip_prefix(&root)
                .ok()?
                .parent()
                .into_iter()
                .flat_map(Path::components)
                .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
                .collect();
            if excluded_name(&stem) || dirs.iter().any(|d| REDIST_DIRS.contains(&d.as_str())) {
                return None;
            }
            let s = norm(&stem);
            let mut score = 0;
            let mut reasons = Vec::new();
            let mut add = |d: i32, r: String| {
                score += d;
                reasons.push(r);
            };
            if !want.is_empty() && s == want {
                add(100, "named like the game".into());
            } else if want.len() >= 3 && s.len() >= 3 && (s.contains(&want) || want.contains(&s)) {
                add(50, "name resembles the game".into());
            } else {
                for t in tokens.iter().filter(|t| s.contains(t.as_str())) {
                    add(10, format!("name contains \"{t}\""));
                }
            }
            if !want.is_empty() && dirs.iter().any(|d| norm(d) == want) {
                add(30, "in a folder named like the game".into());
            }
            if own {
                add(20, "in the chosen install folder".into());
            }
            let path = HostPath::new(&p).ok()?;
            Some(ExeCandidate {
                win: to_win(prefix, &path),
                path,
                score,
                size,
                reasons,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.size.cmp(&a.size))
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// Regular `.exe` files under `dir` to `MAX_DEPTH`; symlinks and Wine's own folders skipped.
fn walk(dir: &Path, depth: usize, f: &mut dyn FnMut(&Path, u64, i64)) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let low = e.file_name().to_string_lossy().to_ascii_lowercase();
        if ft.is_dir() {
            if depth < MAX_DEPTH && !(depth == 0 && SKIP_DIRS.contains(&low.as_str())) {
                walk(&e.path(), depth + 1, f);
            }
        } else if ft.is_file()
            && low.ends_with(".exe")
            && let Ok(m) = e.metadata()
        {
            f(&e.path(), m.len(), m.ctime());
        }
    }
}
