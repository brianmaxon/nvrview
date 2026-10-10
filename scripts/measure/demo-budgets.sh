#!/usr/bin/env bash
# Measures the macOS demo against the budgets in docs/ARCHITECTURE.md from
# the outside: process memory (footprint), CPU, and energy impact (top's
# POWER column; no sudo needed) for NVRViewDemo --renderer metal with 1, 4,
# and 16 tiles. The demo's window opens behind every other window (its
# --selftest mode with --window behind), takes no focus, and has no Dock
# icon; note that macOS throttles drawing for occluded windows, so CPU and
# energy are a lower bound on what a visible grid costs.
#
# usage: scripts/measure/demo-budgets.sh [-d SECONDS] [-t "1 4 16"] [-f FILE.h264]
#   -d  measuring time per run (default 20)
#   -t  tile counts (default "1 4 16")
#   -f  stream the tiles play (default: a synthetic fixture)
# Builds the release demo if needed. Writes target/measure/demo-budgets.md
# and .csv (git-ignored).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
secs=20
tiles="1 4 16"
file="$root/fixtures/synthetic/h264-basic.h264"
while getopts "d:t:f:" o; do
  case "$o" in
    d) secs="$OPTARG" ;;
    t) tiles="$OPTARG" ;;
    f) file="$OPTARG" ;;
    *) exit 2 ;;
  esac
done

[ "$(uname -s)" = Darwin ] || { echo "macOS only" >&2; exit 2; }
bin="$root/apps/macos/.build/release/NVRViewDemo"
if [ ! -x "$bin" ]; then
  (cd "$root/apps/macos" && swift build -c release --product NVRViewDemo)
fi

out="$root/target/measure"
mkdir -p "$out"
csv="$out/demo-budgets.csv"
md="$out/demo-budgets.md"
echo "tiles,cpu_mean_pct,cpu_max_pct,cpu_per_tile_pct,energy_impact_mean,footprint_mean_mb,footprint_max_mb,rss_max_mb,selftest_exit" > "$csv"

warmup=4
for n in $tiles; do
  log="$out/demo-$n.log"
  "$bin" --renderer metal --tiles "$n" --selftest $((secs + warmup + 4)) --window behind "$file" > "$log" 2>&1 &
  pid=$!
  sleep "$warmup"
  # One top sample per second; the first of top's samples is since-launch
  # and is skipped.
  top -l $((secs + 1)) -s 1 -pid "$pid" -stats pid,cpu,power > "$out/top-$n.txt" 2>/dev/null &
  toppid=$!
  : > "$out/mem-$n.txt"
  end=$((SECONDS + secs))
  while [ "$SECONDS" -lt "$end" ] && kill -0 "$pid" 2>/dev/null; do
    fp=$(footprint -p "$pid" -f bytes --noCategories 2>/dev/null | sed -n 's/.*Footprint: \([0-9]*\) B.*/\1/p' | head -1)
    rss=$(ps -o rss= -p "$pid" | tr -d ' ')
    echo "${fp:-0} ${rss:-0}" >> "$out/mem-$n.txt"
    sleep 1
  done
  wait "$toppid" || true
  code=0
  wait "$pid" || code=$?
  python3 -I - "$n" "$out/top-$n.txt" "$out/mem-$n.txt" "$code" >> "$csv" <<'PY'
import sys
n, top, mem, code = sys.argv[1:5]
cpu, power = [], []
rows = [l.split() for l in open(top) if l.strip()[:1].isdigit()]
rows = rows[1:]  # drop the since-launch first sample
for r in rows:
    if len(r) >= 3:
        cpu.append(float(r[1])); power.append(float(r[2]))
fp, rss = [], []
for l in open(mem):
    a, b = l.split()
    if int(a) > 0: fp.append(int(a) / 2**20)
    rss.append(int(b) / 1024)
mean = lambda v: sum(v) / len(v) if v else float("nan")
print(f"{n},{mean(cpu):.1f},{max(cpu, default=float('nan')):.1f},{mean(cpu)/int(n):.2f},"
      f"{mean(power):.1f},{mean(fp):.1f},{max(fp, default=float('nan')):.1f},{max(rss, default=float('nan')):.1f},{code}")
PY
done

{
  echo "# NVRViewDemo budgets (outside measurement)"
  echo
  echo "- machine: $(sysctl -n machdep.cpu.brand_string), $(( $(sysctl -n hw.memsize) / 1073741824 )) GB, macOS $(sw_vers -productVersion)"
  echo "- build: release (\`swift build -c release\`), renderer metal, window behind other windows"
  echo "- stream: $(basename "$file") on every tile; ${secs} s per run after a ${warmup} s warm-up"
  echo "- date: $(date -u +%Y-%m-%d)"
  echo
  echo "| tiles | CPU mean % | CPU max % | CPU per tile % | energy impact (mean) | footprint mean MB | footprint max MB | RSS max MB | selftest exit |"
  echo "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
  tail -n +2 "$csv" | sed 's/,/ | /g; s/^/| /; s/$/ |/'
  echo
  echo "CPU is percent of one core (top). Energy impact is top's POWER column, an"
  echo "approximation of Activity Monitor's Energy Impact. Occluded windows draw at a"
  echo "throttled rate, so treat both as lower bounds."
} > "$md"
cat "$md"
