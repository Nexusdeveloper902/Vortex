//! Cover art: the cached image when the game has one (`vortex_core::artwork`), otherwise a
//! placeholder with the game's initials on a deterministic color. Fully offline.

use crate::app::blocking;
use gtk4::prelude::*;
use gtk4::{gdk, gdk_pixbuf, gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const COLORS: usize = 8;

/// Decoded size bound: enough for the 150x225 tile and 120x180 hero at scale 2+, ~0.8 MB each.
const DECODE_W: i32 = 360;
const DECODE_H: i32 = 540;

/// First letters of the first two words, uppercase.
pub fn initials(name: &str) -> String {
    let s: String = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if s.is_empty() { "?".into() } else { s }
}

/// Stable color slot for a name (FNV-1a).
pub fn color_index(name: &str) -> usize {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.to_lowercase().bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    h as usize % COLORS
}

struct Slot {
    id: i64,
    widget: glib::WeakRef<gtk4::Overlay>,
}

thread_local! {
    /// Live covers, so new artwork shows everywhere (library and game page) at once.
    static SLOTS: RefCell<Vec<Slot>> = const { RefCell::new(Vec::new()) };
    /// Decoded textures by game id.
    static TEXTURES: RefCell<HashMap<i64, gdk::Texture>> = RefCell::new(HashMap::new());
    /// Bumped on every artwork change of a game, so stale decodes are dropped.
    static GENERATION: RefCell<HashMap<i64, u64>> = RefCell::new(HashMap::new());
}

fn generation(id: i64) -> u64 {
    GENERATION.with_borrow(|g| g.get(&id).copied().unwrap_or(0))
}

/// Off the GTK thread: decode scaled down. Content-sniffed, so the cache file name is irrelevant.
fn decode(path: &Path) -> Option<gdk::Texture> {
    let bytes = glib::Bytes::from_owned(std::fs::read(path).ok()?);
    let stream = gio::MemoryInputStream::from_bytes(&bytes);
    let pb = gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        DECODE_W,
        DECODE_H,
        true,
        None::<&gio::Cancellable>,
    )
    .ok()?;
    let format = if pb.has_alpha() {
        gdk::MemoryFormat::R8g8b8a8
    } else {
        gdk::MemoryFormat::R8g8b8
    };
    let tex = gdk::MemoryTexture::new(
        pb.width(),
        pb.height(),
        format,
        &pb.read_pixel_bytes(),
        pb.rowstride() as usize,
    );
    Some(tex.upcast())
}

fn show_picture(o: &gtk4::Overlay, tex: &gdk::Texture) {
    let p = gtk4::Picture::for_paintable(tex);
    p.set_content_fit(gtk4::ContentFit::Cover);
    p.set_can_shrink(true);
    o.add_overlay(&p);
}

/// Shows `art` on `o` (on top of the placeholder), or only the placeholder.
fn render(o: &gtk4::Overlay, id: i64, art: Option<&Path>) {
    // the placeholder is the main child; overlays are pictures
    let mut c = o.first_child();
    while let Some(w) = c {
        c = w.next_sibling();
        if w.is::<gtk4::Picture>() {
            o.remove_overlay(&w);
        }
    }
    let Some(path) = art.map(Path::to_path_buf) else {
        return;
    };
    if let Some(t) = TEXTURES.with_borrow(|t| t.get(&id).cloned()) {
        return show_picture(o, &t);
    }
    let (weak, gen_at_start) = (o.downgrade(), generation(id));
    glib::spawn_future_local(async move {
        let Some(Some(tex)) = blocking(move || decode(&path)).await else {
            return;
        };
        if generation(id) != gen_at_start {
            return; // changed meanwhile; a newer render is under way
        }
        TEXTURES.with_borrow_mut(|t| t.insert(id, tex.clone()));
        if let Some(o) = weak.upgrade() {
            show_picture(&o, &tex);
        }
    });
}

/// A rounded `w` x `h` cover for game `id`. `art` is the cached image, if any.
/// Overlay children do not take part in sizing, so the picture never changes the cover's size.
pub fn cover(id: i64, name: &str, art: Option<&Path>, w: i32, h: i32) -> gtk4::Overlay {
    let b = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    b.set_size_request(w, h);
    b.add_css_class(&format!("cover-{}", color_index(name)));
    let l = gtk4::Label::builder()
        .label(initials(name))
        .valign(gtk4::Align::Center)
        .vexpand(true)
        .css_classes([if w.min(h) >= 100 {
            "cover-big"
        } else {
            "cover-small"
        }])
        .build();
    b.append(&l);
    let o = gtk4::Overlay::builder()
        .child(&b)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .overflow(gtk4::Overflow::Hidden)
        .css_classes(["cover"])
        .build();
    render(&o, id, art);
    SLOTS.with_borrow_mut(|s| {
        s.retain(|x| x.widget.upgrade().is_some());
        s.push(Slot {
            id,
            widget: o.downgrade(),
        });
    });
    o
}

/// The artwork of game `id` changed (new, replaced or removed): redraw every live cover of it.
pub fn update(id: i64, art: Option<PathBuf>) {
    GENERATION.with_borrow_mut(|g| *g.entry(id).or_insert(0) += 1);
    TEXTURES.with_borrow_mut(|t| t.remove(&id));
    let live: Vec<gtk4::Overlay> = SLOTS.with_borrow(|s| {
        s.iter()
            .filter(|x| x.id == id)
            .filter_map(|x| x.widget.upgrade())
            .collect()
    });
    for o in live {
        render(&o, id, art.as_deref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_and_color() {
        assert_eq!(initials("half-life 2"), "HL");
        assert_eq!(initials("Portal"), "P");
        assert_eq!(initials("  "), "?");
        assert_eq!(color_index("Portal"), color_index("portal"));
        assert!(color_index("anything") < COLORS);
    }

    #[test]
    fn decode_sniffs_content_and_scales_down() {
        let pb =
            gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 1200, 1800).unwrap();
        pb.fill(0x3584e4ff);
        let dir = std::env::temp_dir().join(format!("vortex-cover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for ty in ["jpeg", "png"] {
            let f = dir.join("7.img");
            std::fs::write(&f, pb.save_to_bufferv(ty, &[]).unwrap()).unwrap();
            let t = decode(&f).unwrap();
            assert_eq!((t.width(), t.height()), (DECODE_W, DECODE_H), "{ty}");
        }
        std::fs::write(dir.join("bad.img"), b"<html>").unwrap();
        assert!(decode(&dir.join("bad.img")).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
