//! Artwork lookup against a localhost HTTP server. No real network (except the `#[ignore]` test).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use vortex_core::artwork::{self, Sources};
use vortex_core::db::{ArtKind, ArtSource, Db, GameStatus, NewGame};
use vortex_core::error::Error;
use vortex_core::paths::{HostPath, Paths};

const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4];
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nxxxx";

type Route = (u16, &'static str, Vec<u8>);

struct Server {
    src: Sources,
    hits: Arc<AtomicUsize>,
    paths_seen: Arc<Mutex<Vec<String>>>,
}

/// Serves `route(path)`; unknown paths get 404. Counts every request.
fn serve(route: impl Fn(&str) -> Option<Route> + Send + 'static) -> Server {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (h, s2) = (hits.clone(), seen.clone());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            h.fetch_add(1, Ordering::SeqCst);
            let mut line = String::new();
            let mut r = BufReader::new(&s);
            r.read_line(&mut line).unwrap();
            let mut hdr = String::new();
            while r.read_line(&mut hdr).is_ok_and(|n| n > 2) {
                hdr.clear();
            }
            let path = line.split(' ').nth(1).unwrap_or("").to_string();
            s2.lock().unwrap().push(path.clone());
            let (st, ct, body) = route(&path).unwrap_or((404, "text/html", b"nope".to_vec()));
            let mut s = &s;
            let _ = write!(
                s,
                "HTTP/1.1 {st} X\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(&body);
        }
    });
    Server {
        src: Sources {
            search: format!("{base}/search"),
            cdn: format!("{base}/apps"),
        },
        hits,
        paths_seen: seen,
    }
}

fn search(json: &'static str) -> Route {
    (200, "application/json", json.as_bytes().to_vec())
}

const PORTAL: &str = r#"{"total":2,"items":[{"type":"app","name":"Portal 2","id":620},{"type":"app","name":"Portal 2 Soundtrack","id":9}]}"#;

/// `installed`: the only status background lookups consider.
fn game(name: &str, env: i64) -> NewGame {
    let mut g = NewGame::new(name, env);
    g.status = GameStatus::Installed;
    g.exe_path = Some("/g/game.exe".into());
    g
}

fn setup(name: &str) -> (tempfile::TempDir, Paths, Arc<Mutex<Db>>, i64) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path());
    let db = Db::open(&paths.db_file()).unwrap();
    let env = db.insert_environment("/p/1", true, "/rt").unwrap();
    let id = db.insert_game(&game(name, env)).unwrap();
    (tmp, paths, Arc::new(Mutex::new(db)), id)
}

fn art(db: &Mutex<Db>, id: i64) -> Option<(ArtSource, Option<ArtKind>)> {
    db.lock()
        .unwrap()
        .get_artwork(id)
        .unwrap()
        .map(|a| (a.source, a.kind))
}

#[test]
fn match_downloads_portrait() {
    let s = serve(|p| match p {
        _ if p.starts_with("/search?term=Portal%202&") => Some(search(PORTAL)),
        "/apps/620/library_600x900_2x.jpg" => Some((200, "image/jpeg", JPEG.to_vec())),
        _ => None,
    });
    let (_t, paths, db, id) = setup("Portal 2");
    let p = artwork::fetch(&db, &paths, &s.src, id, false)
        .unwrap()
        .unwrap();
    assert_eq!(p.as_path(), paths.artwork_file(id));
    assert_eq!(std::fs::read(p.as_path()).unwrap(), JPEG);
    assert_eq!(
        art(&db, id),
        Some((ArtSource::Auto, Some(ArtKind::Portrait)))
    );
    let row = db.lock().unwrap().get_artwork(id).unwrap().unwrap();
    assert_eq!(row.steam_app, Some(620));
    // already looked up: a background pass makes no request
    let n = s.hits.load(Ordering::SeqCst);
    assert_eq!(
        artwork::fetch(&db, &paths, &s.src, id, false).unwrap(),
        None
    );
    assert_eq!(s.hits.load(Ordering::SeqCst), n);
    // no temp files left in the cache dir
    let left: Vec<_> = std::fs::read_dir(paths.artwork_dir()).unwrap().collect();
    assert_eq!(left.len(), 1);
}

