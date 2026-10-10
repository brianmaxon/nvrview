// Plays the samples written by `cargo run -p mp4-remux --example mse_sample`.
// No external network: it only fetches ./out/*.json and ./out/*.bin.
"use strict";

const $ = (id) => document.getElementById(id);
const logEl = $("log");
const log = (m, cls) => {
  const line = document.createElement("div");
  if (cls) line.className = cls;
  line.textContent = `${(performance.now() / 1000).toFixed(2)}  ${m}`;
  logEl.appendChild(line);
  logEl.scrollTop = logEl.scrollHeight;
};

// One SourceBuffer's append queue: appendBuffer must not overlap.
class Feed {
  constructor(ms, mime) {
    this.sb = ms.addSourceBuffer(mime);
    this.mime = mime;
    this.queue = [];
    this.busy = false;
    this.sb.addEventListener("updateend", () => { this.busy = false; this.pump(); });
    this.sb.addEventListener("error", () => log("SourceBuffer error", "bad"));
  }
  push(fn) { this.queue.push(fn); this.pump(); }
  pump() {
    if (this.busy || !this.queue.length) return;
    this.busy = true;
    try { this.queue.shift()(this.sb); }
    catch (e) { this.busy = false; log(`append failed: ${e.name}: ${e.message}`, "bad"); }
  }
}

let current = null;

async function play() {
  if (current) { current.stop(); current = null; }
  logEl.textContent = "";
  $("verdict").textContent = "";
  const name = $("sample").value;
  const live = $("live").checked;
  const manifest = await (await fetch(`out/${name}.json`)).json();
  const bin = new Uint8Array(await (await fetch(`out/${name}.bin`)).arrayBuffer());
  log(`${name}: ${manifest.length} segments, ${bin.length} bytes`);

  const mimes = manifest.filter((s) => s.kind === "init").map((s) => s.mime);
  for (const m of new Set(mimes)) {
    const ok = window.MediaSource && MediaSource.isTypeSupported(m);
    log(`${ok ? "supported" : "NOT supported"}: ${m}`, ok ? "ok" : "bad");
    if (!ok) { $("verdict").textContent = "This browser cannot play this sample's codec."; return; }
  }

  const v = $("v");
  const ms = new MediaSource();
  const url = URL.createObjectURL(ms);
  v.src = url;
  let stopped = false;
  const timers = [];
  current = { stop() { stopped = true; timers.forEach(clearTimeout); URL.revokeObjectURL(url); } };
  await new Promise((r) => ms.addEventListener("sourceopen", r, { once: true }));

  const feeds = {}; // track -> Feed
  let first = true;
  let startedAt = 0;
  const firstStart = manifest.find((s) => s.kind === "media").start_us;

  const send = (seg) => {
    if (stopped) return;
    const bytes = bin.subarray(seg.offset, seg.offset + seg.length);
    const key = seg.track;
    if (seg.kind === "init") {
      let feed = feeds[key];
      if (!feed) {
        feed = feeds[key] = new Feed(ms, seg.mime);
        log(`new SourceBuffer (${key})`);
      } else if (seg.reset) {
        // Parameter sets or resolution changed: reset the buffer's decoder.
        feed.push((sb) => {
          if (typeof sb.changeType === "function") sb.changeType(seg.mime);
          log(`changeType (${key}) -> ${seg.mime}`);
          feed.busy = false; feed.pump();
        });
      }
      feed.push((sb) => sb.appendBuffer(bytes));
      log(`init ${key} gen ${seg.generation}${seg.reset ? " (reset)" : ""}`);
    } else {
      feeds[key].push((sb) => sb.appendBuffer(bytes));
      if (first) { first = false; v.play().catch((e) => log(`play: ${e.message}`, "bad")); }
    }
  };

  startedAt = performance.now();
  for (const seg of manifest) {
    if (!live || seg.kind === "init") { send(seg); continue; }
    // Live pacing: release each segment when its end time has "happened".
    const due = (seg.start_us + seg.duration_us - firstStart) / 1000;
    timers.push(setTimeout(() => send(seg), Math.max(0, due - (performance.now() - startedAt))));
  }
  const total = manifest.filter((s) => s.kind === "media").reduce((a, s) => Math.max(a, s.start_us + s.duration_us), 0);
  const secs = (total - firstStart) / 1e6;
  const finish = () => {
    if (stopped) return;
    timers.push(setTimeout(() => {
      try { if (ms.readyState === "open") ms.endOfStream(); } catch (e) { log(e.message, "bad"); }
    }, live ? secs * 1000 + 500 : 500));
  };
  finish();

  v.onerror = () => { log(`video error: ${v.error && v.error.message}`, "bad"); $("verdict").textContent = "FAILED"; $("verdict").className = "bad"; };
  v.onended = () => {
    const ok = Math.abs(v.currentTime - secs) < 1;
    $("verdict").textContent = ok ? `Played ${v.currentTime.toFixed(2)} s of ${secs.toFixed(2)} s: OK` : `Ended at ${v.currentTime.toFixed(2)} s of ${secs.toFixed(2)} s: CHECK`;
    $("verdict").className = ok ? "ok" : "bad";
    log(`ended at ${v.currentTime.toFixed(2)} s (expected about ${secs.toFixed(2)} s)`);
  };
  v.ontimeupdate = () => { v.dataset.t = v.currentTime; };
}

(async () => {
  try {
    const names = await (await fetch("out/index.json")).json();
    for (const n of names) {
      const o = document.createElement("option");
      o.value = o.textContent = n;
      $("sample").appendChild(o);
    }
  } catch (e) {
    log("No samples yet. Run: cargo run -p mp4-remux --example mse_sample", "bad");
  }
  $("go").addEventListener("click", () => play().catch((e) => log(`${e.name}: ${e.message}`, "bad")));
})();
