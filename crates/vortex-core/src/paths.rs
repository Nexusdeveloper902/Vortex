//! XDG directories, managed roots and the host, prefix and Windows path types.
//!
//! The four path kinds (ARCHITECTURE section 4) never mix: `HostPath` (Linux side),
//! `PrefixDir` (`WINEPREFIX`), and `WinPath` (Windows exe or cwd, display only). Convert only via
//! [`to_win`] and [`to_host`].

use crate::error::{EnvId, Error, Result};
use std::path::{Component, Path, PathBuf};

/// Absolute host path without `..` components (`.` components are dropped).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HostPath(PathBuf);

impl HostPath {
    pub fn new(p: impl AsRef<Path>) -> Result<Self> {
        let p = p.as_ref();
        if !p.is_absolute() {
            return Err(Error::InvalidPath(format!(
                "{} is not absolute",
                p.display()
            )));
        }
        if p.components().any(|c| c == Component::ParentDir) {
            return Err(Error::InvalidPath(format!("{} contains '..'", p.display())));
        }
        Ok(Self(p.components().collect()))
    }
    pub fn as_path(&self) -> &Path {
        &self.0
    }
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
    pub fn parent(&self) -> Option<HostPath> {
        self.0.parent().map(|p| Self(p.to_path_buf()))
    }
    pub fn join(&self, rel: impl AsRef<Path>) -> Result<HostPath> {
        Self::new(self.0.join(rel))
    }
}

impl AsRef<Path> for HostPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// A `WINEPREFIX` directory. Drive C is `<prefix>/drive_c`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrefixDir(HostPath);

impl PrefixDir {
    /// Validates a prefix location: must not be `/`, `home`, or a parent of any `known` prefix.
    pub fn new(p: impl AsRef<Path>, home: &Path, known: &[PrefixDir]) -> Result<Self> {
        let h = HostPath::new(p)?;
        if h.0 == Path::new("/") || h.0 == home {
            return Err(Error::InvalidPath(format!(
                "{} cannot be used as a prefix",
                h.0.display()
            )));
        }
        if let Some(k) = known.iter().find(|k| k.0.0.starts_with(&h.0)) {
            return Err(Error::InvalidPath(format!(
                "{} contains the existing prefix {}",
                h.0.display(),
                k.0.0.display()
            )));
        }
        Ok(Self(h))
    }
    pub fn as_host(&self) -> &HostPath {
        &self.0
    }
    pub fn as_path(&self) -> &Path {
        &self.0.0
    }
    pub fn drive_c(&self) -> PathBuf {
        self.0.0.join("drive_c")
    }
}

/// A Windows-style path (`C:\Games\foo.exe`). Used for exe and cwd display and for arguments
/// interpreted by Windows programs. Never persisted; recompute from host paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WinPath(String);

impl WinPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WinPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Host path under `<prefix>/drive_c` becomes `C:\...`; everything else `Z:\...`.
pub fn to_win(prefix: &PrefixDir, host: &HostPath) -> WinPath {
    let (drive, rel) = match host.0.strip_prefix(prefix.drive_c()) {
        Ok(rel) => ("C:", rel),
        Err(_) => ("Z:", host.0.strip_prefix("/").unwrap_or(&host.0)),
    };
    let mut s = String::from(drive);
    let mut any = false;
    for c in rel.components() {
        s.push('\\');
        s.push_str(&c.as_os_str().to_string_lossy());
        any = true;
    }
    if !any {
        s.push('\\');
    }
    WinPath(s)
}

/// Inverse of [`to_win`] for `C:` and `Z:`. Other drives, relative forms and `..` give `None`.
pub fn to_host(prefix: &PrefixDir, win: &WinPath) -> Option<HostPath> {
    let s = win.0.as_str();
    let root = match s.get(..3)? {
        r if r.eq_ignore_ascii_case("c:\\") => prefix.drive_c(),
        r if r.eq_ignore_ascii_case("z:\\") => PathBuf::from("/"),
        _ => return None,
    };
    let mut p = root;
    for seg in s[3..].split('\\').filter(|x| !x.is_empty()) {
        p.push(seg);
    }
    HostPath::new(p).ok()
}

