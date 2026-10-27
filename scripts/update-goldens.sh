#!/usr/bin/env bash
# Regenerates the golden images. Never runs automatically; run it after an
# intended rendering or fixture change, review the images, then commit them
# together with the change.
#
#   scripts/update-goldens.sh           Reference goldens: renders on the
#                                       reference setup (lavapipe, Linux
#                                       x86_64) in CI and downloads them
#                                       into crates/riglet-render/tests/goldens.
#   scripts/update-goldens.sh --local   Renders with THIS machine's GPU.
#                                       For experiments only: never commit
#                                       these.
#
# Reference mode snapshots the working tree's tracked files with
# `git stash create` (nothing in your tree or index changes), pushes the
# snapshot to a temporary branch goldens/tmp-<sha>, runs the "Update
# goldens" workflow there, downloads the result, and deletes the branch.
# Needs `gh` authenticated with write access to the repository (dispatching
# a workflow). New files must be tracked (`git add`) to be included.
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

snapshot="$(git stash create)"
[ -n "$snapshot" ] || snapshot="$(git rev-parse HEAD)"
branch="goldens/tmp-${snapshot:0:12}"
repo="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"

cleanup() {
    git push -q origin --delete "$branch" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> pushing snapshot $snapshot to $branch"
git push -q origin "$snapshot:refs/heads/$branch"

echo "==> running the Update goldens workflow on $branch"
gh workflow run goldens.yml --repo "$repo" --ref "$branch"
run=""
for _ in $(seq 1 30); do
    run="$(gh run list --repo "$repo" --workflow goldens.yml --branch "$branch" --limit 1 \
        --json databaseId --jq '.[0].databaseId // empty')"
    [ -n "$run" ] && break
    sleep 2
done
[ -n "$run" ] || { echo "workflow run did not start" >&2; exit 1; }
echo "    run: https://github.com/$repo/actions/runs/$run"
gh run watch "$run" --repo "$repo" --exit-status --interval 15 >/dev/null

echo "==> downloading reference goldens into $goldens"
tmp="$(mktemp -d)"
gh run download "$run" --repo "$repo" --name goldens-lavapipe-x86_64 --dir "$tmp"
cp "$tmp"/*.png "$goldens"/
rm -rf "$tmp"
git status --short -- "$goldens"
echo "==> review the images, then commit them with your change"
