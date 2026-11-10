//! Failure classification from exit status and log lines.
//!
//! Findings are plain data, not errors. `Confirmed` is used only when the evidence is
//! unambiguous (spec 10); heuristics are `Likely`. Patterns live in the tables below.

/// Coarser than `db::RunKind` on purpose: classification only distinguishes these three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Install,
    Launch,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    Unknown,
}

/// Facts the caller checked on disk; `None` = not checked.
#[derive(Debug, Clone, Copy, Default)]
pub struct Preflight {
    pub exe_exists: Option<bool>,
    pub source_exists: Option<bool>,
    pub runtime_valid: Option<bool>,
    /// Windows working directory equals the exe's directory.
    pub cwd_is_exe_dir: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    InstallerFailure,
    RuntimeInitFailure,
    MissingDependency,
    MissingExecutable,
    AppCrash,
    MissingInstallerData,
    WrongWorkingDirectory,
    /// Vulkan/graphics prerequisite problem (spec 10 "basic Vulkan diagnostics").
    GraphicsPrerequisite,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Confirmed,
    Likely,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub class: Class,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
}

/// (lowercase needle, class, confidence). First column is matched against the lowercased line.
const PATTERNS: &[(&str, Class, Confidence)] = &[
    (
        "toolmanifest.vdf not found",
        Class::RuntimeInitFailure,
        Confidence::Confirmed,
    ),
    // "unable to use parent for game drive" is benign (Proton prints it outside a Steam library)
    // and "executable not found:" needs the run outcome, so both are handled in `classify`.
    (
        "pressure-vessel",
        Class::RuntimeInitFailure,
        Confidence::Likely,
    ),
    (
        "failed to download",
        Class::RuntimeInitFailure,
        Confidence::Likely,
    ),
    (
        "unhandled exception",
        Class::AppCrash,
        Confidence::Confirmed,
    ),
    ("page fault on", Class::AppCrash, Confidence::Confirmed),
    (
        "vk_error_incompatible_driver",
        Class::GraphicsPrerequisite,
        Confidence::Likely,
    ),
    (
        "vkcreateinstance failed",
        Class::GraphicsPrerequisite,
        Confidence::Likely,
    ),
    (
        "libvulkan.so",
        Class::GraphicsPrerequisite,
        Confidence::Likely,
    ),
    ("no vulkan", Class::GraphicsPrerequisite, Confidence::Likely),
];

const NOT_FOUND: &[&str] = &[
    "cannot find",
    "could not find",
    "can't find",
    "file not found",
    "could not open",
    "unable to open",
];
const DATA_EXT: &[&str] = &[".cab", ".bin", ".dat", ".pak", ".msi", ".arc", ".mpq"];

fn strip_prefix(l: &str) -> &str {
    l.strip_prefix("O|")
        .or_else(|| l.strip_prefix("E|"))
        .unwrap_or(l)
}

/// Extract `X.dll` from `err:module:import_dll Library X.dll (which ...) not found`.
fn missing_dll(line: &str) -> Option<&str> {
    let rest = &line[line.find("import_dll")?..];
    let rest = rest[rest.find("Library ")? + 8..].trim_start();
    let name = rest.split_whitespace().next()?;
    (rest.contains("not found") && !name.is_empty()).then_some(name)
}

/// Last whitespace token of a not-found line that looks like a file name (has an extension).
fn named_file(line: &str) -> Option<&str> {
    line.split_whitespace()
        .map(|t| t.trim_matches(|c: char| "\"'.,:;()".contains(c)))
        .find(|t| {
            t.rsplit_once('.')
                .is_some_and(|(s, e)| !s.is_empty() && (1..=4).contains(&e.len()))
        })
}

/// umu logs `Executable not found:` for every `C:\windows\system32\*.exe` launch, even successful
/// ones, so that line is evidence only for a failed run outside system32. `low` is lowercased.
fn is_system32_builtin(low: &str) -> bool {
    low.replace('/', "\\").contains("\\windows\\system32\\")
}

fn push(out: &mut Vec<Finding>, class: Class, confidence: Confidence, line: &str) {
    if let Some(f) = out
        .iter_mut()
        .find(|f| f.class == class && class != Class::MissingDependency)
    {
        f.confidence = f.confidence.max_of(confidence);
        if f.evidence.len() < 3 && !f.evidence.iter().any(|e| e == line) {
            f.evidence.push(line.to_owned());
        }
    } else {
        out.push(Finding {
            class,
            confidence,
            evidence: vec![line.to_owned()],
        });
    }
}

impl Confidence {
    fn max_of(self, o: Self) -> Self {
        if self == Self::Confirmed || o == Self::Confirmed {
            Self::Confirmed
        } else {
            Self::Likely
        }
    }
}