/// Creates `dir` (and parents) with mode 0700 and makes sure `dir` itself ends up 0700.
pub fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(Error::io(dir))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(Error::io(dir))
}

/// A folder name for a game (`<default folder>/<name>` in the wizard): path separators and
/// control characters dropped, never empty, `.` or `..`.
pub fn folder_name(game: &str) -> String {
    let s: String = game
        .chars()
        .filter(|c| *c != '/' && *c != '\\' && !c.is_control())
        .collect();
    let s = s.trim().trim_start_matches('.').trim();
    if s.is_empty() {
        "game".into()
    } else {
        s.into()
    }
}

/// Checks a location where Vortex will create a folder the user chose: it does not exist yet or
/// is an empty directory (not a symlink), and its parent is an existing, writable directory.
/// Returns the path with its parent canonicalized (symlinks in the parent resolved).
pub fn new_dir_location(p: &HostPath) -> Result<HostPath> {
    let bad = |why: &str| Error::InvalidPath(format!("{}: {why}", p.0.display()));
    let (Some(parent), Some(name)) = (p.0.parent(), p.0.file_name()) else {
        return Err(bad("choose a folder below /"));
    };
    let parent = std::fs::canonicalize(parent)
        .ok()
        .filter(|d| d.is_dir())
        .ok_or_else(|| bad("its parent folder does not exist"))?;
    if rustix::fs::access(&parent, rustix::fs::Access::WRITE_OK).is_err() {
        return Err(bad("its parent folder is not writable"));
    }
    let full = parent.join(name);
    match std::fs::symlink_metadata(&full) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::io(&full)(e)),
        Ok(m) if m.is_symlink() => return Err(bad("it is a symlink")),
        Ok(m) if !m.is_dir() => return Err(bad("it is not a folder")),
        Ok(_) => {
            if std::fs::read_dir(&full)
                .map_err(Error::io(&full))?
                .next()
                .is_some()
            {
                return Err(bad("the folder is not empty"));
            }
        }
    }
    HostPath::new(full)
}

/// A user-chosen location for a new prefix (spec 4): a [`new_dir_location`] that is not `/` or
/// `$HOME`, not inside Vortex's data folder (that is the managed default), and neither inside nor
/// around any `known` prefix. Vortex creates it but records it `prefix_managed = 0`
/// (ARCHITECTURE section 11, rule 5): it is never deleted by Vortex.
pub fn check_prefix_location(
    p: &HostPath,
    paths: &Paths,
    known: &[PrefixDir],
) -> Result<PrefixDir> {
    let canon = |x: &Path| std::fs::canonicalize(x).unwrap_or_else(|_| x.to_path_buf());
    let h = new_dir_location(p)?;
    let bad = |why: String| Error::InvalidPath(format!("{}: {why}", h.0.display()));
    if h.0.starts_with(canon(paths.data_dir())) {
        return Err(bad(
            "it is inside Vortex's own folder; leave the prefix location empty instead".into(),
        ));
    }
    for k in known {
        let k = canon(k.as_path());
        if h.0.starts_with(&k) || k.starts_with(&h.0) {
            return Err(bad(format!("it overlaps the prefix {}", k.display())));
        }
    }
    PrefixDir::new(&h, &canon(paths.home()), &[])
}

/// Managed roots. `from_xdg` for the app, `under(root)` for tests (never touches real XDG dirs).
#[derive(Debug, Clone)]
pub struct Paths {
    data: PathBuf,
    home: PathBuf,
    applications: PathBuf,
}

