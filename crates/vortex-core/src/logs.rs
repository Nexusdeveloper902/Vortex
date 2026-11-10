//! Per-run log files and secret redaction.
//!
//! Every line from a child process must pass through [`redact`] before it reaches a file or the UI;
//! [`LogWriter`] does this for the file, UI code calls `redact` itself.

use crate::db::RunKind;
use crate::error::{Error, Result};
use crate::paths::Paths;
use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Write};
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
fn splice(s: &str, ranges: Vec<(usize, usize)>) -> Cow<'_, str> {
    if ranges.is_empty() {
        return Cow::Borrowed(s);
    }
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
            let end = s[inner..].find(q).map_or(s.len(), |i| inner + i);
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
                // closing quote right before, then optional spaces, then a quote
                let Some(q) = s[..i]
                    .chars()
                    .next_back()
                    .filter(|q| matches!(q, '\'' | '"'))
                else {
                    continue;
                };
                let v = s[i + 1..].trim_start_matches(' ');
                let vs = s.len() - v.len();
                if !v.starts_with(q) {
                    continue;
                }
                (i - 1, vs)
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
        let (a, b) = value_range(s, val_start);
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
    let mut from = 0;
    while let Some(i) = lower[from..].find("bearer ") {
        let a = from + i + 7;
        let b = s[a..].find(char::is_whitespace).map_or(s.len(), |j| a + j);
        if a < b {
            r.push((a, b));
        }
        from = b.max(a);
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
        std::fs::create_dir_all(dir).map_err(Error::io(dir))?;
        let f = File::create(&path).map_err(Error::io(&path))?;
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
                "Authorization: Bearer <redacted> rest",
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
    }
}
