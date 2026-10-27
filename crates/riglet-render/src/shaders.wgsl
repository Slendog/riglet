// Riglet renderer shaders. Spec rules cited as R1-R8 (docs/format).
// Everything is premultiplied and gamma-space (R1, R2).

struct Frame {
    // Output size in pixels (W, H) and output scale s (R7).
    size: vec2<f32>,
    scale: f32,
    _pad: f32,
};

struct Draw {
    multiply: vec4<f32>,
    screen: vec4<f32>,
    // x: opacity, y: 1 if a clip mask applies, z: 1 if an invert mask applies,
    // w: this drawable's mask threshold when it is a mask source (S4), or
    // -1 for soft coverage.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<uniform> draw: Draw;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
@group(2) @binding(0) var clip_mask: texture_2d<f32>;
@group(2) @binding(1) var invert_mask: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// R7: model (x, y) -> pixel (x * s + W / 2, y * s + H / 2), +Y down.
@vertex
fn vs_mesh(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    let px = pos * frame.scale + frame.size * 0.5;
    var out: VsOut;
    out.pos = vec4<f32>(px.x / frame.size.x * 2.0 - 1.0, 1.0 - px.y / frame.size.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

// R5: m = clip * (1 - invert), each only when present.
fn mask_value(frag: vec4<f32>) -> f32 {
    let p = vec2<i32>(frag.xy);
    var m = 1.0;
    if draw.params.y > 0.5 {
        m = m * textureLoad(clip_mask, p, 0).r;
    }
    if draw.params.z > 0.5 {
        m = m * (1.0 - textureLoad(invert_mask, p, 0).r);
    }
    return m;
}

// R2: bilinear with a transparent black border (clamp-to-border), on a
// clamp-to-edge sampler. Past the outermost texel centers, filtering
// against the border fades the edge texel linearly to 0 over one texel;
// clamp-to-edge returns the edge texel there, so scale it per axis by that
// fade. Exact for premultiplied data (the border is (0, 0, 0, 0)).
fn sample_bordered(uv: vec2<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(tex));
    let x = uv * size - 0.5;
    let f = clamp(x + 1.0, vec2<f32>(0.0), vec2<f32>(1.0)) * clamp(size - x, vec2<f32>(0.0), vec2<f32>(1.0));
    return textureSample(tex, samp, uv) * (f.x * f.y);
}

// R3: multiply, then screen, in premultiplied form; then opacity and mask.
@fragment
fn fs_drawable(in: VsOut) -> @location(0) vec4<f32> {
    let t = sample_bordered(in.uv);
    let p1 = t.rgb * draw.multiply.rgb;
    let p2 = p1 + draw.screen.rgb * t.a - p1 * draw.screen.rgb;
    let o = draw.params.x * mask_value(in.pos);
    return vec4<f32>(p2 * o, t.a * o);
}

// R5: coverage is the source's texture alpha over its geometry; with a
// mask threshold (S4) it is 1 where alpha > threshold, else 0. Union is done
// by the blend state: dst = src + dst * (1 - src).
@fragment
fn fs_mask(in: VsOut) -> @location(0) vec4<f32> {
    let a = sample_bordered(in.uv).a;
    var k = a;
    if draw.params.w >= 0.0 {
        k = select(0.0, 1.0, a > draw.params.w);
    }
    return vec4<f32>(k, 0.0, 0.0, k);
}

// R6: a finished composite layer, one texel per output pixel.
@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> VsOut {
    let x = f32((i << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(i & 2u) * 2.0 - 1.0;
    var out: VsOut;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(0.0, 0.0);
    return out;
}

@fragment
fn fs_composite(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureLoad(tex, vec2<i32>(in.pos.xy), 0);
    return c * (draw.params.x * mask_value(in.pos));
}
