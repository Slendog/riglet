# M4 design: minimal editor (PROPOSAL, for review)

Status: PROPOSAL 2026-10-31. Nothing implemented yet. Section 9 lists the
decisions needed before starting.

M4 in CLAUDE.md: "Minimal editor: viewport, node tree, parameter sliders,
live preview." This document proposes what that means, how it fits the
existing crates, and how it is tested.

## 1. Scope

In M4:

- **Open** `.riglet`, `.inp` and `.inx` (through `riglet-inochi2d`, with the
  import warnings shown in a panel). **Save** as `.riglet`; Inochi2D export
  as in the CLI (warnings shown).
- **Viewport:** the model rendered live by `riglet-render` (the same
  pipelines and target format as the CLI, spec R1), with pan, zoom, fit to
  window and a background color choice (transparent checkerboard or a
  solid color).
- **Node tree:** the model's hierarchy with type icons, names and
  visibility. Selecting a drawable outlines its evaluated mesh in the
  viewport. A per-session "solo/hide in view" toggle that does not change
  the model.
- **Parameters:** a slider per 1D parameter, a 2D pad per 2D parameter,
  grouped by `Parameter.group`, each with "reset to default" and "reset
  all". The preview updates while dragging.
- **Status:** adapter name, frame time, and the E10 report (non-finite
  positions or values, with the first node, clickable to select it).
- **Info panel:** canvas, counts, provenance and conversion warnings.

Not in M4 (later milestones): editing geometry, keyforms, deformers or
masks; undo/redo (nothing edits the model yet, except saving); physics
(M5); motions playback (could come cheaply after M4: E9 is implemented
nowhere yet, so it needs its own evaluator piece); PSD import; the web
build.

## 2. Crates and dependencies

- New crate `crates/riglet-editor`, binary `riglet-editor`. Depends on
  `riglet-format`, `riglet-render`, `riglet-inochi2d`, `eframe` (approved
  in CLAUDE.md) and `pollster` (already used).
- `eframe 0.37` with the `wgpu` backend (not `glow`): it depends on
  `wgpu ^30.0`, the version the workspace already pins, so editor and
  renderer share one wgpu. Features: `wgpu`, `x11`, `wayland`,
  `default_fonts`, `clipboard` (clipboard comes with eframe through
  `arboard`/`smithay-clipboard`, no separate dependency), `accesskit`.
- **Native file dialogs: needs a decision** (section 9). Candidate: `rfd`
  (cross-platform; on Linux it can use the XDG portal, so no GTK
  dependency). Without it, M4 uses an in-app path field plus drag and drop
  of files onto the window (both built into eframe).
- Dependency direction stays as in CLAUDE.md: only the editor depends on
  egui.

## 3. Rendering in the editor

The editor must use the renderer's code path (R1), but today `Renderer`
creates its own wgpu device and always reads the image back to the CPU.
Proposed API changes in `riglet-render` (small, additive):

