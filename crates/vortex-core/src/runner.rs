//! Translates an Invocation into the umu-run Command (pure, no spawning).
//!
//! Contract: ARCHITECTURE section 5. Every Windows program runs as
//! `umu-run <program> <args...>` with absolute `WINEPREFIX`/`PROTONPATH`, the run marker, an
//! explicit `STEAM_COMPAT_INSTALL_PATH`, and the Windows cwd set only via `current_dir`.

use crate::error::{Error, Result};
use crate::paths::{HostPath, PrefixDir, WinPath, to_win};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// External tools. Tests point `umu_run` at `tests/fake-umu-run`.
#[derive(Debug, Clone)]
pub struct Tools {
    /// `umu-run` binary; a bare name is resolved on `PATH`.
    pub umu_run: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            umu_run: PathBuf::from("umu-run"),
        }
    }
}

/// Result of [`Tools::check`]: what is installed. Missing pieces are `None` / empty, not errors.
#[derive(Debug, Clone)]
pub struct ToolCheck {
    /// Resolved absolute `umu-run`; `None` = not found. Required for anything that runs Windows code.
    pub umu_run: Option<PathBuf>,
    pub runtimes: Vec<crate::runtime::Runtime>,
}

/// Resolve `name` without a shell: a name with a `/` is used as is, a bare name is searched in the
/// `path_var` directories. Only regular files with an execute bit count.
pub fn which(name: &Path, path_var: &std::ffi::OsStr) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let ok = |p: &Path| {
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if name.components().count() > 1 || name.is_absolute() {
        return ok(name).then(|| name.to_path_buf());
    }
    std::env::split_paths(path_var)
        .map(|d| d.join(name))
        .find(|p| ok(p))
}

impl Tools {
    /// Probe umu-run (the configured path) and Proton runtimes.
    pub fn check(&self, path_var: &std::ffi::OsStr, home: &Path, runtimes_dir: &Path) -> ToolCheck {
        ToolCheck {
            umu_run: which(&self.umu_run, path_var),
            runtimes: crate::runtime::discover(home, runtimes_dir),
        }
    }
}

/// Windows built-ins. Always spawned by full `C:\windows\system32\<name>.exe` path: with a bare
/// name umu-run reports "Executable not found" and always exits 0, losing the real code (M1 F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemExe {
    Cmd,
    Msiexec,
    Wineboot,
    /// Wine's `start.exe` (not a shell): `start /wait <exe>` runs a program and returns its exit code.
    Start,
}

impl SystemExe {
    pub fn win_path(self) -> &'static str {
        match self {
            Self::Cmd => r"C:\windows\system32\cmd.exe",
            Self::Msiexec => r"C:\windows\system32\msiexec.exe",
            Self::Wineboot => r"C:\windows\system32\wineboot.exe",
            Self::Start => r"C:\windows\system32\start.exe",
        }
    }
}

/// What umu-run runs (its first argument).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Program {
    /// A Windows exe (or .bat) on the host. Exit codes propagate (low 8 bits only).
    Exe(HostPath),
    /// A Windows built-in by full `C:\` path.
    System(SystemExe),
    /// umu's special `winetricks` verb runner (needs a runtime with `protonfixes/winetricks`).
    Winetricks,
}

/// One umu-run invocation. Build it, call [`Invocation::preflight`], then [`Invocation::command`].
#[derive(Debug, Clone)]
pub struct Invocation {
    pub program: Program,
    /// argv after the program, passed as-is (never joined into a string).
    pub args: Vec<String>,
    /// Host dir; Wine shows it as the Windows cwd (`C:\..` under drive_c, else `Z:\..`).
    pub cwd: HostPath,
    pub prefix: PrefixDir,
    /// Proton dir (`PROTONPATH`).
    pub runtime: HostPath,
    /// Per-game variables. Reserved names are rejected (see [`validate_env`]).
    pub env: Vec<(String, String)>,
    /// Verbose mode: `UMU_LOG=debug`, `PROTON_LOG=1`, `PROTON_LOG_DIR=<dir>`.
    pub verbose_log_dir: Option<HostPath>,
    /// Disables Wine's audio drivers for this process tree only ([`MUTE_AUDIO_OVERRIDES`]
    /// appended to `WINEDLLOVERRIDES`). Set for installer and uninstaller runs, never for
    /// launches; nothing is written to the prefix, so games keep their sound.
    pub mute_audio: bool,
}

