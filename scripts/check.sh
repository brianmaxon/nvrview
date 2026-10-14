#!/bin/sh
# shellcheck disable=SC2086 # $args holds lists of cargo arguments
# Run every local check CI would run, plus the macOS app tests. Stops at the
# first failure. Prefer this over pushing to find out whether something builds.
set -eu
cd "$(git rev-parse --show-toplevel)"

if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

# Build caching, for this repository only: with sccache installed, a shared
# compile cache keyed by each crate's exact inputs, so checkouts and agent
# worktrees reuse each other's dependency builds and can never overwrite each
# other's results; no incremental caches (the largest part of target/, and
# sccache cannot use them). Either can be overridden from the environment.
if [ -z "${RUSTC_WRAPPER:-}" ] && command -v sccache >/dev/null 2>&1; then
  export RUSTC_WRAPPER=sccache
  export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-20G}"
fi
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

# Disk guard: a full disk fails builds halfway and leaves every tool unable to
# write. NVRVIEW_MIN_FREE_GB changes the threshold.
min_free_gb="${NVRVIEW_MIN_FREE_GB:-15}"
free_kb=$(df -Pk . | awk 'NR == 2 { print $4 }')
if [ "$free_kb" -lt $((min_free_gb * 1024 * 1024)) ]; then
  echo "check.sh: $((free_kb / 1024 / 1024)) GB free, needs $min_free_gb GB. Free space first:" >&2
  echo "  remove merged worktrees (git worktree list) and old Docker volumes (scripts/clean-docker-volumes.sh)." >&2
  exit 1
fi


# Test result cache (scripts/suite-cache.py): a suite whose inputs (its
# files as committed or staged, its workspace crates, tool versions) have a
# clean pass on this Mac within the last 72 hours is skipped and printed as
# "cached pass ...". Rust runs only the crates whose fingerprint changed.
# A full run (nothing cached) happens with NVRVIEW_FULL_CHECK=1, when no full
# run passed in the last NVRVIEW_FULL_MAX_AGE_H hours (default 72), and on
# every NVRVIEW_FULL_EVERY-th run when that is set. A table at the end shows
# what ran and what was cached.
. scripts/suite-cache-lib.sh
run_file=""

# Each step prints how long the previous one took, and the end prints the
# total. "step ROW LABEL": ROW names the step's row in the table and, for a
# cached suite or Rust family, what a pass records.
check_start=$(date +%s)
step_start=$check_start
step_name=""
step_row=""
timing_file=$(mktemp)
end_step() {
  now=$(date +%s)
  if [ -n "$step_name" ]; then
    printf '   (%s: %ss)\n' "$step_name" "$((now - step_start))" | tee -a "$timing_file"
    if [ -n "$run_file" ] && [ "$step_row" != "-" ]; then cache_pass "$((now - step_start))" "$step_row"; fi
  fi
  step_name=""
}
step() { # ROW LABEL ("-" for no row)
  end_step
  step_start=$now
  step_row="$1"
  step_name="$2"
  printf '\n== %s\n' "$2"
}
# Runs the step for SUITE unless it is cached (then it prints why and fails).
want() { # SUITE LABEL
  end_step
  if [ -n "$run_file" ] && ! cache_want "$1"; then return 1; fi
  step "$1" "$2"
}
# Sets args to the cargo arguments for the crates of a Rust family that must
# run (empty: all cached); the cached ones are printed.
crates() { # FAMILY
  end_step
  if [ -n "$run_file" ]; then args=$(cache_crates "$1" args); else args=--workspace; fi
}
on_exit() {
  rc=$?
  if [ -n "$run_file" ]; then
    sc finish "$run_file" --failed "$step_name" || true
  fi
  rm -f "$timing_file"
  exit "$rc"
}
trap on_exit EXIT

# Check stamps (scripts/checked.sh): a full pass on a clean tree is recorded
# for HEAD's tree. "skipped" collects steps that run by default but were
# skipped (no stamp then); "ran_optional" lists the opt-in steps that ran.
skipped=""
ran_optional=""
skip() { skipped="$skipped, $1"; }
tree_clean_at_start=0
if [ -z "$(git status --porcelain)" ]; then tree_clean_at_start=1; fi

