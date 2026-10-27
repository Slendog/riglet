/**
 * Rig Project Format — draft v0.1
 *
 * Notation: TypeScript types used as a language-neutral spec.
 * The real implementation can be in any language.
 *
 * On disk: a ZIP package (.rigproj, placeholder name) laid out like:
 *   manifest.json   Manifest
 *   model.json      Model (everything below)
 *   buffers/*.bin   Little-endian binary arrays referenced by BufferRef
 *   textures/*.png  Texture images
 *   source/         Optional original artwork (PSD/KRA) for re-import
 *   editor.json     Editor-only UI state (never affects export)
 *
 * Big arrays (vertices, keyform deltas) live in .bin buffers (glTF-style)
 * so model.json stays small, diffable and fast to parse.
 */

export type Id = string;           // stable UUID, never shown to the user
export type Vec2 = [number, number];
export type RGB = [number, number, number];

// ---------- Package-level ----------

export interface Manifest {
  format: "rigproj";
  schemaVersion: string;           // semver; readers migrate older versions
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
  code: string;                    // e.g. "KEYFORM_INTERACTION_DROPPED"
  objectId?: Id;
  message: string;
}

export interface BufferRef {
  file: string;                    // path inside the package
  byteOffset: number;
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
  repeat?: boolean;                // value wraps around
  group?: string;                  // UI folder only
  extensions?: Extensions;
}

/** One axis of one parameter. A 2d parameter exposes components 0 and 1.
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
 *   group/root     -> model space
 *   warp           -> normalized [0,1]^2 of the warp's rest grid
 *   rotation       -> the rotation deformer's local frame
 *   meshDeformer   -> model space, bound to the deformer's triangles */
interface NodeBase {
  id: Id;
  name: string;
  parent: Id | null;
  children: Id[];                  // UI order only; draw order is separate
  visible: boolean;
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
  rest: { points: BufferRef; opacity: number };
}

export interface RotationDeformerNode extends NodeBase {
  type: "rotation";                // Live2D rotation deformer, Inochi2D transform
  rest: { origin: Vec2; angle: number; scale: number; opacity: number };
}

export interface MeshDeformerNode extends NodeBase {
  type: "meshDeformer";            // Inochi2D MeshGroup
  mesh: { positions: BufferRef; indices: BufferRef };
  rest: { opacity: number };
  // Live2D export: approximated by fitting a warp deformer (emits a warning)
}

// ---------- Keyforms (the core of the rig) ----------

/**
 * A KeyformSet is an N-dimensional grid over parameter axes.
 * Each grid point stores DELTAS from the node's rest values.
 *
 * Evaluation: multilinear interpolation inside a set;
 *             multiple sets on the same node are ADDED together.
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
  axes: KeyAxis[];
  keyforms: Keyform[];             // row-major over axes, axis 0 varies fastest
}

export interface KeyAxis {
  param: ParamAxis;
  keys: number[];                  // strictly ascending parameter values
  interpolation: "linear" | "step";
}

/** Only the properties present are animated; missing = zero delta. */
export interface Keyform {
  positions?: BufferRef;           // drawable vertices / warp / mesh-deformer points
  opacity?: number;
  drawOrder?: number;
  angle?: number;                  // rotation deformers
  origin?: Vec2;
  scale?: number;
  multiplyColor?: RGB;
  screenColor?: RGB;
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
    keys: CurveKey[];
  }[];
}

export interface CurveKey {
  t: number;                       // seconds
  value: number;
  interp: "linear" | "step" | "bezier";
  handles?: [Vec2, Vec2];          // bezier in/out handles, relative
}
