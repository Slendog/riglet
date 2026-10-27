#!/usr/bin/env bash
# Pre-commit gate: formatting, lints and tests must all pass. Every step's
# exit status counts (errexit + pipefail), so no failure can be hidden.
#
#   scripts/check.sh            check the working tree
#   scripts/check.sh --staged   check exactly what is staged (the hook uses
#                               this, so unstaged edits can't make a broken
#                               commit pass)
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

if [ "${1:-}" = "--staged" ]; then
    snapshot="$(mktemp -d)"
    trap 'rm -rf "$snapshot"' EXIT
    git checkout-index --all --prefix="$snapshot/"
    # Share the build cache with the real tree to keep the hook fast.
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
    cd "$snapshot"
    printf '==> checking the staged snapshot in %s\n' "$snapshot"
fi

# Git GUIs and hooks may run without the login shell's PATH.
if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

step() {
    printf '\n==> %s\n' "$*"
    "$@"
}

step cargo fmt --all --check
step cargo clippy --workspace --all-targets -- -D warnings
step cargo test --workspace

# fuzz/ is its own nightly workspace; only check that it still compiles.
if rustup toolchain list | grep -q '^nightly'; then
    step cargo +nightly check --manifest-path fuzz/Cargo.toml
else
    printf '\n==> skipping fuzz/ check: no nightly toolchain\n'
fi

printf '\nAll checks passed.\n'
