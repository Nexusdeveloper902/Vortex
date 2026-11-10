//! Winetricks: verb list, validation, installed verbs, one `umu-run winetricks <verb>` per run.
//!
//! Verbs are validated names checked against `winetricks list-all`; files and arbitrary strings
//! never reach winetricks. Needs a runtime with `protonfixes/winetricks` (GE-Proton or
//! UMU-Proton; Proton Experimental has none, S8).

use crate::db::{Db, DepState, RunKind};
use crate::error::{Error, Result};
use crate::launch::environment_of;
use crate::paths::{HostPath, Paths, PrefixDir};
use crate::process::{Exit, RunEvent, RunHandle, RunSpec, Runs};
use crate::runner::{Invocation, Program, Tools};
use async_channel::Receiver;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verb {
    /// Section of `list-all` without the `=` signs: `dlls`, `fonts`, `settings`, `apps`, ...
    pub section: String,
    pub name: String,
    pub description: String,
}

/// Parses `winetricks list-all`: `===== dlls =====` headers, then `name   description` lines.
pub fn parse_list_all(text: &str) -> Vec<Verb> {
    let mut section = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(h) = line.strip_prefix("=====") {
            section = h.trim_end_matches('=').trim().to_string();
        } else if !line.is_empty() {
            let (name, desc) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            if is_verb_name(name) {
                out.push(Verb {
                    section: section.clone(),
                    // `vd=NxM` is listed with an example value; the verb is `vd`.
                    name: name.split('=').next().unwrap_or(name).into(),
                    description: desc.trim().into(),
                });
            }
        }
    }
    out
}

/// `^[a-zA-Z0-9_-]+(=[a-zA-Z0-9]*)?$` (ARCHITECTURE section 14).
fn is_verb_name(s: &str) -> bool {
    let (name, val) = s.split_once('=').map_or((s, None), |(n, v)| (n, Some(v)));
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && val.is_none_or(|v| v.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Accepts `verb` only if it has the safe shape and its name (before any `=`) is in `known`.
/// Returns the verb unchanged.
pub fn validate_verb<'a>(verb: &'a str, known: &[Verb]) -> Result<&'a str> {
    if !is_verb_name(verb) {
        return Err(Error::Validation(format!(
            "{verb:?} is not a valid winetricks verb name"
        )));
    }
    let name = verb.split('=').next().unwrap_or(verb);
    if !known.iter().any(|v| v.name == name) {
        return Err(Error::Validation(format!(
            "unknown winetricks verb {name:?}"
        )));
    }
    Ok(verb)
}

/// Verbs recorded in `<prefix>/winetricks.log` (one per line). Missing file = none.
pub fn installed(prefix: &PrefixDir) -> Result<BTreeSet<String>> {
    let p = prefix.as_path().join("winetricks.log");
    match std::fs::read_to_string(&p) {
        Ok(s) => Ok(s
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(e) => Err(Error::io(p)(e)),
    }
}

/// Drops verbs already installed (umu-run exits 1 if a listed verb is installed).
pub fn filter_installed(verbs: &[String], installed: &BTreeSet<String>) -> Vec<String> {
    verbs
        .iter()
        .filter(|v| !installed.contains(v.as_str()))
        .cloned()
        .collect()
}

/// Clear refusal for runtimes without `protonfixes/winetricks`.
pub fn require_support(runtime: &HostPath) -> Result<()> {
    if runtime.as_path().join("protonfixes/winetricks").is_file() {
        return Ok(());
    }
    Err(Error::RuntimeInvalid {
        path: runtime.as_path().to_path_buf(),
        reason: "this runtime cannot install dependencies with winetricks; \
                 GE-Proton or UMU-Proton is required (Proton Experimental has no protonfixes)"
            .into(),
    })
}

const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Per-runtime cache of the verb list (it only changes with the runtime).
static VERB_CACHE: Mutex<BTreeMap<std::path::PathBuf, Vec<Verb>>> = Mutex::new(BTreeMap::new());

/// Runs `umu-run winetricks list-all` in the persistent scratch prefix (never a game prefix; not
/// recorded as a run), with a timeout, and parses it. Results are cached per runtime.
pub fn list_verbs(tools: &Tools, runtime: &HostPath, paths: &Paths) -> Result<Vec<Verb>> {
    require_support(runtime)?;
    let key = runtime.as_path().to_path_buf();
    if let Some(v) = VERB_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return Ok(v.clone());
    }
    crate::runner::refuse_root()?;
    let verbs = list_in(tools, runtime, paths.scratch_prefix()?)?;
    VERB_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, verbs.clone());
    Ok(verbs)
}

