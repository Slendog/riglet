//! Render tests: golden images for every fixture, readback at odd widths,
//! uniform alignment, and renderer budgets.
//!
//! Adapters: `RIGLET_REQUIRE_GPU=1` makes a missing adapter a failure (CI
//! sets it on every runner not on the skip allow-list in CLAUDE.md).
//! `RIGLET_EXPECT_BACKEND` (e.g. `Vulkan`, `Dx12`, `Metal`) and
//! `RIGLET_EXPECT_ADAPTER` (substring of the adapter name) make a silent
//! fallback to another backend or adapter a failure too. Without
//! `RIGLET_REQUIRE_GPU`, a missing adapter prints `SKIPPED`.
//!
//! Goldens: `tests/goldens/<fixture>.png`, from the reference setup
//! (lavapipe, Linux x86_64). Regenerate only with
//! `scripts/update-goldens.sh`. On mismatch, expected/actual/diff images
//! are written to `target/tmp/render-failures/` (a CI artifact).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::path::PathBuf;

use riglet_fixtures::fixtures;
use riglet_format::{EvalOutput, Limits, ParamValues, Rig};
use riglet_render::{Budgets, FrameOptions, Image, RenderError, Renderer, RendererOptions};

/// Golden tolerance, two tiers, on premultiplied values (see
/// `pixel_diffs`), measured across six adapters (lavapipe
/// x86_64 and arm64, WARP x86_64 and arm64, Metal on Apple silicon and on
/// the Intel paravirtual device). With texel-aligned textures every
/// fixture but one matched the lavapipe reference exactly; the remaining
/// differences were at most 2 (composite_nested). The magnified filtered
/// ramp, before it was made texel-aligned, differed by up to 24 between
/// adapters, which is why fixtures avoid testing driver filtering
/// precision beyond uv_orientation.
mod tolerance {
    /// Every pixel: no channel may differ by more than this.
    pub const HARD: u8 = 8;
    /// Most pixels: channels within this.
    pub const SOFT: u8 = 2;
    /// Pixels allowed between SOFT and HARD: this fraction of the image...
    pub const SOFT_FRACTION: f64 = 0.005;
    /// ...or this many, whichever is larger (small fixtures).
    pub const SOFT_MIN_PIXELS: usize = 4;
}

