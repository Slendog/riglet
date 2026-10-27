# Riglet

Free, open-source rigging app for VTuber models. Own project format (`.riglet`),
imports/exports Inochi2D (`.inp`/`.inx`) and Live2D files (`.moc3` + JSON
sidecars).

## Decisions

- **Name:** Riglet. Crates are `riglet-*`, CLI binary is `riglet`, project
  file extension is `.riglet` (the spec still says `.rigproj`, a placeholder;
  rename it in the next spec version).
- **License:** MIT OR Apache-2.0. If code (not just format knowledge) is ported
  from Inochi2D (BSD-2-Clause), keep its copyright notice next to that code.
- **Stack:** Rust, wgpu (rendering, also offscreen for the CLI), egui/eframe
  (editor). Browser build later via wasm.
- **Approved dependencies:** serde, serde_json, zip, png, uuid, thiserror,
  wgpu, egui/eframe (editor only), clap (CLI only), cargo-fuzz (dev tool).
  Ask before adding anything else that is large.
- **Spec:** `docs/format/rigproj-format-v0.1.ts` (TypeScript notation,
  language-neutral). Proposed changes live in `docs/format/spec-changes.md`.

## Repo layout

```
docs/format/        format spec, spec-changes.md, moc3-notes.md, moc3.ksy
crates/
  riglet-format/    types, JSON + buffers (de)serialize, package ZIP I/O,
                    validation, migration, evaluator. No GPU, no UI, no
                    filesystem requirement (works from bytes).
  riglet-render/    wgpu renderer: evaluated frame -> pixels (window or offscreen)
  riglet-inochi2d/  .inp/.inx import/export
  riglet-live2d/    model3/physics3/motion3/exp3 JSON, moc3 module
  riglet-cli/       info / validate / render
  riglet-editor/    eframe app
fuzz/               one fuzz target per importer
tools/moc3-re/      Python + Kaitai analysis scripts
tests/fixtures/     self-made test models only
testing_files_gitignored/   third-party sample models (gitignored, never commit)
```

Dependency direction: converters depend only on `riglet-format`. The CLI
depends on format + render + converters. Only the editor depends on egui.

## Milestones (stop for review after each)

1. Format library: types, read/write package (ZIP: manifest.json, model.json,
   buffers/*.bin, textures/), schemaVersion + migration hook, rig evaluator.
   Unit tests for multilinear interpolation, additive sets, child coordinates
   under warp/rotation parents.
2. CLI: `info`, `validate`, `render --params ... -o frame.png`.
3. Inochi2D import/export with round-trip tests (import -> export -> import
   gives an equivalent model).
4. Minimal editor: viewport, node tree, parameter sliders, live preview.
5. Live2D JSON sidecars (model3, physics3, motion3, exp3, cdi3), implemented
   from observed files.
6. .moc3 reverse-engineering toolchain, then read-only importer, then exporter
   for one older moc3 version.

## Rules

- **Live2D clean-room.** Never decompile, disassemble, or copy code from Cubism
  Core, the Cubism SDK/Framework, or the Cubism Editor. Never add them as
  dependencies. Learn `.moc3` only by analyzing sample files. Write findings as
  a human-readable spec in `docs/format/moc3-notes.md` plus a Kaitai Struct
  description (`docs/format/moc3.ksy`). Implement only from that spec.
- **Never commit third-party sample models.** Keep them in
  `testing_files_gitignored/`.
- **Branding.** Never use "Live2D" in product names or branding. "Imports
  Live2D files" in docs is fine.
- **Untrusted input.** Parsers bounds-check every offset, count, and index;
  cap allocation sizes; never panic on malformed input (return errors, no
  `unwrap`/indexing on untrusted data). Every importer gets a fuzz target.
- **No silent data loss.** Every lossy conversion emits a `ConversionWarning`
  with a stable code. Unknown target-specific data goes in `extensions`.
- **Spec discipline.** If the spec is wrong or incomplete, propose a change in
  `docs/format/spec-changes.md` with reasoning instead of diverging. Bump
  `schemaVersion` when the spec changes, and add a migration.
- **Commits.** Small, reviewable, with tests. Ask before adding large
  dependencies or changing the architecture.

## Build and test

Requires the stable Rust toolchain (rustup); fuzzing also needs nightly.
The Cargo workspace is created in M1.

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo +nightly fuzz run <target>     # from the repo root, see fuzz/
```
