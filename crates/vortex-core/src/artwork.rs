//! Cover art: automatic lookup on Steam's public store search + CDN (no API key), user-chosen
//! images, and the per-game cache file at `Paths::artwork_file`.
//!
//! Only two kinds of request are made: the store search JSON endpoint and image files on the CDN.
//! Both base URLs come from [`Sources`] (tests point them at localhost). Redirects are not
//! followed, so every request stays on the configured hosts. Nothing here runs when the
//! `artwork_auto` setting is `"0"` or offline mode is on.

use crate::db::{ArtKind, ArtSource, Db, GameStatus};
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
            } else if c == '&' {
                " and ".chars().collect()
            } else {
                vec![' ']
            }
        })
        .collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Roman numeral token (I..XX, lowercase) to its arabic value.
fn roman(t: &str) -> Option<u32> {
    const R: [&str; 20] = [
        "i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x", "xi", "xii", "xiii", "xiv",
        "xv", "xvi", "xvii", "xviii", "xix", "xx",
    ];
    R.iter().position(|r| *r == t).map(|i| i as u32 + 1)
}

/// `normalize` plus a dropped leading "the" and roman numerals as arabic (whole tokens, never the
/// first one: "I Am Bread" stays a name, "Hades II" == "Hades 2").
fn key(s: &str) -> String {
    let n = normalize(s);
    let n = n.strip_prefix("the ").unwrap_or(&n);
    n.split(' ')
        .enumerate()
        .map(|(i, t)| match roman(t) {
            Some(v) if i > 0 => v.to_string(),
            _ => t.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Store entries that are not the game itself (soundtrack, DLC, demo...), by name.
pub fn is_extra(name: &str) -> bool {
    let n = format!(" {} ", normalize(name));
    [
        "soundtrack",
        "ost",
        "dlc",
        "demo",
        "season pass",
        "artbook",
        "art book",
    ]
    .iter()
    .any(|w| n.contains(&format!(" {w} ")))
}

/// What a search result list says about a game name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// Exact (normalized, numerals equivalent) or the subtitle rule: safe to attach.
    Confident(i64),
    /// Results exist but none is certain: never attached automatically, the user chooses.
    Candidates(Vec<(i64, String)>),
    None,
}

/// Judges `(id, name)` search results for `name`. The base game is preferred: extras
/// (soundtrack, DLC, demo, season pass, artbook) are ignored unless the query itself is one or
/// nothing else was found. Confident = any exact match, else the first result whose title before
/// a `:` / ` - ` subtitle matches. Everything else is only a candidate list.
pub fn classify(name: &str, items: &[(i64, String)]) -> Match {
    let q = key(name);
    if q.is_empty() {
        return Match::None;
    }
    let mut pool: Vec<&(i64, String)> = items.iter().collect();
    if !is_extra(name) {
        pool.sort_by_key(|(_, n)| is_extra(n)); // stable: base games first
    }
    let pool_has_base = pool.iter().any(|(_, n)| !is_extra(n));
    if !is_extra(name) && pool_has_base {
        pool.retain(|(_, n)| !is_extra(n));
    }
    if let Some((id, _)) = pool.iter().find(|(_, n)| key(n) == q) {
        return Match::Confident(*id);
    }
    // The first result in the store's own order (among the kept ones).
    if let Some((id, first)) = pool.first() {
        let head = first.split(':').next().unwrap_or(first);
        let head = head.split(" - ").next().unwrap_or(head);
        if key(head) == q {
            return Match::Confident(*id);
        }
    }
    if pool.is_empty() {
        Match::None
    } else {
        Match::Candidates(pool.into_iter().take(10).cloned().collect())
    }
}

/// `Some(app)` only on a confident match (see [`classify`]).
pub fn best_match(name: &str, items: &[(i64, String)]) -> Option<i64> {
    match classify(name, items) {
        Match::Confident(id) => Some(id),
        _ => None,
    }
}

/// Words and phrases dropped from a game name before searching (lowercase).
const NOISE_PHRASES: &[&str] = &[
    "dev build",
    "development build",
    "closed beta",
    "open beta",
    "press build",
    "review build",
    "reviewer build",
    "early access",
    "release candidate",
];
const NOISE_WORDS: &[&str] = &[
    "debug", "shipping", "beta", "alpha", "preview", "playtest", "nightly", "demo", "internal",
    "rc", "build", "win64", "win32", "x64", "x86", "x86_64", "windows", "pc",
];
const INSTALLER_WORDS: &[&str] = &["setup", "installer", "install"];

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// A version-looking token: has a dot between digits (`0.9`, `v1.2.3`), or `v<n>`, `b<nnn>`,
/// `r<nnn>`. A plain number (a sequel number or a year) is not.
fn is_version(t: &str) -> bool {
    let body = t.strip_prefix('v').unwrap_or(t);
    if body.contains('.') {
        return body.split('.').all(digits);
    }
    match t.as_bytes().first() {
        Some(b'v') => digits(&t[1..]),
        Some(b'b' | b'r') => digits(&t[1..]) && t.len() > 3,
        _ => false,
    }
}

fn token_is_noise(t: &str) -> bool {
    NOISE_WORDS.contains(&t) || INSTALLER_WORDS.contains(&t) || is_version(t)
}

/// The Steam search term for a game name: build, edition and platform noise removed as whole
/// words, separators collapsed. The first word is never dropped except installer words, and a
/// plain number is never dropped ("Dark Souls 3", "2077"). May be empty.
pub fn clean_query(name: &str) -> String {
    // Brackets: dropped with their content when the content holds noise, else just unwrapped.
    let mut flat = String::new();
    let mut inner = String::new();
    let mut depth = 0;
    for c in name.chars() {
        match c {
            '(' | '[' | '{' if depth == 0 => depth = 1,
            ')' | ']' | '}' if depth == 1 => {
                depth = 0;
                let low = inner.to_lowercase();
                let words: Vec<&str> = low
                    .split(|c: char| c == '_' || c == ',' || c.is_whitespace())
                    .filter(|w| !w.is_empty())
                    .collect();
                if !words.iter().any(|w| token_is_noise(w)) {
                    flat.push(' ');
                    flat.push_str(&inner);
                    flat.push(' ');
                }
                inner.clear();
            }
            _ if depth == 1 => inner.push(c),
            _ => flat.push(c),
        }
    }
    flat.push_str(&inner); // unclosed bracket: keep the text
    // `_` is a separator; a dot is one unless it sits between digits (versions).
    let chars: Vec<char> = flat.chars().collect();
    let flat: String = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| match c {
            '_' => ' ',
            '.' if !(i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit())) =>
            {
                ' '
            }
            c => c,
        })
        .collect();
    let toks: Vec<&str> = flat.split_whitespace().collect();
    let low: Vec<String> = toks.iter().map(|t| t.to_lowercase()).collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    'outer: while i < toks.len() {
        for ph in NOISE_PHRASES {
            let n = ph.split(' ').count();
            if i > 0 && i + n <= toks.len() && low[i..i + n].join(" ") == *ph {
                i += n;
                continue 'outer;
            }
        }
        let t = &low[i];
        // "version 1.2", "build 1234": the word and the number after it.
        if i > 0 && (t == "version" || t == "build") {
            let next = low.get(i + 1).map(String::as_str).unwrap_or("");
            if t == "build" || is_version(next) || digits(next) {
                i += 1 + usize::from(is_version(next) || digits(next));
                continue;
            }
        }
        // Hyphen-joined noise ("Game-Beta", "Foo-x64"): drop noise parts, keep real hyphens.
        let parts: Vec<&str> = toks[i].split('-').collect();
        let kept: Vec<&str> = parts
            .iter()
            .enumerate()
            .filter(|(j, p)| {
                let lp = p.to_lowercase();
                let noise = if i == 0 && *j == 0 {
                    INSTALLER_WORDS.contains(&lp.as_str())
                } else {
                    token_is_noise(&lp)
                };
                !p.is_empty() && !noise
            })
            .map(|(_, p)| *p)
            .collect();
        if !kept.is_empty() {
            out.push(kept.join("-"));
        } else if parts.iter().all(|p| p.is_empty()) {
            out.push("-".into()); // a lone dash: keep as separator for now
        }
        i += 1;
    }
    while out.first().is_some_and(|t| t == "-") {
        out.remove(0);
    }
    while out.last().is_some_and(|t| t == "-") {
        out.pop();
    }
    out.dedup_by(|a, b| a == "-" && b == "-");
    out.join(" ")
}

