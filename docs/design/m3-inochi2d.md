# M3 design: Inochi2D import/export (PROPOSAL, for review)

Status: APPROVED with amendments 2026-10-27 (section 0). Implementation in progress.

## 0. Review decisions and source checks (2026-10-27)

These override the sections below where they differ.

1. **Textures:** in-house TGA decoder for exactly the variants in section 2
   (truecolor 24/32-bit and grayscale 8-bit, uncompressed and RLE, both
   origins); everything else is a clear error. BC7: placeholder +
   `TEXTURE_UNDECODABLE`, raw bytes kept for re-export. Export writes PNG.
   Manual check added to the test plan (section 9).
2. **Spec v0.4.0** takes S1, S2 and S4 together (see spec-changes.md).
   - S2 combine order checked in Inochi2D 0.8.7: every bindable property
     is either additive (`transform.t.*`, `transform.r.*`, `zSort`,
     `screenTint.*`, `emissionStrength`) or multiplicative (`transform.s.*`,
     `opacity`, `tint.*`, `alphaThreshold`), never both. Offsets reset per
     frame (`beginUpdate`: opacity 1, tint (1,1,1), screenTint 0) and the
     final value is clamped after combining (`clamp(tint * offsetTint, 0,
     1)`, `clamp(offsetOpacity * opacity, 0, 1)`; `part/package.d`
     l.207-281, 798-803). So `(rest + sum of additive) * product of
     multiplicative`, then clamp, reproduces it.
3. **Multi-parameter sets on export:** first pair two 1D parameters into
   one Inochi2D 2D parameter when a set has exactly two axes from two
   different 1D parameters (exact); greedy pairing; the default-anchored
   split with `KEYFORM_INTERACTION_DROPPED` only for what's left. Pairing is
   recorded in ext and undone on re-import; an info message says the
   parameter layout seen by tracking setups changes.
   - Checked in 0.8.7 (`param/binding.d` `interpolateLinear`): a 2D
     parameter binding is bilinear over `values[x][y]` (lerp along y, then
     x), i.e. multilinear like a Riglet set; a node bound to one of the pair
     gets a grid constant along the other axis. Parameter values are mapped
     to 0..1 and clamped before lookup (`mapValue`), matching E2.
   - `interpolateNearest` sends ties to the UPPER key (`offset >= 0.5`).
     The midpoint-key conversion to step axes (section 4) matches that.
4. **Tint/screen order: converted, not just warned.** For Inochi2D's
   screen-then-multiply, use `S' = S*M`, `M' = M(1-S)/(1-S*M)` per channel
   (`S*M = 1` gives `M' = 1, S' = 1`), on rest values and every keyform
   point. Warn `COLOR_ORDER_INTERPOLATION` (with the largest difference)
   only where bindings make the result differ between keys. Export applies
   the inverse. Unit test over a grid of (C, M, S).
   - **Source check finding:** Inochi2D's shader screens PREMULTIPLIED color
     as `1 - (1 - t)(1 - S*a)` = `t + S*a - t*S*a` (`shaders/basic/basic.frag`
     at v0.8.7), where the standard premultiplied screen is `t + S*a - t*S`.
     The extra factor `a` in the last term means the conversion is exact
     where the texel is opaque or S = 0; at partial alpha it differs by at
     most `t*M*S*(1-a)`. No constant per drawable can reproduce that, so the
     importer warns `COLOR_ORDER_PARTIAL_ALPHA` when S != 0 on a texture with
     partially transparent texels.
5. **Blend modes:** see section 4a.
6. **Real models:** run on Aka and Midori (CC-BY 4.0, never committed, no
   committed renders), report warning counts by code, answer open questions
   1-5. No new spec fields from that yet.
7. **JSON depth:** serde's recursion limit (128) caps node-tree depth at
   about 60; map that failure to the limit error `INP_NESTING_TOO_DEEP`, with
   a test.

## 4a. Blend modes (from Inochi2D 0.8.7 `core/nodes/common.d`)

Inochi2D has two paths: with `KHR_blend_equation_advanced` it uses the KHR
equations for Multiply, Screen, Overlay, Darken, Lighten, ColorDodge,
ColorBurn, HardLight, SoftLight, Difference, Exclusion; without it
(`inSetBlendModeLegacy`) fixed-function blending. **Inochi2D's own output
therefore depends on the GPU.** Riglet matches the legacy path, which is
deterministic and fixed-function.

