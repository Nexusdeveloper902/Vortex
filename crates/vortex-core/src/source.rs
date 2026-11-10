//! Installer source analysis and candidate ranking. Pure filesystem reads; nothing is executed.
//! Symlinks are never followed: they are skipped during the walk, so no candidate can escape the root.

// TODO(M2): switch to vortex_core::Error / HostPath
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

const MAX_DEPTH: usize = 2;
/// Top two candidates closer than this are ambiguous.
const THRESHOLD: i32 = 10;
const REDIST_DIRS: &[&str] = &[
    "redist",
    "directx",
    "_commonredist",
    "support",
    "vcredist",
    "dotnet",
    "physx",
];

// TODO(M2): switch to vortex_core::Error
#[derive(Debug)]
pub enum SourceError {
    /// Path contains `..` or is not absolute-resolvable.
    InvalidPath(PathBuf),
    /// Neither a regular file nor a directory.
    NotFileOrDir(PathBuf),
    /// A file that is not `.exe` or `.msi`.
    Unsupported(PathBuf),
    Io(PathBuf, std::io::Error),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(p) => write!(f, "invalid path: {}", p.display()),
            Self::NotFileOrDir(p) => write!(f, "not a file or directory: {}", p.display()),
            Self::Unsupported(p) => write!(
                f,
                "unsupported installer type (expected .exe or .msi): {}",
                p.display()
            ),
            Self::Io(p, e) => write!(f, "{}: {e}", p.display()),
        }
    }
}

impl std::error::Error for SourceError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub score: i32,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAnalysis {
    pub kind: SourceKind,
    /// Sorted best first (ties by path).
    pub candidates: Vec<Candidate>,
    /// The UI must ask the user to pick.
    pub ambiguous: bool,
}

fn lower(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn ext(name: &str) -> &str {
    name.rsplit_once('.').map_or("", |(_, e)| e)
}

pub fn analyze(path: &Path) -> Result<SourceAnalysis, SourceError> {
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(SourceError::InvalidPath(path.to_owned()));
    }
    // The user's explicit choice may itself be a symlink; resolve it once, then never follow again.
    let root = fs::canonicalize(path).map_err(|e| SourceError::Io(path.to_owned(), e))?;
    let meta = fs::metadata(&root).map_err(|e| SourceError::Io(root.clone(), e))?;
    if meta.is_file() {
        if !matches!(ext(&lower(&root)), "exe" | "msi") {
            return Err(SourceError::Unsupported(root));
        }
        return Ok(SourceAnalysis {
            kind: SourceKind::File,
            candidates: vec![Candidate {
                path: root,
                score: 100,
                reasons: vec!["selected file".into()],
            }],
            ambiguous: false,
        });
    }
    if !meta.is_dir() {
        return Err(SourceError::NotFileOrDir(root));
    }

    // (path relative to root, depth)
    let mut files = Vec::new();
    walk(&root, &root, 0, &mut files)?;
    let rel_lower = |p: &Path| p.to_string_lossy().to_lowercase();
    let has = |dir: &Path, name: &str| {
        files
            .iter()
            .any(|f| f.parent() == Some(dir) && lower(f) == name)
    };
    let autorun = autorun_target(&root, &files);

    let mut candidates: Vec<Candidate> = files
        .iter()
        .filter(|f| matches!(ext(&lower(f)), "exe" | "msi"))
        .map(|f| {
            let name = lower(f);
            let dir = f.parent().unwrap_or(Path::new(""));
            let depth = f.components().count() - 1;
            let mut score = 10;
            let mut reasons = vec!["executable found".to_string()];
            let mut add = |d: i32, r: &str| {
                score += d;
                reasons.push(r.to_string());
            };
            if depth > 0 {
                add(-30, "not at top level");
            }
            if name == "setup.exe" || name == "install.exe" {
                add(50, "conventional installer name");
            } else if name.starts_with("setup") && name.ends_with(".exe") {
                add(20, "setup-style name");
            }
            if name.starts_with("setup_") && name.ends_with(".exe") {
                let stem = name.trim_end_matches(".exe");
                let gog = files.iter().any(|b| {
                    let n = lower(b);
                    b.parent() == Some(dir)
                        && n.starts_with(&format!("{stem}-"))
                        && n.ends_with(".bin")
                });
                if gog {
                    add(60, "has numbered .bin data (GOG/Inno style)");
                }
            }
            if ext(&name) == "exe" && (has(dir, "data1.cab") || has(dir, "data1.hdr")) {
                add(30, "sibling data1.cab/hdr (InstallShield)");
            }
            if name.ends_with(".msi") && depth == 0 {
                add(40, "top-level MSI");
            }
            if autorun.as_deref() == Some(f.as_path()) {
                add(70, "autorun.inf open target");
            }
            let rel = rel_lower(f);
            if rel.split('/').take(depth).any(|c| REDIST_DIRS.contains(&c)) {
                add(-60, "inside redistributable directory");
            }
            if name.starts_with("unins") {
                add(-80, "uninstaller");
            }
            if name.starts_with("dxsetup") || name.starts_with("vc_redist") {
                add(-50, "prerequisite installer");
            }
            Candidate {
                path: root.join(f),
                score,
                reasons,
            }
        })
        .collect();
    candidates.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    let ambiguous = match candidates.as_slice() {
        [] => true,
        [_] => false,
        [a, b, ..] => a.score - b.score < THRESHOLD,
    };
    Ok(SourceAnalysis {
        kind: SourceKind::Dir,
        candidates,
        ambiguous,
    })
}

