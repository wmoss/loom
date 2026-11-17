#!/usr/bin/env bash
# Back up a compose-deployed loom: a consistent sqlite snapshot plus git
# bundles of the managed repo store, extracted out of the container into
# $DEST/loom-<timestamp>/ — a small directory that restic (or rsync, or any
# off-device tool) can pick up as-is. Run it on the docker host, from the
# directory holding docker-compose.yml (or set COMPOSE_FILE):
#
#   deploy/standalone $ ./backup.sh
#   $ restic backup /path/to/loom-backups
#
# What is captured — the irreplaceable state, ~1 GB here vs ~200 GB of volume:
#   * weaver.db    sessions, ACP chat journals, channels, artifacts, issues,
#                  users + API tokens, settings. Snapshotted with sqlite's
#                  online .backup, which is consistent while loom keeps
#                  writing (a raw file copy of a WAL db is not).
#   * repos/*.bundle   every managed repo's full ref set, including branches
#                  that were never pushed to GitHub. Worktree checkouts are
#                  reconstructable from these + the db, so the 99% of the
#                  volume that is .worktrees build junk is skipped.
#
# Deliberately not captured:
#   * ~/.iris conversation captures. For ACP sessions (the default) they are
#     derived from the chat journal already inside weaver.db; only
#     terminal-protocol sessions have conversations that live nowhere else.
#     Set INCLUDE_IRIS=1 if you run those.
#   * uncommitted worktree changes and scratch/ files — git-ignored by design.
#   * logins (~/.claude.json, gcloud): one-time conveniences, not data.
#
# Restore: fresh deploy -> stop loom -> restore weaver.db into $WEAVER_HOME ->
# `git clone <bundle> <owner>/<name>` per repo -> start loom; sessions come
# back as "orphaned" and can be adopted from the dashboard.
#
# Configuration (env): DEST (default ./loom-backups), COMPOSE (default
# `docker compose`; e.g. `COMPOSE="docker compose -p loom"` from elsewhere),
# COMPOSE_FILE, SERVICE (default loom), KEEP (newest local run dirs kept,
# default 2; 0 keeps all — restic snapshots are the real history).

set -Eeuo pipefail

DEST=${DEST:-./loom-backups}
SERVICE=${SERVICE:-loom}
CONTAINER_HOME=${CONTAINER_HOME:-/home/app}
KEEP=${KEEP:-2}
INCLUDE_IRIS=${INCLUDE_IRIS:-0}
COMPOSE=${COMPOSE:-docker compose}

compose() { $COMPOSE "$@"; }

STAMP=$(date -u +%Y%m%dT%H%M%SZ)
WH="$CONTAINER_HOME/.weaver"
STAGE="$WH/backups/$STAMP"
RUN_DIR="$DEST/loom-$STAMP"

mkdir -p "$DEST"
LOCK="$DEST/.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
  echo "refusing to run: $LOCK exists — another backup may be in flight," \
    "or a previous run crashed; remove it if stale" >&2
  exit 1
fi

ok=0
cleanup() {
  compose exec -T "$SERVICE" rm -rf "$STAGE" >/dev/null 2>&1 || true
  [ "$ok" = 1 ] || rm -rf "$RUN_DIR"
  rmdir "$LOCK" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> snapshotting inside the container ($STAGE)"
compose exec -T "$SERVICE" sh -ceu '
  set -eu
  stage=$1 wh=$2 home=$3 include_iris=$4
  mkdir -p "$stage/repos"

  # sqlite online backup: consistent snapshot while the server keeps writing.
  sqlite3 "$wh/weaver.db" ".backup $stage/weaver.db"
  check=$(sqlite3 "$stage/weaver.db" "PRAGMA quick_check;")
  if [ "$check" != "ok" ]; then
    echo "snapshot failed sqlite quick_check: $check" >&2
    exit 1
  fi

  # One bundle per managed repo (<owner>/<name>): all refs, unpushed weaver/*
  # branches included.
  found=0
  for repo in "$wh"/repos/*/*/; do
    [ -d "${repo%/}/.git" ] || continue
    repo=${repo%/}
    found=1
    name=$(basename "$(dirname "$repo")")-$(basename "$repo")
    echo "  bundle: $name"
    git -C "$repo" bundle create "$stage/repos/$name.bundle" --all \
      >/dev/null 2>&1
    # bundle verify needs a repository context; run it from the source repo.
    git -C "$repo" bundle verify -q "$stage/repos/$name.bundle" >/dev/null 2>&1
  done
  if [ "$found" = 0 ]; then
    echo "  warning: no managed repos found under $wh/repos" >&2
  fi

  if [ "$include_iris" = 1 ] && [ -d "$home/.iris" ]; then
    echo "  iris: conversation captures"
    tar czf "$stage/iris.tgz" -C "$home" .iris
  fi
' sh "$STAGE" "$WH" "$CONTAINER_HOME" "$INCLUDE_IRIS"

echo "==> extracting to $RUN_DIR"
mkdir -p "$RUN_DIR"
compose exec -T "$SERVICE" tar cf - -C "$STAGE" . | tar xf - -C "$RUN_DIR"

echo "==> checksumming"
if command -v sha256sum >/dev/null 2>&1; then
  hash=sha256sum
else
  hash="shasum -a 256"
fi
(
  cd "$RUN_DIR"
  find . -type f ! -name SHA256SUMS -exec $hash {} + > SHA256SUMS
  $hash -c SHA256SUMS >/dev/null
)

if [ "$KEEP" -gt 0 ]; then
  ls -1dt "$DEST"/loom-*/ 2>/dev/null \
    | tail -n +"$((KEEP + 1))" \
    | while IFS= read -r dir; do rm -rf "$dir"; done
fi

ok=1
echo "==> done"
du -sh "$RUN_DIR"
ls -lh "$RUN_DIR" "$RUN_DIR/repos"
echo "restic backup $DEST"