#[test]
fn portrait_404_falls_back_to_header() {
    let s = serve(|p| match p {
        _ if p.starts_with("/search?") => Some(search(PORTAL)),
        "/apps/620/header.jpg" => Some((200, "image/jpeg", JPEG.to_vec())),
        _ => None,
    });
    let (_t, paths, db, id) = setup("Portal 2");
    assert!(
        artwork::fetch(&db, &paths, &s.src, id, false)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        art(&db, id),
        Some((ArtSource::Auto, Some(ArtKind::Landscape)))
    );
    let seen = s.paths_seen.lock().unwrap().clone();
    assert_eq!(
        &seen[1..],
        [
            "/apps/620/library_600x900_2x.jpg",
            "/apps/620/library_600x900.jpg",
            "/apps/620/header.jpg"
        ]
    );
}

#[test]
fn no_match_records_none_and_is_not_retried() {
    let s = serve(|p| p.starts_with("/search?").then(|| search(PORTAL)));
    let (_t, paths, db, id) = setup("Portal 3");
    assert_eq!(
        artwork::fetch(&db, &paths, &s.src, id, false).unwrap(),
        None
    );
    assert_eq!(art(&db, id), Some((ArtSource::None, None)));
    assert_eq!(s.hits.load(Ordering::SeqCst), 1); // search only, no image request
    assert!(
        db.lock()
            .unwrap()
            .games_without_artwork()
            .unwrap()
            .is_empty()
    );
    assert!(!paths.artwork_file(id).exists());
}

#[test]
fn non_image_is_rejected_and_nothing_recorded() {
    // 200 text/html for every image, and a 200 "image/jpeg" whose bytes are HTML
    for (ct, body) in [
        ("text/html", b"<html>".to_vec()),
        ("image/jpeg", b"<html>".to_vec()),
    ] {
        let s = serve(move |p| {
            if p.starts_with("/search?") {
                Some(search(PORTAL))
            } else {
                Some((200, ct, body.clone()))
            }
        });
        let (_t, paths, db, id) = setup("Portal 2");
        let r = artwork::fetch(&db, &paths, &s.src, id, false);
        assert!(
            matches!(&r, Err(Error::Download(m)) if m.contains("image")),
            "{r:?}"
        );
        assert_eq!(art(&db, id), None, "transient failure: retried next time");
        assert!(!paths.artwork_file(id).exists());
    }
}

#[test]
fn oversize_is_rejected() {
    let s = serve(|p| {
        if p.starts_with("/search?") {
            Some(search(PORTAL))
        } else {
            let mut big = JPEG.to_vec();
            big.resize(6 * 1024 * 1024, 0);
            Some((200, "image/jpeg", big))
        }
    });
    let (_t, paths, db, id) = setup("Portal 2");
    let r = artwork::fetch(&db, &paths, &s.src, id, false);
    assert!(
        matches!(&r, Err(Error::Download(m)) if m.contains("larger")),
        "{r:?}"
    );
    assert_eq!(art(&db, id), None);
    assert!(!paths.artwork_file(id).exists());
}

