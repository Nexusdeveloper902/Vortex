//! Per-run log files and secret redaction.
//!
//! Every line from a child process must pass through [`redact`] before it reaches a file or the UI;
//! [`LogWriter`] does this for the file, UI code calls `redact` itself.

use crate::db::RunKind;
use crate::error::{Error, Result};
use crate::paths::{Paths, create_private_dir};
use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const MASK: &str = "<redacted>";
// ponytail: name heuristics; add a user-defined redact list if someone needs it.
// Known false positive by design: `KEYBOARD_LAYOUT=us` is redacted because the name contains "key".
const SECRET_WORDS: [&str; 8] = [
    "token",
    "secret",
    "pass",
    "key",
    "auth",
    "cookie",
    "session",
    "credential",
];

fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SECRET_WORDS.iter().any(|w| n.contains(w))
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '-')
}

/// Replaces the byte ranges (sorted, non-overlapping) with the mask.
fn splice(s: &str, mut ranges: Vec<(usize, usize)>) -> Cow<'_, str> {
    if ranges.is_empty() {
        return Cow::Borrowed(s);
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (a, b) in ranges {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    let ranges = merged;
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for (a, b) in ranges {
        out.push_str(&s[last..a]);
        out.push_str(MASK);
        last = b;
    }
    out.push_str(&s[last..]);
    Cow::Owned(out)
}

/// Value range starting at `start`: a quoted value (content only) or up to whitespace.
fn value_range(s: &str, start: usize) -> (usize, usize) {
    let rest = &s[start..];
    match rest.chars().next() {
        Some(q @ ('\'' | '"')) => {
            let inner = start + 1;
            let mut esc = false;
            let end = s[inner..]
                .char_indices()
                .find(|&(_, c)| {
                    let hit = c == q && !esc;
                    esc = c == '\\' && !esc;
                    hit
                })
                .map_or(s.len(), |(i, _)| inner + i);
            (inner, end)
        }
        _ => (
            start,
            rest.find(char::is_whitespace)
                .map_or(s.len(), |i| start + i),
        ),
    }
}

/// `NAME=value` and `'NAME': 'value'` where NAME looks secret.
fn pairs(s: &str) -> Vec<(usize, usize)> {
    let mut r: Vec<(usize, usize)> = Vec::new();
    for (i, c) in s.char_indices() {
        let (name_end, val_start) = match c {
            '=' => (i, i + 1),
            ':' => {
                let v = s[i + 1..].trim_start_matches(' ');
                let vs = s.len() - v.len();
                match s[..i].chars().next_back() {
                    // `'NAME': 'value'`: closing quote before, same quote opens the value
                    Some(q @ ('\'' | '"')) if v.starts_with(q) => (i - 1, vs),
                    // unquoted `NAME: value`
                    Some(c) if is_name_char(c) => (i, vs),
                    _ => continue,
                }
            }
            _ => continue,
        };
        let name_start = s[..name_end]
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_name_char(*c))
            .last()
            .map_or(name_end, |(j, _)| j);
        if name_start == name_end || !is_secret_name(&s[name_start..name_end]) {
            continue;
        }
        let (a, b) = if c == ':'
            && s[name_start..name_end]
                .to_ascii_lowercase()
                .contains("auth")
        {
            // `Authorization: <scheme> <credentials>`: everything after the colon
            (val_start, s.len())
        } else {
            value_range(s, val_start)
        };
        if a < b && r.last().is_none_or(|&(_, e)| a >= e) {
            r.push((a, b));
        }
    }
    r
}