/// A Steam app id from a pasted store URL (`store.steampowered.com/app/<id>/...`) or a bare id.
pub fn parse_app_id(input: &str) -> Option<i64> {
    let t = input.trim();
    let id = if digits(t) {
        t
    } else {
        let rest = t.split("store.steampowered.com/app/").nth(1)?;
        rest.split(|c: char| !c.is_ascii_digit()).next()?
    };
    id.parse().ok().filter(|n| *n > 0)
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

/// Result of looking a name up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// A confident match and its downloaded cover.
    Found(Image),
    /// Nothing usable (no results, or the matched app has no art).
    NoMatch,
    /// This many possible matches, none certain: not attached.
    Ambiguous(usize),
}

fn search_url(src: &Sources, term: &str) -> String {
    format!("{}?term={}&l=english&cc=US", src.search, encode(term))
}

/// `(id, name)` results of a store search for `term`, base games before extras.
pub fn search(src: &Sources, term: &str) -> Result<Vec<(i64, String)>> {
    let mut items = parse_search(&get(&search_url(src, term), MAX_JSON, false)?)?;
    items.sort_by_key(|(_, n)| is_extra(n));
    Ok(items)
}

/// Downloads the best cover of Steam app `app`: portrait 2x, portrait, then the landscape
/// header. `Ok(None)` = the app has none of these images.
pub fn download(src: &Sources, app: i64) -> Result<Option<Image>> {
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

/// Looks the game up by its [`clean_query`] term and downloads the cover on a confident match.
pub fn lookup(src: &Sources, name: &str) -> Result<Lookup> {
    let term = clean_query(name);
    if key(&term).is_empty() {
        return Ok(Lookup::NoMatch);
    }
    Ok(match classify(&term, &search(src, &term)?) {
        Match::Confident(app) => download(src, app)?.map_or(Lookup::NoMatch, Lookup::Found),
        Match::Candidates(c) => Lookup::Ambiguous(c.len()),
        Match::None => Lookup::NoMatch,
    })
}

/// [`lookup`], keeping only a confident match with art.
pub fn find(src: &Sources, name: &str) -> Result<Option<Image>> {
    Ok(match lookup(src, name)? {
        Lookup::Found(i) => Some(i),
        _ => None,
    })
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
        let g = db.get_game(game_id)?;
        if !force && g.status != GameStatus::Installed {
            return Ok(None); // fail closed: only installed games, never a failed/NDA install
        }
        g.name
    };
    let found = lookup(src, &name)?;
    let db = lock(db);
    let current = db.get_artwork(game_id)?;
    if !force && current.is_some() {
        return Ok(None);
    }
    match found {
        Lookup::Found(img) => {
            let p = write(paths, game_id, &img.bytes)?;
            db.set_artwork(
                game_id,
                ArtSource::Auto,
                Some(img.kind),
                Some(img.steam_app),
            )?;
            Ok(Some(p))
        }
        Lookup::NoMatch => {
            if current.is_none() {
                db.set_artwork(game_id, ArtSource::None, None, None)?;
            }
            Ok(None)
        }
        Lookup::Ambiguous(n) => {
            if current.is_none() {
                db.set_artwork_candidates(game_id, n as i64)?;
            }
            Ok(None)
        }
    }
}