#[test]
fn turned_off_makes_zero_requests() {
    let s = serve(|_| Some(search(PORTAL)));
    for (key, val) in [(artwork::KEY_AUTO, "0"), (artwork::KEY_OFFLINE, "1")] {
        let (_t, paths, db, id) = setup("Portal 2");
        db.lock().unwrap().set_setting(key, val).unwrap();
        let rx = artwork::spawn_auto(db.clone(), paths.clone(), s.src.clone(), vec![id]);
        assert!(rx.recv_blocking().is_err(), "closed at once");
        assert!(matches!(
            artwork::fetch(&db, &paths, &s.src, id, true),
            Err(Error::Validation(_))
        ));
        assert_eq!(art(&db, id), None);
    }
    assert_eq!(s.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn spawn_auto_sends_updates_for_unlooked_games() {
    let s = serve(|p| match p {
        _ if p.starts_with("/search?term=Portal%202&") => Some(search(PORTAL)),
        _ if p.starts_with("/search?") => Some(search(r#"{"total":0,"items":[]}"#)),
        "/apps/620/library_600x900_2x.jpg" => Some((200, "image/png", PNG.to_vec())),
        _ => None,
    });
    let (_t, paths, db, a) = setup("Portal 2");
    let b = {
        let d = db.lock().unwrap();
        d.insert_game(&game("Nothing Like It", 1)).unwrap()
    };
    let ids = db.lock().unwrap().games_without_artwork().unwrap();
    assert_eq!(ids, [a, b]);
    let rx = artwork::spawn_auto(db.clone(), paths.clone(), s.src.clone(), ids);
    let ev = rx.recv_blocking().unwrap();
    assert_eq!(ev.game_id, a);
    assert!(rx.recv_blocking().is_err()); // b had no match: no event, channel closes
    assert_eq!(art(&db, b), Some((ArtSource::None, None)));
}

#[test]
fn custom_image_set_and_remove() {
    let s = serve(|p| match p {
        _ if p.starts_with("/search?") => Some(search(PORTAL)),
        "/apps/620/library_600x900_2x.jpg" => Some((200, "image/jpeg", JPEG.to_vec())),
        _ => None,
    });
    let (t, paths, db, id) = setup("Portal 2");
    let pick = t.path().join("mine.png");
    std::fs::write(&pick, PNG).unwrap();
    let bad = t.path().join("notes.txt");
    std::fs::write(&bad, "hello").unwrap();
    {
        let d = db.lock().unwrap();
        assert!(matches!(
            artwork::set_custom(&d, &paths, id, &HostPath::new(&bad).unwrap()),
            Err(Error::Validation(_))
        ));
        assert_eq!(artwork::path(&d, &paths, id).unwrap(), None);
        let p = artwork::set_custom(&d, &paths, id, &HostPath::new(&pick).unwrap()).unwrap();
        assert_eq!(std::fs::read(p.as_path()).unwrap(), PNG);
        assert_eq!(artwork::path(&d, &paths, id).unwrap(), Some(p));
    }
    assert_eq!(art(&db, id), Some((ArtSource::Custom, None)));
    // a background pass never overwrites the custom image
    assert_eq!(
        artwork::fetch(&db, &paths, &s.src, id, false).unwrap(),
        None
    );
    assert_eq!(std::fs::read(paths.artwork_file(id)).unwrap(), PNG);
    assert_eq!(s.hits.load(Ordering::SeqCst), 0);

    // remove deletes only the cache file; the picked source stays
    artwork::remove(&db.lock().unwrap(), &paths, id).unwrap();
    assert!(!paths.artwork_file(id).exists());
    assert!(pick.exists());
    assert_eq!(art(&db, id), Some((ArtSource::None, None)));
    assert_eq!(
        artwork::path(&db.lock().unwrap(), &paths, id).unwrap(),
        None
    );

    // explicit refresh fetches again even after removal
    assert!(
        artwork::fetch(&db, &paths, &s.src, id, true)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        art(&db, id),
        Some((ArtSource::Auto, Some(ArtKind::Portrait)))
    );
}

#[test]
fn refresh_without_match_keeps_current_image() {
    let s = serve(|p| p.starts_with("/search?").then(|| search(PORTAL)));
    let (t, paths, db, id) = setup("Something Else");
    let pick = t.path().join("mine.jpg");
    std::fs::write(&pick, JPEG).unwrap();
    artwork::set_custom(
        &db.lock().unwrap(),
        &paths,
        id,
        &HostPath::new(&pick).unwrap(),
    )
    .unwrap();
    assert_eq!(artwork::fetch(&db, &paths, &s.src, id, true).unwrap(), None);
    assert_eq!(art(&db, id), Some((ArtSource::Custom, None)));
    assert!(paths.artwork_file(id).exists());
}

#[test]
fn deleting_the_game_removes_its_cover() {
    let (t, paths, db, id) = setup("G");
    let pick = t.path().join("mine.jpg");
    std::fs::write(&pick, JPEG).unwrap();
    let d = db.lock().unwrap();
    artwork::set_custom(&d, &paths, id, &HostPath::new(&pick).unwrap()).unwrap();
    let plan = vortex_core::delete::plan_game_removal(&d, &paths, id).unwrap();
    vortex_core::delete::execute(&d, &paths, &plan).unwrap();
    assert!(!paths.artwork_file(id).exists());
    assert!(pick.exists());
}

#[test]
fn v1_database_migrates() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path());
    let id = {
        let db = Db::open(&paths.db_file()).unwrap();
        let env = db.insert_environment("/p/1", true, "/rt").unwrap();
        db.insert_game(&game("Old", env)).unwrap()
    };
    // Simulate a v1 file: drop the v2 table and version.
    let c = rusqlite_conn(&paths);
    c.execute_batch("DROP TABLE artwork; ALTER TABLE installs DROP COLUMN installer_args; PRAGMA user_version=1;")
        .unwrap();
    drop(c);
    let db = Db::open(&paths.db_file()).unwrap();
    assert_eq!(db.user_version().unwrap(), 4);
    assert_eq!(db.games_without_artwork().unwrap(), [id]);
    let mut bak = paths.db_file().into_os_string();
    bak.push(".bak-v1");
    assert!(std::path::Path::new(&bak).exists());
}

fn rusqlite_conn(paths: &Paths) -> rusqlite::Connection {
    rusqlite::Connection::open(paths.db_file()).unwrap()
}

#[test]
#[ignore = "network: queries Steam's store search and CDN"]
fn real_steam_lookup() {
    let img = artwork::find(&Sources::default(), "Portal 2")
        .unwrap()
        .unwrap();
    assert_eq!(img.steam_app, 620);
    assert_eq!(img.kind, ArtKind::Portrait);
    assert!(artwork::is_image(&img.bytes));
}

const DS3: &str = r#"{"total":3,"items":[
 {"type":"app","name":"DARK SOULS™ III Soundtrack","id":9},
 {"type":"app","name":"DARK SOULS™ III - The Ringed City","id":8},
 {"type":"app","name":"DARK SOULS™ III","id":374320}]}"#;

