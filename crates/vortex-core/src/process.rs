//! Process supervision: spawn, pipe readers, completion barriers, stop.
//!
//! ARCHITECTURE section 6. Completion = umu-run exit, then Proton's `wineserver -w`, then a drain
//! of processes carrying `VORTEX_RUN_ID=<marker>`. Stop = `wineserver -k`, SIGTERM umu-run,
//! SIGKILL whatever still carries the marker. Nothing here guesses success: exit codes are the low
//! 8 bits only, and leftovers are reported as `Uncertain`.

use crate::db::{Db, NewRun, Run, RunKind, RunState, proc_start_time};
use crate::error::{EnvId, Error, Result};
use crate::logs::{LogWriter, Stream, redact};
use crate::paths::{HostPath, Paths, PrefixDir};
use crate::runner::{Invocation, Tools, new_marker};
use async_channel::{Receiver, Sender};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// A process carrying the run marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: i32,
    /// `/proc/<pid>/cmdline` joined by spaces (Wine processes show the Windows exe path).
    pub name: String,
    /// Wine/umu/pressure-vessel plumbing; the UI hides these from "still running" lists.
    pub infra: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for umu-run to exit.
    Running,
    /// umu-run exited; waiting on Proton's `wineserver -w` (no timeout).
    WaitingForChildren,
    /// Waiting for leftover marker processes (pressure-vessel teardown).
    Draining,
}

/// How umu-run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Low 8 bits of the Windows exit code. umu-run truncates: 3010 arrives as 194, 1641 as 105.
    /// The full code is unknowable; compare with [`Exit::matches`].
    Low8(u8),
    /// umu-run was killed by this signal (e.g. by Stop).
    Signal(i32),
}

impl Exit {
    /// True if a Windows code `code` would have been reported as this exit (`code & 0xff`).
    pub fn matches(self, code: u32) -> bool {
        self == Exit::Low8((code & 0xff) as u8)
    }

    fn from_status(s: ExitStatus) -> Option<Exit> {
        use std::os::unix::process::ExitStatusExt;
        s.code()
            .map(|c| Exit::Low8((c & 0xff) as u8))
            .or_else(|| s.signal().map(Exit::Signal))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// `None` when the user marked the run finished before umu-run exited.
    pub exit: Option<Exit>,
    /// The stop sequence ran.
    pub stopped: bool,
    /// The user chose "Mark as finished"; processes may still be running.
    pub user_forced: bool,
}

/// Plain-data events for the UI (ARCHITECTURE section 12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEvent {
    Phase(Phase),
    /// One redacted output line of umu-run (umu/Proton text only; installer console output does
    /// not reach the pipes, M1 F2). Lines starting with `vortex: ` are the supervisor's notes.
    Line {
        stream: Stream,
        text: String,
    },
    /// Every `Timing::scan_every` while waiting.
    Processes(Vec<ProcInfo>),
    /// Marker processes outlived the drain (or the stop). The supervisor keeps watching; the UI
    /// offers Stop and Mark as finished.
    Uncertain(Vec<ProcInfo>),
    /// Last event of a run.
    Finished(Outcome),
}

/// Supervisor timings. Defaults follow ARCHITECTURE section 6; tests shorten them.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub poll: Duration,
    pub scan_every: Duration,
    pub drain: Duration,
    pub stop_grace: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(250),
            scan_every: Duration::from_secs(2),
            drain: Duration::from_secs(10),
            stop_grace: Duration::from_secs(5),
        }
    }
}

/// What to run and which rows it belongs to.
#[derive(Debug, Clone)]
pub struct RunSpec {
    pub kind: RunKind,
    pub env_id: EnvId,
    pub game_id: Option<i64>,
    pub install_id: Option<i64>,
    pub invocation: Invocation,
    /// Verbose run: Proton writes its own log (`PROTON_LOG=1`) into
    /// [`crate::logs::proton_log_dir`] of this run's log, redacted in place when the run ends.
    pub verbose: bool,
}

#[derive(Default)]
struct Ctl {
    stop: AtomicBool,
    forced: AtomicBool,
}