/// Manual pick ("Search Steam..."): downloads app `app`'s art and records it as the game's cover
/// (source `auto`, `steam_app` = the choice). Works when only the automatic switch is off (the
/// user asked for it), but not in offline mode. Replaces any current image.
pub fn pick(
    db: &Mutex<Db>,
    paths: &Paths,
    src: &Sources,
    game_id: i64,
    app: i64,
) -> Result<HostPath> {
    ensure_manual(&lock(db))?;
    let img = download(src, app)?
        .ok_or_else(|| Error::Validation(format!("Steam app {app} has no cover art")))?;
    let db = lock(db);
    let p = write(paths, game_id, &img.bytes)?;
    db.set_artwork(game_id, ArtSource::Auto, Some(img.kind), Some(app))?;
    Ok(p)
}

/// A manual search is allowed unless offline mode is on.
pub fn ensure_manual(db: &Db) -> Result<()> {
    if db.get_setting(KEY_OFFLINE)?.as_deref() == Some("1") {
        return Err(Error::Validation(
            "Offline mode is on: nothing is downloaded".into(),
        ));
    }
    Ok(())
}

/// Manual store search for the pick dialog (same checks as [`pick`]).
pub fn search_manual(db: &Mutex<Db>, src: &Sources, term: &str) -> Result<Vec<(i64, String)>> {
    ensure_manual(&lock(db))?;
    search(src, term)
}

/// Opt a game out of automatic lookups (NDA titles): records `none` unless it already has a row.
pub fn opt_out(db: &Db, game_id: i64) -> Result<()> {
    if db.get_artwork(game_id)?.is_none() {
        db.set_artwork(game_id, ArtSource::None, None, None)?;
    }
    Ok(())
}

/// [`opt_out`] for the game of an install (the wizard, before the game is confirmed).
pub fn opt_out_install(db: &Mutex<Db>, install_id: i64) -> Result<()> {
    let db = lock(db);
    let game = db.get_install(install_id)?.game_id;
    opt_out(&db, game)
}

