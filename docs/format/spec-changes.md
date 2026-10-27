# Spec change proposals

Each entry is a proposed change to the Riglet project format spec
(currently `docs/format/riglet-format-v0.4.ts`). Accepted entries are folded
into the next spec version and `schemaVersion` is bumped. The spec file is the
normative text; entries here record the reasoning.

---

## v0.3.0 — rendering semantics and composite fields (status: ACCEPTED with amendments, 2026-10-27)

The v0.2.2 proposal below, as amended in review. Composites gained fields,
so this became a minor version (additive) instead of a patch.

### Amendments from review

- **R1:** textures are uploaded as non-sRGB formats (`Rgba8Unorm`, never
  `Rgba8UnormSrgb`), or sampling would silently linearize them. The M4
  editor viewport uses the same render path and target formats as the CLI.
- **R2:** exact premultiply formula `(c * a + 127) / 255` in the spec. UV
  top-left / +v down accepted; each converter owns its flip; an asymmetric
  texture fixture catches flipped UVs. No mipmaps; small-render aliasing
  added to Known gaps.
- **R3:** accepted as proposed.
- **R4:** destination alpha kept (PROVISIONAL). Additive over transparent
  pixels gives `C > A`; R8 clamps it. Documented that additive effects
  outside the silhouette vanish on transparent backgrounds (OBS). Fixture:
  additive drawable partly over transparency.
- **R5:** "shape" means the source's CURRENT deformed geometry for the
  frame. Masks render at output resolution, coverage in canvas space.
- **R6 (amended):** a composite has its own draw order instead of borrowing
  its lowest or highest child's. `GroupNode` gains optional
  `rest.drawOrder`, `blendMode` and `masks`, allowed only when
  `composite: true` (V2); drawOrder is keyable. Composites sort as one unit
  in their enclosing scope; their contents sort only among themselves; masks
  and blend apply to the finished layer. Migration from 0.2.x gives each
  composite the lowest draw order of its drawables (a migration rule, not
  the semantics). Removed from Known gaps; composite tint stays.
- **R7:** full mapping stated (x and y, pixel centers, wgpu's top-left
  fill rule). Culling only with `culling: true`; fixture with a warp that
  folds a mesh over itself. Optional AA later added to Known gaps; goldens
  never use it.
- **R8:** `rgb > a` clamp and exact un-premultiply rounding written out.
- **R9 (amended):** offscreen targets (and mask buffers) are allocated per
  nesting DEPTH and reused across sibling composites. New renderer budget:
  1 GiB total offscreen memory, checked before allocating. Test: max-depth
  composites at max output size fail cleanly or fit.

---

## v0.2.2 — rendering semantics (status: SUPERSEDED by v0.3.0 above)

v0.2.1 defines evaluated geometry, opacity, colors and draw order, but not
how pixels are produced. M2 (`riglet render`, golden images) needs that
fixed. Every rule here is a clarification of existing fields; nothing adds a
field, so this is a patch version (see the versioning policy). Rules that
copy behavior I believe Live2D/Inochi2D have, but haven't verified, are
marked PROVISIONAL; they get checked against sample renders by black-box
comparison only (clean-room rule).

Notation: premultiplied colors. A source pixel is `(Cs, As)`, the
destination already in the target is `(Cd, Ad)`, results are `(C, A)`.
Colors are RGB triples in [0,1]; products are per channel.

### R1. Color space (PROVISIONAL)

- All blending, filtering and interpolation happen on the stored (sRGB
  encoded, "gamma space") values. Texture texels are used as stored, with
  no sRGB-to-linear decode; render targets are non-sRGB formats
  (`Rgba8Unorm`), so nothing converts on write either.
- The output PNG holds those same values and is tagged sRGB.
- **Why:** Live2D and Inochi2D output looks like gamma-space blending.
  Linear-space blending would make every imported model's soft edges and
  semi-transparent layers look different from its source tool.
- **Cost:** gamma-space blending is not physically correct (dark fringes on
  bright-over-bright edges). A future `colorSpace` field could make linear
  opt-in; that would be a minor version.

### R2. Alpha and textures

- **Premultiplied everywhere:** in textures after load, in every render
  target, and in every blend formula below.
