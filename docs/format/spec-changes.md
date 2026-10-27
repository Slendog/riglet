# Spec change proposals

Each entry is a proposed change to the Riglet project format spec
(currently `docs/format/riglet-format-v0.2.ts`). Accepted entries are folded
into the next spec version and `schemaVersion` is bumped. The spec file is the
normative text; entries here record the reasoning.

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
