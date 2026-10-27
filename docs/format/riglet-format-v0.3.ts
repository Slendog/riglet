/**
 * Riglet Project Format — v0.2.1
 *
 * Notation: TypeScript types used as a language-neutral spec.
 * The real implementation can be in any language.
 * Normative rules that types cannot express are in the "Evaluation",
 * "Validation" and "Limits" sections at the end of this file.
 * Rules marked PROVISIONAL may change after verification; a change bumps
 * schemaVersion like any other spec change.
 *
 * On disk: a ZIP package (.riglet) laid out like:
 *   manifest.json   Manifest
 *   model.json      Model (everything below)
 *   buffers/*.bin   Little-endian binary arrays referenced by BufferRef
 *   textures/*.png  Texture images
 *   source/         Optional original artwork (PSD/KRA) for re-import
 *   editor.json     Editor-only UI state (never affects export)
 *
 * Big arrays (vertices, keyform deltas) live in .bin buffers (glTF-style)
 * so model.json stays small, diffable and fast to parse.
 *
 * Paths inside the package are always relative and "/"-separated.
 *
 * Changes: see docs/format/spec-changes.md (v0.2.0, v0.2.1).
 *
 * Versioning policy (schemaVersion is semver):
 *   - patch (0.2.x): clarifications only, no new fields. Any 0.2.x reader
 *     reads any 0.2.x file; no migration.
 *   - minor (0.x): additive fields. Older readers keep what they don't know
 *     via `extensions` and unknown-entry preservation. While major is 0, a
 *     reader rejects a newer minor than its own.
 *   - major: breaking changes; readers migrate older majors.
 */

export type Id = string;           // stable UUID, never shown to the user
export type Vec2 = [number, number];
export type RGB = [number, number, number];

// ---------- Package-level ----------

export interface Manifest {
  format: "riglet";                // v0.1 files said "rigproj"; migrated on read
  schemaVersion: string;           // semver, "0.2.1"; see the versioning policy
  generator: string;               // app name + version that wrote the file
  provenance: Provenance[];        // import history of this model
}

export interface Provenance {
  source: "native" | "live2d-moc3" | "inochi2d-inp" | "inochi2d-inx" | "psd" | "kra";
  sourceVersion?: string;          // e.g. moc3 header version
  importedAt: string;              // ISO 8601
  warnings: ConversionWarning[];
}

export interface ConversionWarning {
  code: string;                    // stable, e.g. "KEYFORM_INTERACTION_DROPPED"
  objectId?: Id;
  message: string;
}

export interface BufferRef {
  file: string;                    // path inside the package, under buffers/
  byteOffset: number;              // aligned to the element size (2 or 4)
  count: number;                   // number of elements
  type: "f32" | "u16" | "u32";
  components: 1 | 2;               // 2 = array of Vec2
}

/** Namespaced bag for target-specific data not modeled yet ("live2d", "inochi2d").
 *  Preserved untouched on round-trip so nothing is silently lost. */
export type Extensions = Record<string, unknown>;

// ---------- Model ----------

export interface Model {
  canvas: Canvas;
  textures: Texture[];
  parameters: Parameter[];
  root: Id;                        // root GroupNode
  nodes: Record<Id, Node>;
  physics: PhysicsRig[];
  physicsEnvironment: { gravity: Vec2; wind: Vec2 };
  motions: Motion[];
  exportIds: ExportIdMap;
  extensions?: Extensions;
}

/** Model space: canvas pixels, origin at canvas center, +Y down.
 *  Importers/exporters convert to each target's units. */
export interface Canvas { width: number; height: number; }

export interface Texture {
  id: Id;
  file: string;
  width: number;
  height: number;
  premultipliedAlpha: boolean;
}

/** Live2D needs string IDs ("ParamAngleX"); Inochi2D uses numeric UUIDs.
 *  Kept separately so renaming in the editor never breaks tracking setups. */