/// Control of one active run. Cheap to clone; never blocks.
#[derive(Clone)]
pub struct RunHandle {
    run_id: i64,
    env_id: EnvId,
    kind: RunKind,
    marker: String,
    ctl: Arc<Ctl>,
}

impl RunHandle {
    pub fn run_id(&self) -> i64 {
        self.run_id
    }
    pub fn env_id(&self) -> EnvId {
        self.env_id
    }
    pub fn kind(&self) -> RunKind {
        self.kind
    }
    pub fn marker(&self) -> &str {
        &self.marker
    }
    /// Requests the stop sequence (6.3). Returns at once; `Finished { stopped: true }` follows.
    pub fn stop(&self) {
        self.ctl.stop.store(true, Ordering::SeqCst);
    }
    /// "Mark as finished": stop waiting and emit `Finished { user_forced: true }`. Kills nothing.
    pub fn mark_finished(&self) {
        self.ctl.forced.store(true, Ordering::SeqCst);
    }
}

/// Environments busy in this session: a supervised run, or `None` for a [`Guard`] reservation.
type Active = Arc<Mutex<HashMap<EnvId, Option<RunHandle>>>>;

/// Holds an environment busy (see [`Runs::reserve`]) until dropped.
pub struct Guard {
    active: Active,
    env: EnvId,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut a = lock(&self.active);
        if matches!(a.get(&self.env), Some(None)) {
            a.remove(&self.env);
        }
    }
}

