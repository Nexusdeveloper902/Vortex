//! M18: tool probing with tempdir PATHs.
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use vortex_core::error::Error;
use vortex_core::runner::{Tools, which};

fn file(dir: &Path, name: &str, mode: u32) {
    let p = dir.join(name);
    std::fs::write(&p, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn finds_executables_on_path() {
    let (bin, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    file(bin.path(), "umu-run", 0o755);
    let c = Tools::default().check(bin.path().as_os_str(), home.path(), &home.path().join("rt"));
    assert_eq!(c.umu_run, Some(bin.path().join("umu-run")));
}

#[test]
fn missing_and_non_executable_are_none() {
    let (bin, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    file(bin.path(), "umu-run", 0o644);
    let c = Tools::default().check(bin.path().as_os_str(), home.path(), &home.path().join("rt"));
    assert!(c.umu_run.is_none());
}

#[test]
fn configured_absolute_path_ignores_path_var() {
    let d = tempfile::tempdir().unwrap();
    let t = Tools {
        umu_run: format!("{}/tests", env!("CARGO_MANIFEST_DIR")).into(),
    };
    assert!(
        which(&t.umu_run, "".as_ref()).is_none(),
        "a directory is not a tool"
    );
    let t = Tools {
        umu_run: format!("{}/tests/fake-umu-run", env!("CARGO_MANIFEST_DIR")).into(),
    };
    assert_eq!(
        which(&t.umu_run, d.path().as_os_str()),
        Some(t.umu_run.clone())
    );
    assert!(which(Path::new("/nonexistent/umu-run"), "".as_ref()).is_none());
}

#[test]
fn umu_not_found_message_is_friendly() {
    let e = Error::Io {
        path: "umu-run".into(),
        source: std::io::ErrorKind::NotFound.into(),
    };
    assert!(e.to_string().contains("umu-launcher"));
}