- **Loading:** a texture with `premultipliedAlpha: false` is premultiplied
  once, on the 8-bit values on the CPU, with exact integer rounding:
  `c' = (c * a + 127) / 255` (integer division). This is deterministic on
  every platform, unlike doing it in a shader. A texture with
  `premultipliedAlpha: true` is uploaded as is; a texel with `c > a` is
  clamped to `a` on load (invalid premultiplied data).
- **UV convention** (missing from v0.2.1): `uv (0, 0)` is the top-left
  corner of the image, `+v` points down, `(1, 1)` is the bottom-right
  corner. Texel centers are at `(i + 0.5) / width`.
- **Filtering:** bilinear for magnification and minification, address mode
  clamp-to-edge on both axes, **no mipmaps** in v0.2.2. Mipmap generation
  differs between GPU drivers and would make golden images unstable; the
  cost is aliasing when a model is drawn much smaller than its texture.
  Mipmaps can come later as a renderer quality option (they don't change the
  format).

### R3. Drawable color (where multiplyColor/screenColor apply)

For one drawable, per fragment, with `t = (Pt, At)` the filtered
premultiplied texel, `M` = evaluated multiplyColor, `S` = evaluated
screenColor, `o` = effective opacity (E4, but see R6 inside composites):

```
P1 = Pt * M                     multiply, on premultiplied color
P2 = P1 + S * At - P1 * S       screen, premultiplied form of c + S - c*S
Cs = P2 * o
As = At * o
```

- Equivalent to applying multiply then screen to the straight color
  `c = Pt / At` and premultiplying again, but needs no division, so it is
  exact at `At = 0`.
- Defaults `M = (1,1,1)`, `S = (0,0,0)` give `Cs = Pt * o`.
- Applies to drawables only. Not to mask coverage (R5), not to composite
  results (R6), not inherited (E4 already says so).

### R4. Blend modes

```
normal:    C = Cs + Cd * (1 - As)       A = As + Ad * (1 - As)
additive:  C = Cs + Cd                  A = Ad                 (PROVISIONAL)
multiply:  C = Cs * Cd + Cd * (1 - As)  A = Ad                 (PROVISIONAL)
```

- Results saturate at 1 (8-bit unorm targets).
- **additive / multiply keep destination alpha:** these modes change the
  color of what is already there and don't add coverage. Drawn over a fully
  transparent target, they leave it transparent. PROVISIONAL because I
  haven't verified the alpha channel against Live2D/Inochi2D output; it
  matters only where these layers extend past opaque art, or inside
  composites (R6), which then composite with less coverage.
- **multiply** reduces to `Cd` where the source is transparent (`As = 0`,
  `Cs = 0`) and to `Cs * Cd` where it is opaque.
- The target starts as transparent black `(0, 0)`. The CLI can fill a
  background color after rendering; that is not part of the model.

### R5. Masks

Each drawable with masks gets a per-pixel mask value `m` in [0, 1], and its
fragment output from R3 is scaled by it: `(Cs, As) * m`, before blending.

- **Coverage of one source** `k(x)`: the source drawable's texture alpha `At`
  over its deformed geometry, rasterized like the drawable itself (same
  filtering, same pixel rules). It ignores the source's opacity,
  multiplyColor, screenColor, blend mode and its own masks.
  - **Opacity ignored (PROVISIONAL):** riggers fade mask shapes out
    (opacity 0) so only the clipped art shows, which only works if the fade
    doesn't also delete the clipped art. Using opacity would also make a
    mask's effect depend on its parents' opacity.
  - **Own masks ignored:** no recursion; V3 already forbids cycles, this
    also keeps chains cheap (each source is rasterized once per frame).
- **Hidden sources still mask (PROVISIONAL, the question E4 deferred):**
  `visible: false` on a source or its ancestors stops it from being drawn,
  not from masking. Hiding a mask shape to see the art under it is the
  common workflow, and an invisible "mask-only" mesh is a common rig
  pattern.
- **Combining several MaskRefs:**
  ```
  clip   = 1 - Π over clip sources   (1 - k_i)    union of clip shapes
  invert = 1 - Π over invert sources (1 - k_j)    union of invert shapes
  m = (clip if any clip source else 1) * (1 - invert if any invert source else 1)
  ```
  So: draw inside the union of the clip shapes AND outside the union of the
  invert shapes. Order of MaskRefs never matters. A Live2D mesh (one mask
  list, one inverted flag) maps to all-clip or all-invert; Inochi2D
  mask/dodge maps per ref.