fn renderer() -> Option<Renderer> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    match Renderer::new(RendererOptions::default()) {
        Ok(r) => {
            let a = r.adapter();
            eprintln!("render tests: adapter `{}` ({}, {}, {})", a.name, a.backend, a.device_type, a.driver);
            if let Some(want) = env("RIGLET_EXPECT_BACKEND") {
                assert_eq!(a.backend, want, "expected backend {want}, got {} (`{}`)", a.backend, a.name);
            }
            if let Some(want) = env("RIGLET_EXPECT_ADAPTER") {
                let name = a.name.to_lowercase();
                assert!(name.contains(&want.to_lowercase()), "expected adapter `{want}`, got `{}`", a.name);
            }
            Some(r)
        }
        Err(e) => {
            if env("RIGLET_REQUIRE_GPU").as_deref() == Some("1") {
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

/// Per-pixel largest channel difference, compared in PREMULTIPLIED space.
/// The images are straight alpha (R8), and un-premultiplying amplifies a
/// one-step GPU rounding difference by 255 / alpha: at alpha 48 (a
/// half-transparent texture inside two composites) one premultiplied step
/// became 6 straight steps on the Intel Metal runner. Premultiplied values
/// weight color error by how visible the pixel is.
fn pixel_diffs(actual: &Image, golden: &Image) -> Vec<u8> {
    let premultiplied = |p: &[u8; 4]| {
        let pre = riglet_render::image::premultiply_channel;
        [pre(p[0], p[3]), pre(p[1], p[3]), pre(p[2], p[3]), p[3]]
    };
    actual
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(golden.rgba.as_chunks::<4>().0)
        .map(|(a, g)| {
            let (a, g) = (premultiplied(a), premultiplied(g));
            a.iter().zip(&g).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
        })
        .collect()
}

/// `None` when within both tolerance tiers, else a description.
fn compare(actual: &Image, golden: &Image) -> Option<String> {
    use tolerance::*;
    if (actual.width, actual.height) != (golden.width, golden.height) {
        return Some(format!("size {}x{} vs golden {}x{}", actual.width, actual.height, golden.width, golden.height));
    }
    let diffs = pixel_diffs(actual, golden);
    let worst = diffs.iter().copied().max().unwrap_or(0);
    let over_hard = diffs.iter().filter(|&&d| d > HARD).count();
    let over_soft = diffs.iter().filter(|&&d| d > SOFT).count();
    let allowed = ((diffs.len() as f64 * SOFT_FRACTION).floor() as usize).max(SOFT_MIN_PIXELS);
    if over_hard > 0 {
        Some(format!("{over_hard} pixels differ by more than {HARD} (worst {worst})"))
    } else if over_soft > allowed {
        Some(format!("{over_soft} pixels differ by more than {SOFT} (allowed {allowed}, worst {worst})"))
    } else {
        None
    }
}

/// Writes expected, actual and a diff heat map (black = equal, brighter
/// red = larger difference, white = over the hard limit).
fn write_failure(name: &str, actual: &Image, golden: Option<&Image>) -> PathBuf {
    let dir = failures_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.actual.png")), actual.to_png().unwrap()).unwrap();
    if let Some(g) = golden {
        std::fs::write(dir.join(format!("{name}.expected.png")), g.to_png().unwrap()).unwrap();
        if (g.width, g.height) == (actual.width, actual.height) {
            let rgba = pixel_diffs(actual, g)
                .iter()
                .flat_map(|&d| {
                    if d > tolerance::HARD {
                        [255, 255, 255, 255]
                    } else {
                        [(u32::from(d) * 255 / u32::from(tolerance::HARD)) as u8, 0, 0, 255]
                    }
                })
                .collect();
            let diff = Image { width: g.width, height: g.height, rgba };
            std::fs::write(dir.join(format!("{name}.diff.png")), diff.to_png().unwrap()).unwrap();
        }
    }
    dir
}

#[test]
fn tolerance_tiers() {
    let img = |px: &[[u8; 4]]| Image { width: px.len() as u32, height: 1, rgba: px.concat() };
    let base = vec![[100, 100, 100, 255]; 1000];
    assert_eq!(compare(&img(&base), &img(&base)), None);
    // Soft differences: up to 5 pixels (0.5% of 1000) may exceed 2.
    let mut soft = base.clone();
    for p in soft.iter_mut().take(5) {
        p[0] = 106;
    }
    assert_eq!(compare(&img(&soft), &img(&base)), None);
    soft[5][0] = 106;
    assert!(compare(&img(&soft), &img(&base)).is_some());
    // Any pixel over the hard limit fails, even just one.
    let mut hard = base.clone();
    hard[0][1] = 109;
    assert!(compare(&img(&hard), &img(&base)).is_some());
    // Straight-alpha noise on nearly transparent pixels is not a difference.
    let faint = vec![[100, 100, 100, 3]; 1000];
    let mut noisy = faint.clone();
    for p in noisy.iter_mut().take(100) {
        p[0] = 180;
    }
    assert_eq!(compare(&img(&noisy), &img(&faint)), None);
}

#[test]
fn goldens() {
    let Some(r) = renderer() else { return };
    // Only scripts/update-goldens.sh and the goldens workflow set this.
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
            let dir = write_failure(f.name, &image, None);
            failures.push(format!("{}: no golden at {} (actual in {})", f.name, path.display(), dir.display()));
            continue;
        };
        let golden = Image::from_png(&bytes).unwrap();
        if let Some(why) = compare(&image, &golden) {
            let dir = write_failure(f.name, &image, Some(&golden));
            failures.push(format!("{}: {why} (expected/actual/diff in {})", f.name, dir.display()));
        }
    }
    assert!(failures.is_empty(), "golden mismatches on `{}`:\n{}", r.adapter().name, failures.join("\n"));
}

#[test]
fn uniform_stride_follows_the_device_alignment() {
    let Some(r) = renderer() else { return };
    let align = u64::from(r.adapter().uniform_offset_alignment);
    let stride = r.uniform_stride();
    assert!(align > 0);
    assert_eq!(stride % align, 0, "stride {stride} is not a multiple of the device alignment {align}");
    assert!(stride >= 48, "stride {stride} is smaller than the Draw uniform");
    assert!(stride < 48 + align, "stride {stride} is padded more than needed for alignment {align}");
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