# A commit that changes only VERSION and *.md files after a checked tree needs
# no build or test: the Mac stamp of the checked tree is carried over to this
# one (scripts/checked.sh). A markdown change first passes the checks that
# read markdown (scripts/check-docs.sh). Only on a clean tree that has no Mac
# stamp yet, and never in a full run.
if [ "$tree_clean_at_start" = 1 ] && [ "${NVRVIEW_FULL_CHECK:-0}" != "1" ] &&
  [ ! -f "$(git rev-parse --git-common-dir)/nvrview-checks/$(git rev-parse 'HEAD^{tree}').mac" ] &&
  scripts/checked.sh --carry-version mac; then
  echo "Only VERSION and markdown files differ from a checked tree: nothing more to run (stamp carried over)."
  exit 0
fi

# Docs-only fast path. The changes being checked are the staged, unstaged,
# and untracked changes, plus the commits not yet on the branch's upstream
# (when it has one). A clean tree with no upstream has no changes to judge
# and gets the full check. When there are changes and every one is a *.md
# file, only sanitization, self-test parity, and the secret scan run. So does
# an uncommitted change that differs from a tree with a Mac stamp only in *.md
# files (and VERSION), however many unpushed commits main is ahead. The fast
# path never writes a check stamp (committing the change and running this
# again carries the stamp over). NVRVIEW_FULL_CHECK=1 forces the full run;
# NVRVIEW_DOCS_FAST_PATH=0 turns the fast path off but keeps the cache
# (scripts/merge-branch.sh).
fast=0
if [ "${NVRVIEW_FULL_CHECK:-0}" != "1" ] && [ "${NVRVIEW_DOCS_FAST_PATH:-1}" != "0" ]; then
  changed=$( {
    git diff --name-only HEAD
    git ls-files --others --exclude-standard
    if upstream=$(git rev-parse --abbrev-ref '@{upstream}' 2>/dev/null); then
      git diff --name-only "$upstream"...HEAD
    fi
  } | sort -u)
  if [ -n "$changed" ] && ! printf '%s\n' "$changed" | grep -qv '\.md$'; then
    fast=1
  elif stamped_changed=$(. scripts/stamps-lib.sh && docs_only_since_stamp mac); then
    fast=1
    changed="$stamped_changed"
    echo "differs from a checked tree only in markdown files and VERSION:"
  fi
  if [ "$fast" = 1 ]; then
    echo "docs-only change ($(printf '%s\n' "$changed" | wc -l | tr -d ' ') markdown files): taking the fast path (sanitization, parity, secret scan; NVRVIEW_FULL_CHECK=1 runs everything)"
  fi
fi

# Per-test time limits come from .config/nextest.toml. Install cargo-nextest
# when it is missing, as the check cannot run its tests without it.
if [ "$fast" = 0 ] && ! cargo nextest --version >/dev/null 2>&1; then
  echo "installing cargo-nextest (cargo install cargo-nextest --locked)"
  cargo install cargo-nextest --locked
fi

if [ "$fast" = 0 ]; then
  cache_start mac
fi

step sanitization "sanitization"
"$py" -I scripts/check-sanitized.py --all

step parity "self-test parity"
"$py" -I scripts/check-parity.py

step test-modules "test files are modules of the one test executable"
"$py" -I scripts/check-test-modules.py

step selftest-areas "self-test areas (Windows and macOS)"
"$py" -I scripts/check-selftest-areas.py

# The secret scan (scripts/secret-scan.sh): gitleaks over the commits added
# since the last clean scan plus the working tree changes; the full history
# once a day and always with NVRVIEW_FULL_CHECK=1 (the release scripts).
# NVRVIEW_SECRET_SCAN=0 skips it. False positives go in .gitleaksignore
# (synthetic examples only).
if [ "${NVRVIEW_SECRET_SCAN:-1}" = "0" ]; then
  step - "secret scan skipped (NVRVIEW_SECRET_SCAN=0)"
  skip "secret scan"
else
  step secret-scan "secret scan"
  scan_rc=0
  scripts/secret-scan.sh || scan_rc=$?
  if [ "$scan_rc" = 3 ]; then
    skip "secret scan"
  elif [ "$scan_rc" != 0 ]; then
    exit "$scan_rc"
  fi
fi

if [ "$fast" = 1 ]; then
  now=$(date +%s)
  printf '   (%s: %ss)\n' "$step_name" "$((now - step_start))"
  printf '\nDocs-only fast path passed in %ss (sanitization, parity, secret scan). No check stamp: run a full check before a release.\n' "$((now - check_start))"
  exit 0
fi

