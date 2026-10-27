//! Render tests: golden images for every fixture, readback at odd widths,
//! and renderer budgets.
//!
//! GPU availability: without an adapter these tests print `SKIPPED` and
//! pass, unless `RIGLET_REQUIRE_GPU=1` (set on CI runners with a software
//! adapter installed), where a missing adapter fails the test.
//!
//! Goldens: `tests/goldens/<fixture>.png`, generated on the reference
//! setup (lavapipe, Linux x86_64; see CLAUDE.md) with
//! `RIGLET_UPDATE_GOLDENS=1`. Everywhere else they are compared with a
//! per-channel tolerance plus a maximum number of differing pixels.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::path::PathBuf;

use riglet_fixtures::fixtures;
use riglet_format::{EvalOutput, Limits, ParamValues, Rig};
use riglet_render::{Budgets, FrameOptions, Image, RenderError, Renderer, RendererOptions};

/// Largest per-channel difference that still counts as equal.
const CHANNEL_TOLERANCE: u8 = 4;
/// Pixels allowed to differ by more than the tolerance (edge rounding
/// between rasterizers), as a fraction of all pixels.
const MAX_DIFFERING_FRACTION: f64 = 0.005;

fn renderer() -> Option<Renderer> {
    match Renderer::new(RendererOptions::default()) {
        Ok(r) => {
            let a = r.adapter();
            eprintln!("render tests: adapter `{}` ({}, {}, {})", a.name, a.backend, a.device_type, a.driver);
            Some(r)
        }
        Err(e) => {
            if std::env::var("RIGLET_REQUIRE_GPU").as_deref() == Ok("1") {
                panic!("RIGLET_REQUIRE_GPU=1 but no adapter: {e}");
            }
            eprintln!("render tests: SKIPPED, no GPU adapter ({e})");
            None
        }
    }
}

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens")
}

fn failures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("render-failures")
}

fn render_fixture(r: &Renderer, f: &fixtures::Fixture) -> Image {
    let doc = f.builder.document();
    let rig = Rig::prepare(&doc, &Limits::default()).unwrap_or_else(|i| panic!("{}: {i:?}", f.name));
    let mut params = ParamValues::defaults(&rig);
    for (id, v) in &f.params {
        assert!(params.set(&rig, id, *v));
    }
    let mut out = EvalOutput::new();
    rig.evaluate(&params, &mut out);
    riglet_render::render_document(r, &doc, &rig, &out, &FrameOptions::default()).unwrap()
}

/// `None` when equal within tolerance, else a description.
fn compare(actual: &Image, golden: &Image) -> Option<String> {
    if (actual.width, actual.height) != (golden.width, golden.height) {
        return Some(format!("size {}x{} vs golden {}x{}", actual.width, actual.height, golden.width, golden.height));
    }
    let mut differing = 0usize;
    let mut worst = 0u8;
    for (a, g) in actual.rgba.as_chunks::<4>().0.iter().zip(golden.rgba.as_chunks::<4>().0) {
        let d = a.iter().zip(g).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        worst = worst.max(d);
        if d > CHANNEL_TOLERANCE {
            differing += 1;
        }
    }
    let pixels = (actual.width * actual.height) as usize;
    let allowed = (pixels as f64 * MAX_DIFFERING_FRACTION).floor() as usize;
    (differing > allowed).then(|| {
        format!("{differing} pixels differ by more than {CHANNEL_TOLERANCE} (allowed {allowed}, worst {worst})")
    })
}

#[test]
fn goldens() {
    let Some(r) = renderer() else { return };
    let update = std::env::var("RIGLET_UPDATE_GOLDENS").as_deref() == Ok("1");
    let mut failures = Vec::new();
    for f in fixtures::all() {
        let image = render_fixture(&r, &f);
        let path = goldens_dir().join(format!("{}.png", f.name));
        if update {
            std::fs::create_dir_all(goldens_dir()).unwrap();
            std::fs::write(&path, image.to_png().unwrap()).unwrap();
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            failures.push(format!("{}: no golden at {}", f.name, path.display()));
            continue;
        };
        let golden = Image::from_png(&bytes).unwrap();
        if let Some(why) = compare(&image, &golden) {
            std::fs::create_dir_all(failures_dir()).unwrap();
            let actual = failures_dir().join(format!("{}.actual.png", f.name));
            std::fs::write(&actual, image.to_png().unwrap()).unwrap();
            failures.push(format!("{}: {why} (actual written to {})", f.name, actual.display()));
        }
    }
    assert!(failures.is_empty(), "golden mismatches on `{}`:\n{}", r.adapter().name, failures.join("\n"));
}