/// Allow automatic lookups again for a game that has no image: the next library pass queries it.
pub fn allow_auto(db: &Db, game_id: i64) -> Result<()> {
    if db
        .get_artwork(game_id)?
        .is_some_and(|a| a.source == ArtSource::None)
    {
        db.delete_artwork(game_id)?;
    }
    Ok(())
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
    fn cleaning() {
        for (from, to) in [
            ("Dark Souls 3 dev build", "Dark Souls 3"),
            ("ProjectX_Beta_0.9", "ProjectX"),
            ("Game Name - Press Build v1.2.3 (Win64)", "Game Name"),
            ("Portal 2", "Portal 2"),
            ("Half-Life 2 beta", "Half-Life 2"),
            ("Cyberpunk 2077", "Cyberpunk 2077"),
            ("Cyberpunk 2077 v2.1", "Cyberpunk 2077"),
            ("Hades II demo", "Hades II"),
            ("Hades_II_Closed_Beta_build_1234", "Hades II"),
            ("Foo.Bar.Release.Candidate.r1234", "Foo Bar"),
            ("Foo Development Build b12345 x64", "Foo"),
            ("Foo (Early Access) [Nightly 2024]", "Foo"),
            ("Foo (2019)", "Foo 2019"),
            ("Foo - The Sequel", "Foo - The Sequel"),
            ("Foo-Beta", "Foo"),
            ("Foo version 1.2 Setup", "Foo"),
            ("setup_foo_installer", "foo"),
            ("Alpha Protocol", "Alpha Protocol"), // first word is real
            ("PC Building Simulator", "PC Building Simulator"),
            ("Foo Internal Playtest Debug Shipping", "Foo"),
            ("Foo RC", "Foo"),
            ("Dark Souls III 4", "Dark Souls III 4"), // plain numbers stay
            ("Mr. Foo", "Mr Foo"),
            ("", ""),
        ] {
            assert_eq!(clean_query(from), to, "{from}");
            assert_eq!(clean_query(to), to, "idempotent: {to}");
        }
    }

    #[test]
    fn numerals_and_symbols() {
        assert_eq!(key("DARK SOULS™ III"), key("Dark Souls 3"));
        assert_eq!(key("Hades II"), key("Hades 2"));
        assert_eq!(key("Final Fantasy XX"), "final fantasy 20");
        assert_ne!(key("Dark Souls XXI"), key("Dark Souls 21")); // beyond XX: untouched
        assert_eq!(key("I Am Bread"), "i am bread"); // first token is never converted
        assert_eq!(key("Ratchet & Clank"), key("Ratchet and Clank"));
        assert_eq!(key("The Witcher® 3"), key("Witcher III"));
        assert_ne!(key("Dark Souls 3"), key("Dark Souls 2"));
    }

    #[test]
    fn classification_and_base_game_preference() {
        let r = items(&[
            (1, "DARK SOULS™ III - The Ringed City"),
            (2, "DARK SOULS III Soundtrack"),
            (374320, "DARK SOULS™ III"),
        ]);
        assert_eq!(classify("Dark Souls 3", &r), Match::Confident(374320));
        // extras first in the store order never win
        let e = items(&[(9, "Portal 2 - Soundtrack"), (620, "Portal 2")]);
        assert_eq!(best_match("Portal 2", &e), Some(620));
        // only extras found: candidates, never attached
        let only = items(&[(9, "Portal 2 Soundtrack"), (8, "Portal 2 Demo")]);
        assert!(matches!(classify("Portal 2", &only), Match::Candidates(c) if c.len() == 2));
        // ambiguous: candidates, base games only
        let a = items(&[(1, "Portal"), (2, "Portal 2"), (3, "Portal Soundtrack")]);
        assert!(matches!(classify("Portal 3", &a), Match::Candidates(c) if c.len() == 2));
        assert_eq!(classify("Zzz", &[]), Match::None);
        // a query that is itself a soundtrack may match one
        assert_eq!(
            best_match("Portal 2 Soundtrack", &items(&[(9, "Portal 2 Soundtrack")])),
            Some(9)
        );
    }

    #[test]
    fn app_ids() {
        for (i, id) in [
            ("620", Some(620)),
            (" 374320 ", Some(374320)),
            (
                "https://store.steampowered.com/app/374320/DARK_SOULS_III/",
                Some(374320),
            ),
            ("store.steampowered.com/app/620", Some(620)),
            ("https://store.steampowered.com/app/620?l=german", Some(620)),
            ("https://evil.example/app/620", None),
            ("https://store.steampowered.com/sub/620", None),
            ("0", None),
            ("dark souls", None),
        ] {
            assert_eq!(parse_app_id(i), id, "{i}");
        }
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