pub fn classify(kind: RunKind, exit: Exit, lines: &[String], pre: &Preflight) -> Vec<Finding> {
    let mut out = Vec::new();
    let why = |s: &str| s.to_owned();

    if pre.exe_exists == Some(false) {
        push(
            &mut out,
            Class::MissingExecutable,
            Confidence::Confirmed,
            &why("preflight: executable does not exist"),
        );
    }
    if pre.source_exists == Some(false) {
        push(
            &mut out,
            Class::MissingInstallerData,
            Confidence::Confirmed,
            &why("preflight: installer source no longer exists"),
        );
    }
    if pre.runtime_valid == Some(false) {
        push(
            &mut out,
            Class::RuntimeInitFailure,
            Confidence::Confirmed,
            &why("preflight: runtime directory is not a valid Proton"),
        );
    }

    // Installers legitimately return 3010 / 1641 (reboot required); never treat those as failure.
    let failed = matches!(exit, Exit::Code(c) if c != 0 && c != 3010 && c != 1641);

    for raw in lines {
        let line = strip_prefix(raw).trim();
        let low = line.to_lowercase();
        if let Some(dll) = missing_dll(line) {
            out.push(Finding {
                class: Class::MissingDependency,
                confidence: Confidence::Confirmed,
                evidence: vec![format!("missing DLL {dll}: {line}")],
            });
            continue;
        }
        for &(needle, class, conf) in PATTERNS {
            if low.contains(needle) {
                push(&mut out, class, conf, line);
            }
        }
        if failed && low.contains("executable not found:") && !is_system32_builtin(&low) {
            push(&mut out, Class::MissingExecutable, Confidence::Likely, line);
        }
        if NOT_FOUND.iter().any(|n| low.contains(n))
            && let Some(file) = named_file(line)
        {
            let lf = file.to_lowercase();
            if kind == RunKind::Install && DATA_EXT.iter().any(|e| lf.ends_with(e)) {
                push(
                    &mut out,
                    Class::MissingInstallerData,
                    Confidence::Likely,
                    line,
                );
            }
            // Relative name + cwd differs from exe dir: only ever Likely.
            if pre.cwd_is_exe_dir == Some(false) && !file.contains(['/', '\\', ':']) {
                push(
                    &mut out,
                    Class::WrongWorkingDirectory,
                    Confidence::Likely,
                    line,
                );
            }
        }
    }

    if let (Exit::Signal(s), RunKind::Launch | RunKind::Install) = (exit, kind) {
        push(
            &mut out,
            Class::AppCrash,
            Confidence::Confirmed,
            &format!("terminated by signal {s}"),
        );
    }
    if out.is_empty() && failed {
        if kind == RunKind::Install {
            push(
                &mut out,
                Class::InstallerFailure,
                Confidence::Likely,
                &format!("installer exited with {exit:?} and no known pattern matched"),
            );
        } else {
            push(
                &mut out,
                Class::Unknown,
                Confidence::Likely,
                &format!("exited with {exit:?}; no known pattern matched"),
            );
        }
    }
    out
}

/// Maps a run's exit (only the low 8 bits are known) to [`Exit`]; the installer reboot codes
/// 3010 / 1641 are mapped back so `classify` accepts them.
pub fn exit_of(e: Option<crate::process::Exit>) -> Exit {
    use crate::process::Exit as P;
    match e {
        Some(x) if x.matches(3010) => Exit::Code(3010),
        Some(x) if x.matches(1641) => Exit::Code(1641),
        Some(P::Low8(c)) => Exit::Code(i32::from(c)),
        Some(P::Signal(s)) => Exit::Signal(s),
        None => Exit::Unknown,
    }
}

/// The output lines of a run log: everything after the header's `--` separator.
pub fn log_lines(log: &str) -> Vec<String> {
    log.lines()
        .skip_while(|l| *l != "--")
        .skip(1)
        .map(str::to_owned)
        .collect()
}

const CLASSES: &[Class] = &[
    Class::InstallerFailure,
    Class::RuntimeInitFailure,
    Class::MissingDependency,
    Class::MissingExecutable,
    Class::AppCrash,
    Class::MissingInstallerData,
    Class::WrongWorkingDirectory,
    Class::GraphicsPrerequisite,
    Class::Unknown,
];

/// The JSON stored in `runs.findings`: `[{"class", "confidence", "evidence"}]`.
pub fn to_json(f: &[Finding]) -> String {
    let v: Vec<_> = f
        .iter()
        .map(|f| {
            serde_json::json!({
                "class": format!("{:?}", f.class),
                "confidence": format!("{:?}", f.confidence),
                "evidence": f.evidence,
            })
        })
        .collect();
    serde_json::Value::Array(v).to_string()
}

