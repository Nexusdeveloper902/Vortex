//! GE-Proton release lookup, download, verification and extraction.

use crate::error::{Error, Result};
use crate::paths::HostPath;
use crate::runtime::validate;
use sha2::{Digest, Sha512};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

pub const LATEST_URL: &str =
    "https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases/latest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub asset_name: String,
    pub asset_url: String,
    pub sum_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Downloading { done: u64, total: Option<u64> },
    Verifying,
    Extracting,
}

fn net(e: impl std::fmt::Display) -> Error {
    Error::Download(e.to_string())
}

fn no_asset() -> Error {
    Error::Download("no suitable GE-Proton asset in release".into())
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    ureq::get(url)
        .header("User-Agent", "vortex")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(net)
}

/// Pick the x86_64 tarball and its `.sha512sum` from a GitHub release JSON.
pub fn parse_release(json: &[u8]) -> Result<Release> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(net)?;
    let tag = v["tag_name"].as_str().ok_or_else(no_asset)?.to_string();
    let assets: Vec<(&str, &str)> = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some((a["name"].as_str()?, a["browser_download_url"].as_str()?)))
        .collect();
    let (asset_name, asset_url) = assets
        .iter()
        .find(|(n, _)| {
            n.starts_with("GE-Proton") && n.ends_with(".tar.gz") && !n.contains("aarch64")
        })
        .ok_or_else(no_asset)?;
    let sum_name = format!("{}.sha512sum", asset_name.trim_end_matches(".tar.gz"));
    let (_, sum_url) = assets
        .iter()
        .find(|(n, _)| *n == sum_name)
        .ok_or_else(no_asset)?;
    Ok(Release {
        tag,
        asset_name: asset_name.to_string(),
        asset_url: asset_url.to_string(),
        sum_url: sum_url.to_string(),
    })
}

pub fn latest_release() -> Result<Release> {
    let body = get(LATEST_URL)?.body_mut().read_to_vec().map_err(net)?;
    parse_release(&body)
}

/// Removes a path (file or dir) on drop; used for `.part` and `.tmp-*`.
struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        let _ = fs::remove_file(&self.0);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

/// Download, verify, extract and atomically install `release` into `runtimes_dir/<top-level dir>`.
/// Any failure or cancel leaves nothing behind except already-installed runtimes.
pub fn install(
    release: &Release,
    runtimes_dir: &Path,
    progress: &Sender<Progress>,
    cancel: &AtomicBool,
) -> Result<HostPath> {
    fs::create_dir_all(runtimes_dir).map_err(Error::io(runtimes_dir))?;
    let part = runtimes_dir.join(format!(".tmp-{}.part", release.tag));
    let _part = Cleanup(part.clone());
    let actual = download(&release.asset_url, &part, progress, cancel)?;

    let _ = progress.send(Progress::Verifying);
    let sum = get(&release.sum_url)?
        .body_mut()
        .read_to_string()
        .map_err(net)?;
    let expected = sum
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if expected != actual {
        return Err(Error::Download(format!(
            "checksum mismatch: expected {expected}, got {actual}"
        )));
    }

    let _ = progress.send(Progress::Extracting);
    let tmp = runtimes_dir.join(format!(".tmp-{}", release.tag));
    let _tmp = Cleanup(tmp.clone());
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir(&tmp).map_err(Error::io(&tmp))?;
    let top = extract(&part, &tmp, cancel)?;
    let staged = tmp.join(&top);
    validate(&staged)?;
    let dest = runtimes_dir.join(&top);
    if dest.exists() {
        return Err(Error::Validation(format!(
            "{} is already installed",
            dest.display()
        )));
    }
    check(cancel)?;
    fs::rename(&staged, &dest).map_err(Error::io(&dest))?;
    HostPath::new(dest)
}

/// Stream to `part`, returning the lowercase hex sha512.
fn download(
    url: &str,
    part: &Path,
    progress: &Sender<Progress>,
    cancel: &AtomicBool,
) -> Result<String> {
    let mut resp = get(url)?;
    let total = resp.body().content_length();
    let mut r = resp.body_mut().as_reader();
    let mut f = File::create(part).map_err(Error::io(part))?;
    let (mut h, mut done, mut buf) = (Sha512::new(), 0u64, vec![0u8; 64 * 1024]);
    loop {
        check(cancel)?;
        let n = r.read(&mut buf).map_err(Error::io(part))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        f.write_all(&buf[..n]).map_err(Error::io(part))?;
        done += n as u64;
        let _ = progress.send(Progress::Downloading { done, total });
    }
    f.sync_all().map_err(Error::io(part))?;
    Ok(hex(&h.finalize()))
}

