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
  `.riglet` next to it. **Export to Inochi2D** (File menu, Cmd/Ctrl+E)
  writes through the same atomic path as Save. When the target is the
  file the model was imported from, `EditorState::export_inochi2d`
  refuses unless the call confirms that exact file
  (`NeedsSourceConfirmation`); the UI asks "Replace <file>?" naming it.
  Other existing files are confirmed by the save dialog. Exporting is not
  an edit (no undo step, dirty state unchanged). Its warnings appear in
  the warnings panel right after writing, and the panel opens if there
  are any.
- **Packages are byte-identical on every OS:** the writer fixes every
  ZIP field the host could otherwise set (made-by Unix, DOS time
  1980-01-01 00:00, 0644 files and 0755 directories, no flags, extra
  fields or comments); a test checks each entry.
- **Autosave and crash recovery** (`recovery.rs`): each running editor
  owns a session folder, `<data dir>/recovery/<pid>-<nanos>-<n>/`, and
  holds an exclusive lock on its `lock` file (`File::try_lock`) while it
  runs. The data dir is `RIGLET_DATA_DIR` if set, else
  `~/Library/Application Support/Riglet` (macOS), `%LOCALAPPDATA%\Riglet`
  (Windows), `$XDG_DATA_HOME/riglet` or `~/.local/share/riglet` (Linux);
  folders are owner-only on Unix. Without one, autosave is off and the
  status bar says why.
  - While the document is dirty, a copy (`model.riglet`) and a JSON
    record (`record.json`: absolute source path and kind, save path,
    time) are written atomically on a background thread, at most once per
    60 seconds and only when the document changed. The first copy comes
    60 seconds after the first unsaved change. An import is dirty from
    the start, so it gets a copy too.
  - The copy is removed when the document becomes clean (Save, or undo
    back to the saved state), when another document is opened, and with
    the whole session on a clean exit (nothing unsaved, or the user chose
    Discard). Autosave never writes anywhere else, so never over the
    user's file.
  - At start, sessions whose lock can be taken are orphans (the OS
    releases a lock when its process dies, so a running editor's session
    is never offered). The new editor keeps each orphan locked, so a
    second editor starting at the same time doesn't offer it too. A
    dialog lists them with the original file and age: **Restore**,
    **Discard** (asks once more) or **Decide later** (File > Recovered
    Changes… shows it again; unclaimed at exit, offered next start).
  - Restore loads the copy through the background loader, with the same
    parsers and limits as any file, and opens it with
    `EditorState::open_recovered`: dirty, with the recorded source (so
    Save still never writes over an imported file and Export over it still
    asks) and save path (only a `.riglet`). The restored changes are
    written to the new session before the old one is removed, so a crash
    in between loses nothing.
  - The folder is the user's, but its contents are read as untrusted:
    only `<digits>-<digits>-<digits>` folders, never through links,
    records capped at 64 KiB and ignored when malformed (the copy is then
    restored without its original path), sessions without a copy removed,
    and removal deletes only the session's own files.
- **Unsaved changes:** opening another file, restoring a recovered copy
  or quitting with unsaved changes asks "Save changes to <file>?" with
  Save (Save As for a model without a `.riglet`; a cancelled dialog or a
  failed save cancels the action), Discard changes or Cancel.

## 7. Warnings panel

A bottom panel (toggled from the status bar, opened automatically when
a file has import warnings or an export produced some), grouped and
labeled by source (`warnings::Origin`):

- **From import:** the `ConversionWarning`s in `manifest.provenance`,
  one group per import, labeled with the source format, version and
  import time, and "may be out of date after edits": they describe the
  model as imported.
- **Last export:** the warnings of this session's last export, labeled
  with the file name.
- **Current frame (live):** the E10 report of the current evaluation
  (non-finite vertex positions; non-finite opacity, color or draw-order
  values), with the first node. Entries are a pure function of the
  frame, which only changes when parameters or the model change, so the
  panel shows the current state without flicker and an entry disappears
  as soon as the values are finite again.

Each entry shows its code and message. Its object id is resolved
against the current document (`warnings::Target`): a node or parameter
is a link that selects it (a node is also revealed in the tree:
ancestors expanded, scrolled into view); a motion or texture is named;
an id that matches nothing (removed, or stale data in a file) is shown
as "no longer in the model" and never crashes or links. The importer
names the object that exists after import (a node with a transform is
`inochi:<uuid>:transform`), so fresh imports link fully. A code filter
narrows the list. Names from files are shown as plain text.

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

**Manual check (step 3 onward).** Run by the project owner on
2026-10-31 on the step 4 build (`f3a2770`): items 1, 2, 5, 6 and 7 passed
(the editor's gray ramp at 100% matched the CLI's PNG; posing, parameter
and node editing, undo/redo; Windows scaling at 125% and 150%). Items 3
and 4 were not reported separately. On 2026-11-07, on `5701df4`: items
3, 4, 8 and 9 passed; item 10 is explained there.

