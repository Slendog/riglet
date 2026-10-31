//! The editor viewport across window resizes: one target at a time, GPU
//! allocations bounded. Its own test binary, because wgpu's object
//! counters are process-wide.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use riglet_editor::{EditorState, Loaded, Source, SourceKind, Viewport};
use riglet_fixtures::fixtures;
use riglet_format::{Limits, Rig};
use riglet_render::Renderer;

#[test]
fn resizing_the_viewport_keeps_gpu_allocations_bounded() {
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
    let f = fixtures::all().into_iter().find(|f| f.name.starts_with("composite")).expect("a composite fixture");
    let document = f.builder.document();
    let rig = Rig::prepare(&document, &Limits::default()).unwrap();
    let loaded = Loaded {
        document,
        rig,
        source: Source { path: "fixture.riglet".into(), kind: SourceKind::Riglet },
        import_warnings: Vec::new(),
    };
    let mut state = EditorState::open(loaded, Limits::default());
    let mut viewport = Viewport::new(r);
    let frame = |viewport: &mut Viewport, state: &mut EditorState, w: u32, h: u32| {
        let target = viewport.render(state, w, h).unwrap().clone();
        // Waits for the GPU, so freed objects are released (tests only).
        viewport.renderer().read_target(&target, None).unwrap();
    };
    let counters = |viewport: &Viewport| {
        let c = viewport.renderer().device().get_internal_counters();
        (c.hal.textures.read(), c.hal.texture_memory.read())
    };

    frame(&mut viewport, &mut state, 300, 200);
    let generation = viewport.target_generation();
    frame(&mut viewport, &mut state, 300, 200);
    assert_eq!(viewport.target_generation(), generation, "same size: the target is reused");
    let (textures0, memory0) = counters(&viewport);
    assert!(textures0 > 0, "wgpu counters are not available on this backend");
    let mut peak = textures0;
    for i in 0..300u32 {
        frame(&mut viewport, &mut state, 300 + i, 200 + (i * 13) % 150);
        peak = peak.max(counters(&viewport).0);
    }
    frame(&mut viewport, &mut state, 300, 200);
    let (textures1, memory1) = counters(&viewport);
    assert!(peak <= textures0 + 2, "textures grew to {peak} from {textures0}");
    assert_eq!((textures1, memory1), (textures0, memory0), "allocations left behind after resizing");
}