/// Unpack into `into`, rejecting unsafe paths; returns the single top-level dir name.
fn extract(tarball: &Path, into: &Path, cancel: &AtomicBool) -> Result<String> {
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(
        File::open(tarball).map_err(Error::io(tarball))?,
    ));
    let mut tops = BTreeSet::new();
    let bad = |e: String| Error::Download(format!("bad archive: {e}"));
    for e in ar.entries().map_err(|e| bad(e.to_string()))? {
        check(cancel)?;
        let mut e = e.map_err(|e| bad(e.to_string()))?;
        let path = e.path().map_err(|e| bad(e.to_string()))?.into_owned();
        let mut comps = path.components();
        let first = match comps.next() {
            Some(Component::Normal(c)) => c.to_string_lossy().into_owned(),
            Some(Component::CurDir) => continue,
            _ => return Err(bad(format!("unsafe entry {}", path.display()))),
        };
        if comps.any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
            return Err(bad(format!("unsafe entry {}", path.display())));
        }
        tops.insert(first);
        if tops.len() > 1 {
            return Err(bad("more than one top-level entry".into()));
        }
        if !e.unpack_in(into).map_err(|e| bad(e.to_string()))? {
            return Err(bad(format!("unsafe entry {}", path.display())));
        }
    }
    tops.pop_first().ok_or_else(|| bad("empty archive".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::mpsc::channel;
    use std::thread;

    /// Tar.gz with the given (path, mode, content); raw header bytes so `..` can be written.
    fn tarball(entries: &[(&str, u32, &str)]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (p, mode, c) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(c.len() as u64);
            h.set_mode(*mode);
            h.set_entry_type(tar::EntryType::Regular);
            let name = h.as_gnu_mut().unwrap();
            name.name[..p.len()].copy_from_slice(p.as_bytes());
            h.set_cksum();
            b.append(&h, c.as_bytes()).unwrap();
        }
        let tar = b.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    fn good() -> Vec<u8> {
        tarball(&[
            ("GE-Proton9-9/proton", 0o755, "#!/bin/sh"),
            ("GE-Proton9-9/toolmanifest.vdf", 0o644, ""),
            ("GE-Proton9-9/files/bin/wineserver", 0o755, ""),
        ])
    }

    /// Serves `/asset` and `/sum` forever on localhost; returns base URL.
    fn serve(asset: Vec<u8>, sum: String) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut line = String::new();
                let mut r = BufReader::new(&s);
                r.read_line(&mut line).unwrap();
                while r.read_line(&mut line.clone()).is_ok_and(|n| n > 2) {}
                let body = if line.contains("/asset") {
                    asset.clone()
                } else {
                    sum.clone().into_bytes()
                };
                let mut s = &s;
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(&body);
            }
        });
        base
    }

    fn rel(base: &str) -> Release {
        Release {
            tag: "GE-Proton9-9".into(),
            asset_name: "GE-Proton9-9.tar.gz".into(),
            asset_url: format!("{base}/asset"),
            sum_url: format!("{base}/sum"),
        }
    }

    fn sum_of(data: &[u8]) -> String {
        format!("{}  GE-Proton9-9.tar.gz\n", hex(&Sha512::digest(data)))
    }

    fn run(data: Vec<u8>, sum: String, cancel: bool) -> (Result<HostPath>, tempfile::TempDir) {
        let base = serve(data, sum);
        let t = tempfile::tempdir().unwrap();
        let (tx, _rx) = channel();
        let r = install(&rel(&base), t.path(), &tx, &AtomicBool::new(cancel));
        (r, t)
    }

    fn left(t: &tempfile::TempDir) -> Vec<String> {
        fs::read_dir(t.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into())
            .collect()
    }

    #[test]
    fn good_hash_installs() {
        let d = good();
        let (r, t) = run(d.clone(), sum_of(&d), false);
        let p = r.unwrap();
        assert_eq!(p.as_path(), t.path().join("GE-Proton9-9"));
        assert!(validate(p.as_path()).is_ok());
        assert_eq!(left(&t), ["GE-Proton9-9"]);
    }

    #[test]
    fn bad_hash_leaves_nothing() {
        let (r, t) = run(good(), sum_of(b"other"), false);
        assert!(matches!(&r, Err(Error::Download(m)) if m.contains("checksum")));
        assert!(left(&t).is_empty());
    }

    #[test]
    fn cancel_leaves_nothing() {
        let d = good();
        let (r, t) = run(d.clone(), sum_of(&d), true);
        assert!(matches!(r, Err(Error::Cancelled)));
        assert!(left(&t).is_empty());
    }

    #[test]
    fn dotdot_entry_rejected() {
        let d = tarball(&[("GE-Proton9-9/../evil", 0o644, "x")]);
        let (r, t) = run(d.clone(), sum_of(&d), false);
        assert!(
            matches!(&r, Err(Error::Download(m)) if m.contains("bad archive")),
            "{r:?}"
        );
        assert!(left(&t).is_empty());
    }

    #[test]
    fn two_top_level_dirs_rejected() {
        let d = tarball(&[("a/proton", 0o755, ""), ("b/proton", 0o755, "")]);
        let (r, t) = run(d.clone(), sum_of(&d), false);
        assert!(
            matches!(&r, Err(Error::Download(m)) if m.contains("bad archive")),
            "{r:?}"
        );
        assert!(left(&t).is_empty());
    }

    #[test]
    fn asset_selection() {
        let j = br#"{"tag_name":"GE-Proton11-12","assets":[
          {"name":"GE-Proton11-12-aarch64.tar.gz","browser_download_url":"u/a"},
          {"name":"GE-Proton11-12-x86_64.tar.gz","browser_download_url":"u/x"},
          {"name":"GE-Proton11-12-x86_64.sha512sum","browser_download_url":"u/s"}]}"#;
        let r = parse_release(j).unwrap();
        assert_eq!(
            (r.tag.as_str(), r.asset_url.as_str(), r.sum_url.as_str()),
            ("GE-Proton11-12", "u/x", "u/s")
        );
    }

    #[test]
    #[ignore = "network: resolves latest GE release metadata only"]
    fn real_latest_release() {
        let r = latest_release().unwrap();
        println!("{r:?}");
        assert!(r.tag.starts_with("GE-Proton"));
    }
}