/// `Bearer <token>` (case-insensitive) and `gh[pousr]_<20+ alnum>`.
fn tokens(s: &str) -> Vec<(usize, usize)> {
    let lower = s.to_ascii_lowercase();
    let mut r = Vec::new();
    for scheme in ["bearer ", "basic ", "token ", "digest "] {
        let mut from = 0;
        while let Some(i) = lower[from..].find(scheme) {
            let start = from + i;
            let a = start + scheme.len();
            from = a;
            if s[..start].chars().next_back().is_some_and(is_name_char) {
                continue;
            }
            // Digest carries several comma-separated params: take the rest of the line.
            let b = if scheme == "digest " {
                s.len()
            } else {
                s[a..].find(char::is_whitespace).map_or(s.len(), |j| a + j)
            };
            if a < b {
                r.push((a, b));
            }
            from = b.max(a);
        }
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 4 < bytes.len() {
        if bytes[i] == b'g'
            && bytes[i + 1] == b'h'
            && b"pousr".contains(&bytes[i + 2])
            && bytes[i + 3] == b'_'
        {
            let n = bytes[i + 4..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric())
                .count();
            if n >= 20 {
                r.push((i, i + 4 + n));
                i += 4 + n;
                continue;
            }
        }
        i += 1;
    }
    r
}

/// `scheme://user:pw@host`: masks the userinfo.
fn userinfo(s: &str) -> Vec<(usize, usize)> {
    let mut r = Vec::new();
    let mut from = 0;
    while let Some(i) = s[from..].find("://") {
        let a = from + i + 3;
        let end = s[a..]
            .find(|c: char| c == '/' || c.is_whitespace() || matches!(c, '\'' | '"'))
            .map_or(s.len(), |j| a + j);
        if let Some(at) = s[a..end].rfind('@').filter(|&at| at > 0) {
            r.push((a, a + at));
        }
        from = end.max(a);
    }
    r
}

/// Masks secrets in one line. Plain string scanning, no regex.
pub fn redact(line: &str) -> Cow<'_, str> {
    let mut cur = Cow::Borrowed(line);
    for pass in [userinfo, tokens, pairs] {
        let next = splice(&cur, pass(&cur)).into_owned();
        if next != cur {
            cur = Cow::Owned(next);
        }
    }
    cur
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One log file per run: `data/logs/<owner>/<run_id>-<kind>.log`, where `owner` is the game id,
/// or the environment id for runs without a game. Lines are `O|` / `E|` prefixed and redacted.
pub struct LogWriter {
    path: PathBuf,
    out: BufWriter<File>,
}

impl LogWriter {
    pub fn log_path(paths: &Paths, owner: i64, run_id: i64, kind: RunKind) -> PathBuf {
        paths
            .logs_dir()
            .join(owner.to_string())
            .join(format!("{run_id}-{}.log", kind.as_str()))
    }

    pub fn create(paths: &Paths, owner: i64, run_id: i64, kind: RunKind) -> Result<Self> {
        let path = Self::log_path(paths, owner, run_id, kind);
        let dir = path.parent().unwrap_or(Path::new("."));
        create_private_dir(dir)?;
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(Error::io(&path))?;
        Ok(Self {
            path,
            out: BufWriter::new(f),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Header block: `key: value` lines (time, kind, argv, cwd, runtime, prefix), redacted.
    pub fn header(&mut self, fields: &[(&str, &str)]) -> Result<()> {
        for (k, v) in fields {
            let line = redact(v);
            self.write(format_args!("{k}: {line}\n"))?;
        }
        self.write(format_args!("--\n"))
    }

    pub fn line(&mut self, stream: Stream, text: &str) -> Result<()> {
        let tag = if stream == Stream::Stdout { 'O' } else { 'E' };
        let line = redact(text);
        self.write(format_args!("{tag}|{line}\n"))
    }

    pub fn flush(&mut self) -> Result<()> {
        self.out.flush().map_err(Error::io(&self.path))
    }

    fn write(&mut self, a: std::fmt::Arguments<'_>) -> Result<()> {
        self.out.write_fmt(a).map_err(Error::io(&self.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secrets() {
        let cases = [
            (
                "{'GITHUB_TOKEN': 'abc123', 'HOME': '/home/u'}",
                "{'GITHUB_TOKEN': '<redacted>', 'HOME': '/home/u'}",
            ),
            ("API_KEY=x PATH=/bin", "API_KEY=<redacted> PATH=/bin"),
            ("db_password=\"a b\" z", "db_password=\"<redacted>\" z"),
            (
                "Authorization: Bearer abc.def-1 rest",
                "Authorization: <redacted>",
            ),
            (
                "tok ghp_abcdefghijklmnopqrstuvwxyz0123 end",
                "tok <redacted> end",
            ),
            (
                "GET https://user:pw@example.com/x",
                "GET https://<redacted>@example.com/x",
            ),
            ("GET https://example.com/a@b", "GET https://example.com/a@b"),
        ];
        for (i, o) in cases {
            assert_eq!(redact(i), o);
        }
    }

    #[test]
    fn leaves_innocent_lines() {
        for s in [
            "err:module:import_dll Library X.dll",
            "WINEDEBUG=-all",
            "'HOME': '/home/u'",
            "ghp_short",
            "a = b",
            "",
        ] {
            assert!(matches!(redact(s), Cow::Borrowed(_)), "{s}");
        }
    }

    #[test]
    fn keyboard_layout_is_a_known_false_positive() {
        // Chosen behaviour: the name heuristic matches "key" inside KEYBOARD.
        assert_eq!(redact("KEYBOARD_LAYOUT=us"), "KEYBOARD_LAYOUT=<redacted>");
    }

    #[test]
    fn non_ascii_is_safe() {
        assert_eq!(redact("é SECRET=ç ü"), "é SECRET=<redacted> ü");
    }

    #[test]
    fn overlapping_and_unordered_ranges_do_not_panic() {
        assert_eq!(
            redact("Bearer ghp_abcdefghijklmnopqrstuvwxyz0123 x"),
            "Bearer <redacted> x"
        );
        assert_eq!(
            redact("a ghp_abcdefghijklmnopqrstuvwxyz0123 then Bearer tok"),
            "a <redacted> then Bearer <redacted>"
        );
    }

    #[test]
    fn unquoted_colon_and_auth_schemes() {
        let cases = [
            ("password: hunter2 ok", "password: <redacted> ok"),
            ("SECRET=a", "SECRET=<redacted>"),
            (
                "Authorization: Basic dXNlcjpwdw== extra",
                "Authorization: <redacted>",
            ),
            (
                "Authorization: Digest user=\"a\", nonce=\"b\"",
                "Authorization: <redacted>",
            ),
            ("sent Token abc123 now", "sent Token <redacted> now"),
            ("csrftoken abc", "csrftoken abc"),
            (
                "'API_KEY': 'a\\'b c', 'X': 'y'",
                "'API_KEY': '<redacted>', 'X': 'y'",
            ),
            ("KEY=\"a\\\"b\" z", "KEY=\"<redacted>\" z"),
        ];
        for (i, o) in cases {
            assert_eq!(redact(i), o, "{i}");
        }
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        let atoms = [
            "é",
            "ü",
            "日",
            "🦀",
            "'",
            "\"",
            "\\",
            "=",
            ":",
            "@",
            "://",
            " ",
            "Bearer ",
            "bearer ",
            "Basic ",
            "Token ",
            "Digest ",
            "ghp_",
            "ghs_abcdefghijklmnopqrstuvwxyz",
            "KEY",
            "pass",
            "Authorization",
            "a",
            "_",
            "-",
            "\n",
        ];
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..20_000 {
            let mut line = String::new();
            for _ in 0..(x % 12) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                line.push_str(atoms[(x % atoms.len() as u64) as usize]);
            }
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let _ = redact(&line);
        }
    }

    #[test]
    fn writer_redacts_and_tags() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());
        let mut w = LogWriter::create(&paths, 3, 9, RunKind::Launch).unwrap();
        w.header(&[("argv", "run API_KEY=x")]).unwrap();
        w.line(Stream::Stdout, "hello").unwrap();
        w.line(Stream::Stderr, "Bearer tok").unwrap();
        w.flush().unwrap();
        let s = std::fs::read_to_string(w.path()).unwrap();
        assert_eq!(
            s,
            "argv: run API_KEY=<redacted>\n--\nO|hello\nE|Bearer <redacted>\n"
        );
        assert!(w.path().ends_with("logs/3/9-launch.log"));
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(w.path()), 0o600);
        assert_eq!(mode(w.path().parent().unwrap()), 0o700);
    }
}