fn list_in(tools: &Tools, runtime: &HostPath, prefix: PrefixDir) -> Result<Vec<Verb>> {
    use std::io::Read;
    let inv = Invocation {
        program: Program::Winetricks,
        args: vec!["list-all".into()],
        cwd: prefix.as_host().clone(),
        prefix: prefix.clone(),
        runtime: runtime.clone(),
        env: vec![],
        verbose_log_dir: None,
    };
    let mut child = inv
        .command(tools, "vortex-list-verbs")?
        .spawn()
        .map_err(Error::io(&tools.umu_run))?;
    let drain = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut b);
            }
            b
        })
    };
    let out = drain(child.stdout.take().map(|r| Box::new(r) as _));
    let err = drain(child.stderr.take().map(|r| Box::new(r) as _));
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(Error::io(&tools.umu_run))? {
            break s;
        }
        if start.elapsed() > LIST_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            // Proton's own wineserver, for the scratch prefix only (ARCHITECTURE section 6).
            if let Ok(ws) = inv.wineserver() {
                let _ = std::process::Command::new(ws.as_path())
                    .arg("-k")
                    .env("WINEPREFIX", prefix.as_path())
                    .status();
            }
            return Err(Error::Validation("`winetricks list-all` timed out".into()));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let verbs = parse_list_all(&String::from_utf8_lossy(&stdout));
    if verbs.is_empty() {
        let tail: String = String::from_utf8_lossy(&stderr).chars().take(300).collect();
        return Err(Error::Validation(format!(
            "`winetricks list-all` produced no verbs (exit {:?}): {}",
            status.code(),
            crate::logs::redact(tail.trim())
        )));
    }
    Ok(verbs)
}

/// Installs one verb into the environment (kind `winetricks`). `known` comes from [`list_verbs`].
/// Returns `Ok(None)` when it is already installed (nothing is run). On `Finished`, before that
/// event is forwarded, the `dependencies` row is written: `installed` if exit 0 and the verb is
/// in `winetricks.log`, else `failed`.
pub fn install_verb(
    runs: &Runs,
    db: &std::sync::Arc<Mutex<Db>>,
    paths: &Paths,
    env_id: i64,
    verb: &str,
    known: &[Verb],
) -> Result<Option<(RunHandle, Receiver<RunEvent>)>> {
    let verb = validate_verb(verb, known)?.to_string();
    let (_, prefix, runtime) = environment_of(paths, db, env_id)?;
    require_support(&runtime)?;
    if installed(&prefix)?.contains(verb.split('=').next().unwrap_or(&verb)) {
        return Ok(None);
    }
    let invocation = Invocation {
        program: Program::Winetricks,
        args: vec![verb.clone()],
        cwd: prefix.as_host().clone(),
        prefix: prefix.clone(),
        runtime,
        env: vec![],
        verbose_log_dir: None,
    };
    let (handle, rx) = runs.start(RunSpec {
        kind: RunKind::Winetricks,
        env_id,
        game_id: None,
        install_id: None,
        invocation,
    })?;
    let (tx, out) = async_channel::bounded(1024);
    let (db, run_id) = (db.clone(), handle.run_id());
    std::thread::spawn(move || {
        while let Ok(ev) = rx.recv_blocking() {
            if let RunEvent::Finished(o) = &ev {
                let name = verb.split('=').next().unwrap_or(&verb);
                let ok = o.exit == Some(Exit::Low8(0))
                    && installed(&prefix).is_ok_and(|s| s.contains(name));
                let state = if ok {
                    DepState::Installed
                } else {
                    DepState::Failed
                };
                let _ = db
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .upsert_dependency(env_id, name, Some(run_id), state);
            }
            if tx.send_blocking(ev).is_err() {
                break;
            }
        }
    });
    Ok(Some((handle, out)))
}
