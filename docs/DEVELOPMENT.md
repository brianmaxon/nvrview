# Development

How to work on nvrview: running the checks, the Windows test computers, and
the test result cache. Start with the [README](../README.md) to build and run
the apps; read [SANITIZATION.md](SANITIZATION.md) and
[CLEAN-ROOM.md](CLEAN-ROOM.md) before your first commit.

## The macOS app

```sh
cd apps/macos
swift test                                  # app and bridge tests (fake core)
swift run NVRView --fake ../../fixtures/synthetic/h264-basic.h264
# the app's self-test on the fake core (about 20 s; --areas a,b runs some, see scripts/macos-selftest-areas.txt)
swift run NVRView --fake --data-dir "$(mktemp -d)" --selftest 60 ../../fixtures/synthetic/h264-basic.h264 \
  ../../fixtures/synthetic/h265-basic.h265 ../../fixtures/synthetic/fisheye/h264-fisheye.h264
scripts/build-xcframework.sh                # from the repository root: the real core
NVRVIEW_REAL_CORE=1 swift test              # against the real core (after copying the XCFramework, see Package.swift)
scripts/macos-real-selftest.sh              # from the repository root: the self-test on the real core against mock-nvr
```

## Windows test computers

`scripts/windows-check.sh` runs the Windows checks on such a computer over SSH
from a Mac, including uncommitted work:

```sh
NVRVIEW_WINDOWS_HOST=<ssh host> scripts/windows-check.sh
```

