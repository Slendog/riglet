//! Replays every texture_png fuzz crash input in
//! `tests/regressions/texture_png/` (CLAUDE.md, fuzzing policy).
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use riglet_render::image::decode_texture;

#[test]
fn texture_png_regressions() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/regressions/texture_png");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().is_some_and(|n| n != "README.md"))
        .collect();
    paths.sort();
    for path in paths {
        eprintln!("replaying {}", path.display());
        let bytes = std::fs::read(&path).unwrap();
        for premultiplied in [false, true] {
            let _ = decode_texture("regression", &bytes, premultiplied, 4096);
        }
    }
}
