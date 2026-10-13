#!/bin/sh
# The secret scan of scripts/check.sh: gitleaks over the git history, native
# or in Docker. Normally incremental; a full-history scan runs when due.
#
#   scripts/secret-scan.sh
#
# Incremental: only the commits that are not reachable from a commit already
# scanned clean (git log HEAD --not <clean commits>), plus the staged and
# unstaged changes of the working tree (gitleaks protect). Full: all history,
# when no full scan passed in the last 24 hours (NVRVIEW_SECRET_FULL_MAX_AGE_H),
# when NVRVIEW_FULL_CHECK=1 (scripts/release-check.sh and
# scripts/release.sh --full-check set it), when the gitleaks rules or ignore
# list changed since, or when there is no record. A failed scan records
# nothing, so the next run scans the same commits again.
#
# The record is <git common dir>/nvrview-checks/secret-scan.state: full=EPOCH,
# config=HASH of .gitleaks.toml and .gitleaksignore, and one clean=COMMIT per
# line (tips of the history scanned clean; shared by all worktrees).
#
# Exit: 0 passed, 1 findings or a failed scan, 3 no gitleaks and no Docker
# (nothing scanned). For tests: NVRVIEW_GITLEAKS_CMD replaces the tool (it
# gets the gitleaks arguments), NVRVIEW_SCAN_NOW the clock.
set -eu
cd "$(git rev-parse --show-toplevel)"

now="${NVRVIEW_SCAN_NOW:-$(date +%s)}"
max_age=$(awk -v h="${NVRVIEW_SECRET_FULL_MAX_AGE_H:-24}" 'BEGIN { printf "%d", h * 3600 }')
common="$(cd "$(git rev-parse --git-common-dir)" && pwd)"
state="$common/nvrview-checks/secret-scan.state"
config=$(cat .gitleaks.toml .gitleaksignore 2>/dev/null | git hash-object --stdin)
head=$(git rev-parse HEAD)
log=$(mktemp)
trap 'rm -f "$log"' EXIT

# The tool: gl ARGS... (runs in the repository root).
if [ -n "${NVRVIEW_GITLEAKS_CMD:-}" ]; then
  gl() { sh -c "$NVRVIEW_GITLEAKS_CMD" gitleaks "$@"; }
elif command -v gitleaks >/dev/null 2>&1; then
  gl() { gitleaks "$@"; }
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  # Mounted at the same paths as on the host, with the repository's common
  # git directory too: in a git worktree .git is a file pointing there, and
  # without it git fails and gitleaks scans nothing yet exits 0.
  gl() {
    docker run --rm -v "$PWD:$PWD" -v "$common:$common" -w "$PWD" \
      zricethezav/gitleaks:v8.18.4 "$@"
  }
else
  echo "secret scan skipped (no gitleaks or Docker)"
  exit 3
fi

field() { sed -n "s/^$1=//p" "$state" 2>/dev/null || true; }

mode=full
why=""
last_full=$(field full | head -n 1)
tips=""
for t in $(field clean); do
  if git cat-file -e "$t^{commit}" 2>/dev/null; then tips="$tips $t"; fi
done
tips="${tips# }"
if [ "${NVRVIEW_FULL_CHECK:-0}" = 1 ]; then
  why="NVRVIEW_FULL_CHECK=1"
elif [ -z "$last_full" ] || [ -z "$tips" ]; then
  why="no earlier scan on record"
elif [ "$(field config | head -n 1)" != "$config" ]; then
  why="the gitleaks rules or ignore list changed"
elif [ $((now - last_full)) -gt "$max_age" ]; then
  why="the last full scan was $(((now - last_full) / 3600)) h ago"
else
  mode=incremental
fi

# Scans the commits (everything, or what is new); verifies that gitleaks saw
# commits when there were some.
scan_commits() { # EXPECTED_COUNT [LOG_OPTS]
  if [ $# -ge 2 ]; then
    gl detect --source="$PWD" --redact --no-banner --log-opts="$2" >"$log" 2>&1 || { cat "$log"; return 1; }
  else
    gl detect --source="$PWD" --redact --no-banner >"$log" 2>&1 || { cat "$log"; return 1; }
  fi
  cat "$log"
  if [ "$1" -gt 0 ] && ! grep -Eq '[1-9][0-9]* commits scanned' "$log"; then
    echo "secret scan: no commits were scanned" >&2
    return 1
  fi
}

if [ "$mode" = full ]; then
  echo "secret scan (full history: $why)"
  scan_commits 1 || exit 1
else
  # shellcheck disable=SC2086 # a list of commit ids
  new=$(git rev-list --count HEAD --not $tips)
  if [ "$new" = 0 ]; then
    echo "secret scan (incremental): no commits since the last clean scan"
  else
    # A merge shows no changes in plain `git log -p`, so gitleaks would see
    # "0 commits" for a range of merges alone. Commits with their own changes
    # are scanned as they are; merges with -m, against each parent, so what a
    # conflict resolution added is scanned too.
    # shellcheck disable=SC2086
    plain=$(git rev-list --count --no-merges HEAD --not $tips)
    merges=$((new - plain))
    echo "secret scan (incremental): $new new commits ($plain with changes, $merges merges)"
    if [ "$plain" -gt 0 ]; then
      # shellcheck disable=SC2086
      scan_commits "$plain" "--no-merges HEAD --not $tips" || exit 1
    fi
    if [ "$merges" -gt 0 ]; then
      # shellcheck disable=SC2086
      scan_commits "$merges" "--merges -m HEAD --not $tips" || exit 1
    fi
  fi
  # Staged, then unstaged, changes. Only when there are some.
  if ! git diff --quiet --cached; then
    gl protect --staged --redact --no-banner --source="$PWD" >"$log" 2>&1 || { cat "$log"; exit 1; }
  fi
  if ! git diff --quiet; then
    gl protect --redact --no-banner --source="$PWD" >"$log" 2>&1 || { cat "$log"; exit 1; }
  fi
fi

# Passed: record it. Tips already contained in HEAD are redundant; the others
# (other branches' scans) stay, at most 8 in all.
keep=""
for t in $tips; do
  if ! git merge-base --is-ancestor "$t" "$head" 2>/dev/null; then keep="$keep $t"; fi
done
mkdir -p "$(dirname "$state")"
{
  if [ "$mode" = full ]; then echo "full=$now"; else echo "full=$last_full"; fi
  echo "config=$config"
  echo "clean=$head"
  # shellcheck disable=SC2086
  for t in $(echo $keep | tr ' ' '\n' | head -n 7); do echo "clean=$t"; done
} >"$state.$$"
mv "$state.$$" "$state"
