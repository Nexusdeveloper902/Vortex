//! Cover art: automatic lookup on Steam's public store search + CDN (no API key), user-chosen
//! images, and the per-game cache file at `Paths::artwork_file`.
//!
//! Only two kinds of request are made: the store search JSON endpoint and image files on the CDN.
//! Both base URLs come from [`Sources`] (tests point them at localhost). Redirects are not
//! followed, so every request stays on the configured hosts. Nothing here runs when the
//! `artwork_auto` setting is `"0"` or offline mode is on.

use crate::db::{ArtKind, ArtSource, Db};
use crate::error::{Error, Result};
use crate::paths::{HostPath, Paths};
use async_channel::Receiver;
use std::fs;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Settings key; `"0"` turns automatic artwork off. Absent means on.
pub const KEY_AUTO: &str = "artwork_auto";
/// Settings key shared with the GTK preferences: `"1"` = never download anything.
pub const KEY_OFFLINE: &str = "offline_mode";

const MAX_IMAGE: u64 = 5 * 1024 * 1024;
const MAX_CUSTOM: u64 = 20 * 1024 * 1024;
const MAX_JSON: u64 = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(20);

/// Where lookups go. `Default` is Steam; tests use a localhost server.
#[derive(Debug, Clone)]
pub struct Sources {
    /// Store search endpoint; `?term=...&l=english&cc=US` is appended.
    pub search: String,
    /// CDN base; `/<appid>/<file>` is appended.
    pub cdn: String,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            search: "https://store.steampowered.com/api/storesearch/".into(),
            cdn: "https://shared.steamstatic.com/store_item_assets/steam/apps".into(),
        }
    }
}

/// A downloaded, validated image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub bytes: Vec<u8>,
    pub kind: ArtKind,
    pub steam_app: i64,
}

/// Plain-data event from [`spawn_auto`]: new artwork for a game is in the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Updated {
    pub game_id: i64,
    pub path: HostPath,
}

/// Lowercase, without ™®© and apostrophes, other punctuation as spaces, spaces collapsed.
pub fn normalize(s: &str) -> String {
    let s: String = s
        .chars()
        .filter(|c| !"™®©'’".contains(*c))
        .flat_map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().collect::<Vec<_>>()
            } else {
                vec![' ']
            }
        })
        .collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `normalize` plus a dropped leading "the".
fn key(s: &str) -> String {
    let n = normalize(s);
    match n.strip_prefix("the ") {
        Some(rest) => rest.to_string(),
        None => n,
    }
}

/// Picks the app for `name` from `(id, name)` search results. Any exact (normalized) match wins;
/// otherwise only the first result, and only if its title before a `:` / ` - ` subtitle matches.
/// Anything else is no match: wrong art is worse than the placeholder.
pub fn best_match(name: &str, items: &[(i64, String)]) -> Option<i64> {
    let q = key(name);
    if q.is_empty() {
        return None;
    }
    if let Some((id, _)) = items.iter().find(|(_, n)| key(n) == q) {
        return Some(*id);
    }
    let (id, first) = items.first()?;
    let head = first.split(':').next().unwrap_or(first);
    let head = head.split(" - ").next().unwrap_or(head);
    (key(head) == q).then_some(*id)
}

/// `(id, name)` of the `app` items in a store search response.
pub fn parse_search(json: &[u8]) -> Result<Vec<(i64, String)>> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(net)?;
    Ok(v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["type"].as_str().is_none_or(|t| t == "app"))
        .filter_map(|i| Some((i["id"].as_i64()?, i["name"].as_str()?.to_string())))
        .collect())
}

/// JPEG or PNG by magic bytes.
pub fn is_image(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8, 0xFF]) || b.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn net(e: impl std::fmt::Display) -> Error {
    Error::Download(e.to_string())
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// GET returning the body (at most `limit` bytes). 404 is `NotFound`; redirects and other
/// statuses are errors. When `image`, the Content-Type and magic bytes must be JPEG/PNG.
fn get(url: &str, limit: u64, image: bool) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let mut resp = agent
        .get(url)
        .header("User-Agent", "vortex")
        .call()
        .map_err(net)?;
    let st = resp.status();
    if st == 404 {
        return Err(Error::NotFound(url.to_string()));
    }
    if !st.is_success() {
        return Err(net(format!("HTTP {st} from {url}")));
    }
    if image {
        let ct = resp.body().mime_type().unwrap_or("");
        if ct != "image/jpeg" && ct != "image/png" {
            return Err(net(format!("{url} is not an image ({ct:?})")));
        }
    }
    if resp.body().content_length().is_some_and(|n| n > limit) {
        return Err(net(format!("{url} is larger than {limit} bytes")));
    }
    let body = resp
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(net)?;
    if image && !is_image(&body) {
        return Err(net(format!("{url} is not a JPEG or PNG image")));
    }
    Ok(body)
}

