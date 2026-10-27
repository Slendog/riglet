# Riglet

Free, open-source rigging app for VTuber models. Own project format (`.riglet`),
imports/exports Inochi2D (`.inp`/`.inx`) and Live2D files (`.moc3` + JSON
sidecars).

## Decisions

- **Name:** Riglet. Crates are `riglet-*`, CLI binary is `riglet`, project
  file extension is `.riglet`, manifest `format: "riglet"`.
- **License:** MIT OR Apache-2.0. If code (not just format knowledge) is ported
  from Inochi2D (BSD-2-Clause), keep its copyright notice next to that code.
- **Stack:** Rust, wgpu (rendering, also offscreen for the CLI), egui/eframe
  (editor). Browser build later via wasm.
- **Platforms:** Linux, macOS, and Windows are all first-class (x86_64 and
  arm64). wgpu backends: Vulkan (Linux, Windows), Metal (macOS), DX12
  (Windows), with GL as fallback. Every change must build and pass tests on
  all three. See "Cross-platform rules" below.
- **Approved dependencies:** serde, serde_json, zip, png, uuid, thiserror,
  wgpu, egui/eframe (editor only), clap (CLI only), cargo-fuzz (dev tool).
  Ask before adding anything else that is large.
- **Spec:** `docs/format/riglet-format-v0.3.ts` (TypeScript notation,
  language-neutral; `schemaVersion` "0.2.1", versioning policy in its
  header). Its Evaluation (E*),
  Validation (V*) and Limits sections are normative; cite rule IDs in code
  comments and tests. Rules marked PROVISIONAL may change after verification.
  Proposals and reasoning live in `docs/format/spec-changes.md`; open issues
  in the spec's "Known gaps" section.

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
2. CLI: `info`, `validate`, `render --params ... -o frame.png`. Starts only
   after CI is green on all platforms.
   - `render` must work without a GPU: use wgpu's software fallback
     (lavapipe/llvmpipe on Linux, WARP on Windows). Render tests skip with
     a clear message when no adapter exists, instead of failing.
   - Masks, composites and blend modes are new here: golden-image tests
     need a synthetic fixture for each one.
   - Decide whether a hidden drawable still acts as a mask source (spec E4,
     PROVISIONAL).
3. Inochi2D import/export with round-trip tests (import -> export -> import
   gives an equivalent model).
   - Inochi2D allows children under Parts (spec V2 does not): restructure
     without loss (insert a group, keep transforms), with a round-trip test.
   - Convert non-PNG textures (TGA, BC7, ...) to PNG.
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

## Cross-platform rules

- **CI matrix:** `ubuntu-24.04`, `ubuntu-24.04-arm`, `macos-latest` (arm64),
  `macos-15-intel`, `windows-latest`, `windows-11-arm` run fmt, clippy and
  tests on every push. A change is not done until all of them pass. If a
  runner becomes unavailable, report it; never drop a job silently.
  `macos-15-intel` is GitHub's last Intel image (announced until about
  August 2027). **Decision needed before then:** drop Intel macOS
  support, keep it untested, or find other Intel CI.
- **Toolchain pin:** `rust-toolchain.toml` pins Rust (currently 1.99.1) so a
  new stable release can't break CI overnight. Dependabot can't update this
  file, so the weekly `latest-stable` workflow is the signal: once it has
  been green on a new stable for a couple of weeks, bump the pin in its own
  commit (fixing any new lints in that commit). `rust-version` in
  `Cargo.toml` follows the pin. `latest-stable` never blocks anything.
- **Actions** are pinned to full commit SHAs with the version in a comment.
  Dependabot (`.github/dependabot.yml`) proposes updates weekly. The
  toolchain comes from the preinstalled `rustup`, not a third-party action.