(a) Reproducible with fixed-function blending (legacy path), proposed for
v0.4.0 with a golden fixture each. Premultiplied, `C`/`A` as in R4:

| Inochi2D | GL (legacy) | C | A |
|---|---|---|---|
| Screen | `ONE, ONE_MINUS_SRC_COLOR` | Cs + Cd(1 - Cs) | As + Ad(1 - As) |
| LinearDodge | `ONE, ONE_MINUS_SRC_COLOR` / alpha `ONE, ONE_MINUS_SRC_ALPHA` | same as Screen | same as Screen |
| Lighten | `GL_MAX` | max(Cs, Cd) | max(As, Ad) |
| ColorDodge | `DST_COLOR, ONE` | Cs*Cd + Cd | As*Ad + Ad |
| AddGlow | `ONE, ONE` / alpha `ONE, ONE_MINUS_SRC_ALPHA` | Cs + Cd | As + Ad(1 - As) |
| Subtract | reverse subtract `ONE_MINUS_DST_COLOR, ONE` / alpha add | Cd - Cs(1 - Cd) | As(1 - Ad) + Ad |
| Exclusion | `ONE_MINUS_DST_COLOR, ONE_MINUS_SRC_COLOR` / alpha `ONE, ONE` | Cs(1 - Cd) + Cd(1 - Cs) | As + Ad |
| Inverse | `ONE_MINUS_DST_COLOR, ONE_MINUS_SRC_ALPHA` | Cs(1 - Cd) + Cd(1 - As) | As(1 - Ad) + Ad(1 - As) |
| DestinationIn | `ZERO, SRC_ALPHA` | Cd*As | Ad*As |
| ClipToLower | `DST_ALPHA, ONE_MINUS_SRC_ALPHA` | Cs*Ad + Cd(1 - As) | Ad |
| SliceFromLower | `ZERO, ONE_MINUS_SRC_ALPHA` | Cd(1 - As) | Ad(1 - As) |

Normal and Multiply already exist in Riglet with exactly these legacy
formulas. LinearDodge is identical to Screen on the legacy path, so it maps
to `screen`. Riglet's own `additive` (A = Ad, from Live2D) stays; Inochi2D's
AddGlow differs from it only in alpha, hence its own mode. The KHR
versions of Multiply/Screen/Lighten/ColorDodge/Exclusion differ from these:
models rendered by Inochi2D on KHR hardware will look slightly different
(documented, not fixable).

(b) Needing destination reads in the shader (KHR only; Inochi2D's legacy
path falls back to Normal for them): Overlay, Darken, ColorBurn, HardLight,
SoftLight, Difference. These stay approximated as `normal` with
`BLEND_MODE_UNSUPPORTED` and go to Known gaps.

Sources: the Inochi2D repositories (BSD-2-Clause), read to learn the format:
`inochi2d` at tag v0.8.7 (commit e2235f6) and `main` (4975d24),
`inochi-creator` v0.8.6, `inochi-session` v0.8.7, `inochi2d-docs` (593c657,
CC-BY 3.0). Paths below are relative to `source/inochi2d/` in `inochi2d`
unless noted. Claims marked *(unverified)* still need a check against a real
file before implementation.

## 1. Versions

| Version | Status | Support |
|---|---|---|
| 0.7.x (container "INP1") | old stable | **read** |
| 0.8.0 - 0.8.7 (INP1) | current stable (0.8.7, 2024-10) and what Creator 0.8.6 / Session 0.8.7 ship | **read + write** (we write 0.8.7-compatible files) |
| 0.9 / `main` (container "INP2", magic `TRNSRTS2`) | in development, unreleased, still changing | **not supported**: detected by magic, clear error `INP2_UNSUPPORTED` |

- 0.7 -> 0.8 kept the container; the JSON gained `emissionStrength`,
  `merge_mode`, `animations`, `MeshGroup`, `grid_axes`, SimplePhysics
  `local_only`, and (0.8.3) `propagate_meshgroup`. Enums are written by
  name, so the reordered BlendMode enum is not a problem.
