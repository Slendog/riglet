# Spec change proposals

Each entry is a proposed change to the Riglet project format spec
(currently `docs/format/riglet-format-v0.2.ts`). Accepted entries are folded
into the next spec version and `schemaVersion` is bumped. The spec file is the
normative text; entries here record the reasoning.

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
