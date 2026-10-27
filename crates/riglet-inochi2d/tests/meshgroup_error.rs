//! Measures how far the importer's MeshGroup mapping is from Inochi2D:
//! evaluates the real sample models (`testing_files_gitignored/inochi2d/`)
//! both with Riglet and with a test-only reimplementation of Inochi2D's
//! vertex pipeline (`reference/`), at sampled parameter values, and reports
//! vertex errors in model pixels. Not run in CI.
//!
//! `cargo test --release -p riglet-inochi2d --test meshgroup_error -- --ignored --nocapture`
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod common;
mod reference;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use riglet_format::{EvalOutput, Limits, ParamValues, Rig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Class {
    /// No MeshGroup above.
    NoMeshGroup,
    /// Drawable chain straight from the MeshGroup, all transforms static.
    Direct,
    /// Drawable chain with an animated transform (S7 pointwise).
    Animated,
    /// A plain Node (or non-propagating composite) between: Inochi2D's
    /// MeshGroup doesn't reach the drawable (at most translates the node).
    BehindNode,
}

fn percentile(v: &mut [f32], p: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[((v.len() - 1) as f32 * p).round() as usize]
}

/// Deterministic xorshift, so runs are comparable.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Per drawable: whether its nearest MeshGroup ancestor is dynamic.
fn nearest_mesh_group(p: &reference::Puppet) -> HashMap<u32, bool> {
    let mut out = HashMap::new();
    let mut stack = vec![(0usize, None::<bool>)];
    while let Some((i, mg)) = stack.pop() {
        let n = &p.nodes[i];
        if let Some(d) = mg {
            out.insert(n.uuid, d);
        }
        let below = if n.ty == "MeshGroup" { Some(n.dynamic) } else { mg };
        stack.extend(n.children.iter().map(|&c| (c, below)));
    }
    out
}

#[test]
#[ignore = "needs models in testing_files_gitignored/inochi2d/"]
fn meshgroup_error() {
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
        let import = riglet_inochi2d::import(&bytes, &riglet_inochi2d::Options { inx, ..Default::default() }).unwrap();
        let rig = Rig::prepare(&import.document, &Limits::default()).unwrap();
        let container = riglet_inochi2d::container::parse(&bytes, &Limits::default()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&container.json).unwrap();
        let puppet = reference::Puppet::from_json(&json);

        // Drawables to compare: enabled subtrees only (Inochi2D skips
        // updating disabled subtrees), classified by the path from the
        // nearest MeshGroup ancestor down to the drawable (MeshGroup.setGroup
        // recurses only through drawables and propagating composites).
        // Nodes with transform bindings (animated transforms).
        let flagged: HashSet<u32> = json["param"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|p| p["bindings"].as_array().into_iter().flatten())
            .filter(|b| b["param_name"].as_str().is_some_and(|n| n.starts_with("transform.")))
            .filter_map(|b| b["node"].as_u64().map(|u| u as u32))
            .collect();
        let mut compare: Vec<(u32, Class)> = Vec::new();
        let mut stack = vec![(0usize, Class::NoMeshGroup)];
        while let Some((i, class)) = stack.pop() {
            let n = &puppet.nodes[i];
            if !n.enabled {
                continue;
            }
            let here = match class {
                Class::NoMeshGroup => Class::NoMeshGroup,
                c if !matches!(n.ty.as_str(), "Part" | "Mask" | "MeshGroup" | "Composite") => {
                    if c == Class::Direct || c == Class::Animated { Class::BehindNode } else { c }
                }
                Class::Direct if flagged.contains(&n.uuid) => Class::Animated,
                c => c,
            };
            if matches!(n.ty.as_str(), "Part" | "Mask") && !n.verts.is_empty() {
                compare.push((n.uuid, here));
            }
            let below = if n.ty == "MeshGroup" { Class::Direct } else { here };
            stack.extend(n.children.iter().map(|&c| (c, below)));
        }
        // Variant: per-vertex like Inochi2D, but each vertex keeps the
        // triangle E6 binds it to at rest (what a per-vertex mode for
        // rotation deformers under mesh deformers would do).
        let fixed: reference::FixedTriangles =
            puppet.rest_e6().into_iter().map(|(u, e)| (u, e.triangles)).filter(|(_, t)| !t.is_empty()).collect();
        let dynamic_of: HashMap<u32, bool> = nearest_mesh_group(&puppet);
        let names: HashMap<u32, &str> = puppet.nodes.iter().map(|n| (n.uuid, n.name.as_str())).collect();

        // Samples: each parameter alone at every axis point and midpoint
        // (per component), then random combinations of all parameters.
        let mut samples: Vec<(String, HashMap<u32, [f32; 2]>)> = vec![("defaults".into(), HashMap::new())];
        for p in &puppet.params {
            for c in 0..if p.vec2 { 2 } else { 1 } {
                let ax = &p.axis[c];
                let mut ts: Vec<f32> = ax.clone();
                ts.extend(ax.windows(2).map(|w| (w[0] + w[1]) / 2.0));
                for t in ts {
                    let mut v = p.defaults;
                    v[c] = p.min[c] + t * (p.max[c] - p.min[c]);
                    samples.push((p.name.clone(), HashMap::from([(p.uuid, v)])));
                }
            }
        }
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for k in 0..200 {
            let mut m = HashMap::new();
            for p in &puppet.params {
                let v = [0, 1].map(|c| p.min[c] + rng.next() * (p.max[c] - p.min[c]));
                m.insert(p.uuid, if p.vec2 { v } else { [v[0], p.defaults[1]] });
            }
            samples.push((format!("random #{k}"), m));
        }

        let mut out = EvalOutput::new();
        // Errors per class; bitmask misses (Inochi2D leaves the vertex
        // undeformed, Riglet extrapolates) counted separately.
        let mut errs: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        let mut errs_fixed: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        let mut errs_frame: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        let mut per_param: BTreeMap<String, (f32, u32)> = BTreeMap::new();
        let mut per_drawable: HashMap<u32, f32> = HashMap::new();
        let mut defaults_max = 0.0f32;
        for (label, values) in &samples {
            let inochi = puppet.evaluate(values);
            let variant = puppet.evaluate_with(values, Some(&fixed));
            let per_frame = puppet.evaluate_e6_per_frame(values);
            let mut p = ParamValues::defaults(&rig);
            for (uuid, v) in values {
                assert!(p.set(&rig, &format!("inochi:{uuid}"), *v));
            }
            rig.evaluate(&p, &mut out);
            let mut sample_max = (0.0f32, 0u32);
            for &(uuid, class) in &compare {
                let Some(reference) = inochi.get(&uuid) else { continue };
                let ours = common::positions(&rig, &out, uuid);
                assert_eq!(ours.len(), reference.positions.len(), "vertex count of {uuid}");
                for (k, (a, b)) in ours.iter().zip(&reference.positions).enumerate() {
                    let e = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
                    let key = if reference.outside.contains(&k) {
                        "bitmask miss"
                    } else {
                        match class {
                            Class::NoMeshGroup => "no MeshGroup",
                            Class::Direct => "direct",
                            Class::Animated => "animated transform",
                            Class::BehindNode => "behind a Node",
                        }
                    };
                    let mg = match dynamic_of.get(&uuid) {
                        Some(true) => " (dynamic MG)",
                        Some(false) => " (static MG)",
                        None => "",
                    };
                    errs.entry(format!("{key}{mg}")).or_default().push(e);
                    for (src, dst) in [(&variant, &mut errs_fixed), (&per_frame, &mut errs_frame)] {
                        if let Some(vp) = src.get(&uuid).and_then(|x| x.positions.get(k)) {
                            let ev = ((vp[0] - b[0]).powi(2) + (vp[1] - b[1]).powi(2)).sqrt();
                            dst.entry(format!("{key}{mg}")).or_default().push(ev);
                        }
                    }
                    if key == "animated transform" && e > sample_max.0 {
                        sample_max = (e, uuid);
                    }
                    if key != "bitmask miss" {
                        let d = per_drawable.entry(uuid).or_default();
                        *d = d.max(e);
                    }
                    if label == "defaults" {
                        defaults_max = defaults_max.max(e);
                    }
                }
            }
            if !label.starts_with("random") {
                let entry = per_param.entry(label.clone()).or_default();
                if sample_max.0 > entry.0 {
                    *entry = sample_max;
                }
            }
        }
        let name = path.file_name().unwrap().to_string_lossy();
        let count = |c: Class| compare.iter().filter(|x| x.1 == c).count();
        println!(
            "== {name}: {} samples; drawables: {} without MeshGroup, {} direct, {} animated transform, {} behind a Node",
            samples.len(),
            count(Class::NoMeshGroup),
            count(Class::Direct),
            count(Class::Animated),
            count(Class::BehindNode)
        );
        println!("   at defaults: max {defaults_max:.3} px (all classes)");
        println!("   Riglet vs Inochi2D:");
        for (label, v) in &mut errs {
            let max = v.iter().copied().fold(0.0f32, f32::max);
            println!(
                "   {label:20} {:8} vertex-samples   max {max:8.3} px   p95 {:8.3} px   p99 {:8.3} px",
                v.len(),
                percentile(v, 0.95),
                percentile(v, 0.99)
            );
        }
        for (title, errs) in [
            ("per-vertex, triangles bound at rest by E6 (S7), vs Inochi2D (reference variant):", &mut errs_fixed),
            ("per-vertex, E6 triangle chosen every frame at the current position, vs Inochi2D:", &mut errs_frame),
        ] {
            println!("   {title}");
            for (label, v) in errs.iter_mut() {
                let max = v.iter().copied().fold(0.0f32, f32::max);
                println!(
                    "   {label:20} {:8} vertex-samples   max {max:8.3} px   p95 {:8.3} px   p99 {:8.3} px",
                    v.len(),
                    percentile(v, 0.95),
                    percentile(v, 0.99)
                );
            }
        }
        let mut params: Vec<_> = per_param.into_iter().collect();
        params.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
        println!("   animated transform: worst parameters (alone, max px, drawable):");
        for (label, (e, uuid)) in params.iter().take(12) {
            println!("     {e:8.3}  {label}  ({})", names.get(uuid).unwrap_or(&"?"));
        }
        let mut draws: Vec<_> = per_drawable.into_iter().collect();
        draws.sort_by(|a, b| b.1.total_cmp(&a.1));
        println!("   worst drawables (max px over all samples, bitmask misses excluded):");
        for (uuid, e) in draws.iter().take(20) {
            let class = compare.iter().find(|c| c.0 == *uuid).map(|c| c.1);
            println!("     {e:8.3}  {}  {class:?}", names.get(uuid).unwrap_or(&"?"));
        }
    }
}
