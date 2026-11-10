//! Placeholder cover art: the game's initials on a deterministic color. No artwork, fully offline.

use gtk4::prelude::*;

pub const COLORS: usize = 8;

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

/// A rounded cover of `w` x `h` pixels; the label scales with the smaller side.
pub fn cover(name: &str, w: i32, h: i32) -> gtk4::Box {
    let b = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    b.set_size_request(w, h);
    b.set_halign(gtk4::Align::Center);
    b.add_css_class("cover");
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
    b
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
}
