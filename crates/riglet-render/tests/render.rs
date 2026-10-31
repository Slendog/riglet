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
use riglet_render::{Budgets, DeviceLimits, FrameOptions, Image, RenderError, Renderer, RendererOptions};

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

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// Device limits for the render tests: `RIGLET_DEVICE_LIMITS=webgl2` runs
/// everything under wgpu's WebGL2 downlevel limits (the GL CI job).
fn device_limits() -> DeviceLimits {
    match env("RIGLET_DEVICE_LIMITS").as_deref() {
        Some("webgl2") => DeviceLimits::WebGl2,
        Some(other) if other != "adapter" => panic!("unknown RIGLET_DEVICE_LIMITS `{other}`"),
        _ => DeviceLimits::Adapter,
    }
}

fn renderer() -> Option<Renderer> {
    renderer_with(device_limits())
}

fn renderer_with(limits: DeviceLimits) -> Option<Renderer> {
    match Renderer::new(RendererOptions { limits, ..Default::default() }) {
        Ok(r) => {
            let a = r.adapter();
            eprintln!(
                "render tests: adapter `{}` ({}, {}, {}) limits {:?}, max texture {}",
                a.name, a.backend, a.device_type, a.driver, limits, a.max_texture_dimension
            );
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
        let options = FrameOptions { scale: width as f32, ..Default::default() };
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
        r.render(&model, &rig, canvas, &out, &FrameOptions { scale, ..Default::default() })
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

/// The WebGL2 limits a browser build gets, enforced for real: 2048 px max
/// texture size for model textures and for the output.
#[test]
fn webgl2_limits_are_enforced() {
    let Some(r) = renderer_with(DeviceLimits::WebGl2) else { return };
    assert_eq!(r.adapter().max_texture_dimension, 2048);
    assert_eq!(r.adapter().uniform_offset_alignment, 256);

    // A 4096 px wide texture: rejected from the PNG header, before decoding.
    let mut b = riglet_fixtures::Builder::new();
    b.model.canvas = riglet_format::model::Canvas { width: 64.0, height: 64.0 };
    b.texture("wide", 4096, 1, &riglet_fixtures::solid(4096, 1, [255, 0, 0, 255]), false);
    b.quad("q", "root", "wide", [-8.0, -8.0], [8.0, 8.0]);
    let doc = b.document();
    let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
    match r.upload(&doc, &rig) {
        Err(RenderError::TextureTooLarge { width: 4096, max: 2048, .. }) => {}
        Err(e) => panic!("expected TextureTooLarge, got {e}"),
        Ok(_) => panic!("expected TextureTooLarge, got Ok"),
    }

    // Output: 2048 px is fine, 2049 is over the device limit.
    let mut b = riglet_fixtures::Builder::new();
    b.model.canvas = riglet_format::model::Canvas { width: 1.0, height: 1.0 };
    b.quad("q", "root", "tex", [-0.5, -0.5], [0.5, 0.5]);
    let doc = b.document();
    let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
    let mut out = EvalOutput::new();
    rig.evaluate(&ParamValues::defaults(&rig), &mut out);
    let model = r.upload(&doc, &rig).unwrap();
    let frame = |scale: f32| FrameOptions { scale, ..Default::default() };
    assert!(r.render(&model, &rig, (1.0, 1.0), &out, &frame(2048.0)).is_ok());
    assert!(matches!(
        r.render(&model, &rig, (1.0, 1.0), &out, &frame(2049.0)),
        Err(RenderError::OutputSize { max: 2048, .. })
    ));
}

/// E10: a vertex that evaluates to infinity removes only the triangles
/// using it, as a drawable and as a mask source, and the next finite frame
/// draws everything again.
#[test]
fn non_finite_vertices_skip_their_triangles() {
    use riglet_format::model::{MaskMode, MaskRef};
    let Some(r) = renderer() else { return };
    // 4 x 4 canvas, red quad "q" (triangles 0-1-2 top right, 0-2-3 bottom
    // left), and a green quad on top clipped to q.
    let scene = |half: bool, overflow: bool| {
        let mut b = riglet_fixtures::Builder::new();
        b.model.canvas = riglet_format::model::Canvas { width: 4.0, height: 4.0 };
        b.param("p", 0.0, 1.0, 0.0);
        b.texture("red", 1, 1, &[255, 0, 0, 255], false).texture("green", 1, 1, &[0, 255, 0, 255], false);
        // Off center, so the vertex position the renderer substitutes,
        // (0, 0), would make the first triangle cover pixels the second
        // doesn't: drawing it would show.
        let (lo, hi) = ([-2.0, -2.0], [1.0, 2.0]);
        let corners = [[lo[0], lo[1]], [hi[0], lo[1]], [hi[0], hi[1]], [lo[0], hi[1]]];
        let tris: &[u16] = if half { &[0, 2, 3] } else { &[0, 1, 2, 0, 2, 3] };
        b.mesh("q", "root", "red", &corners, &[[0.5; 2]; 4], tris);
        if overflow {
            // Vertex 1 (only in the first triangle): 3e38 + 3e38 = inf.
            for _ in 0..2 {
                let kfs =
                    vec![b.positions(&[[0.0; 2]; 4]), b.positions(&[[0.0; 2], [3.0e38, 0.0], [0.0; 2], [0.0; 2]])];
                b.set("q", &[riglet_fixtures::linear("p", &[0.0, 1.0])], kfs);
            }
        }
        b.quad("top", "root", "green", lo, hi).set_draw_order("top", 1.0);
        b.drawable_mut("top").masks = vec![MaskRef { source: "q".into(), mode: MaskMode::Clip }];
        b
    };
    let render = |b: &riglet_fixtures::Builder, p: f32, model: Option<&riglet_render::GpuModel>| {
        let doc = b.document();
        let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
        let mut values = ParamValues::defaults(&rig);
        assert!(values.set(&rig, "p", [p, 0.0]));
        let mut out = EvalOutput::new();
        rig.evaluate(&values, &mut out);
        let owned;
        let model = match model {
            Some(m) => m,
            None => {
                owned = r.upload(&doc, &rig).unwrap();
                &owned
            }
        };
        let options = FrameOptions { scale: 16.0, ..Default::default() };
        (r.render(model, &rig, (4.0, 4.0), &out, &options).unwrap(), out.non_finite)
    };
    let (want_half, _) = render(&scene(true, false), 1.0, None);
    let (want_full, _) = render(&scene(false, false), 1.0, None);
    assert_ne!(want_half.rgba, want_full.rgba);

    let b = scene(false, true);
    let doc = b.document();
    let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
    let model = r.upload(&doc, &rig).unwrap();
    let (got, report) = render(&b, 1.0, Some(&model));
    assert_eq!(report.vertices, 1);
    assert_eq!(got.rgba, want_half.rgba, "the triangle with the infinite vertex must be skipped");
    // Back to finite: the skipped triangle is restored.
    let (got, report) = render(&b, 0.0, Some(&model));
    assert!(report.is_empty());
    assert_eq!(got.rgba, want_full.rgba);
}

fn evaluated(b: &riglet_fixtures::Builder) -> (riglet_format::Document, Rig, EvalOutput) {
    let doc = b.document();
    let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
    let mut out = EvalOutput::new();
    rig.evaluate(&ParamValues::defaults(&rig), &mut out);
    (doc, rig, out)
}

/// The editor's path (`render_to` into its own texture, then reading it)
/// gives exactly what `render` gives, and a view the size of R7's output
/// with no pan is exactly R7, for every fixture.
#[test]
fn render_to_and_an_r7_sized_view_match_render() {
    let Some(r) = renderer() else { return };
    for f in fixtures::all() {
        let doc = f.builder.document();
        let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
        let mut params = ParamValues::defaults(&rig);
        for (id, v) in &f.params {
            assert!(params.set(&rig, id, *v));
        }
        let mut out = EvalOutput::new();
        rig.evaluate(&params, &mut out);
        let model = r.upload(&doc, &rig).unwrap();
        let canvas = (doc.model.canvas.width, doc.model.canvas.height);
        let plain = FrameOptions::default();
        let want = r.render(&model, &rig, canvas, &out, &plain).unwrap();

        let (w, h) = r.frame_size(canvas, &plain).unwrap();
        let target = r.create_target(w, h);
        r.render_to(&model, &rig, canvas, &out, &plain, &target).unwrap();
        let got = r.read_target(&target, None).unwrap();
        assert_eq!(got, want, "{}: render_to differs from render", f.name);

        let view = FrameOptions { view: Some(riglet_render::View { width: w, height: h, pan: [0.0, 0.0] }), ..plain };
        let got = r.render(&model, &rig, canvas, &out, &view).unwrap();
        assert_eq!(got, want, "{}: an R7-sized view differs from R7", f.name);
    }
}

/// A view places model (0, 0) at the output center plus the pan, at the
/// zoom given by `scale`, on an output of any size.
#[test]
fn views_zoom_and_pan() {
    let Some(r) = renderer() else { return };
    let mut b = riglet_fixtures::Builder::new();
    b.model.canvas = riglet_format::model::Canvas { width: 1.0, height: 1.0 };
    b.texture("c", 1, 1, &[10, 200, 30, 255], false);
    b.quad("q", "root", "c", [-0.5, -0.5], [0.5, 0.5]);
    let (doc, rig, out) = evaluated(&b);
    let model = r.upload(&doc, &rig).unwrap();
    // A 4 x 4 square on a 12 x 8 output, moved 3 px left and 1 px down:
    // center (6 - 3, 4 + 1), so pixels x 1..5, y 3..7.
    for (pan, xs, ys) in [([0.0, 0.0], 4..8, 2..6), ([-3.0, 1.0], 1..5, 3..7)] {
        let view = riglet_render::View { width: 12, height: 8, pan };
        let options = FrameOptions { scale: 4.0, view: Some(view), ..Default::default() };
        let image = r.render(&model, &rig, (1.0, 1.0), &out, &options).unwrap();
        assert_eq!((image.width, image.height), (12, 8));
        for y in 0..8 {
            for x in 0..12 {
                let at = (y * 12 + x) * 4;
                let want: [u8; 4] = if xs.contains(&x) && ys.contains(&y) { [10, 200, 30, 255] } else { [0, 0, 0, 0] };
                assert_eq!(image.rgba[at..at + 4], want, "pan {pan:?}: pixel ({x}, {y})");
            }
        }
    }
}

#[test]
fn bad_views_and_targets_are_errors() {
    let Some(r) = renderer() else { return };
    let mut b = riglet_fixtures::Builder::new();
    b.model.canvas = riglet_format::model::Canvas { width: 4.0, height: 4.0 };
    b.texture("c", 1, 1, &[255; 4], false);
    b.quad("q", "root", "c", [-2.0, -2.0], [2.0, 2.0]);
    let (doc, rig, out) = evaluated(&b);
    let model = r.upload(&doc, &rig).unwrap();
    let view = |width, height, pan| FrameOptions {
        view: Some(riglet_render::View { width, height, pan }),
        ..Default::default()
    };
    let render = |o: &FrameOptions| r.render(&model, &rig, (4.0, 4.0), &out, o);
    assert!(matches!(render(&view(0, 4, [0.0; 2])), Err(RenderError::OutputSize { .. })));
    assert!(matches!(render(&view(4, u32::MAX, [0.0; 2])), Err(RenderError::OutputSize { .. })));
    assert!(matches!(render(&view(4, 4, [f32::NAN, 0.0])), Err(RenderError::BadScale)));
    assert!(matches!(render(&view(4, 4, [0.0, f32::INFINITY])), Err(RenderError::BadScale)));

    let options = FrameOptions::default();
    let wrong_size = r.create_target(5, 4);
    let got = r.render_to(&model, &rig, (4.0, 4.0), &out, &options, &wrong_size);
    assert!(matches!(got, Err(RenderError::Target(_))), "{got:?}");
    // The right size still renders after a rejected call.
    let target = r.create_target(4, 4);
    r.render_to(&model, &rig, (4.0, 4.0), &out, &options, &target).unwrap();
    assert!(r.read_target(&target, None).unwrap().rgba.iter().all(|&v| v == 255));
}

/// `from_device` on a device the caller opened (as the editor does with
/// egui's) renders exactly what `Renderer::new` renders.
#[test]
fn from_device_renders_on_a_callers_device() {
    let Some(own) = renderer() else { return };
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).unwrap();
    let limits = match device_limits() {
        DeviceLimits::Adapter => adapter.limits(),
        DeviceLimits::WebGl2 => wgpu::Limits::downlevel_webgl2_defaults(),
    };
    let (device, queue) = pollster::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor { required_limits: limits, ..Default::default() }),
    )
    .unwrap();
    let shared = Renderer::from_device(&adapter.get_info(), device.clone(), queue, Budgets::default());
    assert_eq!(shared.adapter().name, own.adapter().name);
    for f in fixtures::all().into_iter().take(4) {
        assert_eq!(render_fixture(&shared, &f), render_fixture(&own, &f), "{}", f.name);
    }
    // The caller's device stays usable after the renderer is gone.
    drop(shared);
    let _ = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
}