/// The program file name of a run, from its recorded argv (`[umu-run, program, args...]`), for
/// the UI to name a run that has no game ("setup.exe", "winetricks").
pub fn program_name(argv_json: &str) -> Option<String> {
    let argv: Vec<String> = serde_json::from_str(argv_json).ok()?;
    let p = argv.get(1)?;
    let base = p.rsplit(['/', '\\']).next().unwrap_or(p);
    (!base.is_empty()).then(|| base.to_string())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts and tracks runs. One active run per environment (`Error::Busy`).
pub struct Runs {
    paths: Paths,
    tools: Tools,
    db: Arc<Mutex<Db>>,
    timing: Timing,
    active: Active,
}

impl Runs {
    pub fn new(paths: Paths, tools: Tools, db: Arc<Mutex<Db>>) -> Self {
        Self {
            paths,
            tools,
            db,
            timing: Timing::default(),
            active: Arc::default(),
        }
    }

    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }

    /// The active run of an environment in this session, if any.
    pub fn active(&self, env: EnvId) -> Option<RunHandle> {
        lock(&self.active).get(&env).cloned().flatten()
    }

    /// Occupies `env` for a long operation on its prefix (backup, restore, deletion): while the
    /// returned guard lives, [`Runs::start`] fails with `Busy`. Fails with `Busy` itself if the
    /// environment has a run (this session or detached) or another reservation.
    pub fn reserve(&self, env: EnvId) -> Result<Guard> {
        let mut active = lock(&self.active);
        if active.contains_key(&env) || lock(&self.db).active_run_for_environment(env)?.is_some() {
            return Err(Error::Busy(env));
        }
        active.insert(env, None);
        Ok(Guard {
            active: self.active.clone(),
            env,
        })
    }

    /// Runs recorded `running`/`waiting` that no supervisor of this session drives: they survived
    /// a previous Vortex session ("running (detached)", ARCHITECTURE 6.4). Every kind, installs
    /// included. The UI polls [`Runs::check_detached`] and offers [`Runs::stop_detached`].
    pub fn detached(&self) -> Result<Vec<Run>> {
        let active = lock(&self.active);
        let mine: Vec<i64> = active.values().flatten().map(|h| h.run_id).collect();
        let runs = lock(&self.db).active_runs()?;
        Ok(runs.into_iter().filter(|r| !mine.contains(&r.id)).collect())
    }

    /// For a detached run: true while umu-run (pid + start time) or any marker process lives.
    /// Once both are gone the row becomes `interrupted` with no exit code (nobody saw it end).
    /// False for a run that is no longer `running`/`waiting`.
    pub fn check_detached(&self, run_id: i64) -> Result<bool> {
        if lock(&self.active)
            .values()
            .flatten()
            .any(|h| h.run_id == run_id)
        {
            return Ok(true);
        }
        let run = lock(&self.db).get_run(run_id)?;
        if !matches!(run.state, RunState::Running | RunState::Waiting) {
            return Ok(false);
        }
        let pid_alive = matches!((run.pid, run.pid_start),
            (Some(p), Some(s)) if proc_start_time(p) == Some(s));
        if pid_alive || !scan_marker(&run.marker).is_empty() {
            return Ok(true);
        }
        lock(&self.db).finish_run(run_id, RunState::Interrupted, None, false)?;
        Ok(false)
    }

    /// Validates, records the run row (`running`) and log, spawns umu-run and its supervisor.
    /// Fails with `Busy` if the environment has an active run (this session or a detached one in
    /// the DB), and with preflight errors (`NotFound`, `RuntimeInvalid`, `Validation`) before
    /// anything is written.
    pub fn start(&self, spec: RunSpec) -> Result<(RunHandle, Receiver<RunEvent>)> {
        let mut inv = spec.invocation;
        inv.preflight()?;
        let wineserver = inv.wineserver()?;
        let marker = new_marker()?;

        let mut active = lock(&self.active);
        let db = lock(&self.db);
        if active.contains_key(&spec.env_id)
            || db.active_run_for_environment(spec.env_id)?.is_some()
        {
            return Err(Error::Busy(spec.env_id));
        }
        let argv: Vec<String> = std::iter::once(self.tools.umu_run.to_string_lossy().into_owned())
            .chain(inv.argv())
            .map(|a| redact(&a).into_owned())
            .collect();
        let argv_json = serde_json::to_string(&argv)
            .map_err(|e| Error::Validation(format!("cannot encode argv: {e}")))?;
        let run_id = db.insert_run(&NewRun {
            marker: marker.clone(),
            kind: spec.kind,
            environment_id: spec.env_id,
            game_id: spec.game_id,
            install_id: spec.install_id,
            argv: argv_json.clone(),
            log_path: String::new(),
        })?;
        let fail = |e: Error| {
            let _ = db.finish_run(run_id, RunState::Finished, None, false);
            e
        };

        let owner = spec.game_id.unwrap_or(spec.env_id);
        let mut log = LogWriter::create(&self.paths, owner, run_id, spec.kind).map_err(fail)?;
        db.set_run_log_path(run_id, &log.path().to_string_lossy())
            .map_err(fail)?;
        let verbose_dir = if spec.verbose {
            let d = crate::logs::proton_log_dir(log.path());
            crate::paths::create_private_dir(&d).map_err(fail)?;
            inv.verbose_log_dir = Some(HostPath::new(&d).map_err(fail)?);
            Some(d)
        } else {
            None
        };
        let mut cmd = inv.command(&self.tools, &marker).map_err(fail)?;
        let now = crate::db::now().to_string();
        let cwd = inv.cwd.as_path().to_string_lossy();
        let cwd_win = inv.cwd_win();
        let runtime = inv.runtime.as_path().to_string_lossy();
        let prefix = inv.prefix.as_path().to_string_lossy();
        let proton_log = verbose_dir
            .as_ref()
            .map(|d| d.to_string_lossy().into_owned());
        let mut header = vec![
            ("time", now.as_str()),
            ("kind", spec.kind.as_str()),
            ("argv", &argv_json),
            ("cwd", &cwd),
            ("cwd_win", cwd_win.as_str()),
            ("runtime", &runtime),
            ("prefix", &prefix),
            ("marker", &marker),
        ];
        if let Some(d) = &proton_log {
            header.push(("proton_log_dir", d));
        }
        log.header(&header)
            .and_then(|()| log.flush())
            .map_err(fail)?;

        let mut child = cmd
            .spawn()
            .map_err(Error::io(&self.tools.umu_run))
            .map_err(fail)?;
        let pid = child.id() as i64;
        db.set_run_pid(run_id, pid, proc_start_time(pid))
            .map_err(fail)?;
        drop(db);

        let (tx, rx) = async_channel::bounded(1024);
        let log = Arc::new(Mutex::new(log));
        let readers = [
            child
                .stdout
                .take()
                .map(|r| reader(r, Stream::Stdout, &log, &tx)),
            child
                .stderr
                .take()
                .map(|r| reader(r, Stream::Stderr, &log, &tx)),
        ]
        .into_iter()
        .flatten()
        .collect();

        let handle = RunHandle {
            run_id,
            env_id: spec.env_id,
            kind: spec.kind,
            marker,
            ctl: Arc::default(),
        };
        active.insert(spec.env_id, Some(handle.clone()));
        drop(active);

        let sup = Supervisor {
            h: handle.clone(),
            db: self.db.clone(),
            active: self.active.clone(),
            timing: self.timing,
            wineserver,
            prefix: inv.prefix.clone(),
            tx,
            log,
            readers,
            verbose_dir,
        };
        thread::spawn(move || sup.run(child));
        Ok((handle, rx))
    }

    /// Stop sequence for a run that has no supervisor in this session (found `running` after a
    /// restart, ARCHITECTURE 6.4). Blocking (up to ~2x `stop_grace`); call from a worker thread.
    /// Returns survivors; when there are none the row becomes `stopped`.
    pub fn stop_detached(&self, run_id: i64) -> Result<Vec<ProcInfo>> {
        let (run, env) = {
            let db = lock(&self.db);
            let run = db.get_run(run_id)?;
            let env = db.get_environment(run.environment_id)?;
            (run, env)
        };
        if lock(&self.active).contains_key(&run.environment_id) {
            return Err(Error::Busy(run.environment_id));
        }
        let prefix = PrefixDir::new(&env.prefix_path, self.paths.home(), &[])?;
        let ws = crate::runtime::validate(Path::new(&env.runtime_path))
            .ok()
            .and_then(|r| r.wineserver())
            .ok_or_else(|| Error::RuntimeInvalid {
                path: env.runtime_path.clone().into(),
                reason: "no wineserver to stop the run with".into(),
            })?;
        let umu = match (run.pid, run.pid_start) {
            (Some(p), Some(s)) if proc_start_time(p) == Some(s) => i32::try_from(p).ok(),
            _ => None,
        };
        let left = stop_sequence(
            ws.as_path(),
            &prefix,
            &run.marker,
            umu,
            self.timing.stop_grace,
        );
        if left.is_empty() {
            lock(&self.db).finish_run(run_id, RunState::Stopped, None, false)?;
        }
        Ok(left)
    }
}

