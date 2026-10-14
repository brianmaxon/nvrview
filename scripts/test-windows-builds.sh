#!/bin/bash
# Tests that a release packages the publish output the Windows tests ran, even
# after the worker that built it was deleted (scripts/windows-app.sh mark-tested
# and package, scripts/tested-builds-lib.sh), in a throwaway folder with fake
# build tools (cargo, dotnet, tar, cygpath): no Windows computer, build, or
# network, and no waiting on the clock.
#
#   scripts/test-windows-builds.sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; failures=$((failures + 1)); }

bin="$work/bin"
log="$work/calls.log"
mkdir -p "$bin"
printf '#!/bin/sh\necho "cargo $*" >>"$FAKE_LOG"\nexit 1\n' >"$bin/cargo"
printf '#!/bin/sh\necho "dotnet $*" >>"$FAKE_LOG"\nexit 1\n' >"$bin/dotnet"
printf '#!/bin/sh\nshift; echo "$1"\n' >"$bin/cygpath"
# The fake tar writes the file named after -f and records what it zipped.
cat >"$bin/tar" <<'EOF'
#!/bin/sh
while [ $# -gt 0 ]; do
  if [ "$1" = -f ]; then ls -1 >"$2"; fi
  shift
done
EOF
chmod +x "$bin"/*
export FAKE_LOG="$log" PATH="$bin:$PATH" NVRVIEW_TAR="$bin/tar"
export NVRVIEW_TESTED_STORE="$work/kept"

# A "worker": a source folder whose scripts are the ones under test.
new_worker() { # DIR VERSION
  rm -rf "$1"
  mkdir -p "$1/scripts"
  cp "$here/windows-app.sh" "$here/tested-builds-lib.sh" "$1/scripts/"
  echo "$2" >"$1/VERSION"
}
tree_of() { printf '%040d' "$1"; }
exe_dir() { echo "$1/target/dotnet/publish/NvrView.App/release_win-x64"; }
# What `publish` leaves behind: the output and its fingerprint, as pending.
fake_publish() { # DIR TREE CONTENT
  d=$(exe_dir "$1")
  rm -rf "$1/target/dotnet/publish"
  mkdir -p "$d"
  echo "exe $3" >"$d/NvrView.App.exe"
  echo "dll $3" >"$d/NvrView.Video.dll"
  (cd "$1" && printf 'tree=%s\n' "$2" >target/dotnet/publish.pending &&
    (cd "$d" && find . -type f ! -path ./version.txt -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) >>target/dotnet/publish.pending)
}
app() { # DIR TREE [VERSION_ONLY_OF] COMMAND: windows-app.sh in a worker
  dir=$1; tree=$2; vo=$3; shift 3
  (cd "$dir" && NVRVIEW_TREE="$tree" NVRVIEW_VERSION_ONLY_OF="$vo" bash scripts/windows-app.sh "$@")
}

w="$work/worker1"
new_worker "$w" 0.1.0
t1=$(tree_of 1)
fake_publish "$w" "$t1" one
app "$w" "$t1" "" mark-tested >"$work/mt1.out" 2>&1 || { cat "$work/mt1.out"; fail "mark-tested works"; }
[ -f "$NVRVIEW_TESTED_STORE/$t1/publish.tested" ] && [ -f "$NVRVIEW_TESTED_STORE/$t1/publish/NvrView.App/release_win-x64/NvrView.App.exe" ] && pass "mark-tested keeps the tested publish output outside the worker" || fail "mark-tested keeps the tested publish output outside the worker"

# The worker is deleted (windows-check.sh --clean / --remove-worker), then a
# new one packages the tree.
w2="$work/worker2"
new_worker "$w2" 0.1.0
: >"$log"
app "$w2" "$t1" "" package >"$work/pk1.out" 2>&1 && pass "a package after the tested worker was removed works" || { cat "$work/pk1.out"; fail "a package after the tested worker was removed works"; }
grep -q "restoring the tested publish output kept for tree $t1" "$work/pk1.out" && grep -q 'reusing the publish output' "$work/pk1.out" && [ ! -s "$log" ] && pass "...reusing the kept build, with nothing compiled" || { cat "$work/pk1.out" "$log"; fail "...reusing the kept build, with nothing compiled"; }
[ -f "$w2/target/dotnet/package/NvrView-0.1.0-win-x64.zip" ] && [ "$(cat "$(exe_dir "$w2")/version.txt")" = 0.1.0 ] && pass "...into the package, with version.txt written" || fail "...into the package, with version.txt written"

# A VERSION-only successor of the tested tree reuses it too.
w3="$work/worker3"
new_worker "$w3" 0.2.0
t2=$(tree_of 2)
: >"$log"
app "$w3" "$t2" "$t1" package >"$work/pk2.out" 2>&1 && grep -q "kept for tree $t1" "$work/pk2.out" && [ ! -s "$log" ] && [ "$(cat "$(exe_dir "$w3")/version.txt")" = 0.2.0 ] && pass "a VERSION-only tree reuses the kept build of its ancestor, with the new version" || { cat "$work/pk2.out"; fail "a VERSION-only tree reuses the kept build of its ancestor, with the new version"; }

# An unrelated tree, or a kept build whose files no longer match, is built.
w4="$work/worker4"
new_worker "$w4" 0.3.0
: >"$log"
if app "$w4" "$(tree_of 9)" "" package >"$work/pk3.out" 2>&1; then fail "a tree with no kept build is built (the fake build fails)"; else
  grep -q 'no matching tested publish output; building it' "$work/pk3.out" && grep -q '^cargo build' "$log" && pass "a tree with no kept build is built" || { cat "$work/pk3.out"; fail "a tree with no kept build is built"; }
fi
echo "tampered" >"$NVRVIEW_TESTED_STORE/$t1/publish/NvrView.App/release_win-x64/NvrView.Video.dll"
w5="$work/worker5"
new_worker "$w5" 0.1.0
: >"$log"
if app "$w5" "$t1" "" package >"$work/pk4.out" 2>&1; then fail "a kept build with changed files is not used"; else
  grep -q 'does not match its fingerprint' "$work/pk4.out" && grep -q 'building it' "$work/pk4.out" && pass "a kept build whose files changed is not used" || { cat "$work/pk4.out"; fail "a kept build whose files changed is not used"; }
fi

# The store holds the newest 3 trees.
rm -rf "$NVRVIEW_TESTED_STORE"
for n in 11 12 13 14 15; do
  wn="$work/worker-$n"
  new_worker "$wn" 0.1.0
  fake_publish "$wn" "$(tree_of $n)" "build $n"
  app "$wn" "$(tree_of $n)" "" mark-tested >/dev/null 2>&1
  touch -t "2026010100$n" "$NVRVIEW_TESTED_STORE/$(tree_of $n)"
done
kept=$(ls -1 "$NVRVIEW_TESTED_STORE" | wc -l | tr -d ' ')
[ "$kept" = 3 ] && [ -d "$NVRVIEW_TESTED_STORE/$(tree_of 15)" ] && [ -d "$NVRVIEW_TESTED_STORE/$(tree_of 13)" ] && [ ! -d "$NVRVIEW_TESTED_STORE/$(tree_of 12)" ] && pass "the kept builds are bounded to the newest 3 trees" || { ls "$NVRVIEW_TESTED_STORE"; fail "the kept builds are bounded to the newest 3 trees"; }

# A tree with changes (no known tree) keeps nothing.
wd="$work/worker-dirty"
new_worker "$wd" 0.1.0
fake_publish "$wd" dirty x
before=$(ls -1 "$NVRVIEW_TESTED_STORE" | wc -l | tr -d ' ')
app "$wd" dirty "" mark-tested >/dev/null 2>&1
[ "$(ls -1 "$NVRVIEW_TESTED_STORE" | wc -l | tr -d ' ')" = "$before" ] && pass "a tree with changes keeps nothing" || fail "a tree with changes keeps nothing"

echo
if [ "$failures" -ne 0 ]; then
  echo "$failures test(s) failed" >&2
  exit 1
fi
echo "all windows build tests passed"