export interface ExportIdMap {
  live2d?: Record<Id, string>;
  inochi2d?: Record<Id, number>;
}

// ---------- Parameters ----------

export interface Parameter {
  id: Id;
  name: string;
  kind: "1d" | "2d";               // 2d = Inochi2D-style XY parameter
  min: number[];                   // length 1 or 2, matching kind
  max: number[];
  default: number[];
  repeat?: boolean;                // value wraps around (see E2)
  group?: string;                  // UI folder only
  extensions?: Extensions;
}

/** One axis of one parameter. A 1d parameter exposes component 0 only;
 *  a 2d parameter exposes components 0 and 1.
 *  Live2D export: each 2d parameter becomes two 1d parameters. */
export interface ParamAxis { parameter: Id; component: 0 | 1; }

// ---------- Node tree ----------

export type Node =
  | GroupNode
  | DrawableNode
  | WarpDeformerNode
  | RotationDeformerNode
  | MeshDeformerNode;

/** Child positions are stored in the parent's local space:
 *   group/root     -> the group's parent space (model space at the root)
 *   warp           -> normalized [0,1]^2 of the warp's rest grid
 *   rotation       -> the rotation deformer's local frame (see E5)
 *   meshDeformer   -> the same space as the mesh deformer's own positions
 *                     (its parent space), bound to its triangles (see E6) */
interface NodeBase {
  id: Id;
  name: string;
  parent: Id | null;
  children: Id[];                  // UI order and tie-break order; must agree with parent
  visible: boolean;                // model visibility, exported (E4). Editor-only
                                   // "hide while I work" state lives in editor.json.
  keyformSets: KeyformSet[];
  extensions?: Extensions;
}

export interface GroupNode extends NodeBase {
  type: "group";                   // Live2D Part, Inochi2D Node/Composite
  composite: boolean;              // render children offscreen first (Inochi2D Composite)
  rest: { opacity: number };
}

export interface DrawableNode extends NodeBase {
  type: "drawable";                // Live2D ArtMesh, Inochi2D Part
  texture: Id;
  mesh: {
    positions: BufferRef;          // rest-pose vertices, parent space
    uvs: BufferRef;
    indices: BufferRef;            // triangle list
  };
  blendMode: "normal" | "additive" | "multiply";
  culling: boolean;
  masks: MaskRef[];
  rest: {
    opacity: number;
    drawOrder: number;             // higher draws on top
    multiplyColor: RGB;
    screenColor: RGB;
  };
}

export interface MaskRef { source: Id; mode: "clip" | "invert"; }

export interface WarpDeformerNode extends NodeBase {
  type: "warp";                    // Live2D warp deformer
  rows: number;                    // cells; control points = (rows+1)*(cols+1)
  cols: number;
  rest: { points: BufferRef; opacity: number };   // parent space, row-major, x fastest
}

export interface RotationDeformerNode extends NodeBase {
  type: "rotation";                // Live2D rotation deformer, Inochi2D transform
  rest: {
    origin: Vec2;                  // parent space
    angle: number;                 // degrees, positive = clockwise on screen
    scale: number;
    opacity: number;
  };
}

export interface MeshDeformerNode extends NodeBase {
  type: "meshDeformer";            // Inochi2D MeshGroup
  mesh: { positions: BufferRef; indices: BufferRef };   // parent space
  rest: { opacity: number };
  // Live2D export: approximated by fitting a warp deformer (emits a warning)
}

// ---------- Keyforms (the core of the rig) ----------

/**
 * A KeyformSet is an N-dimensional grid over parameter axes.
 * Each grid point stores DELTAS from the node's rest values.
 *
 * Evaluation: multilinear interpolation inside a set (draw order excepted,
 *             see E3); multiple sets on the same node are ADDED together.
 *
 * Live2D import:   one set per object, axes = the parameters it's bound to.
 *                  rest = the keyform at default parameter values.
 * Inochi2D import: one set per binding (one parameter, 1 or 2 axes).
 *
 * Live2D export:   sets merged into one grid over the union of axes
 *                  (exact, but the grid can get large -> warning).
 * Inochi2D export: a set whose axes span several different parameters
 *                  has interaction terms Inochi2D can't express; it is split
 *                  into per-parameter sets and the interaction is dropped
 *                  -> KEYFORM_INTERACTION_DROPPED warning.
 */