- **Paths:** use `std::path::Path`/`PathBuf` for OS paths, never string
  concatenation with `/` or `\`. Paths *inside* packages (ZIP entries,
  `BufferRef.file`, texture files) are always `/`-separated and relative;
  readers reject `\`, absolute paths, drive letters, and `..`.
- **Case and names:** never rely on filesystem case sensitivity (macOS and
  Windows are case-insensitive by default). Treat package entry names that
  differ only by case as a validation error.
- **Text and bytes:** binary data is always little-endian, read with explicit
  `from_le_bytes`, never by casting memory. Write JSON with `\n` line endings.
  `.gitattributes` forces LF in the repo and marks binaries as binary.
- **No platform-specific code** outside `cfg` blocks with an implementation
  for every target. Prefer crates that already cover all three platforms.
- **Float tests:** evaluator tests compare floats with explicit tolerances,
  never exact equality. Results differ slightly between x86_64 and arm64.
- **GPU in tests:** headless render tests must run without a real GPU: CI
  uses Mesa lavapipe (Linux), WARP (Windows), and Metal (macOS).
- **Golden images:** render tests use a per-pixel tolerance plus a maximum
  number of differing pixels, because GPU backends rasterize slightly
  differently. Goldens live in `tests/fixtures/` and come from our own
  synthetic models only.
- **Editor:** native file dialogs and clipboard via cross-platform crates
  (ask before adding them in M4). Linux builds need X11 and Wayland support.

## Build and test

Requires rustup; `rust-toolchain.toml` selects the pinned toolchain.
Fuzzing also needs `rustup toolchain install nightly` and
`cargo +nightly install cargo-fuzz` (always run it as `cargo +nightly fuzz`;
the pin would otherwise select stable). CI (`.github/workflows/ci.yml`) runs fmt,
clippy and tests on Linux, macOS and Windows (x86_64 and arm64), plus a short
fuzz run.

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo test --release -p riglet-format --test binding -- --ignored   # worst-case binding time
# Fuzz from the repo root; corpus/ is gitignored, seeds/ is committed.
cargo +nightly fuzz run package fuzz/corpus/package fuzz/seeds/package -- -timeout=10 -max_len=16384
cargo +nightly fuzz run model_json fuzz/corpus/model_json fuzz/seeds/model_json -- -timeout=10 -max_len=16384
cargo test --test malformed -- --ignored write_fuzz_seeds          # regenerate seeds after format changes
```

Fuzz crashes become regression tests in
`crates/riglet-format/tests/malformed.rs` (input under
`crates/riglet-format/tests/regressions/`), not just corpus files.

`fuzz/` is its own Cargo workspace (it needs nightly), so `--workspace`
commands skip it. Check that it still compiles with
`cargo check --manifest-path fuzz/Cargo.toml`.

## Commit gate

Every commit must pass `scripts/check.sh` (fmt --check, clippy
`-D warnings`, tests, fuzz crate compiles). The hook runs it with
`--staged`, on a checkout of the index, so what is checked is exactly what
is committed. Enable the hook once per clone:

```sh
git config core.hooksPath .githooks
```

- Never pipe a check command without `set -o pipefail` (or don't pipe it):
  a pipe hides the command's exit status. The script uses
  `set -euo pipefail`.
- Every commit in history must build and pass on its own (keeps
  `git bisect` useful). Before pushing, fix broken unpushed commits by
  squashing, not with a follow-up commit.

## Code conventions

- Lints (workspace `Cargo.toml`): `unsafe_code` is forbidden; `unwrap`,
  `expect` and slice indexing warn in library code (CI denies warnings).
  Use `get()` and return errors instead. Tests may use them
  (`clippy.toml`, and an `#![allow]` at the top of each integration test).
- `rustfmt.toml`: max width 120.
- Cite spec rule IDs (E*, V*) in comments next to the code that implements
  them. Validation issue codes (`V5_KEYS`, `LIMIT_NODES`, ...) are stable:
  never rename one, only add new ones.
- Evaluator API: `rig.evaluate(&params, &mut EvalOutput)`. Callers keep one
  `EvalOutput` per rig and reuse it every frame. `EvalOutput::drawables`
  slots are stable (node pre-order); `draw_order` lists slots bottom first.
- Untrusted input must not make loading slow either: any work that grows
  faster than linear in input size needs a cap or an index (see `bind.rs`
  and `Limits::binding_work_budget`), plus an adversarial timing test.
  Budgets count work deterministically (never wall-clock time) and are
  loader resource limits, not format rules (spec "Loader resource
  limits"). Their issue codes say so in the message.
- The dev profile builds `riglet-format` at opt-level 2 (workspace
  `Cargo.toml`), so debug builds of tests, CLI and editor load realistic
  files at realistic speed. Debug assertions stay on.
- `riglet-format` layout: `model` (serde types, on-disk shape only),
  `package` (ZIP I/O + limits), `migrate`, `buffer`, `rig` (validation ->
  prepared `Rig` with indices instead of IDs), `eval` (evaluator),
  `geometry` (warp/rotation/triangle math), `bind` (mesh-deformer binding
  grid).
- Tests: unit tests next to the code; integration tests in
  `crates/riglet-format/tests/` build synthetic models with
  `tests/common/mod.rs` (`Builder`). Malformed-input smoke tests
  (`tests/malformed.rs`) run on stable; every new importer adds both a smoke
  test and a fuzz target.