The script takes a lock on the computer itself (`scripts/windows-lock.sh`:
merges and releases go before agents), so parallel sessions queue without
extra steps. Several computers can share the load:
`NVRVIEW_WINDOWS_HOSTS="host-a host-b"` (see
[apps/windows/README.md](../apps/windows/README.md#adding-a-windows-test-computer)).
Each run prints the computer's free disk space (and warns under 10 GB), then
the seconds every step took. Parallel sessions give themselves a worker folder
with `NVRVIEW_WINDOWS_WORKER=<name>` (default `main`); each keeps about 5 GB of
build output, so delete a finished one with
`NVRVIEW_WINDOWS_HOST=<ssh host> scripts/windows-check.sh --clean <name>`.
The worker builds without debug information (`NVRVIEW_WORKER_DEBUGINFO=1`
keeps it) and builds the release library and the C# projects in the
background while the Rust tests run. Optionally, in an administrator
PowerShell on that computer, exclude its build folders from Microsoft
Defender real-time scanning, which slows builds:

```powershell
Add-MpPreference -ExclusionPath "$env:USERPROFILE\nvrview", "$env:USERPROFILE\.cargo", "$env:USERPROFILE\.rustup", "$env:USERPROFILE\.nuget\packages", "$env:LOCALAPPDATA\Mozilla\sccache"
```

| folder | holds |
| --- | --- |
| `nvrview` | the check workers (source and `target`, several GB each) and the clone; most build traffic |
| `.cargo`, `.rustup` | the Rust toolchain, crate sources, and installed tools |
| `.nuget\packages` | the .NET packages |
| `Mozilla\sccache` | the shared compile cache (sccache's default folder, which the workers use) |

If Defender still scans heavily during builds, the build programs can be
excluded too, so the files they open are not scanned. This is broader: what
those programs touch anywhere goes unscanned.

```powershell
Add-MpPreference -ExclusionProcess "cargo.exe", "rustc.exe", "link.exe", "sccache.exe", "dotnet.exe", "MSBuild.exe"
```

`Get-MpPreference | Select-Object -ExpandProperty ExclusionPath` lists the
folder exclusions. That is your decision: it trades some protection for those
folders (and programs) for speed.

## Checks

`scripts/check.sh` runs, in order and stopping at the first failure:
sanitization and a full-history secret scan, `cargo fmt`, `clippy`, the Rust
tests, the C header check, the C smoke test (plain and with AddressSanitizer),
Windows and Linux compile checks, the C# smoke test in Docker, the macOS app
tests, and the macOS GUI self-tests: the app's self-test on the fake core,
by area, and the two renderer demos (their windows open behind other windows;
with no desktop session they are skipped and the run writes no stamp). Full
runs (`NVRVIEW_FULL_CHECK=1`, the release checks) add the app's self-test on
the real core against `tools/mock-nvr` (`scripts/macos-real-selftest.sh`).
Environment variables skip the slow or optional steps:
`NVRVIEW_CSHARP_TESTS=0`, `NVRVIEW_GUI_TESTS=0`, `NVRVIEW_SECRET_SCAN=0`.
`NVRVIEW_LINUX_TESTS=1` adds the full Linux tests in Docker (CI runs them on
every push) and `NVRVIEW_BROWSER_TESTS=1` the browser playback check. `scripts/clean-docker.sh` removes what the Docker steps leave behind.

### Test result cache

`scripts/check.sh` and `scripts/windows-check.sh` skip a test suite whose
inputs have not changed since a clean pass on the same platform in the last
72 hours, and print it as `cached pass <suite> (<age>, key <short>)`; a table
at the end shows each suite as ran or cached, with its time.
`scripts/suite-cache.py` holds the rules:

- **Inputs.** A suite's fingerprint covers its files as committed or staged
  (unstaged edits and untracked files outside its inputs do not matter), the
  tool versions, and the suite name. Each Rust crate's clippy, tests, and
  doctests cover the crate, every workspace crate it depends on (from
  `cargo metadata`; dev-dependencies count for the crate itself),
  `Cargo.lock`, the workspace and Cargo settings, and `fixtures/synthetic/`.
  Changing one crate reruns that crate and the crates above it, with
  `-p` arguments. The C header and C smoke tests cover the `viewer-ffi` and
  `mock-nvr` crates; the Swift tests and demo self-tests `apps/macos/`, the C
  header, and the fixtures; the C# and Windows app suites `apps/windows/`, the
  core crates they load, and the fixtures. The Windows app self-test is split
  into areas (startup, viewer, tiles, layouts, header, settings, calls,
  recordings, capture, ptz), each cached on its own for the debug build:
  `scripts/windows-selftest-areas.txt` maps every `// parity:` step to one
  area (`scripts/check-selftest-areas.py` fails otherwise), the app runs only
  the areas asked for with `--areas a,b` (starting once), and an area's
  fingerprint is everything under `apps/windows/` except the files that only
  another area exercises (`WINDOWS_OWNED` in `scripts/suite-cache.py`), so
  shared code reruns every area and a change to a recordings file reruns
  recordings and the always-run startup smoke. A full run covers all areas.
  The macOS app self-test (fake core) is cached by area the same way
  (`mac-app.AREA`: startup, viewer, tiles, layouts, header, settings, calls,
  recordings, capture, ptz, alerts): `scripts/macos-selftest-areas.txt` maps
  its steps, `NVRView --selftest N --areas a,b` (a self-test build) runs some, the larger areas'
  steps live in `SelfTest+Area.swift` files, and `MAC_OWNED` in
  `scripts/suite-cache.py` lists the files only one area exercises. Its
  real-core run is not cached; it runs in full runs only.
  The other Windows suites cover only the C# projects they build (and what
  those reference): the video self-test the video demo project, the smoke test
  the smoke project, the tile tests and the shell tests their own projects,
  each with the shared build settings; the update test covers the updater and
  start-up files, the packaging script, and the signing tool's crates (the
  project table in `scripts/suite-cache.py` is checked against the project
  files by `suite-cache.py check-projects`). Sanitization, self-test parity,
  the secret scan, and `cargo fmt` always run.
- **Trimmed build.** Trimming is checked statically on every build (the trim
  analyzer, warnings as errors) and at publish (the app's own assemblies are
  trimmer roots, kept whole). The trimmed publish with its plain start and a
  short self-test (`app-publish`: `--smoke`, the startup area and one wiring
  check per area; the whole self-test runs on the debug build) and the update
  test run in full runs (releases) and when the last passing trimmed run on
  Windows is more than 24 hours old (`NVRVIEW_TRIMMED_MAX_AGE_H`); other runs
  print them as `deferred` and count them like cached suites (merge stamps,
  never a release). The trade-off: a problem that only trimming causes at run
  time shows at the daily run or the release, not at the merge that
  introduced it.
- **Secret scan.** Normal checks scan the commits not yet covered by a clean
  scan (`scripts/secret-scan.sh` records them in the git directory) and the
  staged and unstaged changes; the whole history is scanned when the last full
  scan is a day old, with `NVRVIEW_FULL_CHECK=1` (releases), and when the
  gitleaks ignore list changes. A failed scan records nothing.
- **Timings.** Every check ends with a table of its step times; merges and
  releases add the total and the slowest three steps. Each run appends a line
  (JSON: time, kind, total, steps) to `check-timings.jsonl` in the git
  directory, which keeps the last 500.
- **Parallel merges.** `scripts/merge-branch.sh` and `scripts/merge-train.sh`
  run the Mac check and the Windows check at the same time, each with its own
  log; the merge reaches main only when both pass.
- **A full check after every merge.** With two or more Windows computers
  configured (`NVRVIEW_WINDOWS_HOSTS`), a merge ends by starting a full check
  (`NVRVIEW_FULL_CHECK=1`, Mac and Windows) of the new main in the background
  (`scripts/full-check-bg.sh`; `--full-check-after` / `--no-full-check-after`
  or `NVRVIEW_BACKGROUND_FULL_CHECK=1|0` override the default). It runs in its
  own worktree (`.worktrees/fullcheck`, build output kept between runs) with
  its own Windows worker, at the lock priority `background`, below merges and
  agents: it takes a computer only when none of them is waiting for it (one
  that already started is not interrupted, so an agent waits at most one
  check). A later merge supersedes it: the older check's process group is
  stopped and its locks given back. A tree that already has full stamps starts
  nothing. When it passes, `scripts/release.sh` (without `--full-check`) finds
  full stamps for main and only packages. `scripts/full-check-bg.sh status`
  shows it; a stopped or failed one leaves no stamps.
- **Releases ship what was tested.** The full Mac check runs the real-core
  self-test on the shipped app: the release build as the signed bundle
  `build/NVRView.app` (`scripts/macos-real-selftest.sh --bundle`), with the
  release's bundled files when `scripts/release.sh` runs the check. A pass on a
  clean tree leaves `build/NVRView.app.tested` (tree hash and a hash of the
  bundled files); `scripts/package-macos.sh` then zips that bundle, redoing only
  the bundled files and the signature (`--rebuild` forces a build). The core
  framework is built once per tree (`scripts/build-xcframework.sh`).
  **The self-test is a module of its own.** The app's code is the SwiftPM
  library `NVRViewApp`; `NVRViewSelfTest` holds the self-test and its mocks;
  the executable `NVRView` is only `main.swift`. The shipped app links no
  self-test (`--selftest` answers "this build has no self-test"); building
  with `NVRVIEW_SELFTEST=1` links it (what the checks and
  `scripts/macos-real-selftest.sh` do). The bundle test builds that variant in
  the same build folder, so `NVRViewApp`, the other modules and the core are
  the very objects the bundle was linked from, and runs it from a copy of the
  bundle with the self-test executable swapped in; the only difference from the
  shipped binary is the self-test module and the entry point that starts it.
  (`NVRViewApp` is built with `-enable-testing` in release so the self-test
  can `@testable import` its internals.) On Windows
  the stamp records the host that ran the app half (`; host NAME`), and
  `scripts/windows-check.sh --package` builds the package on that host, where
  the tested publish output is. Every Windows run prints a `split decision:`
  line saying whether the check was split over two computers and why not.
- **Named failures, narrow retries.** A failed Mac or Windows check (merges,
  trains, `scripts/release-check.sh`, `scripts/windows-check.sh`) prints the
  suite or self-test area that was running and the failing step's lines, for
  example `playback pacing: shown pictures are uneven (...)`. The merge scripts
  retry the Windows check (up to 3 tries) only after a known environmental
  flake (a software H.264 fallback, the real microphone step, the playback
  pacing jitter bound), and a retry runs only the suites and areas that did not
  pass: the earlier tries' passes come from this cache (`NVRVIEW_RETRY_SINCE`,
  set by the retry loop, makes that hold in a full run too). Any other failure
  is not retried. Each flake is appended to `flakes.jsonl` in the git
  directory (date, tree, suite, step, line; last 500); `scripts/flakes.sh`
  prints the most frequent flaky steps.