1. **[passed 2026-10-31]** `cargo run -p riglet-cli -- render tests/fixtures/gray-ramp.riglet -o
   ramp.png` and open `ramp.png` in an image viewer at 100%.
2. **[passed 2026-10-31]** `cargo run -p riglet-editor -- tests/fixtures/gray-ramp.riglet`, press
   `1` (actual size). The 16 gray steps (0, 17, .., 255) must look like the
   viewer's: equal steps, no washed-out darks (an extra gamma decode) or
   crushed darks (an extra encode). The alpha row over the checkerboard
   must fade evenly from invisible to white.
3. **[passed 2026-11-07]** Drag to pan, scroll or pinch to zoom about the cursor, `F` to fit.
   Resize the window: the ramp stays sharp (no blur at 100%).
4. **[passed 2026-11-07]** Open Aka and Midori (`.inp`, `.inx`): loading shows a spinner and the
   window stays responsive; Save asks for a `.riglet` and never offers the
   imported file. Open another file while one loads: only the last one
   opens.
5. **[passed 2026-10-31]** Parameters: drag sliders and 2D pads (double-click a pad resets);
   the title shows no unsaved-changes dot. Select a parameter name:
   rename it, empty the name (an error shows, the model keeps the old
   name), drag its default and range (one undo step per drag; a range
   that excludes the default or a key is refused with a tooltip).
6. **[passed 2026-10-31]** Nodes: select one (its mesh is outlined), untick its box (hidden in
   the viewport only, no dot in the title), toggle Visible in Properties
   (an undoable edit). Undo and redo everything above.
7. **[passed 2026-10-31]** Windows at 125% and 150% display scaling: the gray ramp at actual size
   is sharp, with no blurred or doubled rows.
8. **[passed 2026-11-07]** Open Midori.inp: the warnings panel opens with 55 import warnings,
   every one linked to a node (none "no longer in the model"). Click one
   deep in the tree: the node is selected, outlined, and the tree
   expands and scrolls to it. Filter by `MESH_EMPTY`.
9. **[passed 2026-11-07]** File > Export to Inochi2D: export Midori over `Midori.inp` itself: a
   dialog names the file before anything is written; Cancel leaves it
   untouched. Export to a new `.inp`: the status bar reports the warning
   count and the panel shows a "Last export" group. Open the export in
   Inochi Creator.
10. Drag `Head:: Yaw-Pitch` on Midori.inp to a corner: if the broken
    Creator values overflow, a live E10 entry appears and goes away when
    the pad returns to the center, without flickering.
    **[2026-11-07: no E10 entry; hair and face stretch into long streaks
    off the canvas.]** This comes from the file: Midori's `Head:: Yaw-Pitch`
    keyforms hold `deform` deltas up to about 1.5e20 px on 20 nodes
    (`DEFORM_OUT_OF_RANGE` import warnings, listed in the panel). Those are
    finite in f32 (the limit is about 3.4e38), so nothing overflows and E10
    stays silent; the triangles are drawn reaching far off the canvas.
    `meshgroup_error` shows Riglet matching the test port of Inochi2D's
    vertex pipeline on everything else (max 0.6 px), and the port reaches
    the same huge values there. Open: confirm the same streaks in Inochi
    Creator or Session with the original `Midori.inp` at that pose.
11. Autosave: open Aka.inp (an import is unsaved from the start), wait
    over a minute: the status bar says "autosaved" (hover shows the
    folder). Force-quit the editor (Activity Monitor, Task Manager or
    `kill -9`). Start it again: the recovery dialog lists Aka.inp with its
    path. Restore: the model opens with unsaved changes; Save proposes a
    `.riglet` next to Aka.inp, never Aka.inp itself.
12. Repeat 11 but choose Discard twice: the dialog closes and the next
    start offers nothing. Repeat with Decide later: File > Recovered
    Changes… shows it again, and so does the next start.
13. With unsaved changes, Quit: "Save changes to …?" Save on an import
    opens Save As; cancelling it keeps the editor open. Discard changes
    quits, and the next start offers nothing. Start two editors at once
    after a crash: only one offers the recovered changes.

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
5. Inochi2D export (atomic, source confirmation), the warnings panel
   (import, export, live E10; linked, stale-safe), ZIP determinism.
6. Autosave and crash recovery (not in the editor core commit),
   unsaved-changes prompts, save dialogs. Done: `recovery.rs`, tests in
   `tests/recovery.rs`, manual checklist items 11-13.

**Review stop** at the end of M4a, with the manual checklist.
