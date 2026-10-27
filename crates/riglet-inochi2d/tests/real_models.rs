//! Imports real Inochi2D models from `testing_files_gitignored/inochi2d/`
//! (never committed; licensed models only, e.g. the CC-BY 4.0 examples Aka
//! and Midori). Skipped when the folder is absent; not run in CI.
//!
//! `cargo test -p riglet-inochi2d --test real_models -- --ignored --nocapture`
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use riglet_format::{EvalOutput, Limits, ParamValues, Rig};

#[test]
#[ignore = "needs models in testing_files_gitignored/inochi2d/"]
fn import_real_models() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testing_files_gitignored/inochi2d");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("SKIPPED: {} not found", dir.display());
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "inp" || e == "inx"))
        .collect();
    paths.sort();
    for path in paths {
        let bytes = std::fs::read(&path).unwrap();
        let inx = path.extension().is_some_and(|e| e == "inx");
        let start = std::time::Instant::now();
        let result = riglet_inochi2d::import(&bytes, &riglet_inochi2d::Options { inx, ..Default::default() });
        let elapsed = start.elapsed();
        let import = match result {
            Ok(i) => i,
            Err(e) => panic!("{}: {} ({e})", path.display(), e.code()),
        };
        let doc = &import.document;
        let rig = Rig::prepare(doc, &Limits::default()).unwrap_or_else(|i| panic!("{i:?}"));
        let mut out = EvalOutput::new();
        rig.evaluate(&ParamValues::defaults(&rig), &mut out);
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for w in &import.warnings {
            *counts.entry(w.code.as_str()).or_default() += 1;
        }
        let m = &doc.model;
        println!(
            "== {} ({:.1} MB, imported in {elapsed:?})",
            path.file_name().unwrap().to_string_lossy(),
            bytes.len() as f64 / 1e6
        );
        println!(
            "   canvas {}x{}, {} textures, {} parameters, {} nodes ({} drawables, {} composites), {} keyform sets, {} motions, binding work {}",
            m.canvas.width,
            m.canvas.height,
            m.textures.len(),
            m.parameters.len(),
            m.nodes.len(),
            rig.drawables.len(),
            rig.composites.len(),
            rig.nodes.iter().map(|n| n.sets.len()).sum::<usize>(),
            m.motions.len(),
            rig.binding_work()
        );
        println!("   version {:?}", doc.manifest.provenance[0].source_version);
        // Open question 1: premultiplied data never has a channel above alpha.
        let (mut texels, mut above, mut partial) = (0u64, 0u64, 0u64);
        for t in &m.textures {
            let Some(bytes) = doc.files.get(&t.file) else { continue };
            let mut dec = png::Decoder::new(std::io::Cursor::new(bytes.as_slice()));
            dec.set_transformations(png::Transformations::EXPAND);
            let mut r = dec.read_info().unwrap();
            let mut buf = vec![0; r.output_buffer_size().unwrap()];
            let f = r.next_frame(&mut buf).unwrap();
            if f.color_type != png::ColorType::Rgba {
                continue;
            }
            for p in buf[..f.buffer_size()].as_chunks::<4>().0 {
                texels += 1;
                if p[3] > 0 && p[3] < 255 {
                    partial += 1;
                    if p[0].max(p[1]).max(p[2]) > p[3] {
                        above += 1;
                    }
                }
            }
        }
        println!("   texels {texels}, partial alpha {partial}, of those with rgb > alpha: {above}");
        for (code, n) in &counts {
            println!("   {n:5}  {code}");
        }
        // Optional renders for a visual check; NEVER commit them (the
        // models' license covers use, not redistribution in this repo).
        if let Ok(dir) = std::env::var("RIGLET_RENDER_DIR") {
            let renderer = riglet_render::Renderer::new(Default::default()).unwrap();
            let options = riglet_render::FrameOptions {
                scale: std::env::var("RIGLET_RENDER_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(0.12),
                background: Some([255, 255, 255]),
            };
            let image = riglet_render::render_document(&renderer, doc, &rig, &out, &options).unwrap();
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            std::fs::write(PathBuf::from(dir).join(format!("{name}.png")), image.to_png().unwrap()).unwrap();
        }
        if let Ok(filter) = std::env::var("RIGLET_SHOW_WARNINGS") {
            for w in import.warnings.iter().filter(|w| w.code == filter) {
                println!("      {:?}: {}", w.object_id, w.message);
            }
        }
    }
}