/// Wine DLL overrides that disable every Wine audio driver: Windows programs then see no audio
/// device (ARCHITECTURE section 5).
pub const MUTE_AUDIO_OVERRIDES: &str = "winepulse.drv,winealsa.drv,wineoss.drv=d";

/// Names the user may not set: Vortex owns them.
fn is_reserved(name: &str) -> bool {
    matches!(name, "WINEPREFIX" | "PROTONPATH" | "VORTEX_RUN_ID")
        || name.starts_with("STEAM_COMPAT_")
        || name.starts_with("PRESSURE_VESSEL_")
}

/// Validates user env: name `^[A-Za-z_][A-Za-z0-9_]*$`, not reserved, no NUL in the value.
pub fn validate_env(env: &[(String, String)]) -> Result<()> {
    for (k, v) in env {
        let mut chars = k.chars();
        let ok_name = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok_name {
            return Err(Error::Validation(format!(
                "invalid environment variable name {k:?}"
            )));
        }
        if is_reserved(k) {
            return Err(Error::Validation(format!(
                "{k} is set by Vortex and cannot be overridden"
            )));
        }
        if v.contains('\0') {
            return Err(Error::Validation(format!("value of {k} contains NUL")));
        }
    }
    Ok(())
}

/// Refuses to spawn anything as root (defence in depth; the app also refuses to start).
pub fn refuse_root() -> Result<()> {
    if rustix::process::geteuid().is_root() {
        return Err(Error::Validation(
            "Vortex must not run Windows programs as root".into(),
        ));
    }
    Ok(())
}

/// Locations pressure-vessel already shares; anything else goes to
/// `PRESSURE_VESSEL_FILESYSTEMS_RW`.
fn shared_by_default(p: &Path, home: Option<&Path>) -> bool {
    home.is_some_and(|h| p.starts_with(h))
        || ["/mnt", "/media", "/run/media", "/srv", "/opt", "/tmp"]
            .iter()
            .any(|r| p.starts_with(r))
}

impl Invocation {
    /// Exported as `STEAM_COMPAT_INSTALL_PATH`: the exe's dir for host exes, else the cwd.
    /// Always set explicitly; umu leaves it empty for anything that is not a host file.
    pub fn install_path(&self) -> &Path {
        match &self.program {
            Program::Exe(p) => p.as_path().parent().unwrap_or(self.cwd.as_path()),
            _ => self.cwd.as_path(),
        }
    }

    /// Windows view of the exe (display/diagnostics).
    pub fn exe_win(&self) -> Option<WinPath> {
        match &self.program {
            Program::Exe(p) => Some(to_win(&self.prefix, p)),
            _ => None,
        }
    }

    /// Windows view of the cwd (display/diagnostics).
    pub fn cwd_win(&self) -> WinPath {
        to_win(&self.prefix, &self.cwd)
    }

    /// umu-run's argv after the binary: program, then args.
    pub fn argv(&self) -> Vec<String> {
        let first = match &self.program {
            Program::Exe(p) => p.as_path().to_string_lossy().into_owned(),
            Program::System(s) => s.win_path().to_string(),
            Program::Winetricks => "winetricks".to_string(),
        };
        std::iter::once(first)
            .chain(self.args.iter().cloned())
            .collect()
    }

    /// Filesystem checks before spawning. The caller turns errors into diagnose findings:
    /// `NotFound` (exe or cwd) and `RuntimeInvalid`.
    pub fn preflight(&self) -> Result<()> {
        refuse_root()?;
        validate_env(&self.env)?;
        if let Program::Exe(p) = &self.program
            && !p.as_path().is_file()
        {
            return Err(Error::NotFound(format!(
                "executable {}",
                p.as_path().display()
            )));
        }
        if !self.cwd.as_path().is_dir() {
            return Err(Error::NotFound(format!(
                "working directory {}",
                self.cwd.as_path().display()
            )));
        }
        self.wineserver().map(drop)
    }

    /// Validates the runtime and returns its `wineserver` (used for the barrier and stop).
    pub fn wineserver(&self) -> Result<HostPath> {
        crate::runtime::validate(self.runtime.as_path())?
            .wineserver()
            .ok_or_else(|| Error::RuntimeInvalid {
                path: self.runtime.as_path().to_path_buf(),
                reason: "no files/bin/wineserver or dist/bin/wineserver".into(),
            })
    }

