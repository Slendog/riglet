# M4 design: editor

Status: APPROVED 2026-10-31 (decisions in section 10). M4a in progress.

M4 in CLAUDE.md: "Minimal editor: viewport, node tree, parameter sliders,
live preview." This document says what that means, how it fits the
existing crates, and how it is tested.

## 1. Phases

- **M4a: viewer and basic editing.** Everything in sections 2 to 9. The
  editing operations are few (section 5), but they go through the same
  command and undo machinery that M4b extends.
- **M4b: mesh and keyform editing.** Selecting and moving vertices,
  editing a keyform's deltas at the current parameter values, adding and
  removing keyforms, deformer parenting. Builds on M4a's commands, history
  and dirty tracking; designed in its own document before it starts.
- **M4c: PSD and KRA import.** Layers to drawables with an auto-mesh,
  through `riglet-psd`; designed in its own document. Every importer gets
  a fuzz target, as always.
- **Not in M4:** physics and motion playback move to M5 together (both
  are time-stepped; the E9 motion evaluator is written there).

## 2. M4a scope

- **Open** `.riglet`, `.inp` and `.inx` (through `riglet-inochi2d`, with
  the import warnings in the warnings panel). **Save** as `.riglet`;
  **Export** to Inochi2D as in the CLI (export warnings in the panel).
- **Viewport:** the model rendered live by `riglet-render` (the same
  pipelines and target format as the CLI, spec R1), with pan, zoom, fit to
  window and a background choice (transparent checkerboard or a solid
  color).
- **Node tree:** hierarchy with type icons, names and the model's
  `visible` flag. Selecting a drawable outlines its evaluated mesh in the
  viewport. A per-session "hide in view" toggle that is view-only (section
  4).
- **Parameters:** a slider per 1D parameter, a 2D pad per 2D parameter,
  grouped by `Parameter.group`, each with "reset to default" and "reset
  all". The preview updates while dragging. Moving a slider is not an edit
  (parameter values are view state, not model data).
- **Warnings panel:** section 7.
- **Status bar:** adapter name, frame time, dirty marker, autosave state.
- **Info panel:** canvas, counts, provenance.

## 3. Crates and dependencies

- New crate `crates/riglet-editor`, binary `riglet-editor`. Library part
  (`EditorState`, commands, history, saving, recovery) has no egui types
  and is tested without a window; the binary part is the egui UI.
  Depends on `riglet-format`, `riglet-render`, `riglet-inochi2d`,
  `eframe` and `pollster` (already used).
- `eframe 0.37` with the `wgpu` backend (not `glow`): it depends on
  `wgpu ^30.0`, the version the workspace already pins, so editor and
  renderer share one wgpu. Features: `wgpu`, `x11`, `wayland`,
  `default_fonts`, `accesskit`. Clipboard comes with eframe.
- `rfd` for native file dialogs, with the `xdg-portal` backend on Linux
  (no GTK dependency). Drag and drop of files onto the window also works.
- Dependency direction stays as in CLAUDE.md: only the editor depends on
  egui.

## 4. Rendering and view state

The editor uses the renderer's code path (R1). Changes in `riglet-render`
(small, additive):

