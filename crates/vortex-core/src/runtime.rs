//! Proton runtime discovery and validation.
//!
//! TODO(M2): switch to `vortex_core::Error` / `HostPath` once M2 merges.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("invalid runtime {}: {reason}", path.display())]
    Invalid { path: PathBuf, reason: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("network error: {0}")]
    Net(String),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    Checksum { expected: String, actual: String },
    #[error("bad archive: {0}")]
    Archive(String),
    #[error("no suitable GE-Proton asset in release")]
    NoAsset,
    #[error("{} is already installed", .0.display())]
    AlreadyInstalled(PathBuf),
    #[error("cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    /// Canonical absolute Proton dir (PROTONPATH).
    pub path: PathBuf,
    pub label: String,
    pub version: Option<String>,
}

impl Runtime {
    /// `files/bin/wineserver` (or older `dist/bin/wineserver`), if present.
    pub fn wineserver(&self) -> Option<PathBuf> {
        ["files", "dist"]
            .iter()
            .map(|d| self.path.join(d).join("bin/wineserver"))
            .find(|p| p.is_file())
    }
}

fn invalid(path: &Path, reason: &str) -> RuntimeError {
    RuntimeError::Invalid {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

/// Valid iff an executable `proton` and a `toolmanifest.vdf` exist.
pub fn validate(dir: &Path) -> Result<Runtime, RuntimeError> {
    let path = fs::canonicalize(dir).map_err(|e| invalid(dir, &e.to_string()))?;
    let proton = path.join("proton");
    let exec =
        fs::metadata(&proton).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if !exec {
        return Err(invalid(&path, "missing executable `proton`"));
    }
    if !path.join("toolmanifest.vdf").is_file() {
        return Err(invalid(&path, "missing toolmanifest.vdf"));
    }
    let label = fs::read_to_string(path.join("compatibilitytool.vdf"))
        .ok()
        .and_then(|s| vdf_value(&s, "display_name"))
        .unwrap_or_else(|| {
            path.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into())
        });
    // `version` is `<unix-ts> <name>`; keep the name.
    let version = fs::read_to_string(path.join("version")).ok().and_then(|s| {
        let l = s.lines().next()?.trim();
        let v = l.split_once(' ').map_or(l, |(_, n)| n.trim());
        (!v.is_empty()).then(|| v.to_string())
    });
    Ok(Runtime {
        path,
        label,
        version,
    })
}

/// Value of `"key"  "value"` in a VDF text (first match, not structure-aware).
fn vdf_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let mut q = l.split('"').skip(1).step_by(2);
        (q.next()? == key).then(|| q.next().map(str::to_string))?
    })
}

/// Library roots from `libraryfolders.vdf` (`"path"` entries).
fn steam_libraries(steam: &Path) -> Vec<PathBuf> {
    let mut v = vec![steam.to_path_buf()];
    if let Ok(t) = fs::read_to_string(steam.join("steamapps/libraryfolders.vdf")) {
        v.extend(
            t.lines()
                .filter_map(|l| vdf_value(l, "path"))
                .map(PathBuf::from),
        );
    }
    v
}

/// Validate every child of each `root`, deduped by canonical path, in root order.
pub fn discover_in(roots: &[PathBuf]) -> Vec<Runtime> {
    let mut out: Vec<Runtime> = Vec::new();
    for root in roots {
        let Ok(rd) = fs::read_dir(root) else { continue };
        let mut kids: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        kids.sort();
        for k in kids {
            if let Ok(rt) = validate(&k)
                && !out.iter().any(|r| r.path == rt.path)
            {
                out.push(rt);
            }
        }
    }
    out
}

/// Scan the standard locations (upstream.md 3.1) plus Vortex's own `runtimes_dir`.
pub fn discover(home: &Path, runtimes_dir: &Path) -> Vec<Runtime> {
    let steam = home.join(".local/share/Steam");
    let mut roots = vec![
        runtimes_dir.to_path_buf(),
        steam.join("compatibilitytools.d"),
        home.join(".steam/root/compatibilitytools.d"),
        home.join(".steam/steam/compatibilitytools.d"),
        "/usr/share/steam/compatibilitytools.d".into(),
        "/usr/local/share/steam/compatibilitytools.d".into(),
        home.join(".local/share/umu/compatibilitytools"),
    ];
    for lib in steam_libraries(&steam) {
        roots.push(lib.join("steamapps/common"));
    }
    let flat = home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam");
    roots.push(flat.join("compatibilitytools.d"));
    roots.push(flat.join("steamapps/common"));
    discover_in(&roots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    pub fn fake(dir: &Path, layout: &str) {
        fs::create_dir_all(dir.join(layout).join("bin")).unwrap();
        fs::write(dir.join(layout).join("bin/wineserver"), "").unwrap();
        fs::write(dir.join("proton"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(dir.join("proton"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.join("toolmanifest.vdf"), "").unwrap();
    }

    #[test]
    fn discovery() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        let a = r.join("GE-Proton9-1");
        fake(&a, "files");
        fs::write(a.join("version"), "1700000000 GE-Proton9-1\n").unwrap();
        fs::write(
            a.join("compatibilitytool.vdf"),
            "\"x\"\n{\n\t\"display_name\" \"GE 9-1\"\n}\n",
        )
        .unwrap();
        let dist = r.join("Proton 8");
        fake(&dist, "dist");
        let nomanifest = r.join("nomanifest");
        fake(&nomanifest, "files");
        fs::remove_file(nomanifest.join("toolmanifest.vdf")).unwrap();
        let battleye = r.join("Proton BattlEye Runtime"); // no `proton`
        fs::create_dir_all(&battleye).unwrap();
        fs::write(battleye.join("toolmanifest.vdf"), "").unwrap();
        let other = r.join("other");
        fs::create_dir(&other).unwrap();
        symlink(&a, other.join("dup")).unwrap();

        let found = discover_in(&[r.to_path_buf(), other]);
        let names: Vec<_> = found.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(names, ["GE 9-1", "Proton 8"]); // sorted; symlink dup dropped
        let ge = &found[0];
        assert_eq!(ge.version.as_deref(), Some("GE-Proton9-1"));
        assert!(ge.wineserver().unwrap().ends_with("files/bin/wineserver"));
        assert!(
            found[0]
                .wineserver()
                .unwrap()
                .ends_with("dist/bin/wineserver")
        );
        assert!(validate(&nomanifest).is_err());
        assert!(validate(&battleye).is_err());
    }

    #[test]
    fn steam_library_scan() {
        let t = tempfile::tempdir().unwrap();
        let lib2 = t.path().join("lib2");
        fake(&lib2.join("steamapps/common/Proton X"), "files");
        let sa = t.path().join(".local/share/Steam/steamapps");
        fs::create_dir_all(&sa).unwrap();
        fs::write(
            sa.join("libraryfolders.vdf"),
            format!("\"0\"\n{{\n\"path\"\t\"{}\"\n}}\n", lib2.display()),
        )
        .unwrap();
        let found = discover(t.path(), &t.path().join("rt"));
        assert!(found.iter().any(|r| r.label == "Proton X"));
    }

    #[test]
    #[ignore = "reads real ~/.local/share/Steam read-only"]
    fn real_discovery() {
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let found = discover(&home, &home.join(".local/share/vortex/runtimes"));
        assert!(!found.is_empty(), "no Proton found on this machine");
        for r in &found {
            println!("{} {:?} {}", r.label, r.version, r.path.display());
        }
    }
}
