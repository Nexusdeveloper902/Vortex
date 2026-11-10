//! Prefix backup and restore (ARCHITECTURE section 11, rule 6).
//!
//! Archives never follow symlinks (prefixes contain `dosdevices/z: -> /`) and restore validates
//! every entry before it touches the live prefix.

use crate::db::{Backup, Db, NewBackup, now};
use crate::error::{EnvId, Error, Result};
use crate::paths::{Paths, PrefixDir, create_private_dir};
use std::fs;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};

/// Archives the environment's prefix into `data/backups/<env>-<ts>.tar.gz`.
/// Refuses with `Error::Busy` while a run is active for the environment.
pub fn backup(db: &Db, paths: &Paths, env_id: EnvId, cancel: &AtomicBool) -> Result<Backup> {
    let env = db.get_environment(env_id)?;
    if db.active_run_for_environment(env_id)?.is_some() {
        return Err(Error::Busy(env_id));
    }
    let prefix = Path::new(&env.prefix_path);
    let meta = fs::symlink_metadata(prefix).map_err(Error::io(prefix))?;
    if !meta.is_dir() {
        return Err(Error::Validation(format!(
            "{} is not a directory",
            prefix.display()
        )));
    }
    let game_name = db
        .games_in_environment(env_id)?
        .first()
        .map(|g| g.name.clone())
        .unwrap_or_else(|| format!("environment {env_id}"));
    let dir = paths.backups_dir();
    create_private_dir(&dir)?;
    let ts = now();
    let mut dest = dir.join(format!("{env_id}-{ts}.tar.gz"));
    for n in 1.. {
        if !dest.exists() {
            break;
        }
        dest = dir.join(format!("{env_id}-{ts}-{n}.tar.gz"));
    }
    let part = dest.with_extension("gz.part");
    let res = write_archive(prefix, &part, cancel).and_then(|()| {
        fs::rename(&part, &dest).map_err(Error::io(&dest))?;
        Ok(fs::metadata(&dest).map_err(Error::io(&dest))?.len())
    });
    let size = match res {
        Ok(s) => s,
        Err(e) => {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
    };
    let id = db.insert_backup(&NewBackup {
        environment_id: Some(env_id),
        game_name,
        archive_path: dest.to_string_lossy().into_owned(),
        runtime_path: env.runtime_path,
        prefix_version: fs::read_to_string(prefix.join("version"))
            .ok()
            .map(|s| s.trim().to_string()),
        size_bytes: size as i64,
    })?;
    db.get_backup(id)
}

fn write_archive(prefix: &Path, out: &Path, cancel: &AtomicBool) -> Result<()> {
    let f = fs::File::create(out).map_err(Error::io(out))?;
    let gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
    let mut b = tar::Builder::new(gz);
    b.follow_symlinks(false); // mandatory: z: -> /
    append_tree(&mut b, prefix, Path::new(""), cancel)?;
    b.into_inner()
        .and_then(|gz| gz.finish())
        .map_err(Error::io(out))?;
    Ok(())
}

/// Manual walk so cancel is honoured and symlinks are never descended into.
fn append_tree<W: std::io::Write>(
    b: &mut tar::Builder<W>,
    root: &Path,
    rel: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let dir = root.join(rel);
    for ent in fs::read_dir(&dir).map_err(Error::io(&dir))? {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let ent = ent.map_err(Error::io(&dir))?;
        let rel = rel.join(ent.file_name());
        let full = root.join(&rel);
        let ft = ent.file_type().map_err(Error::io(&full))?;
        if !(ft.is_file() || ft.is_dir() || ft.is_symlink()) {
            continue; // FIFOs, sockets, devices: restore would reject them
        }
        b.append_path_with_name(&full, &rel)
            .map_err(Error::io(&full))?;
        if ent.file_type().map_err(Error::io(&full))?.is_dir() {
            append_tree(b, root, &rel, cancel)?;
        }
    }
    Ok(())
}

/// Restores a backup into `target` (a direct child of `data/prefixes`).
/// Unpacks to `data/prefixes/.restore-<id>`, validates every entry, then renames into place.
/// An existing target is replaced only when `confirm_overwrite` is true; it is moved aside and
/// removed after the rename succeeded.
pub fn restore(
    db: &Db,
    paths: &Paths,
    backup_id: i64,
    target: &PrefixDir,
    confirm_overwrite: bool,
) -> Result<()> {
    restore_limited(
        db,
        paths,
        backup_id,
        target,
        confirm_overwrite,
        MAX_RESTORE_BYTES,
    )
}

/// Upper bound on the total size of unpacked file contents (64 GiB).
pub const MAX_RESTORE_BYTES: u64 = 64 << 30;

/// [`restore`] with an explicit cap on the total unpacked bytes.
pub fn restore_limited(
    db: &Db,
    paths: &Paths,
    backup_id: i64,
    target: &PrefixDir,
    confirm_overwrite: bool,
    max_bytes: u64,
) -> Result<()> {
    let b = db.get_backup(backup_id)?;
    let archive = Path::new(&b.archive_path);
    if !archive.starts_with(paths.backups_dir())
        || archive.components().any(|c| c == Component::ParentDir)
    {
        return Err(Error::InvalidPath(format!(
            "{} is outside the backups directory",
            archive.display()
        )));
    }
    let prefixes = paths.prefixes_dir();
    let t = target.as_path();
    let name = t.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if t.parent() != Some(prefixes.as_path()) || name.is_empty() || name.starts_with('.') {
        return Err(Error::InvalidPath(format!(
            "{} is not a managed prefix location",
            t.display()
        )));
    }
    for e in db.list_environments()? {
        if Path::new(&e.prefix_path) == t && db.active_run_for_environment(e.id)?.is_some() {
            return Err(Error::Busy(e.id));
        }
    }
    create_private_dir(&prefixes)?;
    // names keyed by target, so a crashed restore can be recovered
    let tmp = prefixes.join(format!(".restore-{name}"));
    let old = prefixes.join(format!(".old-{name}"));
    let live = |p: &Path| fs::symlink_metadata(p).is_ok();
    if live(&old) && !live(t) {
        // a previous restore crashed between its two renames: put the user's prefix back
        fs::rename(&old, t).map_err(Error::io(t))?;
    }
    let exists = live(t);
    if exists && !confirm_overwrite {
        return Err(Error::Validation(format!(
            "{} already exists; restoring would overwrite it",
            t.display()
        )));
    }
    if live(&old) {
        // target is in place, so this is the leftover of a finished restore
        fs::remove_dir_all(&old).map_err(Error::io(&old))?;
    }
    if live(&tmp) {
        fs::remove_dir_all(&tmp).map_err(Error::io(&tmp))?;
    }
    fs::create_dir(&tmp).map_err(Error::io(&tmp))?;
    if let Err(e) = unpack_checked(archive, &tmp, max_bytes) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let swap = || -> Result<()> {
        if exists {
            fs::rename(t, &old).map_err(Error::io(t))?;
        }
        if let Err(e) = fs::rename(&tmp, t) {
            if exists {
                let _ = fs::rename(&old, t);
            }
            return Err(Error::Io {
                path: t.to_path_buf(),
                source: e,
            });
        }
        Ok(())
    };
    if let Err(e) = swap() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if exists {
        let _ = fs::remove_dir_all(&old); // new prefix is in place; leftovers are retried next time
    }
    Ok(())
}

fn unpack_checked(archive: &Path, dest: &Path, max_bytes: u64) -> Result<()> {
    let f = fs::File::open(archive).map_err(Error::io(archive))?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    ar.set_preserve_permissions(false);
    let mut total = 0u64;
    ar.set_unpack_xattrs(false);
    let bad = |p: &Path, why: &str| {
        Error::Validation(format!("unsafe backup entry {}: {why}", p.display()))
    };
    for ent in ar.entries().map_err(Error::io(archive))? {
        let mut ent = ent.map_err(Error::io(archive))?;
        let path = ent.path().map_err(Error::io(archive))?.into_owned();
        let mut cur = dest.to_path_buf();
        let comps: Vec<_> = path.components().collect();
        let mut named = 0;
        for (i, c) in comps.iter().enumerate() {
            match c {
                Component::CurDir => {}
                Component::Normal(n) => {
                    named += 1;
                    if i + 1 < comps.len()
                        && fs::symlink_metadata(cur.join(n)).is_ok_and(|m| m.is_symlink())
                    {
                        return Err(bad(&path, "written through a symlink"));
                    }
                    cur.push(n);
                }
                _ => return Err(bad(&path, "absolute or '..' path")),
            }
        }
        if named == 0 {
            continue; // the "." root entry
        }
        let ty = ent.header().entry_type();
        if !(ty.is_file() || ty.is_dir() || ty.is_symlink()) {
            return Err(bad(&path, "unsupported entry type"));
        }
        total = total.saturating_add(ent.size());
        if total > max_bytes {
            return Err(Error::Validation(format!(
                "backup unpacks to more than {max_bytes} bytes; refusing"
            )));
        }
        let mode = ent.header().mode().unwrap_or(0o644);
        if !ent.unpack_in(dest).map_err(Error::io(&cur))? {
            return Err(bad(&path, "rejected"));
        }
        if !ty.is_symlink() {
            // no setuid/setgid/sticky; owner can always read, write (and enter dirs)
            let m = (mode & 0o777) | if ty.is_dir() { 0o700 } else { 0o600 };
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cur, fs::Permissions::from_mode(m)).map_err(Error::io(&cur))?;
        }
    }
    Ok(())
}
