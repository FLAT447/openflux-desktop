#!/bin/sh
# Sync the vendored engine dependencies from the OpenFlux core (openflux-server).
#
# The engine imports the core module via `replace universal-bypass-tool =>
# ../../openflux-server` and builds with `-mod=vendor`, so the core code is
# frozen inside engine/vendor at the last `go mod vendor`. This script re-vendors
# from the core checkout, records the exact core revision in engine/core.rev,
# and rebuilds to prove the refresh compiles.
#
# Usage:
#   ./scripts/update-engine.sh                # use the core found beside this repo
#   REF=origin/main ./scripts/update-engine.sh  # fetch + sync from latest upstream main
#   CORE_DIR=/path/to/openflux-server ./scripts/update-engine.sh
#   CORE_DIR=/path ./scripts/update-engine.sh REF=origin/PR-branch
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
CORE_DIR=${CORE_DIR:-"$(CDPATH= cd -- "$ROOT/../openflux-server" 2>/dev/null && pwd)"}

[ -n "$CORE_DIR" ] || { echo "error: openflux-server not found; set CORE_DIR" >&2; exit 1; }
[ -d "$CORE_DIR/.git" ] || { echo "error: $CORE_DIR is not a git checkout" >&2; exit 1; }

# Vendoring must be deterministic: refuse a dirty core tree so the snapshot matches
# a committed revision, not local WIP.
if ! git -C "$CORE_DIR" diff --quiet || ! git -C "$CORE_DIR" diff --cached --quiet; then
    echo "error: $CORE_DIR has uncommitted changes; commit or stash them first (git -C $CORE_DIR stash)" >&2
    exit 1
fi

# The local core clone may be stale (the real changes land in
# git@github.com:wlruscfd/openflux-server via PRs), so refresh its refs first. A
# fetch never touches the working tree, so it is safe.
git -C "$CORE_DIR" fetch origin >/dev/null 2>&1 || true

if [ -n "${REF:-}" ]; then
    git -C "$CORE_DIR" -c advice.detachedHead=false checkout "$REF" >/dev/null
else
    echo "note: syncing from the local checkout; use REF=origin/main to sync from latest upstream"
    BEHIND=$(git -C "$CORE_DIR" rev-list --count HEAD..origin/main 2>/dev/null || echo "?")
    [ "$BEHIND" = "0" ] || echo "warning: local core is $BEHIND commit(s) behind origin/main"
fi

CORE_REV=$(git -C "$CORE_DIR" rev-parse HEAD)
echo "==> core revision: $CORE_REV ($(git -C "$CORE_DIR" log -1 --format=%s))"
echo "==> re-vendoring engine dependencies…"
cd "$ROOT/engine"
rm -rf vendor
go mod vendor
printf '%s\n' "$CORE_REV" > core.rev
echo "==> pinned core.rev: $(cat core.rev)"
echo "==> verifying engine build (linux + windows)…"
go build -mod=vendor -o /tmp/engine-check .
go vet ./...
GOOS=windows CGO_ENABLED=0 go build -mod=vendor -o /tmp/engine-check.exe .
echo "==> engine synced to $CORE_REV"
echo "review the diff:  git -C $ROOT diff --stat  (engine/vendor, engine/core.rev)"