    /// Builds the Command. Pure except for reading `HOME` (and an inherited `WINEDLLOVERRIDES`
    /// when `mute_audio` is set); the root refusal is in
    /// [`Invocation::preflight`], which `process::Runs::start` always runs before spawning.
    pub fn command(&self, tools: &Tools, marker: &str) -> Result<Command> {
        validate_env(&self.env)?;
        let mut c = Command::new(&tools.umu_run);
        c.args(self.argv())
            .current_dir(self.cwd.as_path())
            .env("GAMEID", "0") // no protonfixes unless the user overrides GAMEID below
            // No Proton "game drive" (S:). With it, a cwd under a mount point (e.g. /tmp, /mnt/x)
            // shows as S:\.. instead of Z:\.., and S: is remapped per run, so paths an installer
            // records break later. Proton still logs "unable to use parent for game drive"
            // (benign). The user may override it.
            .env("PROTON_SET_GAME_DRIVE", "0")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .env("WINEPREFIX", self.prefix.as_path())
            .env("PROTONPATH", self.runtime.as_path())
            .env("VORTEX_RUN_ID", marker)
            .env("STEAM_COMPAT_INSTALL_PATH", self.install_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if self.mute_audio {
            // Merge: the run's own value, else the inherited one, then ours last so it wins.
            // Proton appends its own overrides after this value (`append_to_env_str`).
            let mut v: std::ffi::OsString = self
                .env
                .iter()
                .rev()
                .find(|(k, _)| k == "WINEDLLOVERRIDES")
                .map(|(_, v)| v.into())
                .or_else(|| std::env::var_os("WINEDLLOVERRIDES"))
                .unwrap_or_default();
            if !v.is_empty() {
                v.push(";");
            }
            v.push(MUTE_AUDIO_OVERRIDES);
            c.env("WINEDLLOVERRIDES", v);
        }
        // Keeps terminal Ctrl-C away from umu-run; not used for stop (ARCHITECTURE 6.1).
        std::os::unix::process::CommandExt::process_group(&mut c, 0);

        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut rw: Vec<&Path> = Vec::new();
        for p in [self.cwd.as_path(), self.install_path()] {
            // ponytail: a ':' in a path would split the list; such paths are not added.
            if !shared_by_default(p, home.as_deref())
                && !p.to_string_lossy().contains(':')
                && !rw.contains(&p)
            {
                rw.push(p);
            }
        }
        if !rw.is_empty() {
            let joined: Vec<_> = rw.iter().map(|p| p.to_string_lossy()).collect();
            c.env("PRESSURE_VESSEL_FILESYSTEMS_RW", joined.join(":"));
        }
        if let Some(dir) = &self.verbose_log_dir {
            c.env("UMU_LOG", "debug")
                .env("PROTON_LOG", "1")
                .env("PROTON_LOG_DIR", dir.as_path());
        }
        Ok(c)
    }
}

/// 32 lowercase hex chars from `/dev/urandom`, unique per run (`VORTEX_RUN_ID`).
pub fn new_marker() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(Error::io("/dev/urandom"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn inv(program: Program, cwd: &str) -> Invocation {
        Invocation {
            program,
            args: vec!["/q".into(), "a b".into()],
            cwd: HostPath::new(cwd).unwrap(),
            prefix: PrefixDir::new("/data/pfx/1", Path::new("/home/u"), &[]).unwrap(),
            runtime: HostPath::new("/opt/Proton X").unwrap(),
            env: vec![
                ("DXVK_HUD".into(), "1".into()),
                ("GAMEID".into(), "umu-1".into()),
            ],
            verbose_log_dir: None,
            mute_audio: false,
        }
    }

    fn env_of<'a>(c: &'a Command, k: &str) -> Option<&'a OsStr> {
        c.get_envs().find(|(n, _)| *n == k).and_then(|(_, v)| v)
    }