/// A full-canvas opaque quad must read back exactly at any width, including
/// widths whose rows are not a multiple of 256 bytes.
#[test]
fn readback_handles_odd_widths() {
    let Some(r) = renderer() else { return };
    let mut b = riglet_fixtures::Builder::new();
    b.model.canvas = riglet_format::model::Canvas { width: 1.0, height: 1.0 };
    b.texture("c", 1, 1, &[10, 200, 30, 255], false);
    b.quad("q", "root", "c", [-0.5, -0.5], [0.5, 0.5]);
    let doc = b.document();
    let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
    let mut out = EvalOutput::new();
    rig.evaluate(&ParamValues::defaults(&rig), &mut out);
    let model = r.upload(&doc, &rig).unwrap();
    for width in [1u32, 3, 63, 64, 65, 127, 257] {
        let options = FrameOptions { scale: width as f32, background: None };
        let image = r.render(&model, &rig, (1.0, 1.0), &out, &options).unwrap();
        assert_eq!((image.width, image.height), (width, width));
        assert!(image.rgba.as_chunks::<4>().0.iter().all(|p| *p == [10, 200, 30, 255]), "width {width}: wrong pixels");
    }
}

fn nested_composites(depth: usize) -> riglet_fixtures::Builder {
    let mut b = riglet_fixtures::Builder::new();
    let mut parent = "root".to_owned();
    for i in 0..depth {
        let id = format!("c{i}");
        b.composite(&id, &parent, 1.0, 0.0);
        parent = id;
    }
    b.quad("q", &parent, "tex", [-1.0, -1.0], [1.0, 1.0]);
    b
}

#[test]
fn offscreen_budget_math() {
    // Max depth at max size needs 17 full-size targets: far over 1 GiB.
    let needed = Renderer::offscreen_bytes(8192, 8192, 16, false);
    assert_eq!(needed, 17 * 8192 * 8192 * 4);
    assert!(needed > Budgets::default().max_offscreen_bytes);
    assert_eq!(Renderer::offscreen_bytes(100, 50, 0, true), 3 * 100 * 50 * 4);
}

#[test]
fn budgets_fail_cleanly_before_allocating() {
    let Some(r) = renderer() else { return };
    let render = |b: &riglet_fixtures::Builder, scale: f32| {
        let doc = b.document();
        let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
        let mut out = EvalOutput::new();
        rig.evaluate(&ParamValues::defaults(&rig), &mut out);
        let canvas = (doc.model.canvas.width, doc.model.canvas.height);
        let model = r.upload(&doc, &rig)?;
        r.render(&model, &rig, canvas, &out, &FrameOptions { scale, background: None })
    };

    // Max-depth composites at max output size: over the memory budget (or
    // over the adapter's size limit). Either way a clean error.
    let max = r.budgets().max_output_dimension as f32;
    let deep = nested_composites(16);
    let mut big = deep;
    big.model.canvas = riglet_format::model::Canvas { width: 1.0, height: 1.0 };
    match render(&big, max) {
        Err(RenderError::OffscreenBudget { .. } | RenderError::OutputSize { .. }) => {}
        other => panic!("expected a budget error, got {other:?}"),
    }
    // Small output: 16 levels fit.
    let small = nested_composites(16);
    assert!(render(&small, 1.0 / 16.0).is_ok());
    // 17 levels: too deep, at any size.
    assert!(matches!(render(&nested_composites(17), 1.0 / 16.0), Err(RenderError::NestingTooDeep { .. })));

    // Output size and scale come from untrusted arguments.
    let tiny = nested_composites(0);
    for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert!(matches!(render(&tiny, scale), Err(RenderError::BadScale)), "scale {scale}");
    }
    assert!(matches!(render(&tiny, 1e6), Err(RenderError::OutputSize { .. })));
    assert!(matches!(render(&tiny, 1e-6), Err(RenderError::OutputSize { .. })));
}
