//! Font discovery. Tries the system font DB first (so CJK and OS-installed
//! fonts work), falling back to a small bundled font if none is found.

use ab_glyph::FontArc;
use fontdb::{Database, Source};
use once_cell::sync::Lazy;

static DB: Lazy<Database> = Lazy::new(|| {
    let mut db = Database::new();
    db.load_system_fonts();
    db.load_fonts_dir("assets");
    db
});

pub struct FontLoaded {
    pub font: FontArc,
}

/// Load a font by family name (prefers the given family, falls back to any
/// Sans/Regular family that exists, then to a monospace, then None).
pub fn load_font<S: AsRef<str>>(family: S) -> Option<FontLoaded> {
    let want = family.as_ref().to_string();
    let mut chosen: Option<(String, Vec<u8>)> = None;

    // 1) exact family match
    for id in DB.faces() {
        if id.families.iter().any(|(f, _)| f == &want) {
            if let Some(data) = load_face_data(id) {
                chosen = Some((want.clone(), data));
                break;
            }
        }
    }
    // 2) any Sans / regular
    if chosen.is_none() {
        for id in DB.faces() {
            let fam = id
                .families
                .iter()
                .find(|(_, lang)| *lang == fontdb::Language::English_UnitedStates)
                .or_else(|| id.families.first())
                .map(|(f, _)| f.clone())
                .unwrap_or_default();
            let l = fam.to_lowercase();
            if l.contains("sans") || l.contains("regular") || l.contains("dejavu") {
                if let Some(data) = load_face_data(id) {
                    chosen = Some((fam, data));
                    break;
                }
            }
        }
    }
    // 3) monospace fallback
    if chosen.is_none() {
        for id in DB.faces() {
            let fam = id
                .families
                .iter()
                .map(|(f, _)| f.to_lowercase())
                .find(|l| l.contains("mono"))
                .unwrap_or_default();
            if !fam.is_empty() {
                if let Some(data) = load_face_data(id) {
                    chosen = Some((fam, data));
                    break;
                }
            }
        }
    }
    chosen.and_then(|(_, data)| FontArc::try_from_vec(data).ok().map(|font| FontLoaded { font }))
}

fn load_face_data(id: &fontdb::FaceInfo) -> Option<Vec<u8>> {
    match &id.source {
        Source::File(p) => std::fs::read(p).ok(),
        Source::Binary(data) => Some(data.as_ref().as_ref().to_vec()),
        Source::SharedFile(_, _) => None,
    }
}