#[test]
fn dirty_name_is_cleaned_and_matches_roman_numerals() {
    let s = serve(|p| match p {
        _ if p.starts_with("/search?term=Dark%20Souls%203&") => Some(search(DS3)),
        "/apps/374320/library_600x900_2x.jpg" => Some((200, "image/jpeg", JPEG.to_vec())),
        _ => None,
    });
    let (_t, paths, db, id) = setup("Dark Souls 3 dev build");
    assert!(
        artwork::fetch(&db, &paths, &s.src, id, false)
            .unwrap()
            .is_some()
    );
    let row = db.lock().unwrap().get_artwork(id).unwrap().unwrap();
    assert_eq!((row.source, row.steam_app), (ArtSource::Auto, Some(374320)));
    // the dev-build wording never left the machine
    let seen = s.paths_seen.lock().unwrap().clone();
    assert!(seen[0].contains("term=Dark%20Souls%203&"), "{seen:?}");
}

const AMBIGUOUS: &str = r#"{"total":3,"items":[
 {"type":"app","name":"Project Alpha Remastered","id":1},
 {"type":"app","name":"Project Alpha 2","id":2},
 {"type":"app","name":"Project Alpha Soundtrack","id":3}]}"#;

#[test]
fn ambiguous_is_not_attached_and_records_candidates() {
    let s = serve(|p| p.starts_with("/search?").then(|| search(AMBIGUOUS)));
    let (_t, paths, db, id) = setup("Project_Alpha_Beta_0.9");
    assert_eq!(
        artwork::fetch(&db, &paths, &s.src, id, false).unwrap(),
        None
    );
    let row = db.lock().unwrap().get_artwork(id).unwrap().unwrap();
    assert_eq!(
        (row.source, row.steam_app, row.candidates),
        (ArtSource::None, None, 2)
    );
    assert_eq!(s.hits.load(Ordering::SeqCst), 1, "no image request");
    assert!(!paths.artwork_file(id).exists());
    // not queried again by background passes
    assert!(
        db.lock()
            .unwrap()
            .games_without_artwork()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn pick_by_app_id_attaches_that_app_even_when_auto_is_off() {
    let s = serve(|p| match p {
        "/apps/620/library_600x900_2x.jpg" => Some((200, "image/jpeg", JPEG.to_vec())),
        _ => None,
    });
    let (_t, paths, db, id) = setup("Whatever Internal Name");
    db.lock()
        .unwrap()
        .set_setting(artwork::KEY_AUTO, "0")
        .unwrap();
    let app = artwork::parse_app_id("https://store.steampowered.com/app/620/Portal_2/").unwrap();
    artwork::pick(&db, &paths, &s.src, id, app).unwrap();
    let row = db.lock().unwrap().get_artwork(id).unwrap().unwrap();
    assert_eq!((row.source, row.steam_app), (ArtSource::Auto, Some(620)));
    assert_eq!(std::fs::read(paths.artwork_file(id)).unwrap(), JPEG);
    // offline mode still blocks everything
    db.lock()
        .unwrap()
        .set_setting(artwork::KEY_OFFLINE, "1")
        .unwrap();
    let n = s.hits.load(Ordering::SeqCst);
    assert!(artwork::pick(&db, &paths, &s.src, id, 620).is_err());
    assert!(artwork::search_manual(&db, &s.src, "x").is_err());
    assert_eq!(s.hits.load(Ordering::SeqCst), n);
}

#[test]
fn manual_search_returns_base_games_first() {
    let s = serve(|p| {
        p.starts_with("/search?term=Dark%20Souls&")
            .then(|| search(DS3))
    });
    let (_t, _paths, db, _id) = setup("x");
    let r = artwork::search_manual(&db, &s.src, "Dark Souls").unwrap();
    assert_eq!(r[0].0, 8);
    assert_eq!(r.last().unwrap().0, 9);
}

#[test]
fn opt_out_and_installing_games_make_zero_requests() {
    let s = serve(|_| Some(search(DS3)));
    let (_t, paths, db, id) = setup("Secret Title");
    // an installing game (the wizard before confirm) is invisible to background passes
    let installing = {
        let d = db.lock().unwrap();
        d.insert_game(&NewGame::new("Under NDA", 1)).unwrap()
    };
    assert_eq!(db.lock().unwrap().games_without_artwork().unwrap(), [id]);
    assert_eq!(
        artwork::fetch(&db, &paths, &s.src, installing, false).unwrap(),
        None
    );
    // opt-out before the pass: the pass skips it
    artwork::opt_out(&db.lock().unwrap(), id).unwrap();
    assert!(
        db.lock()
            .unwrap()
            .games_without_artwork()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        artwork::fetch(&db, &paths, &s.src, id, false).unwrap(),
        None
    );
    let rx = artwork::spawn_auto(db.clone(), paths.clone(), s.src.clone(), vec![id]);
    assert!(rx.recv_blocking().is_err());
    assert_eq!(s.hits.load(Ordering::SeqCst), 0);
    // allowing again makes it eligible; opt-out never clobbers an existing row
    artwork::allow_auto(&db.lock().unwrap(), id).unwrap();
    assert_eq!(db.lock().unwrap().games_without_artwork().unwrap(), [id]);
}

#[test]
#[ignore = "network: queries Steam's store search and CDN"]
fn real_steam_dark_souls_3_dev_build() {
    let img = artwork::find(&Sources::default(), "Dark Souls 3 dev build")
        .unwrap()
        .expect("confident match");
    assert_eq!(img.steam_app, 374320);
    assert!(artwork::is_image(&img.bytes));
}

#[test]
fn failed_and_cancelled_installs_are_never_looked_up() {
    let s = serve(|_| Some(search(DS3)));
    let (_t, paths, db, _id) = setup("Installed One");
    let mut ids = vec![];
    for (name, st, optout) in [
        ("Failed NDA", GameStatus::Failed, true),
        ("Interrupted NDA", GameStatus::Interrupted, true),
        ("Failed plain", GameStatus::Failed, false),
        ("Installing plain", GameStatus::Installing, false),
    ] {
        let d = db.lock().unwrap();
        let mut g = NewGame::new(name, 1);
        g.status = st;
        let id = d.insert_game(&g).unwrap();
        if optout {
            artwork::opt_out(&d, id).unwrap();
        }
        ids.push(id);
    }
    // only the installed game is eligible
    assert_eq!(db.lock().unwrap().games_without_artwork().unwrap().len(), 1);
    for id in ids {
        assert_eq!(
            artwork::fetch(&db, &paths, &s.src, id, false).unwrap(),
            None
        );
    }
    assert_eq!(s.hits.load(Ordering::SeqCst), 0);
}