/// Inverse of [`to_json`]; unreadable input or entries give no findings.
pub fn from_json(s: &str) -> Vec<Finding> {
    let v: Vec<serde_json::Value> = serde_json::from_str(s).unwrap_or_default();
    v.iter()
        .filter_map(|f| {
            let class = *CLASSES
                .iter()
                .find(|c| format!("{c:?}") == f["class"].as_str().unwrap_or(""))?;
            let confidence = match f["confidence"].as_str()? {
                "Confirmed" => Confidence::Confirmed,
                _ => Confidence::Likely,
            };
            let evidence = f["evidence"]
                .as_array()?
                .iter()
                .filter_map(|e| e.as_str().map(str::to_owned))
                .collect();
            Some(Finding {
                class,
                confidence,
                evidence,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str) -> Vec<String> {
        let p = format!("{}/tests/fixtures/logs/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(p)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
    fn run(kind: RunKind, exit: Exit, name: &str, pre: Preflight) -> Vec<Finding> {
        classify(kind, exit, &fx(name), &pre)
    }
    fn has(f: &[Finding], c: Class) -> Option<&Finding> {
        f.iter().find(|x| x.class == c)
    }
    const OK: Exit = Exit::Code(0);
    const BAD: Exit = Exit::Code(1);

    #[test]
    fn runtime_init() {
        let f = run(
            RunKind::Launch,
            BAD,
            "real-umu-no-toolmanifest.log",
            Preflight::default(),
        );
        assert_eq!(
            has(&f, Class::RuntimeInitFailure).unwrap().confidence,
            Confidence::Confirmed
        );
        let f = run(
            RunKind::Launch,
            BAD,
            "synthetic-clean.log",
            Preflight::default(),
        );
        assert!(has(&f, Class::RuntimeInitFailure).is_none());
        let pre = Preflight {
            runtime_valid: Some(false),
            ..Default::default()
        };
        assert!(
            has(
                &run(RunKind::Launch, BAD, "synthetic-clean.log", pre),
                Class::RuntimeInitFailure
            )
            .is_some()
        );
    }

    #[test]
    fn missing_dependency_names_dll() {
        let f = run(
            RunKind::Launch,
            BAD,
            "synthetic-missing-dll.log",
            Preflight::default(),
        );
        let d = has(&f, Class::MissingDependency).unwrap();
        assert_eq!(d.confidence, Confidence::Confirmed);
        assert!(d.evidence[0].contains("MSVCP140.dll"));
        assert!(
            has(
                &run(
                    RunKind::Launch,
                    OK,
                    "synthetic-crash.log",
                    Preflight::default()
                ),
                Class::MissingDependency
            )
            .is_none()
        );
    }

    #[test]
    fn missing_executable() {
        let pre = Preflight {
            exe_exists: Some(false),
            ..Default::default()
        };
        let f = run(
            RunKind::Launch,
            Exit::Code(5),
            "real-umu-missing-exe.log",
            pre,
        );
        assert_eq!(
            has(&f, Class::MissingExecutable).unwrap().confidence,
            Confidence::Confirmed
        );
        // log line alone is only Likely
        let f = run(
            RunKind::Launch,
            Exit::Code(5),
            "real-umu-missing-exe.log",
            Preflight::default(),
        );
        assert_eq!(
            has(&f, Class::MissingExecutable).unwrap().confidence,
            Confidence::Likely
        );
        let pre = Preflight {
            exe_exists: Some(true),
            ..Default::default()
        };
        assert!(
            has(
                &run(RunKind::Launch, OK, "synthetic-clean.log", pre),
                Class::MissingExecutable
            )
            .is_none()
        );
    }

    #[test]
    fn benign_proton_and_system32_noise_is_not_a_finding() {
        // Successful run with both benign lines: no findings at all.
        assert!(
            run(
                RunKind::Launch,
                OK,
                "real-umu-system32-builtin.log",
                Preflight::default()
            )
            .is_empty()
        );
        // The same lines on a failed run are still not a runtime or executable finding.
        let f = run(
            RunKind::Launch,
            BAD,
            "real-umu-system32-builtin.log",
            Preflight::default(),
        );
        assert!(has(&f, Class::RuntimeInitFailure).is_none());
        assert!(has(&f, Class::MissingExecutable).is_none());
        // Real failed run with the "unable to use parent" line and a non-system32 path:
        // only the missing executable is reported, never a runtime failure.
        let f = run(
            RunKind::Launch,
            Exit::Code(5),
            "real-umu-missing-exe.log",
            Preflight::default(),
        );
        assert!(has(&f, Class::RuntimeInitFailure).is_none());
        assert!(has(&f, Class::MissingExecutable).is_some());
    }

    #[test]
    fn crash() {
        for n in ["synthetic-crash.log", "synthetic-pagefault.log"] {
            let f = run(RunKind::Launch, BAD, n, Preflight::default());
            assert_eq!(
                has(&f, Class::AppCrash).unwrap().confidence,
                Confidence::Confirmed
            );
        }
        let f = classify(
            RunKind::Launch,
            Exit::Signal(11),
            &[],
            &Preflight::default(),
        );
        assert!(has(&f, Class::AppCrash).is_some());
        assert!(
            has(
                &run(
                    RunKind::Launch,
                    OK,
                    "synthetic-clean.log",
                    Preflight::default()
                ),
                Class::AppCrash
            )
            .is_none()
        );
    }

    #[test]
    fn missing_installer_data() {
        let pre = Preflight {
            source_exists: Some(false),
            ..Default::default()
        };
        let f = run(RunKind::Install, BAD, "synthetic-clean.log", pre);
        assert_eq!(
            has(&f, Class::MissingInstallerData).unwrap().confidence,
            Confidence::Confirmed
        );
        let f = run(
            RunKind::Install,
            BAD,
            "synthetic-missing-data.log",
            Preflight::default(),
        );
        assert_eq!(
            has(&f, Class::MissingInstallerData).unwrap().confidence,
            Confidence::Likely
        );
        assert!(
            has(
                &run(
                    RunKind::Install,
                    OK,
                    "synthetic-clean.log",
                    Preflight::default()
                ),
                Class::MissingInstallerData
            )
            .is_none()
        );
    }

    #[test]
    fn working_dir_only_likely() {
        let pre = Preflight {
            cwd_is_exe_dir: Some(false),
            ..Default::default()
        };
        let f = run(
            RunKind::Install,
            BAD,
            "synthetic-relative-notfound.log",
            pre,
        );
        assert_eq!(
            has(&f, Class::WrongWorkingDirectory).unwrap().confidence,
            Confidence::Likely
        );
        let pre = Preflight {
            cwd_is_exe_dir: Some(true),
            ..Default::default()
        };
        assert!(
            has(
                &run(
                    RunKind::Install,
                    BAD,
                    "synthetic-relative-notfound.log",
                    pre
                ),
                Class::WrongWorkingDirectory
            )
            .is_none()
        );
        let f = run(
            RunKind::Install,
            BAD,
            "synthetic-relative-notfound.log",
            Preflight::default(),
        );
        assert!(has(&f, Class::WrongWorkingDirectory).is_none());
    }

    #[test]
    fn graphics() {
        let f = run(
            RunKind::Launch,
            BAD,
            "synthetic-vulkan.log",
            Preflight::default(),
        );
        assert_eq!(
            has(&f, Class::GraphicsPrerequisite).unwrap().confidence,
            Confidence::Likely
        );
        assert!(
            has(
                &run(
                    RunKind::Launch,
                    OK,
                    "synthetic-clean.log",
                    Preflight::default()
                ),
                Class::GraphicsPrerequisite
            )
            .is_none()
        );
    }

    #[test]
    fn installer_failure_and_clean() {
        let f = run(
            RunKind::Install,
            BAD,
            "synthetic-clean.log",
            Preflight::default(),
        );
        assert_eq!(
            has(&f, Class::InstallerFailure).unwrap().confidence,
            Confidence::Likely
        );
        assert!(
            run(
                RunKind::Install,
                Exit::Code(3010),
                "synthetic-clean.log",
                Preflight::default()
            )
            .is_empty()
        );
        assert!(
            run(
                RunKind::Install,
                OK,
                "synthetic-clean.log",
                Preflight::default()
            )
            .is_empty()
        );
        assert!(
            run(
                RunKind::Launch,
                OK,
                "real-proton-benign-noise.log",
                Preflight::default()
            )
            .is_empty()
        );
    }

    #[test]
    fn findings_json_roundtrip() {
        let f = vec![
            Finding {
                class: Class::AppCrash,
                confidence: Confidence::Confirmed,
                evidence: vec!["wine: Unhandled page fault".into()],
            },
            Finding {
                class: Class::Unknown,
                confidence: Confidence::Likely,
                evidence: vec![],
            },
        ];
        assert_eq!(from_json(&to_json(&f)), f);
        assert!(from_json("not json").is_empty());
        assert_eq!(
            log_lines("kind: launch\n--\nE|a\nO|b"),
            vec!["E|a".to_string(), "O|b".to_string()]
        );
        assert_eq!(
            exit_of(Some(crate::process::Exit::Low8(194))),
            Exit::Code(3010)
        );
        assert_eq!(
            exit_of(Some(crate::process::Exit::Low8(42))),
            Exit::Code(42)
        );
    }
}