fn reader(
    r: impl Read + Send + 'static,
    stream: Stream,
    log: &Arc<Mutex<LogWriter>>,
    tx: &Sender<RunEvent>,
) -> JoinHandle<()> {
    let (log, tx) = (log.clone(), tx.clone());
    thread::spawn(move || {
        let mut r = BufReader::new(r);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let s = String::from_utf8_lossy(&buf);
            let s = s.trim_end_matches(['\n', '\r']);
            {
                let mut l = lock(&log);
                let _ = l.line(stream, s).and_then(|()| l.flush());
            }
            // Drop on full: the log file keeps everything.
            let _ = tx.try_send(RunEvent::Line {
                stream,
                text: redact(s).into_owned(),
            });
        }
    })
}

enum Wake {
    Done,
    Stop,
    Forced,
}

struct Supervisor {
    h: RunHandle,
    db: Arc<Mutex<Db>>,
    active: Active,
    timing: Timing,
    wineserver: HostPath,
    prefix: PrefixDir,
    tx: Sender<RunEvent>,
    log: Arc<Mutex<LogWriter>>,
    readers: Vec<JoinHandle<()>>,
    verbose_dir: Option<std::path::PathBuf>,
}

impl Supervisor {
    fn send(&self, ev: RunEvent) {
        let _ = self.tx.send_blocking(ev);
    }

    /// Supervisor note: goes to the log and the UI as a `vortex: ` stderr line.
    fn note(&self, text: &str) {
        let text = format!("vortex: {}", redact(text));
        {
            let mut l = lock(&self.log);
            let _ = l.line(Stream::Stderr, &text).and_then(|()| l.flush());
        }
        let _ = self.tx.try_send(RunEvent::Line {
            stream: Stream::Stderr,
            text,
        });
    }

    fn db(&self, f: impl FnOnce(&Db) -> Result<()>) {
        if let Err(e) = f(&lock(&self.db)) {
            self.note(&format!("database update failed: {e}"));
        }
    }

