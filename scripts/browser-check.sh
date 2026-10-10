#!/bin/sh
# Plays every Media Source sample from crates/mp4/tests/mse in real browsers
# through Playwright and reports pass or fail per sample. ffmpeg cannot stand
# in for this: browsers' decoders (VideoToolbox in Chrome and Safari on macOS)
# reject streams that ffmpeg accepts.
#
# Needs python3 (to serve the page on 127.0.0.1) and playwright-cli
# (npm install -g @playwright/cli). Browsers run headless and in their own
# Playwright sessions, so no window opens or takes focus; the sessions are
# closed on every exit path.
#
#   NVRVIEW_BROWSERS        browsers to try, default "chrome webkit"
#                           (chrome is the installed Google Chrome, with its
#                           H.264 and H.265 decoders; webkit is Playwright's
#                           WebKit, `playwright-cli install-browser webkit`).
#                           A browser that cannot start is reported as
#                           skipped; the run fails if none started.
#   NVRVIEW_BROWSER_PORT    port to serve the page on, default 8771
#   NVRVIEW_BROWSER_HEADED  1 runs the browsers headed (opens windows)
#
# Each sample plays twice: appended all at once, and paced like a live stream
# (each segment released when its end time has passed). A sample passes when
# playback reaches its end at about the expected time with no media error;
# a resolution-change sample must also show more than one picture size. A
# codec the browser does not support is reported as skipped.
set -eu
cd "$(git rev-parse --show-toplevel)"

if [ -d /opt/homebrew/opt/rustup/bin ]; then
  PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi

browsers="${NVRVIEW_BROWSERS:-chrome webkit}"
port="${NVRVIEW_BROWSER_PORT:-8771}"
headed=""
if [ "${NVRVIEW_BROWSER_HEADED:-0}" = "1" ]; then
  headed="--headed"
fi

if ! command -v playwright-cli >/dev/null 2>&1; then
  echo "playwright-cli not found (npm install -g @playwright/cli)" >&2
  exit 1
fi

tmp=$(mktemp -d)
server=""
sessions=""
cleanup() {
  for s in $sessions; do
    (cd "$tmp" && playwright-cli -s="$s" close >/dev/null 2>&1) || true
  done
  if [ -n "$server" ]; then
    kill "$server" 2>/dev/null || true
  fi
  rm -rf "$tmp"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

echo "== generating samples"
cp crates/mp4/tests/mse/index.html crates/mp4/tests/mse/main.js "$tmp/"
cargo run -q -p mp4-remux --example mse_sample "$tmp/out"

python3 -I -m http.server "$port" --bind 127.0.0.1 --directory "$tmp" \
  >"$tmp/server.log" 2>&1 &
server=$!
up=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  if curl -fsS "http://127.0.0.1:$port/out/index.json" >/dev/null 2>&1; then
    up=1
    break
  fi
  if ! kill -0 "$server" 2>/dev/null; then
    break
  fi
  sleep 0.25
done
if [ "$up" != 1 ]; then
  echo "could not serve the samples on 127.0.0.1:$port (port in use?)" >&2
  cat "$tmp/server.log" >&2
  exit 1
fi

# Plays every sample in both modes; returns one line per run.
cat >"$tmp/run.js" <<EOF
async (page) => {
  const base = "http://127.0.0.1:$port/";
  await page.goto(base);
  const names = await page.evaluate(async () => (await fetch("out/index.json")).json());
  const lines = [];
  for (const live of ["0", "1"]) {
    for (const name of names) {
      await page.goto(base + "?sample=" + name + "&live=" + live);
      let r = { result: "timeout", verdict: "", sizes: "", errors: "" };
      try {
        await page.waitForFunction(() => document.body.dataset.result, null, { timeout: 30000 });
      } catch (e) {}
      r = await page.evaluate((r) => {
        const v = document.getElementById("v");
        return {
          result: document.body.dataset.result || r.result,
          verdict: document.getElementById("verdict").textContent,
          sizes: v.dataset.sizes || "",
          errors: [...document.querySelectorAll("#log .bad")].map((x) => x.textContent).slice(0, 2).join(" | "),
        };
      }, r);
      let status = r.result === "ok" ? "PASS" : r.result === "unsupported" ? "SKIP" : "FAIL";
      let note = r.verdict || r.result;
      if (status === "PASS" && name.includes("resolution-change") && new Set(r.sizes.split(" ")).size < 2) {
        status = "FAIL";
        note = "only one picture size decoded";
      }
      if (status === "FAIL" && r.errors) note += " (" + r.errors + ")";
      const mode = live === "1" ? "live" : "bulk";
      lines.push(status + " " + mode + " " + name + ": " + note + (r.sizes ? " [" + r.sizes + "]" : ""));
    }
  }
  return lines.join("\n");
}
EOF

failed=0
ran=0
for b in $browsers; do
  s="nvrview-browser-check-$b"
  echo "== $b${headed:+ (headed)}"
  # playwright-cli writes its logs to .playwright-cli/ in the current
  # directory, so it runs from the temporary directory.
  # shellcheck disable=SC2086 # $headed is empty or one flag
  if ! (cd "$tmp" && playwright-cli -s="$s" open --browser="$b" $headed \
    --idle-timeout=600000 >"$tmp/open-$b.log" 2>&1); then
    echo "SKIP $b: the browser did not start"
    grep -m1 -i "error" "$tmp/open-$b.log" || true
    continue
  fi
  sessions="$sessions $s"
  ran=$((ran + 1))
  if ! (cd "$tmp" && playwright-cli -s="$s" --raw run-code --filename=run.js) \
    >"$tmp/result-$b.json" 2>&1; then
    cat "$tmp/result-$b.json" >&2
    failed=1
    continue
  fi
  python3 -I -c 'import json, sys; print(json.load(sys.stdin))' \
    <"$tmp/result-$b.json" >"$tmp/result-$b.txt"
  cat "$tmp/result-$b.txt"
  if grep -q '^FAIL' "$tmp/result-$b.txt"; then
    failed=1
  fi
  (cd "$tmp" && playwright-cli -s="$s" close >/dev/null 2>&1) || true
done

if [ "$ran" = 0 ]; then
  echo "no browser started" >&2
  exit 1
fi
if [ "$failed" != 0 ]; then
  echo "browser playback check FAILED" >&2
  exit 1
fi
echo "browser playback check passed"