step rustfmt "rustfmt"
cargo fmt --all --check

# The Rust families run only the crates whose fingerprint missed the cache
# (--workspace when all did). Arguments are lists of "-p crate".
crates clippy
if [ -n "$args" ]; then
  step clippy "clippy ($args)"
  cargo clippy $args --all-targets -- -D warnings
fi

# ---------------------------------------------------------------------------
# The heavy steps are independent of each other, so they run side by side, one
# "lane" each, with every lane's output buffered and printed in order after
# they finish (scripts/run-script-tests.sh does the same for the script
# tests). A step line inside a lane prints "(label: Ns)" like a serial one.
# A failure fails the check and names the step. What they share:
#   cargo      one lane (rust) runs all the cargo builds and tests, as cargo
#              locks the target directory; the C smoke tests' library and mock
#              are built and copied first, before any lane starts
#   SwiftPM    one lane (swift) runs swift test, the app builds, and the GUI
#              self-tests one after another (one .build lock, one desktop)
#   Docker     each Docker lane takes a host-wide slot (scripts/docker-slot.sh)
# NVRVIEW_SERIAL_CHECK=1 runs the lanes one after another instead.
# ---------------------------------------------------------------------------

# Cache decisions are made here, in order, before any lane starts.
lane_dir=$(mktemp -d)
lanes=""
lane_wall_start=0
# lstep ROW LABEL CMD...: runs one step inside a lane. ROW is "-" or a comma
# separated list of cache rows that a pass records.
lstep() {
  _row=$1
  _label=$2
  shift 2
  printf '\n== %s\n' "$_label" >>"$lane_out"
  _s=$(date +%s)
  _rc=0
  "$@" >>"$lane_out" 2>&1 || _rc=$?
  printf '%s\t%s\t%s\t%s\n' "$_row" "$(($(date +%s) - _s))" "$_rc" "$_label" >>"$lane_res"
  if [ "$_rc" != 0 ]; then exit "$_rc"; fi
}
# lane NAME FUNCTION: starts FUNCTION in the background as lane NAME.
lane() {
  _name=$1
  shift
  lane_out="$lane_dir/$_name.out"
  lane_res="$lane_dir/$_name.res"
  : >"$lane_out"
  : >"$lane_res"
  if [ "${NVRVIEW_SERIAL_CHECK:-0}" = 1 ]; then
    ("$@") || true
  else
    ("$@") &
  fi
  lanes="$lanes $_name"
}
# lwant SUITE: decides a suite for a lane; prints why when it is cached.
lwant() {
  end_step
  if [ -n "$run_file" ] && ! cache_want "$1"; then return 1; fi
  return 0
}

# --- the cargo lane ---------------------------------------------------------
# nextest fails a test that runs past its limit (.config/nextest.toml) and
# names it; it does not run doctests, so those run separately.
crates test
test_args=$args
crates doctest
doctest_args=$args
crates wincheck
wincheck_args=$args
linuxcheck_args=""
if rustup target list --installed 2>/dev/null | grep -qx x86_64-unknown-linux-gnu; then
  crates linuxcheck
  linuxcheck_args=$args
else
  step - "Linux compile check"
  echo "Linux target not installed (rustup target add x86_64-unknown-linux-gnu); skipped"
fi
lane_rust() {
  if [ -n "$test_args" ]; then lstep test "tests, per-test time limit ($test_args)" cargo nextest run $test_args; fi
  if [ -n "$doctest_args" ]; then lstep doctest "doctests ($doctest_args)" cargo test $doctest_args --doc; fi
  if [ -n "$wincheck_args" ]; then
    lstep wincheck "Windows compile check ($wincheck_args)" cargo check $wincheck_args --all-targets --target x86_64-pc-windows-msvc
  fi
  if [ -n "$linuxcheck_args" ]; then
    lstep linuxcheck "Linux compile check ($linuxcheck_args)" cargo check $linuxcheck_args --all-targets --target x86_64-unknown-linux-gnu
  fi
}

# --- the C interface smoke tests ---------------------------------------------
run_c_smoke=0
run_c_asan=0
if lwant c-smoke; then run_c_smoke=1; fi
if [ "$(uname)" = "Darwin" ] && lwant c-smoke-asan; then run_c_asan=1; fi
if [ "$run_c_smoke" = 1 ] || [ "$run_c_asan" = 1 ]; then
  step - "C interface smoke tests: build the library and mock once"
  scripts/test-c-ffi.sh --prepare
