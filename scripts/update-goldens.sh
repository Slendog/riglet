#!/usr/bin/env bash
# Regenerates the golden images. Never runs automatically.
#
#   scripts/update-goldens.sh           Reference goldens for the current
#                                       COMMIT: rendered on the reference
#                                       setup (lavapipe, Linux x86_64) in CI,
#                                       downloaded into
#                                       crates/riglet-render/tests/goldens.
#   scripts/update-goldens.sh --local   Renders with THIS machine's GPU.
#                                       For experiments only: never commit
#                                       these.
#
# Reference mode pushes the current commit to a temporary, uniquely named
# branch goldens/<UTC timestamp>-<short sha> on the PUBLIC repository, runs
# the "Update goldens" workflow there, downloads the images from exactly
# that run (verified by commit SHA), and deletes the branch on success,
# failure or interrupt. It refuses to run with uncommitted changes and
# never pushes to any branch outside goldens/.
#
# Flow for a rendering or fixture change (CLAUDE.md, "Golden images"):
#   1. Commit the change on a work branch with `git commit --no-verify`
#      (its goldens are stale, so the pre-commit gate would fail).
#   2. scripts/update-goldens.sh
#   3. Review the images, `git add` them, `git commit --amend` (this time
#      through the gate, which must pass), then merge.
#
# Needs `gh` authenticated with write access to the repository.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
goldens="crates/riglet-render/tests/goldens"

if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

if [ "${1:-}" = "--local" ]; then
    echo "==> rendering goldens with this machine's GPU (NOT the reference; don't commit)"
    RIGLET_REQUIRE_GPU=1 RIGLET_UPDATE_GOLDENS=1 cargo test -p riglet-render --test render goldens -- --nocapture
    git status --short -- "$goldens"
    exit 0
fi
if [ $# -gt 0 ]; then
    echo "usage: scripts/update-goldens.sh [--local]" >&2
    exit 2
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "refusing to run: uncommitted changes (commit them first, see the header)" >&2
    exit 2
fi
if [ -n "$(git ls-files --others --exclude-standard)" ]; then
    echo "refusing to run: untracked files would not be part of the render" >&2
    exit 2
fi

sha="$(git rev-parse HEAD)"
branch="goldens/$(date -u +%Y%m%dT%H%M%SZ)-${sha:0:12}"
case "$branch" in
    goldens/*) ;;
    *) echo "internal error: refusing to push to $branch" >&2; exit 2 ;;
esac
repo="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"

pushed=0
cleanup() {
    if [ "$pushed" = 1 ]; then
        git push -q origin --delete "refs/heads/$branch" 2>/dev/null || echo "warning: could not delete $branch" >&2
    fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

echo "==> pushing $sha to $branch"
git push -q origin "$sha:refs/heads/$branch"
pushed=1

echo "==> running the Update goldens workflow on $branch"
gh workflow run goldens.yml --repo "$repo" --ref "$branch"
run=""
for _ in $(seq 1 30); do
    run="$(gh run list --repo "$repo" --workflow goldens.yml --branch "$branch" --event workflow_dispatch \
        --json databaseId,headSha --jq "map(select(.headSha == \"$sha\")) | .[0].databaseId // empty")"
    [ -n "$run" ] && break
    sleep 2
done
[ -n "$run" ] || { echo "the workflow run for $sha did not start" >&2; exit 1; }
echo "    run: https://github.com/$repo/actions/runs/$run"
gh run watch "$run" --repo "$repo" --exit-status --interval 15 >/dev/null

# Belt and braces: the run must be for exactly this commit.
run_sha="$(gh run view "$run" --repo "$repo" --json headSha --jq .headSha)"
[ "$run_sha" = "$sha" ] || { echo "run $run is for $run_sha, not $sha; not downloading" >&2; exit 1; }

echo "==> downloading reference goldens into $goldens"
tmp="$(mktemp -d)"
gh run download "$run" --repo "$repo" --name goldens-lavapipe-x86_64 --dir "$tmp"
cp "$tmp"/*.png "$goldens"/
rm -rf "$tmp"
git status --short -- "$goldens"
echo "==> review the images, then amend your commit with them (through the gate)"