/// Looks `name` up and downloads the best cover: portrait 2x, portrait, then the landscape
/// header. `Ok(None)` = no matching app, or the app has none of these images.
pub fn find(src: &Sources, name: &str) -> Result<Option<Image>> {
    if key(name).is_empty() {
        return Ok(None);
    }
    let url = format!("{}?term={}&l=english&cc=US", src.search, encode(name));
    let items = parse_search(&get(&url, MAX_JSON, false)?)?;
    let Some(app) = best_match(name, &items) else {
        return Ok(None);
    };
    let mut last = None;
    for (file, kind) in [
        ("library_600x900_2x.jpg", ArtKind::Portrait),
        ("library_600x900.jpg", ArtKind::Portrait),
        ("header.jpg", ArtKind::Landscape),
    ] {
        match get(&format!("{}/{app}/{file}", src.cdn), MAX_IMAGE, true) {
            Ok(bytes) => {
                return Ok(Some(Image {
                    bytes,
                    kind,
                    steam_app: app,
                }));
            }
            Err(Error::NotFound(_)) => {}
            Err(e) => last = Some(e),
        }
    }
    // All 404: the app has no art. Anything else may be transient: report, record nothing.
    last.map_or(Ok(None), Err)
}

/// Whether automatic lookups may make requests.
pub fn enabled(db: &Db) -> Result<bool> {
    Ok(db.get_setting(KEY_AUTO)?.as_deref() != Some("0")
        && db.get_setting(KEY_OFFLINE)?.as_deref() != Some("1"))
}