export interface KeyformSet {
  axes: KeyAxis[];                 // at least one; no ParamAxis twice
  keyforms: Keyform[];             // row-major over axes, axis 0 varies fastest
}

export interface KeyAxis {
  param: ParamAxis;
  keys: number[];                  // strictly ascending, finite parameter values
  interpolation: "linear" | "step";
}

/** Only the properties present are animated; missing = zero delta.
 *  A property must be valid for the node type it belongs to. */
export interface Keyform {
  positions?: BufferRef;           // drawable vertices / warp / mesh-deformer points
  opacity?: number;
  drawOrder?: number;              // drawables
  angle?: number;                  // rotation deformers, degrees
  origin?: Vec2;                   // rotation deformers
  scale?: number;                  // rotation deformers
  multiplyColor?: RGB;             // drawables
  screenColor?: RGB;               // drawables
}

// ---------- Physics ----------

/** Modeled on Live2D's pendulum chains (the more expressive of the two).
 *  Inochi2D export: approximated with SimplePhysics nodes (warning if chain > 1). */
export interface PhysicsRig {
  id: Id;
  name: string;
  inputs: {
    param: ParamAxis;
    weight: number;
    drive: "x" | "y" | "angle";
    reflect: boolean;
  }[];
  pendulum: {
    position: Vec2;
    mobility: number;
    delay: number;
    acceleration: number;
    radius: number;
  }[];
  outputs: {
    param: ParamAxis;
    pendulumIndex: number;
    drive: "x" | "y" | "angle";
    scale: number;
    weight: number;
    reflect: boolean;
  }[];
  normalization: {
    position: { min: number; max: number; default: number };
    angle: { min: number; max: number; default: number };
  };
}

// ---------- Motions (idle animations, expressions) ----------

export interface Motion {
  id: Id;
  name: string;
  duration: number;                // seconds
  loop: boolean;
  tracks: {
    target: { param: ParamAxis } | { node: Id; property: "opacity" };
    keys: CurveKey[];              // strictly ascending t (see E9)
  }[];
}

export interface CurveKey {
  t: number;                       // seconds
  value: number;
  interp: "linear" | "step" | "inverseStep" | "bezier";   // segment to the next key
  handles?: [Vec2, Vec2];          // bezier only, see E9
}