    fn scan(&self) -> Vec<ProcInfo> {
        scan_marker(&self.h.marker)
    }

    /// Polls `done` until true, stop or forced; emits `Processes` every `scan_every`.
    fn wait(&self, mut done: impl FnMut() -> bool) -> Wake {
        let mut last = Instant::now();
        loop {
            if self.h.ctl.stop.load(Ordering::SeqCst) {
                return Wake::Stop;
            }
            if self.h.ctl.forced.load(Ordering::SeqCst) {
                return Wake::Forced;
            }
            if done() {
                return Wake::Done;
            }
            if last.elapsed() >= self.timing.scan_every {
                let _ = self.tx.try_send(RunEvent::Processes(self.scan()));
                last = Instant::now();
            }
            thread::sleep(self.timing.poll);
        }
    }

    fn run(self, mut child: Child) {
        let mut exit = None;
        let reap = |child: &mut Child, exit: &mut Option<Exit>| match child.try_wait() {
            Ok(Some(s)) => {
                *exit = Exit::from_status(s);
                true
            }
            Ok(None) => false,
            Err(_) => true,
        };

        self.send(RunEvent::Phase(Phase::Running));
        let wake = match self.wait(|| reap(&mut child, &mut exit)) {
            Wake::Done => {
                self.note(&format!("umu-run exited: {exit:?}"));
                self.db(|db| db.set_run_state(self.h.run_id, RunState::Waiting));
                self.send(RunEvent::Phase(Phase::WaitingForChildren));
                self.barrier()
            }
            w => w,
        };
        let wake = match wake {
            Wake::Done => {
                self.send(RunEvent::Phase(Phase::Draining));
                self.drain()
            }
            w => w,
        };
        let stopped = matches!(wake, Wake::Stop);
        let wake = if stopped {
            loop {
                // Consumed here, so `uncertain` keeps watching survivors instead of returning at
                // once; a new Stop while they live runs the sequence again (6.3 step 4).
                self.h.ctl.stop.store(false, Ordering::SeqCst);
                self.note("stop requested");
                let left = stop_sequence(
                    self.wineserver.as_path(),
                    &self.prefix,
                    &self.h.marker,
                    i32::try_from(child.id()).ok(),
                    self.timing.stop_grace,
                );
                if left.is_empty() {
                    break Wake::Done;
                }
                match self.uncertain(left) {
                    Wake::Stop => continue,
                    w => break w,
                }
            }
        } else {
            wake
        };
        let user_forced = matches!(wake, Wake::Forced);
        if stopped && exit.is_none() {
            // The marker vanishes from /proc/<pid>/environ at exit_mm, slightly before the
            // kernel lets us reap umu-run; give try_wait a bounded moment to catch up.
            wait_for(self.timing.stop_grace, || reap(&mut child, &mut exit));
        }
        if exit.is_none() && !reap(&mut child, &mut exit) {
            // Forced while umu-run lives: reap it whenever it ends so it never stays a zombie.
            thread::spawn(move || child.wait());
        }
        self.finish(Outcome {
            exit,
            stopped,
            user_forced,
        });
    }

    /// Phase 2: Proton's `wineserver -w` (rc 0 at once when no server runs). No timeout.
    fn barrier(&self) -> Wake {
        let started = Instant::now();
        let mut ws = match wineserver(self.wineserver.as_path(), &self.prefix, "-w").spawn() {
            Ok(c) => c,
            Err(e) => {
                self.note(&format!(
                    "cannot run wineserver -w: {e}; relying on the drain"
                ));
                return Wake::Done;
            }
        };
        let wake = self.wait(|| !matches!(ws.try_wait(), Ok(None)));
        if !matches!(wake, Wake::Done) {
            let _ = ws.kill();
        }
        let _ = ws.wait();
        self.note(&format!(
            "wineserver -w returned after {:.2}s",
            started.elapsed().as_secs_f64()
        ));
        wake
    }

