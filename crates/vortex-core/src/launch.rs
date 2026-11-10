//! Launching games and running exe/msi files in an environment.
//!
//! Both build an [`Invocation`] and hand it to [`Runs::start`], which validates, records the run
//! row (kind `launch` / `exec`, so `Db::last_launch` is the "last launch" info), spawns and
//! supervises. One active run per environment (`Error::Busy`).

use crate::db::{Db, Environment, Game, RunKind};
use crate::error::{Error, Result};
use crate::paths::{HostPath, Paths, PrefixDir, to_win};
use crate::process::{RunEvent, RunHandle, RunSpec, Runs};
use crate::runner::{Invocation, Program, SystemExe};
use async_channel::Receiver;
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

type Started = (RunHandle, Receiver<RunEvent>);

pub(crate) fn environment_of(
    paths: &Paths,
    db: &Mutex<Db>,
    id: i64,
) -> Result<(Environment, PrefixDir, HostPath)> {
    let env = db
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_environment(id)?;
    let prefix = PrefixDir::new(&env.prefix_path, paths.home(), &[])?;
    let runtime = HostPath::new(&env.runtime_path)?;
    Ok((env, prefix, runtime))
}

/// Builds the invocation for a game from its saved config (pure apart from the db reads done
/// by the caller). `launch_args` is a JSON array, `env_vars` a JSON object; the cwd is
/// `work_dir` or the exe's folder.
pub fn game_invocation(game: &Game, prefix: PrefixDir, runtime: HostPath) -> Result<Invocation> {
    let exe = game
        .exe_path
        .as_deref()
        .ok_or_else(|| Error::Validation(format!("{} has no executable configured", game.name)))?;
    let exe = HostPath::new(exe)?;
    if !exe.as_path().is_file() {
        return Err(Error::NotFound(format!(
            "executable {} of {}",
            exe.as_path().display(),
            game.name
        )));
    }
    let args: Vec<String> = serde_json::from_str(&game.launch_args).map_err(|e| {
        Error::Validation(format!(
            "launch arguments are not a JSON array of strings: {e}"
        ))
    })?;
    let env: BTreeMap<String, String> = serde_json::from_str(&game.env_vars).map_err(|e| {
        Error::Validation(format!(
            "environment variables are not a JSON object of strings: {e}"
        ))
    })?;
    let cwd = match game.work_dir.as_deref().filter(|w| !w.is_empty()) {
        Some(w) => HostPath::new(w)?,
        None => exe.parent().ok_or_else(|| {
            Error::InvalidPath(format!("{} has no folder", exe.as_path().display()))
        })?,
    };
    Ok(Invocation {
        program: Program::Exe(exe),
        args,
        cwd,
        prefix,
        runtime,
        env: env.into_iter().collect(),
        verbose_log_dir: None,
    })
}

/// Launches a registered game. Errors before anything is written or spawned: `NotFound` (exe or
/// cwd missing), `Validation`, `RuntimeInvalid`, `Busy`.
pub fn launch_game(runs: &Runs, db: &Mutex<Db>, paths: &Paths, game_id: i64) -> Result<Started> {
    let game = db
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_game(game_id)?;
    let (_, prefix, runtime) = environment_of(paths, db, game.environment_id)?;
    let invocation = game_invocation(&game, prefix, runtime)?;
    runs.start(RunSpec {
        kind: RunKind::Launch,
        env_id: game.environment_id,
        game_id: Some(game_id),
        install_id: None,
        invocation,
    })
}

/// Runs an arbitrary `.exe` (with `args`) or `.msi` (via `msiexec /i`) inside an existing
/// environment. The cwd is the file's folder. Extra `env` goes through `validate_env`.
pub fn run_in_env(
    runs: &Runs,
    db: &Mutex<Db>,
    paths: &Paths,
    env_id: i64,
    file: &HostPath,
    args: Vec<String>,
    env: Vec<(String, String)>,
) -> Result<Started> {
    let (_, prefix, runtime) = environment_of(paths, db, env_id)?;
    let ext = file
        .as_path()
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let (program, args) = match ext.as_deref() {
        Some("exe") => (Program::Exe(file.clone()), args),
        Some("msi") => {
            if !file.as_path().is_file() {
                return Err(Error::NotFound(format!(
                    "installer {}",
                    file.as_path().display()
                )));
            }
            let win = to_win(&prefix, file);
            let mut a = vec!["/i".to_string(), win.as_str().to_string()];
            a.extend(args);
            (Program::System(SystemExe::Msiexec), a)
        }
        _ => {
            return Err(Error::Validation(format!(
                "{} is not an .exe or .msi file",
                file.as_path().display()
            )));
        }
    };
    let cwd = file
        .parent()
        .ok_or_else(|| Error::InvalidPath(format!("{} has no folder", file.as_path().display())))?;
    runs.start(RunSpec {
        kind: RunKind::Exec,
        env_id,
        game_id: None,
        install_id: None,
        invocation: Invocation {
            program,
            args,
            cwd,
            prefix,
            runtime,
            env,
            verbose_log_dir: None,
        },
    })
}