1. `Renderer::from_device(device, queue, adapter_info, options)`: wraps
   an existing device (eframe's), so the viewport texture lives on the
   same device as egui. `Renderer::new` keeps creating its own.
2. `Renderer::render_to(&model, &rig, canvas, &frame, &options, target)`:
   renders into a caller-owned `Rgba8Unorm` texture (no readback).
   `render` becomes `render_to` into its own target plus the existing
   readback, so the goldens keep covering the exact code the editor uses.
3. `FrameOptions` gains a view transform: output size in pixels, scale
   and a pan offset in model units. The CLI passes no offset, which gives
   R7's centered mapping exactly. R7 stays the rule for exported images;
   the offset is an editor convenience (non-normative, noted in R7).

The viewport texture is registered with `egui_wgpu::Renderer::
register_native_texture` and re-rendered only when something changed
(parameter, view, model, window size), not every frame.

Budgets still apply: the viewport size is capped by
`Budgets::max_output_dimension` and the adapter's limits, and errors show
in the status bar instead of panicking.

Selection outlines and the mesh wireframe are drawn with egui's painter
from `EvalOutput` positions, on top of the texture. That needs no
renderer change and no extra GPU pipeline, and skips non-finite points
(E10).

## 4. State and threading

- `EditorState` (no egui types): the loaded `Document`, `Rig`,
  `ParamValues`, one reused `EvalOutput`, selection, view, and the
  warnings list. All model logic lives here so it can be unit tested
  without a window.
- The UI layer reads `EditorState` and sends actions (`SetParam`,
  `Select`, `Open(path)`, `Save(path)`, `ResetParams`, ...). This keeps
  M4 simple and gives undo/redo a natural place later.
- **Loading runs on a background thread** (`std::thread`), so a large or
  hostile file never freezes the window; the UI shows "loading" and
  stays usable. Loads use the same `Limits` as the CLI. A failure shows
  the error (and validation issues) in a dialog and keeps the previous
  model open.
- Evaluation runs on the UI thread on demand (only when a parameter
  changes). Measured on Aka/Midori before committing to that; if it is
  too slow for 60 fps while dragging, evaluation moves to a worker.
- The renderer's `GpuModel` is rebuilt only on load.

## 5. Untrusted input

Same rules as everywhere: the editor opens files from anywhere.

- Every load goes through the existing parsers, validators and limits.
  No `unwrap` on file data; no panics (the workspace lints already deny
  them in library code; the editor follows the same lints).
- Dropped files are treated exactly like opened ones.
- Names from files (node and parameter names) are shown as plain text,
  never interpreted (egui labels don't parse markup).
- Saving writes to a temporary file next to the target and renames it
  into place, so a failed save never destroys the original.

## 6. Platforms

- Builds on all six CI runners (Linux x86_64/arm64 with X11 and Wayland,
  macOS arm64/Intel, Windows x86_64/arm64). CI builds and runs the
  editor's tests; it doesn't open a window.
- Linux: winit's X11 and Wayland backends load their system libraries at
  run time, so building needs no extra packages; to verify on the CI
  images when the crate is added.
- HiDPI: the viewport renders at physical pixels (`pixels_per_point`),
  so the preview is sharp on Retina and scaled Windows displays.

## 7. Tests

- **State layer:** unit tests on `EditorState` with Builder models and
  synthetic Inochi2D puppets: open, parameter changes, reset, selection,
  save then reopen gives an equal `Document`, a failed load keeps the
  previous model, hostile files give errors.
- **Rendering:** a render test that `render_to` plus readback equals
  `render` exactly (same adapter), and the goldens unchanged.
- **View transform:** pixel mapping tests (no offset equals R7; an
  offset moves the image by the expected pixels).
- **Startup smoke test:** not in CI (no display). Manual check list in
  the M4 report: open Aka and Midori, drag `Head:: Yaw-Pitch`, compare
  with the CLI render at the same values.
- UI-level automated tests (for example with `egui_kittest`) would need
  another dependency; not proposed for M4.

## 8. Plan (small commits, each passing CI)

1. `riglet-render`: `from_device`, `render_to`, view transform, with
   tests. No editor yet.
2. `riglet-editor` crate: window, open `.riglet`, viewport with pan and
   zoom, fit to window.
3. Parameter panel (sliders, 2D pads, groups, reset).
4. Node tree, selection outline, view-only hide/solo.
5. Inochi2D open/export, warnings and info panels, E10 status.
6. Save (atomic), background loading, error dialogs.
7. Native dialogs, if approved.

Stop for review after 8.1 (renderer API) and at the end.

## 9. Decisions needed

1. **Native file dialogs:** add `rfd`, or M4 with a path field and drag
   and drop only?
2. **Renderer API changes in section 3** (`from_device`, `render_to`,
   view transform in `FrameOptions`, and a non-normative note in R7):
   OK?
3. **Editor-only state:** the spec reserves `editor.json` in packages.
   Proposal: M4 does not write it (the view and the selection are not
   saved), so saving changes nothing but what the user changed. Defining
   `editor.json` waits until there is state worth keeping.
4. **Motions playback** in M4 or later? It needs an E9 evaluator (curves
   are stored but not evaluated anywhere yet).
