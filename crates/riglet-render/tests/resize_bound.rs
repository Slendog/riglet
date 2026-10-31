//! Resizing through many output sizes keeps GPU allocations bounded: the
//! offscreen cache holds only the current size, not one entry per size
//! ever seen. Its own test binary, because wgpu's object counters are
//! process-wide.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use riglet_fixtures::fixtures;
use riglet_format::{EvalOutput, Limits, ParamValues, Rig};
use riglet_render::{FrameOptions, Renderer, View};

#[test]
fn resizing_keeps_gpu_allocations_bounded() {
    let r = match Renderer::new(Default::default()) {
        Ok(r) => r,
        Err(e) => {
            if std::env::var("RIGLET_REQUIRE_GPU").as_deref() == Ok("1") {
                panic!("RIGLET_REQUIRE_GPU=1 but no adapter: {e}");
            }
            eprintln!("render tests: SKIPPED, no GPU adapter ({e})");
            return;
        }
    };
    // A model with nested composites and masks: every kind of offscreen
    // target is in use.
    let f = fixtures::all()
        .into_iter()
        .find(|f| {
            let doc = f.builder.document();
            let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
            let gpu = r.upload(&doc, &rig).unwrap();
            gpu.composite_depth() > 0 && gpu.has_masks()
        })
        .expect("a fixture with composites and masks");
    let doc = f.builder.document();
    let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
    let model = r.upload(&doc, &rig).unwrap();
    let mut out = EvalOutput::new();
    rig.evaluate(&ParamValues::defaults(&rig), &mut out);
    let canvas = (doc.model.canvas.width, doc.model.canvas.height);
    let render = |w: u32, h: u32| {
        let options = FrameOptions { view: Some(View { width: w, height: h, pan: [0.0, 0.0] }), ..Default::default() };
        let target = r.create_target(w, h);
        r.render_to(&model, &rig, canvas, &out, &options, &target).unwrap();
        // Reading back waits for the GPU, so freed objects are released.
        r.read_target(&target, None).unwrap();
    };
    let counters = || {
        let c = r.device().get_internal_counters();
        (c.hal.textures.read(), c.hal.texture_memory.read())
    };

    render(64, 48);
    let (textures0, memory0) = counters();
    assert!(textures0 > 0, "wgpu counters are not available on this backend");
    let mut peak_textures = textures0;
    // A window drag passes through hundreds of sizes.
    for i in 0..300u32 {
        render(64 + i, 48 + (i * 7) % 200);
        peak_textures = peak_textures.max(counters().0);
    }
    render(64, 48);
    let (textures1, memory1) = counters();
    assert!(peak_textures <= textures0 + 2, "textures grew to {peak_textures} from {textures0}");
    assert_eq!(textures1, textures0, "textures left behind after resizing");
    assert_eq!(memory1, memory0, "texture memory left behind after resizing");
}