- **Soft, not thresholded:** `m` multiplies continuously. Inochi2D's
  per-drawable mask alpha threshold is not modeled yet (Known gap; the M3
  importer keeps it in `extensions` and warns).
- Mask coverage is computed in model space at the output resolution,
  independent of composites: a mask source outside a composite can mask a
  drawable inside it, and the other way round.

### R6. Composites

A group with `composite: true` renders as one layer:

1. Its subtree's drawables (including nested composites, see below) are
   drawn, in their draw order (E4) restricted to the subtree, into a cleared
   transparent offscreen target of the output's size, using their own blend
   modes and masks.
2. Inside the composite, a drawable's opacity `o` is the product of the
   opacities from the drawable up to, but NOT including, the composite group
   (E4's effective opacity would apply the group's opacity twice, and
   overlapping children would show seams, which is what composites exist to
   avoid).
3. The offscreen result `(Cc, Ac)` is drawn onto the parent target with the
   normal blend mode, scaled by the composite group's effective opacity
   `og`: `(Cs, As) = (Cc, Ac) * og`.

- **Place in the parent's draw order (PROVISIONAL):** a composite sorts as
  one unit using the LOWEST evaluated drawOrder among its drawables, ties
  broken by the group's pre-order position. It is drawn where its first
  child would have been. (Alternative: the highest. Inochi2D gives a
  composite its own z value, which our model doesn't have; the M3 importer
  will need a rule either way.)
- **Nesting:** yes. An inner composite is one unit inside the outer
  composite's offscreen pass, with rule 2 applying at each level (opacity
  stops at the nearest composite).
- **Masks on a composite itself: not in v0.2.2.** Only drawables carry
  `masks`, so a composite can't be masked as a unit without a new field
  (a minor version; added to Known gaps). Drawables inside a composite can
  be masked as usual (R5).
- **Composite blend mode and tint:** stays a Known gap (needs fields).
  Composites always draw with `normal`.
- A hidden composite group (or one with a hidden ancestor) draws nothing.
  A composite with no visible drawables draws nothing.

### R7. Rasterization and anti-aliasing

- **No anti-aliasing in v0.2.2:** no MSAA, no post-process AA. Golden images
  stay stable across GPUs, and edges stay as smooth as the textures'
  alpha already makes them (art meshes are usually cut out with transparent
  margins, so geometry edges rarely show). MSAA can come later as a renderer
  option.
- **Projection:** with output scale `s` (CLI `--scale`, default 1), model
  `(x, y)` maps to pixel coordinates `(x * s + W/2, y * s + H/2)`, where
  `W = round(canvas.width * s)` and `H = round(canvas.height * s)`. Pixel
  `(0, 0)` is the top-left; pixel centers are at `+0.5`; standard GPU
  top-left fill rule.
- **Culling (PROVISIONAL):** with `culling: true`, triangles that appear
  counter-clockwise on screen (+Y down) are discarded; clockwise triangles
  are front-facing. With `culling: false`, both are drawn.

### R8. Output image

- PNG RGBA8, **straight (non-premultiplied) alpha**, because PNG defines
  straight alpha: `c = (p * 255 + a / 2) / a` (integer, `c = 0` when
  `a = 0`), clamped to 255.
- Size `W x H` from R7.

### R9. Loader resource limits (implementation-defined, like the binding budget)

Riglet's renderer will refuse, with a clear error, not a crash:

- an output image larger than 8192 x 8192 (CLI cap, also because a CLI
  argument is untrusted input);
- a model texture larger than the GPU adapter's max texture size (2048 on
  some GL/WebGL adapters). No silent downscale; an explicit
  `--downscale-textures` option may come later and would warn;
