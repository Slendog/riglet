//! The editor's viewport target holds exactly the pixels `riglet render`
//! writes for the same model, parameter values and size (design
//! `docs/design/m4-editor.md`, "Color check"). What egui does with the
//! texture afterwards is covered by the manual check with
//! `tests/fixtures/gray-ramp.riglet`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

use riglet_editor::{EditorState, Viewport, load};
use riglet_fixtures::{Builder, fixtures};
use riglet_format::Limits;
use riglet_render::{Image, Renderer};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("viewport").join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn renderer() -> Option<Renderer> {
    match Renderer::new(Default::default()) {
        Ok(r) => Some(r),
        Err(e) => {
            if std::env::var("RIGLET_REQUIRE_GPU").as_deref() == Ok("1") {
                panic!("RIGLET_REQUIRE_GPU=1 but no adapter: {e}");
            }
            eprintln!("cli render tests: SKIPPED, no GPU adapter ({e})");
            None
        }
    }
}

/// `riglet render` output for `file`, decoded.
fn cli_render(file: &Path, dir: &Path, scale: f32, params: &[(&str, [f32; 2])], is_2d: &[&str]) -> Image {
    let png = dir.join("cli.png");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_riglet"));
    cmd.arg("render").arg(file).arg("-o").arg(&png).arg("--scale").arg(scale.to_string());
    for (id, v) in params {
        let value = if is_2d.contains(id) { format!("{},{}", v[0], v[1]) } else { v[0].to_string() };
        cmd.arg("--param").arg(format!("{id}={value}"));
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "riglet render failed: {}", String::from_utf8_lossy(&out.stderr));
    Image::from_png(&std::fs::read(&png).unwrap()).unwrap()
}

/// The editor path: load, set the values, zoom = scale, no pan, render
/// into the viewport target, read it back (tests only).
fn viewport_render(r: Renderer, file: &Path, scale: f32, params: &[(&str, [f32; 2])], size: (u32, u32)) -> Image {
    let mut state = EditorState::open(load(file, &Limits::default()).unwrap(), Limits::default());
    for (id, v) in params {
        assert!(state.set_param(id, *v), "{id}");
    }
    state.view.zoom = scale;
    let mut viewport = Viewport::new(r);
    let target = viewport.render(&mut state, size.0, size.1).unwrap().clone();
    viewport.renderer().read_target(&target, None).unwrap()
}

fn check(name: &str, b: &Builder, scale: f32, params: &[(&str, [f32; 2])]) {
    let Some(r) = renderer() else { return };
    let dir = scratch(name);
    let file = dir.join("m.riglet");
    std::fs::write(&file, riglet_format::write_package_bytes(&b.document()).unwrap()).unwrap();
    let is_2d: Vec<&str> = b
        .model
        .parameters
        .iter()
        .filter(|p| p.kind == riglet_format::model::ParamKind::TwoD)
        .map(|p| p.id.as_str())
        .collect();
    let want = cli_render(&file, &dir, scale, params, &is_2d);
    let got = viewport_render(r, &file, scale, params, (want.width, want.height));
    assert_eq!((got.width, got.height), (want.width, want.height));
    assert!(got.rgba == want.rgba, "{name}: the viewport target differs from `riglet render`");
}

#[test]
fn viewport_target_equals_cli_render_for_the_gray_ramp() {
    check("gray_ramp", &fixtures::gray_ramp(), 1.0, &[]);
    check("gray_ramp_2x", &fixtures::gray_ramp(), 2.0, &[]);
}

/// Every render fixture (blend modes, masks, composites) at its golden
/// parameter values, at scale 2.
#[test]
fn viewport_target_equals_cli_render_for_every_fixture() {
    for f in fixtures::all() {
        check(f.name, &f.builder, 2.0, &f.params);
    }
}

/// `tests/fixtures/gray-ramp.riglet` is `fixtures::gray_ramp()`.
/// `RIGLET_WRITE_FIXTURES=1` rewrites it.
#[test]
fn committed_gray_ramp_is_current() {
    let path = repo().join("tests/fixtures/gray-ramp.riglet");
    let bytes = riglet_format::write_package_bytes(&fixtures::gray_ramp().document()).unwrap();
    if std::env::var("RIGLET_WRITE_FIXTURES").as_deref() == Ok("1") {
        std::fs::write(&path, &bytes).unwrap();
    }
    let committed = std::fs::read(&path).expect("tests/fixtures/gray-ramp.riglet is missing");
    assert!(committed == bytes, "tests/fixtures/gray-ramp.riglet is stale; rerun with RIGLET_WRITE_FIXTURES=1");
}
