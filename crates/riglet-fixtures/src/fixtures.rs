//! One render fixture per feature (spec R1-R8). Canvas 48 x 40 (non-square,
//! not a multiple of 64, so readback row padding is exercised); all shapes
//! use exact coordinates so every platform builds bit-identical files.
//!
//! Each fixture is `(name, builder, params)`: render the built model with
//! those parameter values and compare to `goldens/<name>.png`.

use riglet_format::model::{BlendMode, Canvas, MaskMode, MaskRef};

use crate::{Builder, linear, solid};

pub const WIDTH: f32 = 48.0;
pub const HEIGHT: f32 = 40.0;

/// A named render case: model plus parameter values.
pub struct Fixture {
    pub name: &'static str,
    pub builder: Builder,
    pub params: Vec<(&'static str, [f32; 2])>,
}

fn base() -> Builder {
    let mut b = Builder::new();
    b.model.canvas = Canvas { width: WIDTH, height: HEIGHT };
    b.texture("red", 1, 1, &solid(1, 1, [220, 40, 40, 255]), false);
    b.texture("blue_half", 1, 1, &solid(1, 1, [40, 80, 230, 128]), false);
    b.texture("green", 1, 1, &solid(1, 1, [40, 200, 60, 255]), false);
    b.texture("gray", 1, 1, &solid(1, 1, [128, 128, 128, 255]), false);
    b
}

fn fixture(name: &'static str, builder: Builder) -> Fixture {
    Fixture { name, builder, params: Vec::new() }
}

/// Opaque red square, then a half-transparent blue square over part of it
/// and over transparent background, with `mode`.
fn blend(name: &'static str, mode: BlendMode) -> Fixture {
    let mut b = base();
    b.quad("under", "root", "red", [-20.0, -16.0], [4.0, 8.0]);
    b.quad("over", "root", "blue_half", [-8.0, -4.0], [20.0, 16.0]).set_draw_order("over", 1.0);
    b.drawable_mut("over").blend_mode = mode;
    fixture(name, b)
}

/// Every fixture, in a stable order.
pub fn all() -> Vec<Fixture> {
    let mut out = vec![
        blend("blend_normal", BlendMode::Normal),
        blend("blend_additive", BlendMode::Additive),
        blend("blend_multiply", BlendMode::Multiply),
    ];

    // R4: additive partly over an opaque square, partly over transparency.
    {
        let mut b = base();
        b.quad("under", "root", "gray", [-20.0, -16.0], [0.0, 16.0]);
        b.texture("glow", 1, 1, &solid(1, 1, [200, 150, 30, 255]), false);
        b.quad("glow", "root", "glow", [-10.0, -8.0], [16.0, 8.0]).set_draw_order("glow", 1.0);
        b.drawable_mut("glow").blend_mode = BlendMode::Additive;
        out.push(fixture("additive_over_transparent", b));
    }

    // R3: multiply and screen colors.
    {
        let mut b = base();
        b.quad("mul", "root", "gray", [-20.0, -16.0], [0.0, 16.0]);
        b.drawable_mut("mul").rest.multiply_color = [1.0, 0.5, 0.25];
        b.quad("scr", "root", "blue_half", [2.0, -16.0], [22.0, 16.0]);
        b.drawable_mut("scr").rest.screen_color = [0.5, 0.0, 0.0];
        out.push(fixture("multiply_screen_colors", b));
    }

    // R5: masks.
    let mask = |mode| MaskRef { source: "shape".into(), mode };
    for (name, mode) in [("mask_clip", MaskMode::Clip), ("mask_invert", MaskMode::Invert)] {
        let mut b = base();
        b.quad("shape", "root", "green", [-10.0, -10.0], [10.0, 10.0]).set_draw_order("shape", -1.0);
        b.quad("art", "root", "red", [-20.0, -4.0], [20.0, 4.0]);
        b.drawable_mut("art").masks.push(mask(mode));
        out.push(fixture(name, b));
    }
    {
        // Union of two clip shapes, minus one invert shape; the second clip
        // shape is hidden and must still mask.
        let mut b = base();
        b.quad("clip_a", "root", "green", [-20.0, -12.0], [-2.0, 12.0]);
        b.quad("clip_b", "root", "green", [2.0, -12.0], [20.0, 12.0]);
        b.node_mut("clip_b").base_mut().visible = false;
        b.quad("hole", "root", "green", [-6.0, -3.0], [6.0, 3.0]);
        b.node_mut("hole").base_mut().visible = false;
        b.quad("art", "root", "red", [-22.0, -6.0], [22.0, 6.0]).set_draw_order("art", 1.0);
        let art = b.drawable_mut("art");
        art.masks.push(MaskRef { source: "clip_a".into(), mode: MaskMode::Clip });
        art.masks.push(MaskRef { source: "clip_b".into(), mode: MaskMode::Clip });
        art.masks.push(MaskRef { source: "hole".into(), mode: MaskMode::Invert });
        out.push(fixture("mask_multiple", b));
    }

    // S4: binary mask thresholds. The mask source has two hard alpha levels
    // (0.3 left half, 0.8 right half, texel-aligned so filtering can't
    // move the boundary); threshold 0.5 keeps only the right half. The
    // soft version shows both levels for comparison.
    {
        let mut levels = Vec::new();
        for x in 0..32u32 {
            let a = if x < 16 { 77 } else { 204 };
            levels.extend_from_slice(&[255, 255, 255, a]);
        }
        for (name, mode, threshold) in [
            ("mask_soft_levels", MaskMode::Clip, None),
            ("mask_threshold_clip", MaskMode::Clip, Some(0.5)),
            ("mask_threshold_invert", MaskMode::Invert, Some(0.5)),
        ] {
            let mut b = base();
            b.texture("levels", 32, 1, &levels, false);
            b.quad("shape", "root", "levels", [-16.0, -10.0], [16.0, 10.0]);
            b.node_mut("shape").base_mut().visible = false;
            b.drawable_mut("shape").mask_threshold = threshold;
            b.quad("art", "root", "red", [-22.0, -6.0], [22.0, 6.0]).set_draw_order("art", 1.0);
            b.drawable_mut("art").masks.push(MaskRef { source: "shape".into(), mode });
            out.push(fixture(name, b));
        }
    }

    // R6: a masked composite with its own blend mode. Its two overlapping
    // half-transparent children must not show a seam: the layer opacity is
    // applied once.
    {
        let mut b = base();
        b.quad("bg", "root", "gray", [-24.0, -20.0], [24.0, 20.0]).set_draw_order("bg", -10.0);
        b.quad("shape", "root", "green", [-16.0, -16.0], [16.0, 16.0]);
        b.node_mut("shape").base_mut().visible = false;
        b.composite("layer", "root", 0.5, 1.0);
        b.quad("left", "layer", "red", [-20.0, -8.0], [4.0, 8.0]);
        b.quad("right", "layer", "red", [-4.0, -8.0], [20.0, 8.0]).set_draw_order("right", 1.0);
        let g = b.group_mut("layer");
        g.blend_mode = Some(BlendMode::Multiply);
        g.masks = Some(vec![MaskRef { source: "shape".into(), mode: MaskMode::Clip }]);
        out.push(fixture("composite_masked", b));
    }
    {
        // Nested composites: the inner layer sorts as one unit inside the
        // outer one, below `top` despite its child's high draw order.
        let mut b = base();
        b.composite("outer", "root", 0.75, 0.0);
        b.quad("top", "outer", "green", [-6.0, -14.0], [6.0, 14.0]).set_draw_order("top", 5.0);
        b.composite("inner", "outer", 0.5, 1.0);
        b.quad("a", "inner", "red", [-20.0, -8.0], [8.0, 8.0]).set_draw_order("a", 100.0);
        b.quad("b", "inner", "blue_half", [-8.0, -4.0], [20.0, 12.0]).set_draw_order("b", 101.0);
        out.push(fixture("composite_nested", b));
    }

    // E3/E4: a parameter swaps two drawables' order.
    for (name, p) in [("draw_order_p0", 0.0), ("draw_order_p1", 1.0)] {
        let mut b = base();
        b.param("p", 0.0, 1.0, 0.0);
        b.quad("a", "root", "red", [-16.0, -12.0], [8.0, 12.0]).set_draw_order("a", 1.0);
        b.quad("b", "root", "green", [-8.0, -8.0], [16.0, 16.0]).set_draw_order("b", 2.0);
        let kfs =
            vec![Default::default(), riglet_format::model::Keyform { draw_order: Some(5.0), ..Default::default() }];
        b.set("a", &[linear("p", &[0.0, 1.0])], kfs);
        let mut f = fixture(name, b);
        f.params.push(("p", [p, 0.0]));
        out.push(f);
    }

    // R2: a non-premultiplied texture with soft alpha, and an asymmetric
    // texture that shows any UV flip. The ramp has exactly one texel per
    // output pixel (44 texels over 44 pixels, texel centers on pixel
    // centers), so it tests premultiplication, not the driver's bilinear
    // weight precision: magnified ramps differed by up to 24/255 between
    // adapters. Filtering is covered by uv_orientation.
    {
        let mut b = base();
        let mut ramp = Vec::new();
        for x in 0..44u32 {
            ramp.extend_from_slice(&[250, 200, 20, (x * 255 / 43) as u8]);
        }
        b.texture("ramp", 44, 1, &ramp, false);
        b.quad("ramp", "root", "ramp", [-22.0, -18.0], [22.0, -2.0]);
        // The same colors stored premultiplied, flagged as such.
        let pre: Vec<u8> = ramp
            .chunks(4)
            .flat_map(|c| {
                let a = u32::from(c[3]);
                let p = |v: u8| ((u32::from(v) * a + 127) / 255) as u8;
                [p(c[0]), p(c[1]), p(c[2]), c[3]]
            })
            .collect();
        b.texture("ramp_pre", 44, 1, &pre, true);
        b.quad("ramp_pre", "root", "ramp_pre", [-22.0, 2.0], [22.0, 18.0]);
        out.push(fixture("texture_premultiply", b));
    }
    {
        let mut b = base();
        // 2 x 2 quadrants: red top-left, green top-right, blue bottom-left,
        // white bottom-right.
        let quads = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 255, 255]];
        let pixels: Vec<u8> = quads.iter().flatten().copied().collect();
        b.texture("quadrants", 2, 2, &pixels, false);
        b.quad("q", "root", "quadrants", [-16.0, -16.0], [16.0, 16.0]);
        out.push(fixture("uv_orientation", b));
    }

    // R7: a warp folds the right half of a mesh back over the left half, so
    // those triangles turn counter-clockwise on screen. With culling (left
    // copy) they vanish; without (right copy) they draw over the left half.
    {
        let mut b = base();
        let mut positions = Vec::new();
        let mut uvs = Vec::new();
        for y in [0.0f32, 1.0] {
            for x in [0.0f32, 0.5, 1.0] {
                positions.push([x, y]);
                // A flat color: sample the texel center (no R2 border fade).
                uvs.push([0.5, 0.5]);
            }
        }
        let indices = [0, 1, 4, 0, 4, 3, 1, 2, 5, 1, 5, 4];
        for (warp, mesh, dx, culling) in [("w_cull", "culled", -22.0f32, true), ("w_both", "unculled", 2.0, false)] {
            // Columns at dx, dx + 12, then back to dx + 4: the right cell folds.
            let fold =
                [[dx, -14.0], [dx + 12.0, -14.0], [dx + 4.0, -14.0], [dx, 14.0], [dx + 12.0, 14.0], [dx + 4.0, 14.0]];
            b.warp(warp, "root", 1, 2, &fold);
            b.mesh(mesh, warp, "blue_half", &positions, &uvs, &indices);
            b.drawable_mut(mesh).culling = culling;
        }
        out.push(fixture("culling_fold", b));
    }

    // R2: transparent border. A 2 x 2 texture of one color (so filtering
    // inside it is exact on every adapter), magnified 8x. Left: UVs from
    // -0.5 to 1.5, so the color fades to 0 over one texel past the outer
    // texel centers and is transparent beyond. Right: UVs between the
    // texel centers (0.25 to 0.75): never reaches the border, solid.
    {
        let mut b = base();
        b.texture("teal", 2, 2, &solid(2, 2, [30, 160, 170, 255]), false);
        let uvs = |lo: f32, hi: f32| [[lo, lo], [hi, lo], [hi, hi], [lo, hi]];
        let rect = |x0: f32, y0: f32, x1: f32, y1: f32| [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        b.mesh("fade", "root", "teal", &rect(-22.0, -16.0, 10.0, 16.0), &uvs(-0.5, 1.5), &[0, 1, 2, 0, 2, 3]);
        b.mesh("inset", "root", "teal", &rect(12.0, -8.0, 20.0, 0.0), &uvs(0.25, 0.75), &[0, 1, 2, 0, 2, 3]);
        out.push(fixture("texture_border", b));
    }

    out
}

#[cfg(test)]
mod tests {
    use riglet_format::{Limits, Rig};

    #[test]
    fn every_fixture_is_valid_and_uniquely_named() {
        let mut names = std::collections::HashSet::new();
        for f in super::all() {
            assert!(names.insert(f.name), "duplicate fixture {}", f.name);
            if let Err(issues) = Rig::prepare(&f.builder.document(), &Limits::default()) {
                panic!("{}: {issues:#?}", f.name);
            }
        }
    }
}