- composite nesting deeper than 16 (each level needs an offscreen target of
  the output's size).

These are renderer budgets, not format rules: the same file may render with
another reader, a bigger GPU, or a larger cap.

---

## v0.2.1 — clarifications found while implementing M1 (status: ACCEPTED 1–9 with amendments, 2026-10-27; item 10 REJECTED, replaced)

The M1 implementation already follows these. They don't change the data
format; they settle cases v0.2 doesn't decide. If accepted, I fold them into
the spec and bump `schemaVersion` to "0.2.1" (patch: v0.2.0 files stay valid
and read unchanged).

1. **E6, child deltas under a mesh deformer.** "Keep its barycentric weights"
   would ignore a child's own keyform deltas. Proposal: binding fixes the
   *triangle* only. At evaluation, the child's current point maps through the
   affine map that takes the bound rest triangle onto its deformed triangle.
   At rest this matches the v0.2 text exactly.
2. **E6, points that reach a mesh deformer through other deformers.** For
   example, drawable -> warp -> mesh deformer. Proposal: bind the child's rest
   points after mapping them up the rest chain to the mesh deformer's space.
   At rest, mesh deformers are the identity, so this needs no recursion.
3. **E5, rotation under a mesh deformer.** The rest map of a triangle is the
   identity, so `J0 = I`: dθ is the rotation of the triangle's affine map,
   ds = sqrt(|det J|).
4. **E3, draw order on step axes.** E3 says draw order uses the nearest key
   "per axis". Proposal: only on linear axes; a step axis keeps the lower
   key, as for every other property. Otherwise a step axis would switch draw
   order half-way between keys, which a step axis exists to prevent.
5. **Visibility.** `visible: false` hides the node and its whole subtree.
6. **V2, drawables cannot have children.** The node-space comment lists no
   space for children of a drawable. Proposal: a drawable with children is a
   validation error (`V2_PARENT_TYPE`) until the Inochi2D "transform on every
   node" gap is resolved.
7. **V3, IDs and textures.**
   - Parameter, texture and motion IDs must be unique.
   - `exportIds` keys must refer to an existing parameter or node.
   - A texture file must exist and be a PNG.
8. **Limits, new cap: keyform vertices total = 33 554 432.** Many keyforms
   can point at the same bytes, so the decoded size is not bounded by the
   package size. The existing "vertices total" (4 194 304) is too low to also
   cover keyforms of real Live2D models. The cap is checked before decoding.
9. **Migration, patch versions.** Files with the same minor version and any
   patch version (0.2.x) are readable without migration.
10. **REJECTED — Limits, new cap: mesh-binding pairs = 268 435 456 (2^28).**
    Sum, over every (child node, mesh deformer) binding, of bound points ×
    the deformer's triangles. The grid index makes normal meshes fast, but
    no spatial index has a better worst case than points × triangles when
    triangles overlap freely (many thin triangles crossing one region, points
    in the gaps), so only a cap guarantees bounded load time. It is stated
    on the inputs, not on algorithm work, so it is the same for every
    reader. At the cap, our worst case binds in about 2 s (release, Apple
    M-series). Downside: a large well-behaved mesh can also hit it (for
    example 20 000 triangles × 65 536 points). The code enforces this cap
    already (`LIMIT_MESH_BINDING`); it is not in the spec text until
    accepted.

    **Review outcome:** rejected, because it refuses big but well-behaved
    meshes that the grid binds cheaply. Replaced by a deterministic WORK
    budget (counted candidate tests and grid cells visited, 2^28), which is
    not a format rule: the spec gains a "Loader resource limits" section
    saying readers may refuse files that exceed their own budgets, with
    Riglet's budget documented there as implementation-defined.

### Amendments from review (v0.2.1)

- **1.** The bound triangle's affine map applies to the child's CURRENT
  local position each frame, so keyform deltas and extrapolation stay
  continuous. (Spec E6.)
- **3.** Exact, not an approximation: a mesh deformer is the identity at
  rest. (Spec E5.)
- **5.** Model visibility (exported) is separate from editor visibility,
  which lives in editor.json only. Whether a hidden drawable still acts as a
  mask source is PROVISIONAL, decided in M2. (Spec NodeBase, E4.)
- **6.** Inochi2D allows children under Parts: the M3 importer restructures
  them without loss, with a round-trip test. (Spec Known gaps.)
- **7.** Inochi2D textures may be non-PNG: the M3 importer converts them.
  (Spec Known gaps.)
- **9.** Versioning policy written into the spec header.

---

## v0.2.0 — evaluator semantics (status: ACCEPTED with amendments, 2026-10-27)

v0.1 defines the data model but leaves several evaluation rules open. The M1
evaluator cannot be written without them. No v0.1 files exist in the wild yet,
so the 0.1 -> 0.2 migration only rewrites `format: "rigproj"` to `"riglet"`
and `schemaVersion` to `"0.2.0"`.

Also in v0.2: the package extension is renamed `.rigproj` -> `.riglet`, and the
manifest `format` value `"rigproj"` -> `"riglet"`.

### Amendments from review

These override the original proposal text below where they differ.

- **2. Rotation frame under a warp:** analytic Jacobian of the bilinear warp
  (no finite differences, no epsilon dependence). Rotation from the polar
  decomposition, relative to the rest Jacobian; scale is
  `sqrt(|det J| / |det J0|)` so one unit is one pixel at rest. PROVISIONAL:
  verify against Live2D sample renders by black-box comparison only. (Spec E5.)
- **3. Warp extrapolation:** outside [0,1]^2 the nearest edge cell's bilinear
  map is extended linearly; corner regions extrapolate along both axes.
  M4 editor note: inverse warp via Newton iteration with a bounded iteration
  count and a fallback. (Spec E7.)
- **4. Mesh deformer:** lowest triangle index wins ties, both for shared
  edges/vertices and for "nearest triangle". Zero-area rest triangles are a
  validation error. (Spec E6, V7.)
- **5. Parameters:** interpolation never wraps across the ends of a repeat
  parameter's key range; only the value wraps. A 1d parameter only accepts
  component 0, enforced by validation. (Spec E2, V4.)
- **6. Combining:** clamp ranges listed per property (scale >= 1e-6). Draw
  order uses nearest key (ties to the lower key), not linear interpolation,
  to avoid flicker when drawables cross; PROVISIONAL until verified against
  Live2D samples. Tree-order ties mean depth-first pre-order. (Spec E3, E4.)
- **7. Validation:** added ZIP limits (entry count, total and per-entry
  uncompressed size, compression ratio; checked on declared sizes and while
  streaming), hard caps on nodes, vertices, keyform grid size and texture
  size (checked before allocating), mask cycles. (Spec V1–V9, Limits.)
- **8. Motions:** added `inverseStep` interpolation (used by Live2D motion3).
  (Spec E9.)
- **Deferred gaps:** moved into the spec as a tracked "Known gaps" section.

### 1. Angle units and sign

**Proposal.** `angle` (rest and keyform delta) is in **degrees**. Positive angle
rotates **clockwise on screen**. Because model space is +Y down, this is the
standard matrix `[[cos, -sin], [sin, cos]]` applied to (x, y).

**Why.** Degrees are human-readable in the editor and in diffs of model.json.
Converters do the unit change at the boundary (Inochi2D uses radians).

### 2. Rotation deformer local frame

**Proposal.**
- Local frame unit = 1 model pixel at rest.
- Under a group/root parent: `p_parent = origin + scale * R(angle) * p_local`.
- Under another rotation parent: same formula, then apply the parent's map
  (frames compose).
- Under a warp parent (`origin` is in the warp's normalized space):
  1. `O' = W(origin)` where `W` is the warp's current (deformed) map into the
     warp's parent space.
  2. `J = dW/du` at `origin` (current), `J0 = dW0/du` at `origin` (rest).
     `A = J * inverse(J0)` is how the warp locally distorts pixels at the origin.
  3. Polar-decompose `A = Q * S`; keep the rotation `Q` and the uniform scale
     `sqrt(det(A))`; discard shear.
  4. `p_grandparent = O' + (s_A * scale) * Q * R(angle) * p_local`.
- Under a mesh-deformer parent: same as warp, with `W` the piecewise-affine
  map of the triangle containing `origin`.

**Why.** Normalized warp space has no pixel scale, so the frame needs a
definition. Using `J * inverse(J0)` makes the frame exactly a pixel frame at
rest, and makes it follow the warp when the warp moves, rotates, or stretches.
Dropping shear keeps rotation deformers rigid, which is their purpose.

### 3. Warp interpolation

**Proposal.**
- Control points are row-major, x varies fastest: index `j * (cols + 1) + i`.
- A child point `u` in [0,1]^2 maps by **bilinear interpolation** inside its
  cell.
- Outside [0,1]^2: **linear extrapolation** by continuing the bilinear map of
  the nearest edge/corner cell (clamp the cell index, not the coordinate).

**Why.** Continuous everywhere, cheap, and no tearing when a child sticks out
of its warp, which is common in real rigs. A per-warp option for triangle-split
cells is deferred to the converter work (see "Deferred" below).

### 4. Mesh deformer binding

**Proposal.**
- Fix the comment: children of a mesh deformer are stored in the **same space
  as the mesh deformer's own `positions`** (its parent space), not "model
  space".
- Binding is **derived, never stored**: at load, for each child point, find the
  rest triangle that contains it and store barycentric weights in memory.
- Point on a shared edge/vertex: use the lowest triangle index (deterministic).
- Point outside every triangle: use the nearest triangle (by distance to the
  triangle) with **unclamped** barycentrics (affine extrapolation).
- To verify against Inochi2D MeshGroup behavior in M3; if Inochi2D differs, the
  converter emits a warning rather than changing this rule.

### 5. Parameter values and key lookup

**Proposal.**
- The parameter value is first clamped to `[min, max]`. For `repeat`
  parameters it is wrapped into `[min, max)` instead.
- `repeat` affects the value only, not interpolation. For a seamless loop, put
  keys at `min` and `max` with equal keyforms.
- On each `KeyAxis`, values below `keys[0]` or above `keys[last]` clamp to the
  end key (no extrapolation).
- `linear`: weight `t = (v - k_i) / (k_{i+1} - k_i)` in the containing interval.
- `step`: use the greatest key `<= v` (the lower key); `t = 0`.
- An axis with one key is constant.

### 6. Combining values

**Proposal.**
- Per node: `value = rest + Σ_sets delta_set(params)`, then clamp:
  - `opacity`, `multiplyColor`, `screenColor` components: [0, 1]
  - `scale`: >= 0
  - `angle`, `origin`, `positions`, `drawOrder`: no clamp
- `scale` delta is **additive**, like every other property.
- **Opacity inherits multiplicatively**: effective opacity = own opacity × the
  parent's effective opacity, for every node type (groups and deformers
  included).
- `multiplyColor` / `screenColor` are per drawable only in v0.2 (no
  inheritance).
- **Draw order**: sort drawables by evaluated `drawOrder` (a float, no
  rounding), ascending, higher on top. Ties break by pre-order position in the
  node tree (`children` order).

### 7. Validation invariants

A reader rejects (with an error, never a crash) a file that breaks any of
these:

- Tree: exactly one node with `parent: null`, equal to `Model.root`, of type
  `group`. `parent` and `children` must agree (both are stored; `children`
  gives order). No cycles; every node reachable from root.
- Every `Id` reference resolves (texture, mask source, ParamAxis.parameter,
  physics params, motion targets). `MaskRef.source` is a drawable, not itself.
- `ParamAxis.component` is 0 for 1d parameters, 0 or 1 for 2d. `min`, `max`,
  `default` lengths match `kind`; `min < max`; `default` in range.
- KeyformSet: `axes.length >= 1`; no ParamAxis appears twice in one set;
  every axis has `>= 1` strictly ascending finite keys;
  `keyforms.length == Π axes[i].keys.length`.
- Keyform properties must be valid for the node type (e.g. `angle` only on
  rotation; `positions` only on drawable/warp/meshDeformer).
- Keyform `positions` has the same `count` as the node's rest positions.
- BufferRef:
  - `file` is a relative path under `buffers/`, no `..`, no absolute path,
    no drive letter, no backslash.
- Package: no two ZIP entries whose names differ only by case (they collide
  on macOS and Windows filesystems).
  - `byteOffset` aligned to the element size (4 for f32/u32, 2 for u16).
  - `byteOffset + count * components * size <= file length`.
  - positions/uvs/warp points: `f32`, components 2.
  - indices: `u16` or `u32`, components 1, `count % 3 == 0`, every index
    `< vertex count`.
- Drawable: `uvs.count == positions.count`.
- Warp: `rows >= 1`, `cols >= 1`, `points.count == (rows + 1) * (cols + 1)`.
- All numbers finite (no NaN/Inf).

Resource caps (max nodes, max grid size, max buffer bytes) are reader
configuration, not part of the spec.

### 8. Motion curves

**Proposal.**
- `keys` are strictly ascending in `t`.
- `interp` on key k describes the segment from key k to key k+1. The last
  key's `interp` is ignored.
- `bezier`: `handles[0]` = first control point relative to key k,
  `handles[1]` = second control point relative to key k+1, both as
  `[dt seconds, dvalue]`. Control point times are clamped into the segment so
  the curve stays a function of time.

---

## Deferred

Tracked in the spec's "Known gaps" section.