    #[test]
    fn exe_command_contract() {
        let i = inv(
            Program::Exe(HostPath::new("/mnt/g/Inst dir/setup.exe").unwrap()),
            "/mnt/g/Inst dir",
        );
        let c = i.command(&Tools::default(), "ab12").unwrap();
        assert_eq!(c.get_program(), "umu-run");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, ["/mnt/g/Inst dir/setup.exe", "/q", "a b"]);
        assert_eq!(c.get_current_dir().unwrap(), Path::new("/mnt/g/Inst dir"));
        assert_eq!(env_of(&c, "WINEPREFIX").unwrap(), "/data/pfx/1");
        assert_eq!(env_of(&c, "PROTONPATH").unwrap(), "/opt/Proton X");
        assert_eq!(env_of(&c, "VORTEX_RUN_ID").unwrap(), "ab12");
        assert_eq!(env_of(&c, "GAMEID").unwrap(), "umu-1", "user may override");
        assert_eq!(env_of(&c, "DXVK_HUD").unwrap(), "1");
        assert_eq!(
            env_of(&c, "STEAM_COMPAT_INSTALL_PATH").unwrap(),
            "/mnt/g/Inst dir"
        );
        assert!(env_of(&c, "PRESSURE_VESSEL_FILESYSTEMS_RW").is_none());
        assert!(env_of(&c, "UMU_LOG").is_none());
        assert_eq!(env_of(&c, "PROTON_SET_GAME_DRIVE").unwrap(), "0");
        assert_eq!(
            i.exe_win().unwrap().as_str(),
            r"Z:\mnt\g\Inst dir\setup.exe"
        );
    }

    #[test]
    fn builtins_use_full_c_path_and_install_path_is_cwd() {
        let mut i = inv(Program::System(SystemExe::Msiexec), "/data/src/msi");
        i.args = vec!["/i".into(), r"Z:\data\src\msi\a.msi".into()];
        i.verbose_log_dir = Some(HostPath::new("/data/logs/1").unwrap());
        let c = i.command(&Tools::default(), "m").unwrap();
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args[0], r"C:\windows\system32\msiexec.exe");
        assert_eq!(
            env_of(&c, "STEAM_COMPAT_INSTALL_PATH").unwrap(),
            "/data/src/msi"
        );
        assert_eq!(
            env_of(&c, "PRESSURE_VESSEL_FILESYSTEMS_RW").unwrap(),
            "/data/src/msi"
        );
        assert_eq!(env_of(&c, "PROTON_LOG_DIR").unwrap(), "/data/logs/1");
        assert_eq!(env_of(&c, "UMU_LOG").unwrap(), "debug");
    }

    #[test]
    fn env_validation() {
        for bad in [
            "WINEPREFIX",
            "PROTONPATH",
            "VORTEX_RUN_ID",
            "STEAM_COMPAT_INSTALL_PATH",
            "PRESSURE_VESSEL_FILESYSTEMS_RW",
            "1ABC",
            "A-B",
            "",
        ] {
            assert!(validate_env(&[(bad.into(), "x".into())]).is_err(), "{bad}");
        }
        assert!(validate_env(&[("A".into(), "x\0y".into())]).is_err());
        for ok in [
            "GAMEID",
            "PROTON_USE_WINED3D",
            "DXVK_HUD",
            "WINEDLLOVERRIDES",
            "_x1",
        ] {
            assert!(validate_env(&[(ok.into(), "v".into())]).is_ok(), "{ok}");
        }
        let mut i = inv(Program::Winetricks, "/tmp");
        i.env.push(("STEAM_COMPAT_DATA_PATH".into(), "/x".into()));
        assert!(i.command(&Tools::default(), "m").is_err());
    }

    #[test]
    fn mute_audio_merges_overrides_and_is_off_by_default() {
        let exe = || Program::Exe(HostPath::new("/mnt/g/setup.exe").unwrap());
        let c = inv(exe(), "/mnt/g")
            .command(&Tools::default(), "m")
            .unwrap();
        assert!(env_of(&c, "WINEDLLOVERRIDES").is_none());
        let mut i = inv(exe(), "/mnt/g");
        i.mute_audio = true;
        let c = i.command(&Tools::default(), "m").unwrap();
        // The test process may inherit a value; ours always comes last.
        let v = env_of(&c, "WINEDLLOVERRIDES").unwrap().to_string_lossy();
        assert!(v.ends_with(MUTE_AUDIO_OVERRIDES), "{v}");
        i.env.push(("WINEDLLOVERRIDES".into(), "d3d9=n,b".into()));
        let c = i.command(&Tools::default(), "m").unwrap();
        assert_eq!(
            env_of(&c, "WINEDLLOVERRIDES").unwrap(),
            "d3d9=n,b;winepulse.drv,winealsa.drv,wineoss.drv=d"
        );
    }

    #[test]
    fn marker_is_32_hex() {
        let m = new_marker().unwrap();
        assert_eq!(m.len(), 32);
        assert!(m.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(m, new_marker().unwrap());
    }
}
