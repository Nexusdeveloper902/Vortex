//! Per-game XDG desktop entries.
//!
//! Entries are `<applications dir>/io.github.nexusdeveloper902.Vortex.game-<id>.desktop` and launch
//! `<vortex> --launch <id>`. The applications dir is a parameter, so tests write to a temp dir and
//! never touch the real `~/.local/share/applications`.

use std::fmt;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const APP_ID: &str = "io.github.nexusdeveloper902.Vortex";

// TODO(M2): replace with vortex_core::Error (Validation / Io) and vortex_core::Result once error.rs lands.
#[derive(Debug)]
pub enum Error {
    Invalid {
        field: &'static str,
        reason: &'static str,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid { field, reason } => write!(f, "invalid {field}: {reason}"),
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Invalid { .. } => None,
        }
    }
}

/// Writes (or replaces) the entry for a game and returns its path. Written to a temp file in the
/// same directory, mode 0644, fsynced, then renamed over the target.
pub fn write_entry(
    applications_dir: &Path,
    vortex: &Path,
    game_id: &str,
    name: &str,
) -> Result<PathBuf> {
    check_game_id(game_id)?;
    let program = vortex.to_str().ok_or(Error::Invalid {
        field: "vortex path",
        reason: "not UTF-8",
    })?;
    let exec = exec_value(program, &["--launch", game_id])?;
    let name = name_value(name)?;
    let contents = format!(
        "[Desktop Entry]\nType=Application\nVersion=1.5\nName={name}\nExec={exec}\nIcon={APP_ID}\n\
         Categories=Game;\nTerminal=false\nX-Vortex-Game-Id={game_id}\n"
    );
    let path = entry_path(applications_dir, game_id);
    fs::create_dir_all(applications_dir).map_err(|e| io_err(applications_dir, e))?;
    write_atomic(&path, contents.as_bytes())?;
    Ok(path)
}

/// Removes the entry for a game. Removing a missing entry is not an error.
pub fn remove_entry(applications_dir: &Path, game_id: &str) -> Result<()> {
    check_game_id(game_id)?;
    let path = entry_path(applications_dir, game_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(&path, e)),
    }
}

fn entry_path(dir: &Path, game_id: &str) -> PathBuf {
    dir.join(format!("{APP_ID}.game-{game_id}.desktop"))
}

// Ids are integers in practice; restricting the charset keeps the file name inside `dir`.
fn check_game_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid {
            field: "game id",
            reason: "must be non-empty ASCII letters, digits, '-' or '_'",
        })
    }
}

fn name_value(name: &str) -> Result<String> {
    if name.trim().is_empty() {
        return Err(Error::Invalid {
            field: "game name",
            reason: "empty",
        });
    }
    if name
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
    {
        return Err(Error::Invalid {
            field: "game name",
            reason: "contains a control character",
        });
    }
    Ok(escape_string(name))
}

/// Exec value per the Desktop Entry spec: field-code `%` doubled, each argument quoted when it
/// has characters outside a safe set (with `"` `` ` `` `$` `\` backslash-escaped inside quotes),
/// then the whole value escaped as a string (backslashes doubled).
fn exec_value(program: &str, args: &[&str]) -> Result<String> {
    let mut words = Vec::with_capacity(args.len() + 1);
    for arg in std::iter::once(program).chain(args.iter().copied()) {
        if arg.is_empty() || arg.chars().any(char::is_control) {
            return Err(Error::Invalid {
                field: "exec argument",
                reason: "empty or contains a control character",
            });
        }
        words.push(quote_arg(&arg.replace('%', "%%")));
    }
    Ok(escape_string(&words.join(" ")))
}

fn quote_arg(arg: &str) -> String {
    let safe = arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '=' | ':' | ','));
    if safe {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Desktop Entry string escaping: backslash and the control chars with escape codes.
fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let (Some(dir), Some(file_name)) = (path.parent(), path.file_name()) else {
        return Err(Error::Invalid {
            field: "entry path",
            reason: "has no parent directory",
        });
    };
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.set_permissions(Permissions::from_mode(0o644))?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    })();
    result
        .inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
        .map_err(|e| io_err(path, e))
}