- **Never cached.** A suite with an unstaged or untracked input runs and is
  not recorded. Failures are never recorded. A pass from a Windows run retried
  after a known flake is recorded as retried and never skips a suite.
- **Full runs.** Nothing is skipped with `NVRVIEW_FULL_CHECK=1`, in the release
  scripts (`scripts/release.sh --full-check`, `scripts/release-check.sh`),
  and when no full run on that platform passed in the last 72 hours
  (`NVRVIEW_FULL_MAX_AGE_H`; `NVRVIEW_CACHE_MAX_AGE_H` sets how long a pass
  stays valid). `NVRVIEW_FULL_EVERY=N` also makes every Nth run full.
- **Stamps.** A check stamp from a run with cached suites says so and serves
  `scripts/merge-branch.sh`, which prints the cached suites; a release needs a
  full stamp.
- **Where.** The cache lives in the git directory
  (`.git/nvrview-checks/suite-cache/`), shared by all worktrees, and is
  pruned after 14 days. On Windows the Mac decides which suites to run and
  records the worker's results.
- **One decision per run.** `scripts/suite-cache.py plan` decides every suite
  once at the start of a check (the tree, tools, and store as they are then;
  a pass recorded mid-run never changes a later decision) and writes the
  answers to a plan file that `scripts/check.sh` and `scripts/windows-check.sh`
  read with `awk` (`scripts/suite-cache-lib.sh`). The scripts append each
  result to a run log with `printf`, and one `finish` call folds the log into
  the store, also from the exit trap, so an interrupted run keeps the passes
  of the suites that completed. Python runs twice per check, resolved once to
  the real interpreter (not the pyenv shim).