fi
lane_c_smoke() { lstep c-smoke "C interface smoke test" env NVRVIEW_C_FFI_PREPARED=1 scripts/test-c-ffi.sh; }
lane_c_asan() { lstep c-smoke-asan "C interface smoke test, AddressSanitizer" env NVRVIEW_C_FFI_PREPARED=1 scripts/test-c-ffi.sh --asan; }

# --- the script tests ---------------------------------------------------------
run_script_tests=0
if lwant script-tests; then run_script_tests=1; fi
lane_scripts() {
  lstep script-tests "check-stamp, merge, merge-train, lock, and cache scripts (throwaway repositories, fake checks)" scripts/run-script-tests.sh
}

# --- the C header (quick, so before the lanes) ------------------------------
if command -v cbindgen >/dev/null 2>&1 || [ -x "$HOME/.cargo/bin/cbindgen" ]; then
  if want c-header "C header up to date"; then
    scripts/gen-header.sh --check
  fi
else
  step - "C header up to date"
  echo "cbindgen not installed (cargo install cbindgen --locked); skipped"
fi

# --- Docker lanes -----------------------------------------------------------
# The full Linux tests in Docker repeat the Rust tests in a slower VM, and CI
# runs them on every push, so they are opt-in: NVRVIEW_LINUX_TESTS=1 runs them
# when the Docker daemon is running.
run_linux=0
if [ "${NVRVIEW_LINUX_TESTS:-0}" != "1" ]; then
  step - "Linux tests skipped (NVRVIEW_LINUX_TESTS=1 runs them)"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  ran_optional="$ran_optional, Linux tests"
  if lwant linux-tests; then run_linux=1; fi
else
  step - "Linux tests skipped (Docker is not running)"
fi
lane_linux() { lstep linux-tests "Linux tests (Docker)" scripts/linux-check.sh; }

# The C# bindings, shell tests, and tile tests (apps/windows) on Linux in
# Docker, when the Docker daemon is running; one cached suite per project, one
# container run for those that changed. NVRVIEW_CSHARP_TESTS=0 skips them.
csharp_parts=""
csharp_rows=""
if [ "${NVRVIEW_CSHARP_TESTS:-1}" = "0" ]; then
  step - "C# tests skipped (NVRVIEW_CSHARP_TESTS=0)"
  skip "C# tests"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  for part in smoke shell tiles; do
    if lwant "csharp-docker-$part"; then
      csharp_parts="$csharp_parts $part"
      csharp_rows="$csharp_rows,csharp-docker-$part"
    fi
  done
else
  step - "C# tests skipped (Docker is not running)"
  skip "C# tests"
fi
lane_csharp() {
  # shellcheck disable=SC2086 # csharp_parts is a list of words
  lstep "${csharp_rows#,}" "C# tests (Docker):$csharp_parts" scripts/csharp-check.sh $csharp_parts
}