fn io_err(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Unique temp dir under the system temp dir, removed on drop. Tests never use XDG dirs.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("vortex-desktop-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const TRICKY_PROGRAM: &str = "/opt/My Apps/vortex \"beta\" $HOME `x` \\ 100%";
    const TRICKY_NAME: &str = "Tom \"Dick\" 100% \\ café\tline\nnext";

    #[test]
    fn exact_contents_escape_exec_and_name() {
        let tmp = TempDir::new("contents");
        let dir = tmp.0.join("applications");
        let path = write_entry(&dir, Path::new(TRICKY_PROGRAM), "7", TRICKY_NAME).unwrap();
        assert_eq!(
            path,
            dir.join("io.github.nexusdeveloper902.Vortex.game-7.desktop")
        );
        let expected = concat!(
            "[Desktop Entry]\n",
            "Type=Application\n",
            "Version=1.5\n",
            "Name=Tom \"Dick\" 100% \\\\ café\\tline\\nnext\n",
            r#"Exec="/opt/My Apps/vortex \\"beta\\" \\$HOME \\`x\\` \\\\ 100%%" --launch 7"#,
            "\n",
            "Icon=io.github.nexusdeveloper902.Vortex\n",
            "Categories=Game;\n",
            "Terminal=false\n",
            "X-Vortex-Game-Id=7\n",
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn written_entries_pass_desktop_file_validate() {
        let tmp = TempDir::new("validate");
        let plain = write_entry(&tmp.0, Path::new("/usr/bin/vortex"), "1", "Plain Game").unwrap();
        let tricky = write_entry(&tmp.0, Path::new(TRICKY_PROGRAM), "2", TRICKY_NAME).unwrap();
        for path in [plain, tricky] {
            match Command::new("desktop-file-validate").arg(&path).output() {
                Ok(out) => assert!(
                    out.status.success(),
                    "desktop-file-validate failed for {}: {}{}",
                    path.display(),
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    eprintln!("NOT RUN: desktop-file-validate is not installed");
                    return;
                }
                Err(e) => panic!("cannot run desktop-file-validate: {e}"),
            }
        }
    }

    #[test]
    fn overwrite_is_atomic_mode_0644_and_leaves_no_temp_files() {
        let tmp = TempDir::new("atomic");
        let dir = tmp.0.join("applications");
        write_entry(&dir, Path::new("/usr/bin/vortex"), "3", "Old").unwrap();
        let path = write_entry(&dir, Path::new("/usr/bin/vortex"), "3", "New").unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("Name=New\n"));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "temp file left behind: {names:?}");
    }

    #[test]
    fn remove_is_idempotent() {
        let tmp = TempDir::new("remove");
        let path = write_entry(&tmp.0, Path::new("/usr/bin/vortex"), "4", "Gone").unwrap();
        remove_entry(&tmp.0, "4").unwrap();
        assert!(!path.exists());
        remove_entry(&tmp.0, "4").unwrap();
        remove_entry(&tmp.0.join("never-created"), "4").unwrap();
    }

    #[test]
    fn rejects_bad_ids_names_and_programs_without_writing() {
        let tmp = TempDir::new("reject");
        for id in ["", "../evil", "a/b", "1 2", "x\n"] {
            assert!(
                write_entry(&tmp.0, Path::new("/usr/bin/vortex"), id, "N").is_err(),
                "{id:?}"
            );
            assert!(remove_entry(&tmp.0, id).is_err(), "{id:?}");
        }
        assert!(write_entry(&tmp.0, Path::new("/usr/bin/vortex"), "5", "  ").is_err());
        assert!(write_entry(&tmp.0, Path::new("/usr/bin/vo\nrtex"), "5", "N").is_err());
        assert!(write_entry(&tmp.0, Path::new("/usr/bin/vortex"), "5", "bad\u{1}name").is_err());
        assert_eq!(fs::read_dir(&tmp.0).unwrap().count(), 0);
    }
}