- The `Mask` node type exists up to 0.8.6 and was removed in 0.8.7. We read
  it (a drawable used only as a mask source).
- There is no format version field the reader uses; `meta.version` is an
  informational string (`"v0.8.7"`). We record it in `Provenance.sourceVersion`.

## 2. Container (.inp and .inx)

Both extensions use the same container and JSON schema (`fmt/package.d`,
`fmt/binfmt.d`). `.inx` is Inochi Creator's project file: textures are kept
one per Part (no atlas) and it carries Creator-only JSON (top-level
`groups`, parameter `parentUUID`, Part `psdLayerPath`, `Camera` nodes).

```
"TRNSRTS\0"            8 bytes
u32 jsonLength         big-endian
UTF-8 JSON payload
"TEX_SECT"             8 bytes
u32 textureCount
textureCount x { u32 length | u8 encoding | length bytes }
[optional] "EXT_SECT"  8 bytes
u32 extCount
extCount x { u32 nameLength | name | u32 payloadLength | payload }
```

- All integers are big-endian u32. Texture encoding: 0 = PNG, 1 = TGA,
  2 = BC7. The 0.8.7 writer always writes TGA; its reader ignores the byte
  and sniffs PNG/TGA. Zero-length textures occur (placeholders).
- EXT_SECT payloads are opaque, keyed by reverse-DNS names (seen:
  `com.inochi2d.inochi-session.bindings`, `...animations`,
  `com.inochi2d.creator.FlipConfig`).
- Their reader validates only the two magic strings. Ours treats the file
  as untrusted (section 6).

**Our parser:** every length and count is checked against the remaining
bytes and against `riglet_format::Limits` before allocating (JSON at most
`zip_max_entry_uncompressed`; texture count at most `zip_max_entries`; each
texture and the total at most the per-entry/total caps). The JSON is parsed
into `serde_json::Value` first, then read field by field: tolerant of
missing optional keys, and of the duplicated `timestep` key that 0.8 writes
into every animation (a typed serde struct would reject the whole file).

### Texture encodings

| Encoding | Plan |
|---|---|
| PNG | existing `png` dependency |
| TGA | **small in-house decoder** (~200 lines: truecolor 24/32-bit and grayscale 8-bit, uncompressed and RLE, both origins). No new dependency. Gets a fuzz target. |
| BC7 | **not decoded.** No shipped tool writes it and Inochi2D 0.8 can't read it either. The raw bytes are kept in the package for re-export; the importer substitutes a 1x1 magenta texture and warns `TEXTURE_UNDECODABLE`. |

Alternative to the in-house TGA decoder: the `image` crate with only the
`tga` feature (`default-features = false`), a mature but large dependency.
I recommend in-house: TGA is small, and we must fuzz our wrapper anyway.
**Your approval needed** for either choice.

Export writes PNG (encoding 0). Inochi2D 0.7/0.8 readers sniff the image
format, so PNG loads there.

## 3. Coordinates and conventions

- Inochi2D units are pixels, +Y down, UV origin top-left, +v down: the
  same as Riglet (R2). No UV flip needed in this converter.
- Vertex path: `puppetTransform * nodeGlobal * (vert - origin + deform)`
  (`shaders/basic/basic.vert`). Riglet drawable positions = `vert - origin`
  in the frame of the node's transform (section 4).
- Node transform is T * R * S with `trans [x,y,z]`, `rot [x,y,z]` in
  radians (Euler), `scale [x,y]`.
- Inochi2D has no canvas size. On import the canvas is the bounding box of
  all rest geometry, rounded out, centered at the origin. Riglet-only data
  (canvas, Riglet ids) is written on export to an EXT_SECT entry
  `org.riglet.model` so a round trip is exact.
- Textures are stored premultiplied *(inferred from Creator premultiplying on
  import and the library blending `ONE, ONE_MINUS_SRC_ALPHA`; verify on a
  real file)*: `premultipliedAlpha: true`.

## 4. Mapping: Inochi2D -> Riglet

Legend: **exact**; **approx** (lossy, warning code); **unsupported** (kept
in `extensions`, warning code; restored on export). "ext" alone means kept
in `extensions` losslessly, no warning.

### Nodes and tree

