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
- **Spec:** `docs/format/riglet-format-v0.4.ts` (TypeScript notation,
  language-neutral; `schemaVersion` "0.3.0", versioning policy in its
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
  riglet-cli/       info / validate / render / import / export (binary `riglet`)
  riglet-fixtures/  dev-only: model Builder + one render fixture per feature
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
5. Physics: evaluate the format's `PhysicsRig` (parameters driven by
   simulated motion), with one model that covers both Inochi2D
   SimplePhysics nodes and Live2D physics3 pendulum chains, and the
   importers/exporters mapped onto it. Essential for real VTuber use (hair,
   clothes and accessories swing); until then both importers keep physics
   in `extensions` only (`SIMPLE_PHYSICS_UNSUPPORTED`).
   - The simulation runs on a fixed timestep and is deterministic (same
     inputs and step count give bit-identical outputs on every platform, no
     wall-clock or frame-rate dependence), so it can be tested exactly.
6. Live2D JSON sidecars (model3, physics3, motion3, exp3, cdi3), implemented
   from observed files.
7. .moc3 reverse-engineering toolchain, then read-only importer, then exporter
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
- **Timing tests:** wall-clock limits (binding rejection < 5 s, etc.) are
  the same on every runner; there is no per-runner scale. If a runner
  misses a limit, report it instead of loosening the limit.
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
- **Golden images:** render tests compare against goldens with a two-tier
  tolerance (below). Fixtures are Rust code in
  `crates/riglet-fixtures/src/fixtures.rs` (one per rendering feature);
  goldens live in `crates/riglet-render/tests/goldens/`.
  - **Reference setup:** lavapipe (Mesa's software Vulkan), Linux x86_64,
    `WGPU_BACKEND=vulkan`.
  - **Skips must not look like passes.** Every CI runner sets
    `RIGLET_REQUIRE_GPU=1` and expects a specific backend and adapter
    (`RIGLET_EXPECT_BACKEND`, `RIGLET_EXPECT_ADAPTER`): Vulkan/llvmpipe
    (lavapipe) on Linux, DX12/"Microsoft Basic Render Driver" (WARP) on
    Windows, Metal on macOS (Apple Paravirtual device, both arm64 and
    Intel), and GL/llvmpipe (Mesa EGL) with wgpu's WebGL2 downlevel limits
    in the `ubuntu-24.04-gl-webgl2` job (`RIGLET_DEVICE_LIMITS=webgl2`), so
    the GL backend and the 2048 px texture limit are tested for real. No adapter, another backend or another adapter fails the job,
    so a silent fallback (e.g. to GL) can't pass. Every job also writes its
    adapter to the run summary.
  - **Skip allow-list (runners where render tests may skip): empty.** Add
    a runner (`allow_skip: true` in `ci.yml`) only after it is shown to
    have no adapter, with the justification here.
  - **Tolerance (two tiers, `mod tolerance` in `tests/render.rs`), on
    premultiplied values:** every pixel within 8 per channel; at most 0.5%
    of pixels (minimum 4) beyond 2. Premultiplied because un-premultiplying
    amplifies GPU rounding at low alpha (1 step became 6 at alpha 48). Measured: texel-aligned fixtures match the reference exactly or
    within 2 on all six CI adapters. Fixtures avoid magnified filtered
    textures (those differed by up to 24 between adapters); bilinear
    filtering is covered by `uv_orientation`.
  - **Exact math is tested exactly:** the premultiply and un-premultiply
    integer formulas are checked against an oracle for all 256 x 256
    pairs (`image.rs` unit tests), never only through a toleranced golden.
  - **Updating goldens is explicit:** `scripts/update-goldens.sh`, never
    automatic. It snapshots the STAGED state as a temporary commit object
    with git plumbing (`git write-tree` + `git commit-tree`: no hooks run,
    no branch moves), pushes it to a unique temporary branch
    `goldens/<UTC timestamp>-<sha>` on the public repo (deleted on
    success, failure or interrupt), renders on lavapipe in CI, and
    downloads images only from the run it triggered (checked by commit
    SHA). It refuses unstaged changes and untracked files and never pushes
    outside `goldens/`. Flow: `git add` the change, run the script, review
    the images, `git add` them, make ONE normal commit with change and
    goldens (it passes the gate). There is no sanctioned hook bypass
    anywhere. `--local` renders with your own GPU for experiments; never
    commit those.
  - On a mismatch, expected/actual/diff images go to
    `target/tmp/render-failures/` and CI uploads them as the
    `render-failures-<os>` artifact. Never set `WGPU_BACKEND` to an
    empty string: wgpu reads that as "no backends" and every render test
    silently skips.
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
cargo +nightly fuzz run texture_png fuzz/corpus/texture_png fuzz/seeds/texture_png -- -timeout=10 -max_len=65536
cargo +nightly fuzz run inp_container fuzz/corpus/inp_container fuzz/seeds/inp_container -- -timeout=10 -max_len=65536
cargo +nightly fuzz run inp_json fuzz/corpus/inp_json fuzz/seeds/inp_json -- -timeout=10 -max_len=16384
cargo +nightly fuzz run tga fuzz/corpus/tga fuzz/seeds/tga -- -timeout=10 -max_len=65536
cargo +nightly fuzz run inp_export fuzz/corpus/inp_export fuzz/seeds/package -- -timeout=10 -max_len=16384
cargo test --test malformed -- --ignored write_fuzz_seeds          # regenerate seeds after format changes
cargo test -p riglet-inochi2d --test malformed -- --ignored        # regenerate Inochi2D seeds
# Inochi2D sample models (testing_files_gitignored/inochi2d/): warning counts,
# timing; RIGLET_SHOW_WARNINGS=CODE prints them, RIGLET_RENDER_DIR renders.
cargo test --release -p riglet-inochi2d --test real_models -- --ignored --nocapture
# ... against a test-only port of Inochi2D's vertex pipeline (pixel errors)
cargo test --release -p riglet-inochi2d --test meshgroup_error -- --ignored --nocapture
# ... import -> export -> import
cargo test --release -p riglet-inochi2d --test round_trip -- --ignored --nocapture
```

## Fuzzing policy

- **Per push (CI):** the fuzz crate must build, plus a ~60 s run per
  target from the committed seeds.
- **Weekly (`fuzz-weekly.yml`, Sundays; also manual via workflow_dispatch
  with a `minutes` input):** 30 min per target on Linux x86_64. The corpus
  persists between runs with actions/cache; the committed seeds in
  `fuzz/seeds/` are always the baseline. It blocks nothing, but a crash
  fails the job, uploads the input, and opens an issue.
- **Locally:** no long fuzz sessions as part of normal work. Only when a
  change touches parsing, validation, binding or other untrusted-input
  code: 10 minutes per affected target, unless the maintainer asks for
  more. Don't block on a running session.
- **Every crash input, from any run, becomes a regular regression test:**
  put the file in `crates/riglet-format/tests/regressions/<target>/`
  (package, model_json), `crates/riglet-render/tests/regressions/texture_png/`
  or `crates/riglet-inochi2d/tests/regressions/<target>/` (inp_container,
  inp_json, tga, inp_export), in the same commit as the fix. `tests/regressions.rs`
  (`regressions()` in `tests/malformed.rs` for Inochi2D) replays every file
  through the same path as the fuzz target.

`fuzz/` is its own Cargo workspace (it needs nightly), so `--workspace`
commands skip it. Check that it still compiles with
`cargo check --manifest-path fuzz/Cargo.toml`.

## CLI (`riglet`)

- `riglet info FILE [--json]`, `riglet validate FILE [--json]`,
  `riglet render FILE -o OUT.png [-p NAME=VALUE]... [--scale S]
  [--background RRGGBB] [--software]`,
  `riglet import FILE.inp|FILE.inx -o OUT.riglet [--json]`,
  `riglet export FILE.riglet -o OUT.inp [--json]` (both print every
  `ConversionWarning`; warnings don't change the exit code).
- **Exit codes (stable):** 0 = OK, 1 = the file is invalid (validation
  issues, not a readable package, or an Inochi2D file that can't be
  imported), 2 = error (I/O, no GPU adapter, bad arguments).
- `-p NAME` matches a parameter id, name, Live2D id or Inochi2D id;
  `id:`, `name:`, `live2d:`, `inochi2d:` restrict it to one kind.
  Ambiguous matches are an error listing the candidates; never guess.
- Arguments are untrusted (the CLI may sit behind a web service): values
  must be finite, scale positive, output at most 8192 x 8192, `--param` at
  most 1024 bytes.

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
