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
    pub fn applications_dir(&self) -> &Path {
        &self.applications
    }
    /// Root of Vortex-owned game install dirs; only dirs strictly under it are ever deletable.
    pub fn games_dir(&self) -> PathBuf {
        self.data.join("games")
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
}