    /// Phase 3: wait up to `drain` for marker processes to go; then `Uncertain` and keep watching.
    fn drain(&self) -> Wake {
        let deadline = Instant::now() + self.timing.drain;
        let mut left = Vec::new();
        let wake = self.wait(|| {
            left = self.scan();
            left.is_empty() || Instant::now() >= deadline
        });
        match wake {
            Wake::Done if !left.is_empty() => self.uncertain(left),
            w => w,
        }
    }

    fn uncertain(&self, left: Vec<ProcInfo>) -> Wake {
        self.note(&format!(
            "{} marker process(es) still running: {}",
            left.len(),
            left.iter()
                .map(|p| format!("{} {}", p.pid, p.name))
                .collect::<Vec<_>>()
                .join("; ")
        ));
        self.send(RunEvent::Uncertain(left));
        self.wait(|| self.scan().is_empty())
    }

    fn finish(self, o: Outcome) {
        // Pipes close once every marker process is gone; give the readers a moment to drain
        // so `Finished` is the last event.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.readers.iter().all(JoinHandle::is_finished) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let state = if o.stopped {
            RunState::Stopped
        } else {
            RunState::Finished
        };
        let code = match o.exit {
            Some(Exit::Low8(c)) => Some(i64::from(c)),
            _ => None,
        };
        // Proton wrote this file itself: redact it before anyone reads it (rule 8).
        if let Some(d) = &self.verbose_dir {
            let logs: Vec<_> = std::fs::read_dir(d)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "log"))
                .collect();
            for p in logs {
                if let Err(e) = crate::logs::redact_file(&p) {
                    self.note(&format!("cannot redact {}: {e}", p.display()));
                }
            }
        }
        self.note(&format!("finished: {o:?}"));
        self.db(|db| db.finish_run(self.h.run_id, state, code, o.user_forced));
        lock(&self.active).remove(&self.h.env_id);
        self.send(RunEvent::Finished(o));
    }
}