/* =====================================================================
 * EVALUATION (normative)
 * =====================================================================
 *
 * E1. Order. Nodes are evaluated top-down: a node's evaluated state in its
 *     own parent space is computed first, then mapped through each ancestor
 *     deformer up to model space.
 *
 * E2. Parameter values.
 *     - A value is clamped to [min, max]. For a `repeat` parameter it is
 *       instead wrapped into [min, max).
 *     - Only the value wraps. Interpolation never wraps across the ends of
 *       a key range. For a seamless loop, put keys at min and max with
 *       equal keyforms.
 *
 * E3. Inside one KeyformSet.
 *     - Per axis, values below keys[0] / above keys[last] clamp to the end
 *       key (no extrapolation). An axis with one key is constant.
 *     - linear: in the interval [k_i, k_i+1] containing v,
 *       t = (v - k_i) / (k_i+1 - k_i).
 *     - step: the greatest key <= v (the lower key), t = 0.
 *     - All properties except drawOrder: multilinear blend of the 2^N
 *       corner keyforms with the per-axis weights.
 *     - drawOrder (PROVISIONAL, to verify against Live2D samples): never
 *       interpolated. On each linear axis take the nearest key (ties go to
 *       the lower key); on each step axis take the lower key, as for every
 *       other property. Use that one grid point's delta. This avoids
 *       flicker when two drawables cross.
 *
 * E4. Combining.
 *     - value = rest + sum over the node's sets of the set's delta, then
 *       clamped:
 *         opacity, multiplyColor, screenColor (each channel)   [0, 1]
 *         scale                                                 >= 1e-6
 *         angle, origin, positions, drawOrder                   unclamped
 *     - Effective opacity = own opacity × parent's effective opacity, for
 *       every node type (groups and deformers included).
 *     - multiplyColor / screenColor apply per drawable; no inheritance.
 *     - Draw order: drawables sorted ascending by evaluated drawOrder
 *       (higher on top). Ties break by depth-first pre-order position in
 *       the node tree (following `children` order).
 *     - Visibility: `visible: false` hides the node and its whole subtree.
 *       PROVISIONAL (decided with mask rendering in M2): whether a hidden
 *       drawable still acts as a mask source.
 *
 * E5. Rotation deformer. Local unit = 1 model pixel at rest.
 *     Let R(a) = [[cos a, -sin a], [sin a, cos a]] (a in degrees, converted).
 *     - Parent is a group/root:  p' = origin + scale * R(angle) * p.
 *     - Parent is a rotation deformer: same formula, then the parent's map.
 *     - Parent is a warp (PROVISIONAL, to verify against Live2D sample
 *       renders by black-box comparison only):
 *         J  = analytic Jacobian (E7) of the warp's current map at origin
 *         J0 = analytic Jacobian of the warp's rest map at origin
 *         rot(M) = atan2(M10 - M01, M00 + M11)
 *                  (the rotation part of M's polar decomposition; it is the
 *                  rotation closest to M, so shear is removed)
 *         dθ = rot(J) - rot(J0), converted to degrees
 *         ds = sqrt(|det J| / |det J0|)
 *         p' = W(origin) + ds * scale * R(angle + dθ) * p
 *       If |det J0| < 1e-12 or both atan2 arguments of rot(J) are 0, use
 *       dθ = 0 and ds = 1.
 *     - Parent is a mesh deformer: as for a warp, with W the affine map of
 *       the triangle bound to origin (E6) and J its linear part. A mesh
 *       deformer is the identity map at rest, so J0 = I exactly:
 *       dθ = rot(J), ds = sqrt(|det J|).
 *
 * E6. Mesh deformer binding. Derived at load, never stored.
 *     - Binding fixes one triangle per child point, chosen from the child's
 *       REST position:
 *       - the rest triangle containing it; a point on a shared edge or
 *         vertex goes to the lowest triangle index;
 *       - else the nearest triangle (Euclidean distance to the triangle);
 *         ties go to the lowest triangle index.
 *     - Each frame, the bound triangle's affine map (rest triangle onto
 *       deformed triangle) is applied to the child's CURRENT local position,
 *       i.e. after the child's own keyform deltas. Barycentrics are not
 *       clamped, so points outside the triangle extrapolate affinely. This
 *       keeps keyform deltas and extrapolation continuous.
 *     - Points that reach a mesh deformer through other deformers (for
 *       example drawable -> warp -> mesh deformer) bind using their rest
 *       positions mapped up the REST chain into the mesh deformer's space.
 *       Mesh deformers are the identity at rest, so this needs no recursion.
 *     - To verify against Inochi2D MeshGroup in M3. If Inochi2D differs, the
 *       converter emits a warning; this rule does not change silently.
 *
 * E7. Warp deformer.
 *     - Control point index: j * (cols + 1) + i (row j, column i).
 *     - For a child point (u, v): cell column i = clamp(floor(u * cols), 0,
 *       cols - 1), cell row j = clamp(floor(v * rows), 0, rows - 1);
 *       s = u * cols - i, t = v * rows - j. s and t are NOT clamped.
 *     - W(u, v) = (1-s)(1-t) P00 + s(1-t) P10 + (1-s)t P01 + st P11
 *       with P00 = P(i, j), P10 = P(i+1, j), P01 = P(i, j+1),
 *       P11 = P(i+1, j+1).
 *     - Outside [0,1]^2 the nearest edge cell's bilinear map is extended
 *       linearly (s or t leaves [0,1]). In corner regions both s and t
 *       leave [0,1], so the corner cell extrapolates along both axes.
 *     - Analytic Jacobian, columns dW/du and dW/dv:
 *         dW/du = cols * ((1-t)(P10 - P00) + t(P11 - P01))
 *         dW/dv = rows * ((1-s)(P01 - P00) + s(P11 - P10))
 *
 * E8. Masks. A drawable with masks is drawn only where (clip) or only
 *     outside where (invert) the union of its mask sources is drawn.
 *
 * E9. Motion curves.
 *     - interp on key k describes the segment from key k to key k+1. The
 *       last key's interp is ignored. Before the first key / after the last
 *       key the value holds the end key's value.
 *     - linear: straight line.
 *     - step: value_k for t_k <= t < t_k+1.
 *     - inverseStep: value_k+1 for t_k < t <= t_k+1 (value_k exactly at t_k).
 *     - bezier: cubic Bezier from key k to key k+1. handles[0] is the first
 *       control point relative to key k, handles[1] the second control point
 *       relative to key k+1, both as [dt seconds, dvalue]. Control point
 *       times are clamped into [t_k, t_k+1] so the curve stays a function of
 *       time.
 *
 * Non-normative (editor, M4): reparenting needs the inverse warp map.
 * Use Newton iteration on W with the analytic Jacobian, a bounded iteration
 * count, and a fallback (best iterate, or a search over cells) when it does
 * not converge.
 *
 * =====================================================================
 * VALIDATION (normative)
 * =====================================================================
 * A reader rejects a file that breaks any rule below with an error. It never
 * crashes or hangs. Every check that bounds an allocation runs before the
 * allocation.
 *
 * V1. Package
 *     - ZIP limits (see Limits): entry count, per-entry uncompressed size,
 *       total uncompressed size, per-entry compression ratio. Check the
 *       declared sizes AND enforce them while decompressing, because
 *       declared sizes can lie.
 *     - Entry paths: relative, "/"-separated, no "..", no absolute path, no
 *       drive letter, no backslash.
 *     - No two entries whose names differ only by case.
 *     - manifest.json and model.json present; format == "riglet" (or a
 *       migratable older value).
 * V2. Tree
 *     - Exactly one node has parent == null; it equals Model.root and is a
 *       group.
 *     - parent and children agree for every node. No node is listed twice.
 *     - No cycles; every node is reachable from root.
 *     - A drawable has no children (no node space is defined for them; see
 *       Known gaps).
 * V3. References
 *     - Every Id reference resolves: texture, mask source, ParamAxis
 *       parameter, physics params, motion targets.
 *     - MaskRef.source is a drawable and not the masked drawable itself.
 *     - The mask graph (drawable -> its mask sources) has no cycles.
 *     - Parameter, texture and motion ids are unique.
 *     - exportIds keys refer to an existing parameter or node.
 *     - A texture file exists in the package and is a PNG.
 * V4. Parameters
 *     - min/max/default lengths match kind (1 or 2); min < max; default
 *       within [min, max].
 *     - ParamAxis.component is 0 for a 1d parameter, 0 or 1 for a 2d one.
 * V5. Keyforms
 *     - axes.length >= 1; no ParamAxis appears twice in one set.
 *     - Every axis has >= 1 keys, strictly ascending and finite.
 *     - keyforms.length == product of the axes' key counts.
 *     - Keyform properties are valid for the node type.
 *     - A keyform positions buffer has the same element count as the node's
 *       rest buffer (drawable positions, warp points, mesh-deformer
 *       positions).
 * V6. Buffers
 *     - file under buffers/; byteOffset aligned to the element size (4 for
 *       f32/u32, 2 for u16); byteOffset + count * components * size <= file
 *       length.
 *     - positions, uvs, warp points: f32, components 2.
 *     - indices: u16 or u32, components 1, count % 3 == 0, every index <
 *       the vertex count.
 *     - Drawable uvs.count == positions.count.
 * V7. Geometry
 *     - Warp: rows >= 1, cols >= 1, points.count == (rows+1) * (cols+1).
 *     - Mesh deformer: no zero-area rest triangle (|area| < 1e-9 counts as
 *       zero).
 * V8. Numbers: all numbers are finite (no NaN, no Inf).
 * V9. Motions: keys strictly ascending in t; handles present iff interp is
 *     bezier.
 *
 * =====================================================================
 * LIMITS (normative defaults)
 * =====================================================================
 * Readers enforce these caps. A reader may let the user raise them, but a
 * file within these defaults must load in every conforming reader.
 *
 *   ZIP entries                         10 000
 *   ZIP total uncompressed size         2 GiB
 *   ZIP per-entry uncompressed size     512 MiB
 *   ZIP per-entry compression ratio     1000:1 (entries > 1 MiB only;
 *                                       zero-heavy delta buffers compress
 *                                       well, so a lower ratio would
 *                                       reject real files)
 *   Nodes                               65 536
 *   Vertices per mesh / warp            65 536
 *   Vertices total                      4 194 304 (rest geometry)
 *   Keyform vertices total              33 554 432 (decoded keyform
 *                                       positions, checked before decoding:
 *                                       many keyforms may share bytes)
 *   Keyform grid size per set           65 536 (product of key counts)
 *   Texture width / height              16 384 (checked from the PNG
 *                                       header before decoding)
 *
 * =====================================================================
 * LOADER RESOURCE LIMITS (implementation-defined, not format rules)
 * =====================================================================
 * The Validation and Limits sections above decide whether a file is valid.
 * Separately, an implementation may refuse a valid file whose processing
 * exceeds its own resource budgets (time, memory, work). Such a refusal is
 * not a validity verdict: the same file may load in another reader, or in
 * the same reader with a larger budget. Budgets must be deterministic for a
 * given file (counted work, never wall-clock time), so a file loads or fails
 * the same way on every platform.
 *
 * Riglet's budgets (documentation only; other readers choose their own):
 *   Mesh-deformer binding work           2^28 units (one unit = one
 *                                       candidate point-vs-triangle test
 *                                       or one grid cell visited by
 *                                       Riglet's binding index); about 2 s
 *                                       in release on current hardware.
 *                                       Issue code LIMIT_MESH_BINDING.
 *
 * Every other LIMIT_* check measures file contents (counts and sizes), so
 * those are format limits (see Limits), not loader budgets.
 *
 * =====================================================================
 * KNOWN GAPS (tracked; resolved by future spec versions)
 * =====================================================================
 * Inochi2D (M3):
 *   - Sparse bindings (per-point "is set"): add an optional `defined` mask
 *     on KeyformSet, or fill on import and keep the mask in extensions.
 *   - A transform on every node (Parts too); relative, cumulative zSort.
 *   - Multiple textures per Part (albedo, emissive, bump).
 *   - Composite blend mode and tint.
 *   - Cubic interpolation of bindings.
 *   - Inochi2D allows children under Parts, which V2 rejects. The importer
 *     restructures them without loss (insert a group, keep transforms),
 *     with a round-trip test.
 *   - Inochi2D packages can hold non-PNG textures (e.g. TGA, BC7); the
 *     importer converts them to PNG.
 * Live2D (M5/M6):
 *   - Rotation deformer reflect X/Y.
 *   - Warp quad vs triangle cell interpolation.
 *   - Expressions (exp3: Add/Multiply/Overwrite) as their own type.
 *   - Motion fade-in/out and user-data events.
 *   - Display info (cdi3.json).
 *   - Cubism 5 blend shapes (likely maps to additive sets).
 */