| Inochi2D | Riglet | Kind |
|---|---|---|
| Node uuid | Riglet id `inochi:<uuid>`, `exportIds.inochi2d` | exact |
| Node (no transform) | GroupNode | exact |
| Node with transform | RotationDeformer (origin = trans.xy, angle = rot.z in degrees, scale) holding the children | exact if scale.x == scale.y and rot.x == rot.y == 0; else see spec change S1 / `TRANSFORM_APPROXIMATED` |
| `trans.z` | ext | ext (no visual effect in 2D) |
| `rot.x`, `rot.y` != 0 (rest and `transform.r.x/y` bindings) | per-axis signed scale (S6): projected, `Rz Ry Rx` is `R(rz) [[cos ry, sin ry sin rx], [0, cos rx]]`, so ry scales x by cos ry and rx scales y by cos rx; bindings become additive scaleXY sets | exact alone (bindings at keys); approx `TRANSFORM_3D_ROTATION_INTERPOLATED` between keys or with several bindings on one axis, `TRANSFORM_3D_ROTATION_DROPPED` when x and y both rotate (shear dropped), `TRANSFORM_3D_DEPTH_DROPPED` when z translations below shift points |
| Part (Drawable) | DrawableNode (positions = verts - origin) inside its transform's rotation deformer | exact |
| **Part with children** | a group (or the Part's rotation deformer) holding the drawable FIRST, then the children, preserving transforms; the original nesting is recorded in ext so export rebuilds the Part with children | exact, round-trip tested |
| `enabled: false` | `visible: false` | exact |
| Mask node (<= 0.8.6) | DrawableNode with `visible: false` (hidden sources still mask, R5) | exact |
| Composite | composite GroupNode (own scope), opacity, masks | exact |
| Composite `blend_mode` | see blend table | |
| Composite `tint`, `screenTint` | ext (Known gap: composite tint) | approx `COMPOSITE_TINT_DROPPED` when not default |
| Composite `propagate_meshgroup` | ext | approx `PROPAGATE_MESHGROUP_UNSUPPORTED` when true |
| MeshGroup | MeshDeformer; only what Inochi2D's `setGroup` reaches is under it: drawables, their drawable children and composites with `propagate_meshgroup`. Other nodes are "blocked": moved outside the mesh deformers under rigid `:rigid` twins of the animated frames in between (exact), and their subtree isn't reached by any MeshGroup | exact for reached points its rest bitmask covers; approx `MESHGROUP_OUTSIDE_POINTS` (Inochi2D leaves points whose `floor()` corner misses every rest triangle undeformed, Riglet extrapolates) |
| MeshGroup `dynamic_deformation` | true: maps the deformed child points, like E6, but looks them up each frame (Riglet binds at rest): approx `MESHGROUP_DYNAMIC_APPROXIMATED`. false: adds the child's own deformation after the map (E6 maps it): approx `MESHGROUP_DEFORM_APPROXIMATED` when a reached child has deform bindings | approx |
| MeshGroup `translate_children` | a blocked plain node only has its translation moved by the mesh; Riglet gives it a rigid rotation deformer under the mesh deformer, which also rotates and scales it (E5) | approx `MESHGROUP_TRANSLATE_APPROXIMATED` (proposed S7 "translate") |
| Animated transform on a reached drawable under a MeshGroup | Inochi2D deforms every vertex individually; Riglet maps the rotation deformer rigidly (E5). Static ones are baked into the vertices (exact); baked static ancestors of an animated one become one `:bake` rotation deformer | approx `MESHGROUP_TRANSFORM_APPROXIMATED` (proposed S7 "pointwise"; measured in `tests/meshgroup_error.rs`) |
| `lockToRoot: true` | ext | approx `LOCK_TO_ROOT_UNSUPPORTED` |
| SimplePhysics node | PhysicsRig (one pendulum: length, gravity, damping mapped to mobility/delay/acceleration; output param from `param` + `map_mode`), all original fields in ext | approx `PHYSICS_APPROXIMATED`; exported back exactly from ext when unchanged |
| Camera (.inx), Shapes, Tmp, unknown types | the whole subtree's JSON kept in ext on the parent | unsupported `NODE_TYPE_UNSUPPORTED` (Camera: ext, no warning) |

### Draw order

| Inochi2D | Riglet | Kind |
|---|---|---|
| `zsort`, accumulated parent + child, higher = further back | `drawOrder = -(accumulated zSort)` on each drawable; composites get their own (R6) | exact |
| Composite's children (own sorted list) | composite scope | exact |
| `zSort` binding on a Node (shifts all descendants) | the delta is expanded into every descendant drawable's draw-order keyforms | exact (more data) |
| zSort binding with Linear interpolation | Riglet draw order uses the nearest key (E3), so the crossing point moves from where the interpolated values cross to the key midpoint | approx `DRAW_ORDER_INTERPOLATION` |

### Parts

| Inochi2D | Riglet | Kind |
|---|---|---|
| `textures[0]` (albedo) | texture | exact |
| `textures[1..2]` (emissive, bump) | package files + ext | unsupported `EMISSIVE_BUMP_NOT_RENDERED` when set |
| `emissionStrength` | ext (note: 0.8 loads it into `tint`, a known bug; we keep the stored value) | ext |
| `opacity` | opacity | exact |
| `tint` | multiplyColor | exact alone |
| `screenTint` | screenColor | exact alone |
| tint and screenTint both non-default | Inochi2D screens first, then multiplies; Riglet R3 multiplies first | approx `COLOR_ORDER` |
| `blend_mode` | see blend table | |
| `masks` [{source, Mask / DodgeMask}] | MaskRef clip / invert | exact when all Mask entries come before all DodgeMask entries (Inochi2D's stencil is order-dependent); else approx `MASK_ORDER` |
| `mask_threshold` (binary stencil test on the SOURCE's alpha) | Riglet masks are soft (R5) | approx `MASK_THRESHOLD` (Known gap), or exact with spec change S4 |
| `psdLayerPath` (.inx) | ext | ext |

### Blend modes

Inochi2D has 19 modes. Riglet has normal, additive, multiply.

| Inochi2D | Riglet | Kind |
|---|---|---|
| Normal | normal | exact |
| Multiply (legacy GL: `DST_COLOR, ONE_MINUS_SRC_ALPHA`) | multiply | exact (alpha keeps Ad on both) |
| Screen, LinearDodge (identical on the legacy path), Lighten, ColorDodge, AddGlow, Subtract, Exclusion, Inverse, DestinationIn, ClipToLower, SliceFromLower | group (a) mode of the same name (LinearDodge -> screen) | exact vs Inochi2D's legacy path; under KHR_blend_equation_advanced Inochi2D draws Multiply, Lighten, ColorDodge and Exclusion differently (Screen's KHR form is the same) |
| Overlay, Darken, ColorBurn, HardLight, SoftLight, Difference | normal, original name in ext | unsupported `BLEND_MODE_UNSUPPORTED` |

### Parameters and bindings

| Inochi2D | Riglet | Kind |
|---|---|---|
| Parameter `uuid`, `name` | Parameter id `inochi:<uuid>`, name, `exportIds.inochi2d` | exact |
| `is_vec2`, `min`, `max`, `defaults` | kind 1d/2d, ranges, default | exact |
| `axis_points` (normalized 0..1) | keys = min + p * (max - min) | exact (up to float rounding) |
| `merge_mode` (how inputs combine into the parameter) | ext | ext |
| Binding (one node, one property, one parameter, 1 or 2 axes) | one KeyformSet per binding, axes = the parameter's components | exact |
| `isSet` (sparse grid) | unset points filled with Inochi2D's `reInterpolate` algorithm (what Inochi2D itself evaluates with); the mask kept in ext so export restores it | exact |
| `interpolate_mode` Linear | linear axes | exact |
| Nearest | step axes with an extra key at each midpoint (step uses the lower key, so `[k0, mid, k1]` with values `[v0, v1, v1]` reproduces nearest exactly) | exact |
| Cubic | cubic axes (S5, Inochi2D's Catmull-Rom) | exact |
| `deform` (per-vertex deltas, node-local, before the transform) | keyform positions | exact |
| `transform.t.x/y` (additive) | rotation deformer origin delta | exact |
| `transform.r.z` (additive, radians) | rotation deformer angle delta (degrees) | exact |
| `transform.s.x/y` (MULTIPLICATIVE) | scale deltas | exact for one binding per property: delta = rest * (v - 1); several bindings on the same property multiply in Inochi2D but add in Riglet: approx `MULTIPLICATIVE_BINDINGS`, or exact with S2 |
| `opacity`, `tint.*` (MULTIPLICATIVE) | opacity / multiplyColor deltas | as for scale |
| `screenTint.*` (additive) | screenColor deltas | exact |
| `zSort` (additive) | draw-order deltas (negated) | see draw order |
| `alphaThreshold` | ext (it has no effect in 0.8: the offset starts at 0 and is multiplied) | ext |
| `emissionStrength`, physics-node properties | ext | unsupported `BINDING_UNSUPPORTED` |

### Package-level data

| Inochi2D | Riglet | Kind |
|---|---|---|
| `meta` (name, rigger, artist, rights, copyright, license URL, contact, reference, thumbnail, preservePixels) | `extensions.inochi2d.meta` | ext |
| `physics` {pixelsPerMeter, gravity} | `physicsEnvironment.gravity` = [0, gravity], pixelsPerMeter in ext | exact for the stored values; the physics model itself is approx (above) |
| `automation` (sine / physics) | ext | unsupported `AUTOMATION_UNSUPPORTED` |
| `animations` | Motions: lanes on parameter targets become tracks; frame / timestep -> seconds; Linear, Nearest (as step) exact; Cubic (with `tension`) approximated by bezier; `leadIn`, `leadOut`, `additive`, `animationWeight`, `merge_mode` in ext | exact / approx `ANIMATION_CUBIC_APPROXIMATED` |
| `groups`, `parentUUID` (.inx) | Parameter `group` = group name; full group data in ext | exact + ext |
| EXT_SECT entries | package files `extensions/inochi2d/<name>` (kept verbatim) | ext |

## 5. Export: Riglet -> Inochi2D

| Riglet | Inochi2D | Kind |
|---|---|---|
| GroupNode | Node | exact |
| composite GroupNode | Composite (blend: group (a) and normal/multiply by name; additive -> AddGlow, whose alpha is As + Ad(1 - As) instead of Ad: equal where the destination is opaque, no Inochi2D mode keeps Ad with additive color) | exact, except additive: approx `BLEND_MODE_APPROXIMATED` |
| DrawableNode | Part (rebuilt with children when the import recorded it) | exact |
| RotationDeformer under a group / rotation | Node with transform (trans = origin, rot.z = angle in radians, scale) | exact |
| RotationDeformer under a warp | Node; Inochi2D has no warp frame | approx `ROTATION_UNDER_WARP` |
| WarpDeformer | MeshGroup with the warp grid as triangles (piecewise affine instead of bilinear) | approx `WARP_AS_MESHGROUP` |
| MeshDeformer | MeshGroup (static) | exact for points inside the mesh |
| Masks | clip -> Mask, invert -> DodgeMask, all Mask entries first (exact under the stencil); soft coverage becomes a binary threshold | approx `MASK_SOFTNESS_LOST` |
| KeyformSet over one parameter (1 or 2 axes) | one binding per animated property | exact (linear and step axes; step -> Nearest with shifted keys is approx `STEP_AS_NEAREST`) |
| **KeyformSet over several parameters** | split into one binding per parameter; the interaction is dropped | approx **`KEYFORM_INTERACTION_DROPPED`** |
| Several sets on the same node, property and parameter | summed into one binding | exact |
| Additive opacity / multiplyColor / scale deltas | multiplicative Inochi2D bindings: factor = (rest + delta) / rest | exact for one parameter per property and rest != 0; else approx `MULTIPLICATIVE_BINDINGS` |
| Draw-order keyforms | zSort bindings (negated), Nearest interpolation (= Riglet's nearest key on linear axes) | exact |
| PhysicsRig with preserved ext and unchanged | the original SimplePhysics node | exact |
| other PhysicsRig | SimplePhysics (first pendulum only) | approx `PHYSICS_APPROXIMATED` |
| Motions | animations | exact for linear/step; bezier approximated as Cubic `ANIMATION_BEZIER_APPROXIMATED` |
| Textures | PNG, encoding 0 | exact |
| canvas, Riglet ids | EXT_SECT `org.riglet.model` | exact (ignored by Inochi2D) |

**Splitting a multi-parameter set** (axes over parameters P1..Pn): each
parameter gets the slice of the grid through the other parameters' default
values: `g_i(p_i) = f(defaults with p_i varied)`, minus the value at the
default point for every slice but the first. The result is exact at the
default pose and along each parameter's own line through it; what is
dropped is the interaction term. The warning message reports the largest
dropped delta (in pixels or units), so a rigger can see whether it matters.
Alternative: a least-squares additive fit (smaller average error, but not
exact anywhere in particular). I recommend the default-anchored split.

### 5a. Export: undoing the importer's restructuring (added after the import review)

The importer does not map nodes 1:1, so export first groups Riglet nodes
back into Inochi2D nodes and only then writes them. Every artifact is
marked by its id suffix or by `extensions.inochi2d`:

| Riglet artifact | Export |
|---|---|
| `inochi:U` (group / drawable / mesh deformer / composite) | Inochi2D node U, its raw fields restored from `extensions.inochi2d` |
| `inochi:U:transform` (rotation) | the transform of node U (merged, not a separate Node) |
| `inochi:U:part` drawable + `inochi:U` holder group | one Part U whose children are the holder's other children |
| `inochi:U:mesh` | MeshGroup U's mesh (U had a transform) |
| `inochi:U:bake` (static rotation for baked ancestors) | dropped: it equals the static transforms of U's Inochi2D ancestors, which export writes again |
| `...:rigid` twins (`twinOf`) | dropped: blocked nodes return to their Inochi2D parent |
| any node | placed under `parentUUID` at `childIndex` (recorded on import), not under its Riglet parent |
| vertices and deform deltas under a MeshGroup | un-baked: mapped back through the inverse of the static Inochi2D transforms between the MeshGroup and the node (what the importer baked) |
| Riglet nodes without `inochi:` ids (native models) | new nodes with fresh uuids, mapped by the table above |

Data the importer converted (tint/screen order, multiplicative sets, x/y
rotations as scales, cubic axes) is converted back with the inverse
formulas; the warnings the importer emitted are not repeated, new losses
get their own codes. The S7 decision changes two rows: with "pointwise"
and "translate", rotation deformers under mesh deformers map back to Part
and plain-node transforms exactly instead of approximately.

## 6. Spec changes this needs (proposed v0.4.0, minor, additive)

Without them, the rows marked "or exact with S..." stay approximations.

- **S1. Non-uniform scale on rotation deformers:** `rest.scale` may be a
  number or `[sx, sy]`; Keyform gets `scaleXY?: Vec2`. Inochi2D binds
  `transform.s.x` and `.y` separately. Migration: none needed (a number
  stays valid).
- **S2. Multiplicative keyform sets:** `KeyformSet.combine?: "add" |
  "multiply"` for scalar and color properties: value = (rest + sum of
  additive deltas) * product of multiplicative factors. Makes Inochi2D's
  opacity, tint and scale bindings exact. Live2D export of a multiplicative
  set merges into the grid (exact at grid points, approximated between
  them, with a warning).
- **S4. Binary mask threshold:** optional `maskThreshold?: number` on a
  drawable used as a mask source: its coverage becomes 1 where alpha >
  threshold, else 0. Makes Inochi2D masks exact; Live2D and native models
  keep soft masks.

I can also do M3 without any of these (more warnings) and propose them
later. My recommendation: S1 and S4 now (small and local); S2 now if you
want Inochi2D round trips without warnings on typical rigs, since opacity
and tint bindings are common.

## 7. Crate, API and CLI

- `crates/riglet-inochi2d`: depends on `riglet-format` (+ `png`); no GPU.
- `import(bytes, &Limits) -> Result<Import, InochiError>` with
  `Import { document, warnings: Vec<ConversionWarning> }`, warnings also
  recorded in the document's `Provenance`.
- `export(&Document, &Rig) -> Result<Export, InochiError>` with the bytes
  and warnings.
- CLI: `riglet import IN.inp|IN.inx -o OUT.riglet` and
  `riglet export IN.riglet -o OUT.inp` (`--format inx` for Creator
  projects), printing warnings; exit codes as for the other commands.

## 8. Untrusted input

- Container: section 2 checks.
- JSON: `serde_json::Value` (recursion limit 128). Node trees are walked
  iteratively; at most `Limits::max_nodes` nodes, mesh sizes within the
  vertex limits (indices are u16 in the format anyway), binding grids within
  `max_keyform_grid`, `reInterpolate` bounded by the grid size (it loops
  until every point is set; our version caps the iterations at the grid
  size and fails cleanly).
- TGA decoder: dimensions checked against the texture limit before
  allocating; RLE runs bounded by the remaining pixels.
- After conversion, the result goes through `Rig::prepare` like any other
  file; an importer that produces an invalid model is a bug (tested).

## 9. Test plan

- **Synthetic fixtures built in code:** an Inochi2D puppet builder (JSON +
  container writer) in the test support code, one fixture per mapping row
  marked exact, and one per warning code.
- **Round trips:**
  - Inochi2D -> Riglet -> Inochi2D -> Riglet: the two Riglet documents
    must be equivalent (same tree, ids, parameters; keyform values and
    positions within a float tolerance; same warnings).
  - Riglet -> Inochi2D -> Riglet for fixtures using only exact features:
    equivalent; for lossy features: the expected warning codes and nothing
    else.
  - Beyond structure: evaluate both models at sampled parameter values and
    compare drawable vertices, opacity and draw order.
- **Fuzz targets** (60 s per push, weekly 30 min): `inp_container` (binary
  layer), `inp_json` (payload through conversion and `Rig::prepare`),
  `tga`. Mutation smoke tests on stable, regression replay directories.
- **Real models:** only from `testing_files_gitignored/`, never committed,
  skipped when absent, not run in CI. Candidates: the official example
  models Aka and Midori (CC-BY 4.0, which allows this use). Renders of them
  are not committed either.
- **Manual check (before M3 is done):** a Riglet-exported `.inp` opens and
  renders correctly in Inochi Creator 0.8.6 and Inochi Session 0.8.7.
- **Color conversion:** unit test of the tint/screen identity over a grid
  of (C, M, S).
- **Nesting:** a node tree deeper than serde's limit fails with
  `INP_NESTING_TOO_DEEP`.

## 10. Porting and licenses

The plan is to implement from the format as understood, not to port code.
One exception: **`reInterpolate`** (`core/param/binding.d`, the fill of
unset grid points) must reproduce Inochi2D's results exactly, so our version
will follow its algorithm step by step. I will treat it as a port: the file
`crates/riglet-inochi2d/src/reinterpolate.rs` keeps Inochi2D's BSD-2-Clause
copyright notice next to the code.

Ported files (each keeps the notices of what it ports, in full):

| File | Ports | Licenses |
|---|---|---|
| `crates/riglet-inochi2d/src/reinterpolate.rs` | `ParameterBindingImpl.reInterpolate` (`core/param/binding.d`), inmath `cubic` | Inochi2D BSD-2-Clause, inmath MIT |
| `crates/riglet-inochi2d/tests/reference/mod.rs` (test only) | Inochi2D 0.8.7's vertex pipeline: `Transform.update` (`math/transform.d`), `Parameter.findOffset`/`mapValue` and the binding interpolation (`core/param/`), `MeshGroup.precalculate`/`setGroup`/`filterChildren` (`core/nodes/meshgroup/package.d`), `isPointInTriangle` (`math/triangle.d`); inmath `quat.eulerRotation`/`toMatrix` | Inochi2D BSD-2-Clause, inmath MIT |

No other file is ported.

The format documentation in `inochi2d-docs` is CC-BY 3.0; we don't copy
text from it.

## 11. Open questions (to verify before or while implementing)

1. Texture premultiplication in stored files (check a real file).
2. Exact alpha factors of Inochi2D's Multiply and LinearDodge (legacy GL
   path) vs our R4.
3. Whether Inochi2D draws mask sources with their own transform and
   deform (expected yes).
4. Composite children: whether their transforms are relative to the
   composite (expected yes, like any Node).
5. Cubic interpolation formula (for an accurate resample).
6. Whether Inochi2D preserves unknown EXT_SECT entries on save (our
   `org.riglet.model` entry relies on it for Creator round trips; Riglet
   round trips don't).
