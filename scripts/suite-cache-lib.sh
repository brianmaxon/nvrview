# Shell side of the test result cache (scripts/suite-cache.py), sourced by
# scripts/check.sh and scripts/windows-check.sh. Python runs twice per check
# run (plan, finish); these functions read the plan file and append events to
# the run's log with awk and printf. See the header of suite-cache.py.
#
# Resolves the real interpreter once (the pyenv shim costs about 0.1 s per
# call); -I stays on every call. Set py before sourcing to choose one.
if [ -z "${py:-}" ]; then
  py=$(python3 -I -c 'import sys; print(sys.executable)')
fi
sc() { "$py" -I scripts/suite-cache.py "$@"; }

# cache_start PLATFORM [plan options]: decides every suite; sets run_file,
# plan_file and log_file.
cache_start() {
  run_file=$(sc plan "$@")
  plan_file="${run_file%.json}.plan"
  log_file="${run_file%.json}.log"
}

# cache_want SUITE: exit 0 to run it; exit 1 after printing why it is cached.
cache_want() {
  rc=0
  awk -F '\t' -v s="$1" '$1 == "want" && $2 == s { if ($3 == "run") exit 0; print $4; exit 1 }' "$plan_file" || rc=$?
  if [ "$rc" = 1 ]; then
    printf 'want\t%s\n' "$1" >>"$log_file"
    return 1
  fi
  return 0 # run it (also when the plan is unreadable)
}

# cache_crates FAMILY MODE: prints the cached crates on stderr and, on stdout,
# the cargo arguments (MODE args: --workspace, -p a -p b, or nothing) or the
# list (MODE list: all, none, or a,b).
cache_crates() {
  awk -F '\t' -v f="$1" -v m="$2" '
    $1 == "cmsg" && $2 == f { print $3 > "/dev/stderr" }
    $1 == "crates" && $2 == f { print (m == "list" ? $4 : $3) }' "$plan_file"
  printf 'crates\t%s\n' "$1" >>"$log_file"
}

# cache_pass SECS NAME: a suite, family, or step passed.
cache_pass() { printf 'pass\t%s\t%s\n' "$1" "$2" >>"$log_file"; }

# cache_areas: the Windows app self-test areas, space separated.
cache_areas() { awk -F '\t' '$1 == "areas" { print $2 }' "$plan_file"; }