1. `Renderer::from_device(adapter, device, queue, options)`: wraps an
   existing device (eframe's), so the viewport texture lives on the same
   device as egui. `Renderer::new` creates its own device and then calls
   the same constructor.
2. `Renderer::render_to(&gpu, &output, &frame, target)`: renders into a
   caller-owned texture (no readback). `render` is `render_to` into its
   own target plus the existing readback. The CLI and the goldens go
   through `render`, so they cover the exact code the editor uses, and
   every golden stays byte-identical (checked before the change lands).
3. `FrameOptions` gains a view transform: zoom and pan, applied only in
   the final clip-space mapping of the shaders. It never touches model
   coordinates, `EvalOutput` or evaluation. The default (zoom 1, no pan)
   gives R7's mapping exactly; R7 stays the rule for exported images and
   gets a non-normative note.

The viewport area is snapped to whole physical pixels (`pixel_rect`), so
at fractional scale factors (1.25, 1.5, ...) each texel still lands on
exactly one screen pixel with nearest sampling; a test checks this for
seven scale factors and many panel positions. The viewport texture is
registered with egui-wgpu and shown directly
every frame; it is never read back to the CPU (`read_target` is for the
CLI and tests). It is re-rendered only when something changed (parameter,
view, model, window size). The renderer keeps its offscreen layers and
mask buffers for the current frame size only (one entry, replaced on a
resize) and reuses them (every pass clears
them first); a failed frame drops them. Tests resize through 300 sizes
and check with wgpu's object counters that textures and texture memory
stay bounded, for the renderer and for the editor viewport. Budgets still apply: the viewport size is capped by `Budgets::max_output_dimension`
and the adapter's limits; errors show in the status bar.

**Color check.** egui composites the viewport texture into its UI, so a
format or color-space mismatch there would shift every color (for example
a double gamma conversion from an sRGB texture view). The target is
`Rgba8Unorm` with premultiplied gamma-space values (R1), which is what
egui's texture sampling expects; a test checks that the editor's viewport
target holds exactly the bytes `riglet render` writes for the same model,
parameters and size, and `tests/fixtures/gray-ramp.riglet` (a gray ramp
next to the CLI's render of it) makes a shift visible in the manual check.

Selection outlines and the mesh wireframe are drawn with egui's painter
from `EvalOutput` positions, skipping non-finite points (E10).

**Hide in view** marks drawables and composites under a hidden node
invisible in a copy of the evaluated frame, exactly as model visibility
would (a hidden mask source still masks); no command, never dirty.

**View-only state** (zoom, pan, background, hide-in-view toggles,
selection, panel layout, parameter values) lives in memory only. It never
modifies the `Document`, so it never marks it dirty and never changes what
is saved. M4 writes no `editor.json`. Every package entry the editor does
not own, including another tool's `editor.json`, is kept verbatim on save
(`Document::files`), with a test.

## 5. Document, commands and undo/redo

- `EditorState` holds the loaded `Document`, its `Rig`, the source path
  and kind (`.riglet`, `.inp`, `.inx`), `ParamValues`, one reused
  `EvalOutput`, view state, warnings and the `History`.
- Every change to stored model data (names, defaults, ranges, later
  meshes and keyforms) is a `Command` applied through `EditorState::apply`.
  Parameter values set to preview a pose, and all view state, are session
  state: no command, no undo step, never dirty. A command returns its
  inverse, and `History` keeps undo and redo stacks of them. A new command
  clears the redo stack.
- **Merge rule** (`Merge` in the command API): a continuous edit (a value
  drag from press to release, typing in one field from focus to unfocus)
  is a `Gesture` from `EditorState::begin_gesture`. A command passed with
  `Merge::Gesture(g)` merges into the newest step if that step came from
  the same gesture, edits the same field, and no undo, redo or save came in
  between; the step keeps the state before the gesture as its undo. Two
  gestures are always two steps. `Merge::Never` is always its own step.
- After a command the document is validated and the rig rebuilt; if that
  fails, the command is undone at once and its error shown, so an invalid
  model is never kept. (M4a's commands cannot produce one; M4b's can.)
- History memory is capped by approximate bytes (every command reports
  the heap data it holds, which matters once M4b commands carry meshes or
  textures) and by step count; the oldest steps drop first.
- Dirty tracking compares the history position with the position at the
  last save: undoing back to it clears dirty, undoing or redoing away from
  it sets dirty. Reaching equal content by another route stays dirty.
- M4a commands: `RenameNode`, `SetNodeVisible` (the model's flag, not the
  view-only toggle), `RenameParameter`, `SetParameterGroup`,
  `SetParameterDefault`, `SetParameterRange`.
- Commands keep the model consistent and refuse otherwise, with a message
  naming the problem: names must not be empty (or only whitespace); a
  default must lie inside its range; a range must have min < max, finite,
  and contain the default and every keyform key on that parameter
  component (`RangeExcludesKey` names the node). Nothing is moved or
  clamped silently.
- Renames change display names only, never ids or export ids (tracking
  setups bind to those); a test exports before and after renaming and
  finds only the names changed.
- All of it is plain Rust and tested without the UI: apply, undo, redo,
  coalescing, the cap, dirty tracking, rejection of invalid results.

## 6. Loading, saving and recovery

- **Loading** runs on a background thread (`Loader`), so a large or
  hostile file never freezes the window. It uses the CLI's `Limits`. A
  failure shows the error (and validation issues) and keeps the current
  model. A panic in a reader becomes a `LoadError::Internal` dialog
  instead of taking the editor down. Each new request supersedes the one
  in progress, whose result is dropped, so a slow load never replaces a
  document opened after it. File > Open, drag and drop and the
  command-line argument all go through the same request.
- **Saving** is atomic: write to a temporary file in the target's
  directory (so the rename never crosses file systems), sync it, rename it
  over the target (replacing it, Windows included), then sync the
  directory on Unix. A failed save leaves the original untouched and
  removes the temporary file. Temporary files left by a crash are never
  read; each save removes its target's leftovers older than 10 minutes.
- **Imported files are never overwritten without asking.** A model opened
  from `.inp`/`.inx` has no save path: Save acts as Save As and proposes a
  `.riglet` next to it. Export to Inochi2D asks before replacing an
  existing file, and always asks (with the file named) when the target is
  the file the model was imported from.
- **Autosave and crash recovery:** while the document is dirty, a
  recovery copy is written every 60 seconds (atomically, as a `.riglet`)
  to the editor's data directory, with a small JSON record of the
  original path and time. A clean save or a clean close removes it. At
  start, existing recovery copies are offered: restore (opens as a new,
  dirty document with the original path remembered) or discard. Recovery
  files are read through the same parsers and limits as any other file.
  Autosave never writes over the user's file.
- Closing or opening another file with unsaved changes asks first.

## 7. Warnings panel

One list, grouped by source:

- **Import:** the `ConversionWarning`s from the import that produced the
  model, plus those recorded in `manifest.provenance`.
- **Export:** the warnings of the last export.
- **Evaluation (live):** the E10 report of the current frame (non-finite
  vertex positions or values, with the first node), updated as parameters
  change.

Each entry shows its code and message; entries with an `object_id` (or an
E10 first node) are links that select the node in the tree and the
viewport. Codes are filterable. Names from files are shown as plain text.

## 8. Untrusted input

- Every load goes through the existing parsers, validators and limits. No
  `unwrap` on file data; the editor follows the workspace lints.
- Dropped files and recovery files are treated exactly like opened ones.
- Names from files are shown as plain text, never interpreted.

## 9. Platforms and tests

- Builds on all six CI runners. CI builds and runs the editor's library
  tests; it does not open a window. Linux: winit loads X11 and Wayland at
  run time; the portal backend of `rfd` needs no system packages to build
  (checked when the crate lands).
- HiDPI: the viewport renders at physical pixels.
- Tests:
  - Renderer: `render_to` plus readback equals `render`; goldens unchanged;
    the view transform at its default equals R7; zoom and pan move the
    image by the expected pixels; evaluation output does not depend on the
    view.
  - State: open, parameter changes, reset, selection, commands and
    history (section 5), save then reopen gives an equal `Document`,
    unknown entries and `editor.json` preserved, a failed load keeps the
    previous model, hostile files give errors, view state never marks the
    document dirty.
  - Saving: atomic replace, failure leaves the original, imported sources
    are never a save target, recovery write, list, restore and discard.
  - Startup smoke test: manual (no display in CI), checklist below.

**Manual check (step 3 onward).**

1. `cargo run -p riglet-cli -- render tests/fixtures/gray-ramp.riglet -o
   ramp.png` and open `ramp.png` in an image viewer at 100%.
2. `cargo run -p riglet-editor -- tests/fixtures/gray-ramp.riglet`, press
   `1` (actual size). The 16 gray steps (0, 17, .., 255) must look like the
   viewer's: equal steps, no washed-out darks (an extra gamma decode) or
   crushed darks (an extra encode). The alpha row over the checkerboard
   must fade evenly from invisible to white.
3. Drag to pan, scroll or pinch to zoom about the cursor, `F` to fit.
   Resize the window: the ramp stays sharp (no blur at 100%).
4. Open Aka and Midori (`.inp`, `.inx`): loading shows a spinner and the
   window stays responsive; Save asks for a `.riglet` and never offers the
   imported file. Undo/redo menus stay disabled until there are edits
   (the first edit UI comes in step 4).

## 10. Decisions (2026-10-31)

1. `rfd` approved, `xdg-portal` backend on Linux.
2. Renderer API approved on two conditions: goldens byte-identical (the
   CLI goes through the new path), and the view transform never touches
   model coordinates or evaluation.
3. No `editor.json` in M4; view-only state in memory only; unknown entries
   (including another tool's `editor.json`) preserved.
4. Motion playback moves to M5 with physics; the E9 evaluator is written
   there.

## 11. Plan (small commits, each passing CI)

1. `riglet-render`: `from_device`, `render_to`, view transform, with
   tests.
2. `riglet-editor` library: `EditorState`, commands, history, dirty
   tracking, atomic save, preservation tests. **Review stop** (both need
   no UI dependency; eframe and rfd come in 3).
3. Editor binary (`eframe`, `rfd`): window, open `.riglet` (background
   load, native dialog), viewport with pan, zoom and fit, the color check
   (viewport target equals `riglet render`, gray-ramp fixture). Renderer
   target reuse landed before this step.
4. Parameter panel; node tree, selection outline, hide-in-view.
5. Inochi2D open and export, the warnings panel, E10 live entries.
6. Autosave and crash recovery (not in the editor core commit),
   unsaved-changes prompts, save dialogs.

**Review stop** at the end of M4a, with the manual checklist.
