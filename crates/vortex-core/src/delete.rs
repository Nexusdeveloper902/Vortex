//! Deletion plans restricted to managed paths (ARCHITECTURE section 11, rule 5).
//!
//! The UI shows a [`DeletionPlan`] and [`execute`] removes exactly that plan, re-validating first.
//! Installer sources and `prefix_managed=0` prefixes never appear in a plan.

use crate::db::Db;
use crate::error::{EnvId, Error, Result};
use crate::paths::{HostPath, Paths};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionItem {
    pub path: HostPath,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionPlan {
    pub env_id: EnvId,
    /// The game row removed with the plan (`plan_game_removal`).
    pub game_id: Option<i64>,
    /// Whether the environment row is removed too.
    pub remove_env: bool,
    pub items: Vec<DeletionItem>,
}

/// Plan for deleting a game: its managed install dir, and its environment (prefix) when no other
/// game uses it.
pub fn plan_game_removal(db: &Db, paths: &Paths, game_id: i64) -> Result<DeletionPlan> {
    let game = db.get_game(game_id)?;
    build(db, paths, game.environment_id, Some(game_id))
}

/// Plan for deleting an environment that no game references.
pub fn plan_environment_removal(db: &Db, paths: &Paths, env_id: EnvId) -> Result<DeletionPlan> {
    build(db, paths, env_id, None)
}

fn build(db: &Db, paths: &Paths, env_id: EnvId, game_id: Option<i64>) -> Result<DeletionPlan> {
    let env = db.get_environment(env_id)?;
    if db.active_run_for_environment(env_id)?.is_some() {
        return Err(Error::Busy(env_id));
    }
    let others: Vec<_> = db
        .games_in_environment(env_id)?
        .into_iter()
        .filter(|g| Some(g.id) != game_id)
        .collect();
    let remove_env = others.is_empty();
    if game_id.is_none() && !remove_env {
        return Err(Error::Validation(format!(
            "environment {env_id} is still used by {} game(s)",
            others.len()
        )));
    }
    let roots = Roots::new(paths)?;
    let mut protect: Vec<PathBuf> = Vec::new();
    for e in db.list_environments()? {
        if e.id != env_id {
            protect.extend(canon_opt(Path::new(&e.prefix_path)));
        }
    }
    for g in db.list_games()? {
        if Some(g.id) != game_id {
            protect.extend(
                g.install_dir
                    .as_deref()
                    .and_then(|d| canon_opt(Path::new(d))),
            );
        }
    }
    let mut items = Vec::new();
    if remove_env && env.prefix_managed {
        let p = Path::new(&env.prefix_path);
        if let Some(c) = managed_item(p, &roots, &protect, true)? {
            items.push(c);
        }
    }
    if let Some(gid) = game_id {
        let g = db.get_game(gid)?;
        if let (Some(dir), true) = (&g.install_dir, g.install_dir_managed) {
            let mut prot = protect.clone();
            if let Some(i) = db.latest_install_for_game(gid)? {
                prot.extend(canon_opt(Path::new(&i.source_path)));
                prot.extend(canon_opt(Path::new(&i.installer_path)));
            }
            if let Some(c) = managed_item(Path::new(dir), &roots, &prot, false)? {
                items.push(c);
            }
        }
    }
    let items = items
        .into_iter()
        .map(|c| {
            Ok(DeletionItem {
                bytes: tree_bytes(&c),
                path: HostPath::new(c)?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(DeletionPlan {
        env_id,
        game_id,
        remove_env,
        items,
    })
}

struct Roots {
    data: PathBuf,
    prefixes: PathBuf,
    home: PathBuf,
}

impl Roots {
    fn new(paths: &Paths) -> Result<Self> {
        let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        Ok(Self {
            data: canon(paths.data_dir()),
            prefixes: canon(&paths.prefixes_dir()),
            home: canon(paths.home()),
        })
    }
}

fn canon_opt(p: &Path) -> Option<PathBuf> {
    fs::canonicalize(p).ok()
}

/// Validates one candidate. `Ok(None)` when it does not exist (nothing to delete);
/// `Err` when it is not allowed.
fn managed_item(p: &Path, r: &Roots, protect: &[PathBuf], prefix: bool) -> Result<Option<PathBuf>> {
    let refuse =
        |why: &str| Error::Validation(format!("refusing to delete {}: {why}", p.display()));
    let Ok(meta) = fs::symlink_metadata(p) else {
        return Ok(None);
    };
    if meta.is_symlink() {
        return Err(refuse("it is a symlink"));
    }
    if !meta.is_dir() {
        return Err(refuse("it is not a directory"));
    }
    let c = fs::canonicalize(p).map_err(Error::io(p))?;
    if c == Path::new("/") || c == r.home || r.home.starts_with(&c) || r.data.starts_with(&c) {
        return Err(refuse("it is a protected location"));
    }
    let in_prefixes = c.starts_with(&r.prefixes) && c != r.prefixes;
    if prefix && !in_prefixes {
        return Err(refuse("it is outside the managed prefixes directory"));
    }
    if !prefix && c.starts_with(&r.data) && !in_prefixes {
        return Err(refuse("it is inside Vortex's data directory"));
    }
    if let Some(x) = protect
        .iter()
        .find(|x| x.starts_with(&c) || c.starts_with(x))
    {
        return Err(refuse(&format!("it overlaps {}", x.display())));
    }
    Ok(Some(c))
}

/// Sum of file sizes; symlinks counted as links, never followed.
fn tree_bytes(p: &Path) -> u64 {
    let Ok(m) = fs::symlink_metadata(p) else {
        return 0;
    };
    if !m.is_dir() {
        return m.len();
    }
    fs::read_dir(p)
        .map(|rd| rd.flatten().map(|e| tree_bytes(&e.path())).sum())
        .unwrap_or(0)
}

/// Removes exactly `plan`. The plan is rebuilt first; if its items differ, nothing is deleted.
pub fn execute(db: &Db, paths: &Paths, plan: &DeletionPlan) -> Result<()> {
    let fresh = build(db, paths, plan.env_id, plan.game_id)?;
    let paths_of = |p: &DeletionPlan| p.items.iter().map(|i| i.path.clone()).collect::<Vec<_>>();
    if paths_of(&fresh) != paths_of(plan) || fresh.remove_env != plan.remove_env {
        return Err(Error::Validation(
            "the deletion plan changed; review it again".into(),
        ));
    }
    for it in &plan.items {
        // remove_dir_all does not follow symlinks (prefixes contain dosdevices/z: -> /)
        fs::remove_dir_all(it.path.as_path()).map_err(Error::io(it.path.as_path()))?;
    }
    if let Some(g) = plan.game_id {
        db.delete_game(g)?;
    }
    if plan.remove_env {
        db.delete_environment(plan.env_id)?;
    }
    Ok(())
}