fn wineserver(ws: &Path, prefix: &PrefixDir, arg: &str) -> Command {
    let mut c = Command::new(ws);
    c.arg(arg)
        .env("WINEPREFIX", prefix.as_path())
        .env_remove("VORTEX_RUN_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + limit;
    while !done() && Instant::now() < end {
        thread::sleep(Duration::from_millis(100));
    }
}

/// ARCHITECTURE 6.3: `wineserver -k` (rc 1 = nothing running, fine), wait; SIGTERM umu-run,
/// wait; SIGKILL every process still carrying the marker. Returns the survivors (normally none).
fn stop_sequence(
    ws: &Path,
    prefix: &PrefixDir,
    marker: &str,
    umu: Option<i32>,
    grace: Duration,
) -> Vec<ProcInfo> {
    use rustix::process::{Pid, Signal, kill_process};
    let _ = wineserver(ws, prefix, "-k").status();
    // umu-run carries the marker itself; once it exits (zombie included) its environ is empty.
    let umu_alive = || umu.is_some_and(|p| has_marker(p, marker));
    wait_for(grace, || !umu_alive());
    if umu_alive()
        && let Some(pid) = umu.and_then(Pid::from_raw)
    {
        let _ = kill_process(pid, Signal::TERM);
        wait_for(grace, || !umu_alive());
    }
    for p in scan_marker(marker) {
        // ponytail: re-check then kill has a tiny pid-reuse race; use pidfd if it bites.
        if has_marker(p.pid, marker)
            && let Some(pid) = Pid::from_raw(p.pid)
        {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
    // SIGKILL is asynchronous and a respawner may restart a process: let it land before the
    // first look, so "nothing left" is not concluded from a scan taken mid-teardown.
    thread::sleep(Duration::from_millis(100));
    let mut left = Vec::new();
    wait_for(grace, || {
        left = scan_marker(marker);
        left.is_empty()
    });
    left
}

fn has_marker(pid: i32, marker: &str) -> bool {
    std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|env| environ_has(&env, marker))
}

fn environ_has(environ: &[u8], marker: &str) -> bool {
    let want = format!("VORTEX_RUN_ID={marker}");
    environ.split(|&b| b == 0).any(|e| e == want.as_bytes())
}

/// Processes, as seen by Wine, Proton and pressure-vessel, that only exist to host the program.
const INFRA: &[&str] = &[
    "wineserver",
    "services.exe",
    "winedevice.exe",
    "plugplay.exe",
    "explorer.exe",
    "rpcss.exe",
    "svchost.exe",
    "steam.exe",
    "tabtip.exe",
    "xalia.exe",
    "conhost.exe",
    "pv-adverb",
    "srt-bwrap",
    "pressure-vessel-wrap",
    "steam-runtime-launcher-service",
    "umu-run",
    "proton",
];

fn is_infra(argv: &[&str]) -> bool {
    let base = |s: &str| {
        s.rsplit(['/', '\\'])
            .next()
            .unwrap_or(s)
            .to_ascii_lowercase()
    };
    let Some(first) = argv.first().map(|s| base(s)) else {
        return false;
    };
    // Script interpreters: judge by the script (`python3 /usr/bin/umu-run`, `.../proton`).
    let prog = if first.starts_with("python") || first == "sh" || first == "bash" {
        argv.get(1).map(|s| base(s)).unwrap_or(first)
    } else {
        first
    };
    INFRA.contains(&prog.as_str())
}

/// Every process of this user whose `/proc/<pid>/environ` holds `VORTEX_RUN_ID=<marker>`.
/// Zombies and other users' processes are not listed (their environ is empty or unreadable).
pub fn scan_marker(marker: &str) -> Vec<ProcInfo> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out: Vec<ProcInfo> = dir
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&pid| has_marker(pid, marker))
        .filter_map(|pid| {
            let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            let raw = String::from_utf8_lossy(&raw);
            let argv: Vec<&str> = raw.split('\0').filter(|s| !s.is_empty()).collect();
            let name = if argv.is_empty() {
                std::fs::read_to_string(format!("/proc/{pid}/comm"))
                    .ok()?
                    .trim()
                    .to_string()
            } else {
                argv.join(" ")
            };
            Some(ProcInfo {
                pid,
                infra: is_infra(&argv),
                name: redact(&name).into_owned(),
            })
        })
        .collect();
    out.sort_by_key(|p| p.pid);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_low_8_bits() {
        let e = Exit::Low8(194);
        assert!(e.matches(3010));
        assert!(e.matches(194));
        assert!(!e.matches(0));
        assert!(Exit::Low8(105).matches(1641));
        assert!(Exit::Low8(0).matches(0));
        assert!(!Exit::Signal(9).matches(9));
    }

    #[test]
    fn program_name_from_argv() {
        let a = r#"["/usr/bin/umu-run","/g/My Game/setup.exe","/q"]"#;
        assert_eq!(program_name(a).as_deref(), Some("setup.exe"));
        let w = r#"["umu-run","C:\\windows\\system32\\msiexec.exe"]"#;
        assert_eq!(program_name(w).as_deref(), Some("msiexec.exe"));
        assert_eq!(program_name(r#"["umu-run"]"#), None);
        assert_eq!(program_name("not json"), None);
    }

    #[test]
    fn environ_match_is_exact() {
        let env = b"A=1\0VORTEX_RUN_ID=abc\0B=2\0";
        assert!(environ_has(env, "abc"));
        assert!(!environ_has(env, "ab"));
        assert!(!environ_has(b"XVORTEX_RUN_ID=abc\0", "abc"));
    }

    #[test]
    fn infra_filter() {
        for a in [
            &["C:\\windows\\system32\\services.exe"][..],
            &["c:\\windows\\system32\\steam.exe", "Z:\\x\\setup.exe"],
            &["python3", "/usr/bin/umu-run", "C:\\x.exe"],
            &[
                "/usr/bin/python3",
                "/opt/Proton/proton",
                "waitforexitandrun",
            ],
            &["/x/pressure-vessel/bin/pv-adverb", "--exit-with-parent"],
            &["/opt/Proton/files/bin/wineserver"],
        ] {
            assert!(is_infra(a), "{a:?}");
        }
        for a in [
            &["Z:\\mnt\\g\\setup.exe", "/q"][..],
            &["C:\\windows\\system32\\cmd.exe", "/c", "x.bat"],
            &["C:\\windows\\system32\\msiexec.exe"],
            &[],
        ] {
            assert!(!is_infra(a), "{a:?}");
        }
    }
}