- **Tests.** `scripts/test_suite_cache.py` tests the decision rules in one
  process; `scripts/test-suite-cache.sh` tests the shell wiring (plan file,
  run log, finish, interrupted and retried runs). `scripts/run-script-tests.sh`
  runs them with the stamp, merge-train, Windows split, macOS package and
  background full check tests at the same time, printing
  each file's output in order.

### Build speed

Measured with `cargo build --timings` and Swift's `-warn-long-function-bodies`;
re-measure before changing any of it.

- **Release profile.** The root `Cargo.toml` turns LTO off (Cargo's default
  "thin local" LTO was about half of a cold release build) and keeps the hot
  decode crates on one code generation unit each. The comment there has the
  numbers, including the small CPU cost in the soak.
- **Swift release builds are one serial compile per module.** Whole-module
  optimization runs on one core, so a module's size is its build time: the
  app's code (`NVRViewApp`, about 22,000 lines) is most of the optimized Swift
  build, and the self-test (`NVRViewSelfTest`, about 7,700 lines) builds apart
  from it and not at all in the shipped app. No expression or function body
  takes long to type-check (one `body` at 376 ms with a 300 ms limit), so
  there is nothing to fix there. A further split of `NVRViewApp` needs the
  `AppModel` hub broken up first (the recordings and smart-alert screens and
  their models depend on it and it on them).
- **Not worth it, measured:** `-num-threads` for the Swift module (100 s
  against 105 s), and, for the Rust dependencies, trimming features (the
  heavy ones already use only the features they need).

## Versions

The apps share one version, the `VERSION` file at the repository root. Change
it there only. Tags and releases need the maintainer's approval.

**A version bump retests and rebuilds nothing.** Nothing a check looks at
depends on the version's value, so a commit that changes `VERSION` alone (and
no other file) after a checked tree is checked already:

- The version is not compiled in. The macOS bundle gets it in `Info.plist`
  when `scripts/package-macos.sh` packages it. The Windows build compiles a
  fixed placeholder (`apps/windows/Directory.Build.props`); the app reads its
  version from `version.txt` next to the executable (`AppVersion.Installed`),
  which `scripts/windows-app.sh` writes from `VERSION` after a build and again
  at packaging time. The fingerprint of the tested publish output leaves that
  one file out.
- `VERSION` is never an input of a suite in the test result cache
  (`NEVER_INPUT` in `scripts/suite-cache.py`), so `scripts/check.sh` and
  `scripts/windows-check.sh` report every suite cached after a bump.
- The check stamps are carried over (`carry_version_stamps` in
  `scripts/stamps-lib.sh`): the new tree gets a copy of the stamp of the tree
  that differs from it in `VERSION` alone, with `carried_from=<tree>` naming the
  tree whose check ran (and the same full or cached status). `scripts/checked.sh`,
  `scripts/branch-ready.sh`, `scripts/merge-branch.sh`, `scripts/merge-train.sh`,
  `scripts/full-check-bg.sh` (which starts no background check for such a
  commit) and `scripts/release.sh` all do this and say so in their output. A
  commit that changes `VERSION` and any other file is not covered.
- Packaging reuses the tested build: `scripts/package-macos.sh` reuses the
  tested bundle and rewrites `Info.plist` before signing; the Windows package
  (`scripts/windows-check.sh --package`, `scripts/windows-app.sh package`)
  reuses the publish output the carried stamp names and writes `version.txt`.