impl Paths {
    pub fn from_xdg() -> Result<Self> {
        let d = directories::BaseDirs::new()
            .ok_or_else(|| Error::Validation("cannot determine the home directory".into()))?;
        Ok(Self {
            data: d.data_dir().join("vortex"),
            home: d.home_dir().to_path_buf(),
            applications: d.data_dir().join("applications"),
        })
    }
    pub fn under(root: impl AsRef<Path>) -> Self {
        let r = root.as_ref();
        Self {
            data: r.join("data"),
            home: r.join("home"),
            applications: r.join("applications"),
        }
    }
    pub fn home(&self) -> &Path {
        &self.home
    }
    pub fn data_dir(&self) -> &Path {
        &self.data
    }
    pub fn db_file(&self) -> PathBuf {
        self.data.join("vortex.db")
    }
    pub fn prefixes_dir(&self) -> PathBuf {
        self.data.join("prefixes")
    }
    pub fn runtimes_dir(&self) -> PathBuf {
        self.data.join("runtimes")
    }
    pub fn backups_dir(&self) -> PathBuf {
        self.data.join("backups")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.data.join("logs")
    }
    /// Cached cover images, one file per game (`artwork.rs`).
    pub fn artwork_dir(&self) -> PathBuf {
        self.data.join("artwork")
    }
    pub fn artwork_file(&self, game_id: i64) -> PathBuf {
        self.artwork_dir().join(format!("{game_id}.img"))
    }
    pub fn applications_dir(&self) -> &Path {
        &self.applications
    }
    /// Root of Vortex-owned game install dirs; only dirs strictly under it are ever deletable.
    pub fn games_dir(&self) -> PathBuf {
        self.data.join("games")
    }
    /// Persistent scratch prefix for read-only umu calls (e.g. `winetricks list-all`); never a game
    /// prefix and never deleted. Created on first use.
    pub fn scratch_prefix(&self) -> Result<PrefixDir> {
        let dir = self.data.join("scratch/list-verbs");
        create_private_dir(&dir)?;
        PrefixDir::new(dir, &self.home, &[])
    }
    /// The Vortex-managed prefix for an environment: `data/prefixes/<env_id>`.
    pub fn managed_prefix(&self, env: EnvId) -> Result<PrefixDir> {
        PrefixDir::new(self.prefixes_dir().join(env.to_string()), &self.home, &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pfx() -> PrefixDir {
        PrefixDir::new("/data/prefixes/1", Path::new("/home/u"), &[]).unwrap()
    }
    fn h(s: &str) -> HostPath {
        HostPath::new(s).unwrap()
    }

    #[test]
    fn conversion_table() {
        let p = pfx();
        let cases = [
            ("/data/prefixes/1/drive_c", "C:\\"),
            (
                "/data/prefixes/1/drive_c/Games/Foo/foo.exe",
                "C:\\Games\\Foo\\foo.exe",
            ),
            (
                "/data/prefixes/1/drive_c/Program Files/A B/x.exe",
                "C:\\Program Files\\A B\\x.exe",
            ),
            (
                "/data/prefixes/1/other/x.exe",
                "Z:\\data\\prefixes\\1\\other\\x.exe",
            ),
            (
                "/mnt/games/Installer/setup.exe",
                "Z:\\mnt\\games\\Installer\\setup.exe",
            ),
            (
                "/mnt/jogos/Instalação/ação.exe",
                "Z:\\mnt\\jogos\\Instalação\\ação.exe",
            ),
            ("/", "Z:\\"),
        ];
        for (host, win) in cases {
            let w = to_win(&p, &h(host));
            assert_eq!(w.as_str(), win);
            assert_eq!(to_host(&p, &w).unwrap(), h(host), "round trip {host}");
        }
    }

    #[test]
    fn to_host_rejects_other_drives_and_dotdot() {
        let p = pfx();
        for bad in ["D:\\x", "C:", "relative\\x", "C:\\a\\..\\b", "Z:\\..\\etc"] {
            assert!(to_host(&p, &WinPath(bad.into())).is_none(), "{bad}");
        }
    }

    #[test]
    fn host_path_validation() {
        assert!(HostPath::new("rel/x").is_err());
        assert!(HostPath::new("/a/../b").is_err());
        assert_eq!(
            HostPath::new("/a/./b//c").unwrap().as_path(),
            Path::new("/a/b/c")
        );
    }

    #[test]
    fn prefix_rejects_root_home_and_parents() {
        let home = Path::new("/home/u");
        assert!(PrefixDir::new("/", home, &[]).is_err());
        assert!(PrefixDir::new("/home/u", home, &[]).is_err());
        assert!(PrefixDir::new("/home/u/../u", home, &[]).is_err());
        let known = [pfx()];
        assert!(PrefixDir::new("/data/prefixes", home, &known).is_err());
        assert!(PrefixDir::new("/data/prefixes/1", home, &known).is_err());
        assert!(PrefixDir::new("/data/prefixes/2", home, &known).is_ok());
    }

    #[test]
    fn under_is_self_contained() {
        let p = Paths::under("/tmp/x");
        assert_eq!(p.db_file(), Path::new("/tmp/x/data/vortex.db"));
        assert_eq!(
            p.managed_prefix(7).unwrap().as_path(),
            Path::new("/tmp/x/data/prefixes/7")
        );
    }

    #[test]
    fn folder_names() {
        assert_eq!(folder_name("Foo: Bar"), "Foo: Bar");
        assert_eq!(folder_name(" a/b\\c "), "abc");
        for bad in ["", "  ", ".", "..", "/", "\n"] {
            assert_eq!(folder_name(bad), "game", "{bad:?}");
        }
    }

    #[test]
    fn new_dir_location_rules() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        let at = |rel: &str| new_dir_location(&h(r.join(rel).to_str().unwrap()));
        std::fs::create_dir_all(r.join("empty")).unwrap();
        std::fs::create_dir_all(r.join("full/x")).unwrap();
        std::fs::write(r.join("file"), "x").unwrap();
        std::os::unix::fs::symlink(r.join("empty"), r.join("link")).unwrap();
        assert!(at("new").is_ok());
        assert!(at("empty").is_ok());
        assert!(at("link/new").is_ok(), "symlink in the parent is resolved");
        assert_eq!(
            at("link/new").unwrap().as_path(),
            std::fs::canonicalize(r.join("empty")).unwrap().join("new")
        );
        for bad in ["full", "file", "link", "missing/new", "file/new"] {
            assert!(at(bad).is_err(), "{bad}");
        }
        assert!(new_dir_location(&h("/")).is_err());
    }

    #[test]
    fn prefix_location_rules() {
        let t = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(t.path()).unwrap();
        let paths = Paths::under(&root);
        std::fs::create_dir_all(paths.prefixes_dir()).unwrap();
        std::fs::create_dir_all(paths.home()).unwrap();
        let other = root.join("wine/other");
        std::fs::create_dir_all(&other).unwrap();
        let known = [PrefixDir::new(&other, paths.home(), &[]).unwrap()];
        let at = |p: &Path| check_prefix_location(&h(p.to_str().unwrap()), &paths, &known);
        assert!(at(&root.join("wine/mine")).is_ok());
        assert!(at(&paths.home().join("Games")).is_ok());
        for bad in [
            paths.home().to_path_buf(),
            paths.prefixes_dir().join("9"),
            paths.data_dir().join("x"),
            other.join("inner"),
            other.clone(),
        ] {
            assert!(at(&bad).is_err(), "{bad:?}");
        }
        // around a known prefix: an empty parent of it cannot exist, so check with a fresh dir
        let known2 = [PrefixDir::new(root.join("g/p"), paths.home(), &[]).unwrap()];
        std::fs::create_dir_all(root.join("g")).unwrap();
        assert!(
            check_prefix_location(&h(root.join("g").to_str().unwrap()), &paths, &known2).is_err()
        );
    }
}