/// Collects regular files (relative paths) to `MAX_DEPTH`; symlinks are skipped.
fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<(), SourceError> {
    let rd = fs::read_dir(dir).map_err(|e| SourceError::Io(dir.to_owned(), e))?;
    for entry in rd {
        let entry = entry.map_err(|e| SourceError::Io(dir.to_owned(), e))?;
        let Ok(ft) = entry.file_type() else { continue };
        let p = entry.path();
        if ft.is_file() {
            if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_owned());
            }
        } else if ft.is_dir() && depth < MAX_DEPTH {
            walk(root, &p, depth + 1, out)?;
        }
    }
    Ok(())
}

/// Top-level `autorun.inf` `open=` target, matched case-insensitively against walked files.
fn autorun_target(root: &Path, files: &[PathBuf]) -> Option<PathBuf> {
    let inf = files
        .iter()
        .find(|f| lower(f) == "autorun.inf" && f.components().count() == 1)?;
    let bytes = fs::read(root.join(inf)).ok()?;
    // Windows-1252-ish: lossy is fine for ASCII keys and paths.
    let text = String::from_utf8_lossy(&bytes);
    let line = text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        k.trim().eq_ignore_ascii_case("open").then(|| v.trim())
    })?;
    let target = match line.strip_prefix('"') {
        Some(r) => r.split('"').next()?,
        None => line.split_whitespace().next()?,
    };
    let want = target
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_lowercase();
    files
        .iter()
        .find(|f| f.to_string_lossy().to_lowercase() == want)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for f in files {
            let p = d.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, b"x").unwrap();
        }
        d
    }
    fn top(a: &SourceAnalysis) -> String {
        a.candidates[0]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into()
    }

    #[test]
    fn standalone_exe_and_msi() {
        let d = tree(&["Game.EXE", "a.MSI", "notes.txt"]);
        for n in ["Game.EXE", "a.MSI"] {
            let a = analyze(&d.path().join(n)).unwrap();
            assert_eq!(a.kind, SourceKind::File);
            assert!(!a.ambiguous);
            assert_eq!(a.candidates.len(), 1);
        }
        assert!(matches!(
            analyze(&d.path().join("notes.txt")),
            Err(SourceError::Unsupported(_))
        ));
    }

    #[test]
    fn zip_rejected() {
        let d = tree(&["g.zip"]);
        assert!(matches!(
            analyze(&d.path().join("g.zip")),
            Err(SourceError::Unsupported(_))
        ));
    }

    #[test]
    fn spec_example_tree() {
        let d = tree(&[
            "setup.exe",
            "data1.bin",
            "data2.bin",
            "data3.bin",
            "setup-1.bin",
            "setup-2.bin",
            "Redist/vcredist_x64.exe",
            "Redist/DirectX/DXSETUP.exe",
        ]);
        let a = analyze(d.path()).unwrap();
        assert_eq!(a.kind, SourceKind::Dir);
        assert_eq!(top(&a), "setup.exe");
        assert!(!a.ambiguous);
        assert_eq!(a.candidates.len(), 3);
    }

    #[test]
    fn gog_layout_wins() {
        let d = tree(&[
            "setup_game_1.0.exe",
            "setup_game_1.0-1.bin",
            "extras/other.exe",
        ]);
        let a = analyze(d.path()).unwrap();
        assert_eq!(top(&a), "setup_game_1.0.exe");
        assert!(!a.ambiguous);
    }

    #[test]
    fn installshield_boost() {
        let d = tree(&["launcher.exe", "disc/inst.exe", "disc/data1.cab"]);
        let a = analyze(d.path()).unwrap();
        // depth-1 inst.exe with data1.cab (10-30+30=10) ties launcher (10): both listed, ambiguous.
        assert!(
            a.candidates
                .iter()
                .any(|c| c.reasons.iter().any(|r| r.contains("InstallShield")))
        );
        let d = tree(&["a.exe", "b.exe", "data1.cab"]);
        let a = analyze(d.path()).unwrap();
        assert!(a.ambiguous); // both boosted equally
        let d = tree(&["setup.exe", "data1.hdr", "tool.exe"]);
        assert_eq!(top(&analyze(d.path()).unwrap()), "setup.exe");
    }

    #[test]
    fn autorun_target_wins() {
        let d = tree(&["setup.exe", "start.exe"]);
        fs::write(
            d.path().join("AUTORUN.INF"),
            "[autorun]\r\nOpen=START.EXE /x\r\n",
        )
        .unwrap();
        let a = analyze(d.path()).unwrap();
        assert_eq!(top(&a), "start.exe");
        assert!(!a.ambiguous);
    }

    #[test]
    fn autorun_traversal_ignored() {
        let d = tree(&["setup.exe"]);
        fs::write(d.path().join("autorun.inf"), "open=..\\..\\evil.exe\n").unwrap();
        let a = analyze(d.path()).unwrap();
        assert_eq!(a.candidates.len(), 1);
    }

    #[test]
    fn msi_toplevel() {
        let d = tree(&["game.msi", "Redist/x.exe"]);
        assert_eq!(top(&analyze(d.path()).unwrap()), "game.msi");
    }

    #[test]
    fn ambiguous_equal_setups() {
        let d = tree(&["setup.exe", "install.exe"]);
        let a = analyze(d.path()).unwrap();
        assert!(a.ambiguous);
        let d = tree(&["setup_a.exe", "setup_b.exe"]);
        assert!(analyze(d.path()).unwrap().ambiguous);
    }

    #[test]
    fn empty_dir_ambiguous() {
        let d = tempfile::tempdir().unwrap();
        let a = analyze(d.path()).unwrap();
        assert!(a.candidates.is_empty() && a.ambiguous);
    }

    #[test]
    fn depth_limited_and_uninstaller_penalized() {
        let d = tree(&["a/b/c/deep.exe", "unins000.exe", "game.exe"]);
        let a = analyze(d.path()).unwrap();
        assert!(a.candidates.iter().all(|c| !c.path.ends_with("deep.exe")));
        assert_eq!(top(&a), "game.exe");
    }

    #[test]
    fn parent_dir_rejected() {
        let d = tree(&["setup.exe"]);
        let p = d.path().join("sub/../setup.exe");
        assert!(matches!(analyze(&p), Err(SourceError::InvalidPath(_))));
    }

    #[test]
    fn symlink_loop_terminates() {
        let d = tree(&["setup.exe", "sub/x.txt"]);
        symlink(d.path(), d.path().join("sub/loop")).unwrap();
        let a = analyze(d.path()).unwrap();
        assert_eq!(a.candidates.len(), 1);
    }

    #[test]
    fn symlink_escape_skipped() {
        let outside = tree(&["evil.exe", "dir/evil2.exe"]);
        let d = tree(&["setup.exe"]);
        symlink(outside.path().join("evil.exe"), d.path().join("link.exe")).unwrap();
        symlink(outside.path().join("dir"), d.path().join("linkdir")).unwrap();
        let a = analyze(d.path()).unwrap();
        assert_eq!(a.candidates.len(), 1);
        assert!(
            a.candidates[0]
                .path
                .starts_with(fs::canonicalize(d.path()).unwrap())
        );
    }

    #[test]
    fn symlink_root_resolved_once() {
        let d = tree(&["real/setup.exe"]);
        symlink(d.path().join("real"), d.path().join("lnk")).unwrap();
        let a = analyze(&d.path().join("lnk")).unwrap();
        assert_eq!(top(&a), "setup.exe");
    }
}