# --- the macOS app lane -------------------------------------------------------
# swift test, then the app's self-tests (they open a window for a few seconds,
# behind all other windows and without taking focus, so they need a desktop
# session; NVRVIEW_GUI_TESTS=0 skips them), one after another: one .build
# directory and one desktop.
run_swift_test=0
mac_areas=""
mac_all=1
run_real=0
run_demo_metal=0
run_demo_layer=0
if [ "$(uname)" = "Darwin" ] && [ -d apps/macos ]; then
  if lwant swift-test; then run_swift_test=1; fi
  if [ "${NVRVIEW_GUI_TESTS:-1}" = "0" ]; then
    step - "macOS GUI self-tests skipped (NVRVIEW_GUI_TESTS=0)"
    skip "GUI self-tests"
  elif [ "$(launchctl managername 2>/dev/null)" = "Aqua" ]; then
    # By area (scripts/macos-selftest-areas.txt): the areas with a cached pass
    # are skipped, the rest run in one start of the app (startup always runs).
    mac_list=$(cache_mac_areas)
    [ -n "$mac_list" ] || mac_list=$(awk '!/^#/ && NF && !seen[$1]++ { print $1 }' scripts/macos-selftest-areas.txt)
    for a in $mac_list; do
      if cache_want "mac-app.$a"; then mac_areas="$mac_areas,$a"; else mac_all=0; fi
    done
    mac_areas=${mac_areas#,}
    # The real-core self-test (the Rust library against tools/mock-nvr) builds
    # the release core, so it runs in full runs only (NVRVIEW_FULL_CHECK=1, the
    # release checks, and the periodic full run).
    if [ -n "$(cache_full)" ]; then
      run_real=1
    else
      step - "macOS shipped-app self-test (real core, release bundle) deferred (full runs and releases run it)"
    fi
    if lwant macos-demo-metal; then run_demo_metal=1; fi
    if lwant macos-demo-layer; then run_demo_layer=1; fi
  else
    step - "macOS demo self-tests skipped (no desktop session)"
    skip "GUI self-tests"
  fi
fi
mac_selftest() {
  # The app's self-test on the fake core. Its lines go to a log; a failure
  # prints its problems.
  swiftpm_guard || return 1
  mac_log=$(mktemp)
  mac_data=$(mktemp -d)
  mac_rc=0
  # shellcheck disable=SC2046 # --areas and its list are separate words
  (cd apps/macos && NVRVIEW_SELFTEST=1 swift build --product NVRView &&
    .build/debug/NVRView --fake --data-dir "$mac_data" --selftest 60 $([ "$mac_all" = 1 ] || echo "--areas $mac_areas") \
      ../../fixtures/synthetic/h264-basic.h264 ../../fixtures/synthetic/h265-basic.h265 \
      ../../fixtures/synthetic/fisheye/h264-fisheye.h264) >"$mac_log" 2>&1 || mac_rc=$?
  rm -rf "$mac_data"
  if [ "$mac_rc" != 0 ]; then
    grep -E '^  |error:' "$mac_log" | tail -n 20 || true
    tail -n 30 "$mac_log"
    rm -f "$mac_log"
    return "$mac_rc"
  fi
  grep -E 'selftest took|^selftest: ' "$mac_log" || true
  rm -f "$mac_log"
}
mac_real_selftest() {
  real_log=$(mktemp)
  real_rc=0
  scripts/macos-real-selftest.sh --bundle >"$real_log" 2>&1 || real_rc=$?
  if [ "$real_rc" != 0 ]; then
    grep -E '^  |error:' "$real_log" | tail -n 20 || true
    tail -n 30 "$real_log"
    rm -f "$real_log"
    return "$real_rc"
  fi
  grep -E 'selftest took|^selftest: ' "$real_log" || true
  rm -f "$real_log"
}
# Fails at once, naming the process, when a SwiftPM or test process left behind
# by an earlier check (orphaned, parent 1) still holds apps/macos/.build; SwiftPM
# would otherwise wait on its lock until the step's time limit. A live holder
# (another check running) is only noted: SwiftPM waits for it.
swiftpm_guard() {
  guard_rc=0
  holders=$(sh scripts/swiftpm-holders.sh apps/macos) || guard_rc=$?
  case "$guard_rc" in
    0) return 0 ;;
    3)
      echo "$holders" >&2
      echo "STALE SwiftPM process(es) from an earlier check still hold apps/macos/.build (listed above: kind pid ppid age command)." >&2
      echo "Stop them (kill PID, then kill -9 PID) and rerun; SwiftPM would wait on its lock until the time limit." >&2
      return 1
      ;;
    *)
      echo "note: another SwiftPM process is using apps/macos (live, listed below); this step waits for its lock:" >&2
      echo "$holders" >&2
      ;;
  esac
}
swift_test() {
  # An overall limit (the whole run takes a few minutes); on a hang the test
  # that last started is named, and the tree of processes is stopped.
  swiftpm_guard || return 1
  (cd apps/macos &&
    "$py" -I ../../scripts/with-timeout.py "${NVRVIEW_SWIFT_TEST_TIMEOUT:-600}" "swift test" swift test)
}
demo_selftest() { # [renderer args]
  swiftpm_guard || return 1
  (cd apps/macos && swift build --product NVRViewDemo &&
    .build/debug/NVRViewDemo "$@" --tiles 6 --fps 200 --selftest 60 --until-frames 20 ../../fixtures/synthetic/*.h264 ../../fixtures/synthetic/*.h265)
}
lane_swift() {
  if [ "$run_swift_test" = 1 ]; then lstep swift-test "macOS app tests" swift_test; fi
  if [ -n "$mac_areas" ]; then
    rows=""
    for a in $(echo "$mac_areas" | tr "," " "); do rows="$rows,mac-app.$a"; done
    lstep "${rows#,}" "macOS app self-test, fake core, areas: $([ "$mac_all" = 1 ] && echo all || echo "$mac_areas")" mac_selftest
  fi
  if [ "$run_real" = 1 ]; then lstep macos-real-selftest "macOS app self-test, shipped release bundle on the real core against mock-nvr" mac_real_selftest; fi
  if [ "$run_demo_metal" = 1 ]; then lstep macos-demo-metal "macOS demo self-test, Metal renderer" demo_selftest; fi
  if [ "$run_demo_layer" = 1 ]; then lstep macos-demo-layer "macOS demo self-test, display-layer renderer" demo_selftest --renderer layer; fi
}

# --- start the lanes, wait, and report in order -------------------------------
end_step
step_name=""
lane_wall_start=$(date +%s)
if [ -n "$test_args$doctest_args$wincheck_args$linuxcheck_args" ]; then lane rust lane_rust; fi
if [ "$run_script_tests" = 1 ]; then lane scripts lane_scripts; fi
if [ "$run_c_smoke" = 1 ]; then lane c-smoke lane_c_smoke; fi
if [ "$run_c_asan" = 1 ]; then lane c-asan lane_c_asan; fi
if [ -n "$csharp_parts" ]; then lane csharp lane_csharp; fi
if [ "$run_linux" = 1 ]; then lane linux lane_linux; fi
if [ "$run_swift_test" = 1 ] || [ -n "$mac_areas" ] || [ "$run_real" = 1 ] || [ "$run_demo_metal" = 1 ] || [ "$run_demo_layer" = 1 ]; then
  lane swift lane_swift
fi
wait
lanes_failed=""
for l in $lanes; do
  cat "$lane_dir/$l.out"
  while IFS="$(printf '\t')" read -r row secs rc label; do
    if [ "$rc" = 0 ]; then
      printf '   (%s [side by side]: %ss)\n' "$label" "$secs" | tee -a "$timing_file"
      if [ -n "$run_file" ] && [ "$row" != "-" ]; then
        for r in $(echo "$row" | tr "," " "); do cache_pass "$secs" "$r"; done
      fi
    else
      printf '   (%s: %ss, FAILED with exit %s)\n' "$label" "$secs" "$rc"
      lanes_failed="$lanes_failed; $label"
    fi
  done <"$lane_dir/$l.res"
done
rm -rf "$lane_dir"
if [ -n "$lanes" ]; then
  printf '\n   (side by side: the lanes%s took %ss together)\n' "$(echo "$lanes" | sed 's/ /, /g; s/^,//')" "$(($(date +%s) - lane_wall_start))"
fi
if [ -n "$lanes_failed" ]; then
  step_name="${lanes_failed#; }"
  printf '\ncheck failed in: %s\n' "$step_name" >&2
  exit 1
fi

# Plays the Media Source samples in real browsers (headless Chrome and
# WebKit through playwright-cli). Off by default: it needs playwright-cli and
# browsers and takes a few minutes. NVRVIEW_BROWSER_TESTS=1 runs it.
if [ "${NVRVIEW_BROWSER_TESTS:-0}" = "1" ]; then
  ran_optional="$ran_optional, browser tests"
  if want browser "browser playback check"; then
    scripts/browser-check.sh
  fi
else
  step - "browser playback check skipped (NVRVIEW_BROWSER_TESTS=1 runs it)"
fi

end_step
step_name=""
summary=$(mktemp)
sc finish "$run_file" --ok ${skipped:+--partial} | tee "$summary"
run_file=""
cached=$(sed -n 's/.*; cached suites: //p' "$summary")
rm -f "$summary"
printf '\nAll checks passed in %ss.\n' "$((now - check_start))"
"$py" -I scripts/timing.py table "$timing_file"
"$py" -I scripts/timing.py log mac "$((now - check_start))" --tree "$(git rev-parse 'HEAD^{tree}')" "$timing_file" || true

if [ "$(uname)" != "Darwin" ]; then
  echo "no check stamp written (the stamp is for the Mac checks)"
elif [ -n "$skipped" ]; then
  echo "no check stamp written; skipped:${skipped#,}"
elif [ "$tree_clean_at_start" != 1 ] || [ -n "$(git status --porcelain)" ]; then
  echo "no check stamp written (the working tree is not clean)"
else
  # A stamp with cached suites serves merges; a release needs a full one.
  scripts/checked.sh --record mac "$((now - check_start))" "all default steps${ran_optional:+; optional:${ran_optional#,}}" "${cached:-none}"
fi