/// The cached image of a game, if it has one.
pub fn path(db: &Db, paths: &Paths, game_id: i64) -> Result<Option<HostPath>> {
    match db.get_artwork(game_id)? {
        Some(a) if a.source != ArtSource::None => {
            let p = paths.artwork_file(game_id);
            if p.is_file() {
                Ok(Some(HostPath::new(p)?))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

/// tmp + rename into the cache dir.
fn write(paths: &Paths, game_id: i64, bytes: &[u8]) -> Result<HostPath> {
    let dir = paths.artwork_dir();
    crate::paths::create_private_dir(&dir)?;
    let dest = paths.artwork_file(game_id);
    let tmp = dir.join(format!(".tmp-{game_id}"));
    let r = (|| {
        let mut f = fs::File::create(&tmp).map_err(Error::io(&tmp))?;
        f.write_all(bytes).map_err(Error::io(&tmp))?;
        f.sync_all().map_err(Error::io(&tmp))?;
        fs::rename(&tmp, &dest).map_err(Error::io(&dest))
    })();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r?;
    HostPath::new(dest)
}

fn lock(m: &Mutex<Db>) -> MutexGuard<'_, Db> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Looks the game up and caches its cover. The lock is not held during network I/O.
///
/// `force = false` (background): skips games that already have a row, and does not overwrite a
/// row written meanwhile (e.g. a custom image). `force = true` ("Refresh artwork"): replaces the
/// current image when a match is found; with no match the current image is kept.
/// `Ok(None)` = nothing new. Errors when lookups are turned off.
pub fn fetch(
    db: &Mutex<Db>,
    paths: &Paths,
    src: &Sources,
    game_id: i64,
    force: bool,
) -> Result<Option<HostPath>> {
    let name = {
        let db = lock(db);
        if !enabled(&db)? {
            return Err(Error::Validation(
                "Artwork downloads are turned off in Preferences".into(),
            ));
        }
        if !force && db.get_artwork(game_id)?.is_some() {
            return Ok(None);
        }
        db.get_game(game_id)?.name
    };
    let found = find(src, &name)?;
    let db = lock(db);
    let current = db.get_artwork(game_id)?;
    if !force && current.is_some() {
        return Ok(None);
    }
    match found {
        Some(img) => {
            let p = write(paths, game_id, &img.bytes)?;
            db.set_artwork(
                game_id,
                ArtSource::Auto,
                Some(img.kind),
                Some(img.steam_app),
            )?;
            Ok(Some(p))
        }
        None => {
            if current.is_none() {
                db.set_artwork(game_id, ArtSource::None, None, None)?;
            }
            Ok(None)
        }
    }
}

/// Background lookup of `ids` (games with a row already are skipped; see
/// `Db::games_without_artwork`). Returns at once (closed channel) when lookups are off.
/// Failures go to stderr only: artwork is optional, offline keeps placeholders.
pub fn spawn_auto(
    db: Arc<Mutex<Db>>,
    paths: Paths,
    src: Sources,
    ids: Vec<i64>,
) -> Receiver<Updated> {
    let (tx, rx) = async_channel::bounded(256);
    if ids.is_empty() || !enabled(&lock(&db)).unwrap_or(false) {
        return rx;
    }
    std::thread::spawn(move || {
        for game_id in ids {
            match fetch(&db, &paths, &src, game_id, false) {
                Ok(Some(path)) => {
                    let _ = tx.send_blocking(Updated { game_id, path });
                }
                Ok(None) => {}
                Err(Error::Validation(_)) => return, // turned off meanwhile
                Err(e) => eprintln!("vortex: artwork for game {game_id}: {e}"),
            }
        }
    });
    rx
}

/// Copies a user-picked JPEG/PNG into the cache; it replaces any automatic art.
pub fn set_custom(db: &Db, paths: &Paths, game_id: i64, file: &HostPath) -> Result<HostPath> {
    db.get_game(game_id)?;
    let p = file.as_path();
    let f = fs::File::open(p).map_err(Error::io(p))?;
    let mut bytes = Vec::new();
    f.take(MAX_CUSTOM + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::io(p))?;
    if bytes.len() as u64 > MAX_CUSTOM {
        return Err(Error::Validation(format!(
            "{} is larger than {} MB",
            p.display(),
            MAX_CUSTOM / 1024 / 1024
        )));
    }
    if !is_image(&bytes) {
        return Err(Error::Validation(format!(
            "{} is not a JPEG or PNG image",
            p.display()
        )));
    }
    let out = write(paths, game_id, &bytes)?;
    db.set_artwork(game_id, ArtSource::Custom, None, None)?;
    Ok(out)
}

/// Deletes the game's cached image and records `none`, so it is not fetched again automatically.
pub fn remove(db: &Db, paths: &Paths, game_id: i64) -> Result<()> {
    db.get_game(game_id)?;
    remove_file(paths, game_id)?;
    db.set_artwork(game_id, ArtSource::None, None, None)
}

/// Deletes only `Paths::artwork_file(game_id)` (a regular file Vortex wrote), nothing else.
pub fn remove_file(paths: &Paths, game_id: i64) -> Result<()> {
    let p = paths.artwork_file(game_id);
    match fs::remove_file(&p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::io(&p)(e)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(v: &[(i64, &str)]) -> Vec<(i64, String)> {
        v.iter().map(|(i, n)| (*i, n.to_string())).collect()
    }

    #[test]
    fn normalization() {
        assert_eq!(normalize("Half-Life™ 2"), "half life 2");
        assert_eq!(normalize("  DOOM®:  Eternal  "), "doom eternal");
        assert_eq!(normalize("Assassin’s Creed"), "assassins creed");
        assert_eq!(normalize("Tom Clancy's"), "tom clancys");
        assert_eq!(normalize("™®©"), "");
    }

    #[test]
    fn matching() {
        let r = items(&[
            (620, "Portal 2"),
            (323180, "Portal 2 Soundtrack"),
            (400, "Portal"),
        ]);
        assert_eq!(best_match("portal 2", &r), Some(620));
        assert_eq!(best_match("Portal", &r), Some(400)); // exact beats first
        assert_eq!(best_match("PORTAL™ 2", &r), Some(620));
        // first result with a subtitle
        let w = items(&[(292030, "The Witcher® 3: Wild Hunt"), (1, "Other")]);
        assert_eq!(best_match("The Witcher 3", &w), Some(292030));
        assert_eq!(best_match("Witcher 3", &w), Some(292030));
        // not close: no match rather than random art
        assert_eq!(best_match("Portal 3", &r), None);
        assert_eq!(best_match("Hollow", &items(&[(1, "Hollow Knight")])), None);
        assert_eq!(best_match("x", &[]), None);
        assert_eq!(best_match("™", &r), None);
        // subtitle rule only applies to the first result
        let s = items(&[(1, "Unrelated"), (2, "Foo: Bar")]);
        assert_eq!(best_match("foo", &s), None);
    }

    #[test]
    fn search_json() {
        let j = br#"{"total":3,"items":[{"type":"app","name":"Portal 2","id":620},
            {"type":"sub","name":"Bundle","id":5},{"name":"No type","id":7},{"type":"app","id":8}]}"#;
        assert_eq!(
            parse_search(j).unwrap(),
            items(&[(620, "Portal 2"), (7, "No type")])
        );
        assert!(
            parse_search(br#"{"total":0,"items":[]}"#)
                .unwrap()
                .is_empty()
        );
        assert!(parse_search(b"<html>").is_err());
    }

    #[test]
    fn magic_and_encoding() {
        assert!(is_image(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(is_image(b"\x89PNG\r\n\x1a\nrest"));
        assert!(!is_image(b"<html>"));
        assert!(!is_image(b""));
        assert_eq!(encode("Half-Life 2: é&"), "Half-Life%202%3A%20%C3%A9%26");
    }
}
