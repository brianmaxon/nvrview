# Updates from a location you host

The macOS and Windows apps can update themselves from a folder or web server
that the user (or the maintainer) hosts. There is no vendor update server, no
vendor signing key, and no GitHub release feed: whoever hosts the updates
signs them with their own Ed25519 key, and each app accepts only what one of
the keys its user installed has signed.

Off by default. Nothing is fetched until the user sets an update source and
installs at least one trusted key, and nothing is applied until the user says
so.

## The parts

| part | where | what it does |
| --- | --- | --- |
| `crates/updates` | Rust, platform neutral | source rules, fetching (folder, `https://`, local `http://`), the manifest, signatures, versions, the package download, safe unpacking into a staging folder |
| C interface | `crates/viewer-ffi`, ABI 3 minor 2 | `nv_update_check`, `nv_update_download`, `nv_update_cancel`, `nv_update_parse_key`, `nv_update_parse_source`, the `update` event (docs/FFI.md, "Updates") |
| `tools/sign-update` | Rust command-line tool | key generation, building and signing the manifest, checking a hosted source the way the apps do |
| apply and relaunch | each app | moving the staged version into place after the app has quit, keeping the previous version, starting the new one |
| settings and menu | each app | the source, the trusted keys, Check for Updates, the status |

The core does everything that decides whether an update is trustworthy. The
apps only move an already verified, already unpacked version into place.

## What is hosted

A folder (a local folder, a mounted network share, or a directory on a web
server) holds:

```text
updates.json                     the manifest
updates.json.sig                 its detached signature(s)
NVRView-0.4.0-macos-arm64.zip    the macOS package
NvrView-0.4.0-win-x64.zip        the Windows package
```

### The manifest

```json
{
  "format": 1,
  "version": "0.4.0",
  "notes": "Short release notes, shown in the update prompt.",
  "packages": {
    "macos-arm64": {
      "file": "NVRView-0.4.0-macos-arm64.zip",
      "size": 31457280,
      "sha256": "<64 lowercase hex digits>"
    },
    "win-x64": {
      "file": "NvrView-0.4.0-win-x64.zip",
      "size": 73400320,
      "sha256": "<64 lowercase hex digits>"
    }
  }
}
```

- `format` is 1. A manifest with a higher format is refused
  (`unsupported_format`): an older app does not guess at a newer layout.
- `version` is a semantic version (`MAJOR.MINOR.PATCH`, optionally
  `-prerelease`; build metadata after `+` is ignored when comparing). The apps
  compare it with their own version, which both read from the `VERSION` file
  at the repository root when they are built.
- `notes` is plain text, at most 2000 characters.
- `packages` has one entry per platform; a platform without an entry has no
  update in this release. The platform names are `macos-arm64` and `win-x64`
  (others may follow).
- `file` is a bare file name in the same folder as the manifest: letters,
  digits, `.`, `_`, `-`, and `+`, ending in `.zip`, at most 128 characters; no
  path separators and no `..`. `size` is 1 byte to 1 GiB.
- Unknown members are ignored, so later formats can add some without breaking
  older apps.
- The whole manifest is at most 64 KiB.

### The signature

`updates.json.sig` is text, one signature per line:

```text
nvrview-ed25519-sig <key id> <128 hex digits>
```

Each line is an Ed25519 signature over the exact bytes of `updates.json`,
prefixed with the context string `nvrview-update-manifest-v1` and a NUL byte
(so a signature made for this purpose cannot be mistaken for any other use of
the same key). The key ID is the first 8 bytes of the SHA-256 of the public
key, in hex. The manifest is accepted when **any** line verifies with **any**
trusted key; several lines let a host sign with an old and a new key while
users move from one to the other. Blank lines and lines starting with `#` are
ignored; the file is at most 4 KiB.

### Keys

```text
nvrview-ed25519-pub <64 hex digits>       a public key (what users install)
nvrview-ed25519-secret <64 hex digits>    a secret key (the 32-byte seed)
```

The apps accept a pasted public key line, a bare 64-digit hex key, or a file
holding one (lines starting with `#` are comments). They show each key by its
key ID.

### Packages

Packages are zip files, stored or deflated (no encryption, no zip64, no
multi-part archives), with printable ASCII file names.

- **macOS**: `NVRView-<version>-macos-arm64.zip`, holding `NVRView.app` at the
  top, as `ditto -c -k --norsrc --noextattr --keepParent build/NVRView.app`
  makes it (Unix permissions and symbolic links inside the bundle are kept;
  extended attributes are not needed and not restored). Extended attributes
  that archivers store as files are skipped: a `__MACOSX/` folder (Finder's
  Compress) and `._name` AppleDouble files (plain `ditto -c -k` without
  `--norsrc` writes one beside every file; measured). Unpacked, they would
  add files to the bundle and break its code signature.
- **Windows**: `NvrView-<version>-win-x64.zip`, holding the contents of the
  app folder at the top (`NvrView.App.exe`, `nvrview.dll`, and the rest).
  Symbolic links are refused on Windows.

## Update sources

The user's update source is one of:

- **A folder**: an absolute path, including a mounted network share
  (`/Volumes/share/nvrview`, `\\server\share\nvrview`, a mapped drive), or a
  `file://` URL.
- **`https://`** to any host. Certificates are checked against the Mozilla
  root list built into the core (`webpki-roots`), not the system's trust
  settings, so a server with a private certificate authority needs the
  `http://` rule below instead.
- **`http://`** only to a host on the local network: a loopback, private
  (RFC 1918, and IPv6 unique local `fc00::/7`), or link-local address, `localhost`, or a
  name ending in `.local`. The check is on the host as written in the URL.
  Everything fetched is signature-checked whatever the transport, so plain
  HTTP is acceptable there; any other `http://` source is refused
  (`insecure_source`).

A URL source names the folder: `https://updates.example.com/nvrview/` and
`https://updates.example.com/nvrview/updates.json` both mean the manifest is
`.../nvrview/updates.json` and the packages sit next to it. URLs with a user
name or password in them are refused (they would end up in logs and
settings).

### Fetching rules

- **Redirects**: at most 5 per request, each one checked against the source
  rules again (an `https://` source never follows a redirect to `http://`,
  and an `http://` redirect target must be a local host). Redirects are
  followed by the core, not the HTTP library, so the rules apply to every
  hop.
- **Timeouts**: 10 s to resolve and connect, 20 s for a response to begin,
  30 s to read the manifest or signature, 30 minutes for a package.
- **Limits**: manifest 64 KiB, signature 4 KiB, package exactly the size the
  manifest names (and never over 1 GiB). A response that announces or delivers
  more is cut off and refused (`too_large`).
- **TLS**: rustls with the `graviola` crypto provider (pure Rust and
  assembly, no C toolchain needed to build the core for any target). On an
  x86-64 processor without AES-NI, AVX2, BMI2, ADX, and PCLMULQDQ (roughly
  anything older than 2014) HTTPS sources are refused with
  `unsupported_cpu`; folders and local HTTP still work.
- A folder that is not mounted, a server that does not answer, and a missing
  file are all `unreachable` or `not_found`; they are reported, logged, and
  never block the viewer.

## Checking and applying

### Checking

1. Fetch `updates.json` and `updates.json.sig` (size-limited).
2. Verify a signature line against the trusted keys. No trusted key, or no
   line that verifies, is a failure (`no_trusted_keys`, `bad_signature`):
   there is no trust on first use, and no key ships with the app or the
   repository.
3. Parse and validate the manifest (only after the signature verified).
4. Compare versions. Only a version **strictly newer** than the running one
   is offered (`available`); an equal or older one is `up_to_date`. The
   manifest cannot make an app go back: there is no downgrade through this
   path. (Going back is done by hand, see "Rolling back".)
5. Find this platform's package. None is `up_to_date` with `no_package`.

### Downloading and staging (after the user says yes)

1. Fetch and verify the manifest again (steps 1 to 5 above). The version
   must still be the one the user accepted; a source that changed in between
   is refused (`version_changed`).
2. Download the package into the staging folder, counting bytes as they
   arrive: more than the manifest's `size` stops the download.
3. Check the size and the SHA-256 against the signed manifest. Nothing is
   unpacked before both match (`bad_package` otherwise).
4. Unpack into the staging folder, refusing the whole package
   (`unsafe_archive`) for:
   - absolute paths, drive letters, `..` components, empty components, or
     names that are not printable ASCII or that Windows would treat
     specially (`:`, `<>"|?*`, a trailing dot or space, `CON`, `NUL`, ...);
   - two entries whose paths differ only in letter case;
   - symbolic links whose target is absolute or leaves the package (targets
     are resolved through the package's other links), and any entry whose
     path runs through a symbolic link; symbolic links at all on Windows;
   - encrypted entries, zip64, unknown compression methods, entries whose
     data overlaps, a CRC mismatch, sizes that do not match, more than 200,000
     entries, or more than 4 GiB unpacked.

   Files are created new (never opened through something already there),
   symbolic links are created last, and permissions are reduced to
   `0644`/`0755`.
5. Delete the zip and report `ready` with the unpacked folder.

The staging folder is next to the installed app, so moving the new version
into place is a rename on the same volume, never a copy:

- macOS: `<folder holding NVRView.app>/.NVRView-update/`
- Windows: `<folder holding the app folder>\.NvrView-update\`

Leftovers from an interrupted download are removed the next time the app
starts. On any failure the core removes everything it created; nothing next
to the app changes.

### Applying (the user always confirms)

When the new version is ready the app asks once: **"Update to 0.4.0?"** with
**Restart Now** and **Later**. Live view keeps running while the check, the
download, and the question are on screen.

#### macOS

The app starts its own executable again as a helper,
`NVRView --apply-update ...`, and quits normally (closing every device
session). The helper:

1. waits for the app's process to exit (at most 2 minutes; otherwise it gives
   up and changes nothing);
2. checks the staged `NVRView.app`: its `CFBundleIdentifier` matches the
   installed app's, its `CFBundleShortVersionString` is the accepted version,
   and `codesign --verify --deep --strict` accepts it;
3. swaps the staged bundle and the installed one in a single atomic rename
   (`renamex_np` with `RENAME_SWAP`), so `NVRView.app` is never missing;
4. moves the old bundle to `.NVRView-update/previous/NVRView.app` (replacing
   an older one kept there);
5. with Restart Now, opens the new `NVRView.app` with `open`.

**Later** installs the same way when the user next quits the app, without
reopening it. The helper writes what it did to `.NVRView-update/update.log`,
and the app shows the outcome the next time it starts.

The app refuses to update, with a message saying why, when it runs:

- from a translocated location (a quarantined app opened where it was
  downloaded; macOS runs it from a random read-only path): move it to
  Applications, open it from there, and update again;
- from a read-only volume (a disk image);
- from a folder the user cannot write to (installed by another user);
- not from an app bundle at all (a development build).

#### Windows

Versions live side by side: the update is unpacked into a new folder next to
the running one, named `NvrView-<version>` (with `-2`, `-3`, ... if that name
is taken), and the running folder is never modified.

**Restart Now** starts `NvrView-<version>\NvrView.App.exe --wait-for-pid
<pid>` and closes the app. The new version waits for the old process to exit
(at most 2 minutes) before it logs in to any device, so the NVR never sees
both versions' sessions at once. **Later** leaves the new folder in place
(marked with a `.nvrview-version` file); when the old version next checks
and is offered that version, it offers the switch again without
downloading it.

The folder-switch habit: shortcuts and pinned taskbar entries point at the
old folder's `NvrView.App.exe`. After an update, open the new folder and pin
or link its `NvrView.App.exe` instead (the Settings page shows which folder
the running app lives in). Delete old version folders by hand once the new
one works; the app never deletes them.

No step needs administrator rights. An app installed under `Program Files`
(not writable without elevation) reports that it cannot stage the update and
asks for the folder to be moved somewhere the user owns (for example
`%LOCALAPPDATA%\Programs`).

### Never touched

An update never reads, writes, or deletes the data directory
(`~/Library/Application Support/NVRView` on macOS, `%LOCALAPPDATA%\NvrView` on
Windows: devices, layouts, groups, window positions, the core's store, logs)
or device settings. Passwords stay in the Keychain or Credential Manager.
Update settings (source, trusted keys, automatic checks, the last check time)
are kept with the app's other settings in the data directory, written by the
app as it writes any setting, never by the updater.

### Rolling back

- macOS: quit NVRView, then move `.NVRView-update/previous/NVRView.app` (in the
  folder holding NVRView.app; press Command-Shift-. in Finder to show hidden
  folders) back in place of `NVRView.app`.
- Windows: start the previous folder's `NvrView.App.exe`.

## Settings and checks in the apps

- **Settings, Updates** (a tab on macOS; a section under the devices in the
  Windows Settings dialog): the update source (a text field; on macOS also
  Choose Folder), the trusted keys (listed by key ID; Add Key takes a pasted
  key, and on macOS Choose File reads a key file; Remove), **Check
  automatically** (off by default), **Check Now**, and one line of status
  ("Up to date (0.3.0), checked 10:42", "Version 0.4.0 is available", "The
  update's signature did not verify with any trusted key."). Windows also
  shows the folder the running version lives in.
- **Menu**: "Check for Updates..." in the app menu (macOS) or the stage's
  right-click menu after "Settings..." (Windows). When an automatic check
  finds a newer version, the item reads "Update to 0.4.0..." instead;
  automatic checks never open a window on their own.
- **Schedule**: with a source, a key, and Check automatically on, the app
  checks once shortly after it starts and then about every 24 hours while it
  runs (the time of the last check is kept, so restarting the app does not
  check again within the 24 hours). The schedule runs on the app's clock
  abstraction, and tests move a simulated clock; no test waits for real time.
- **Tests and self-tests** never check for updates unless a test enables it
  explicitly with its own temporary source and key.
- **Logging**: each check, download, and apply logs one line with its outcome
  (failures with the reason); the core's log is rate-limited as always, and
  sources are logged at debug level only (they can name hosts on the user's
  network).

## Maintainer workflow

Done once:

```sh
cargo run -p sign-update -- keygen --secret ~/.config/nvrview-signing/update.key
```

writes the secret key with mode `0600` (refusing to overwrite a file, and
refusing any path inside a git working tree) and prints the public key and
its ID. Keep the secret key off the repository and back it up; publish the
public key line where users can find it (release notes, the project page).

For each release:

```sh
scripts/package-macos.sh     # build/NVRView.app and build/NVRView-<version>-macos-arm64.zip
cp build/NVRView-"$(cat VERSION)"-macos-arm64.zip dist/
# on Windows, from the app folder (Windows' own tar writes zip with -a):
#   tar.exe -a -c -f dist\NvrView-<version>-win-x64.zip *
# (scripts/windows-app.sh update does exactly this for its test)

cargo run -p sign-update -- manifest --version "$(cat VERSION)" \
  --notes-file notes.txt --out dist/updates.json dist/*.zip
cargo run -p sign-update -- sign --secret ~/.config/nvrview-signing/update.key dist/updates.json
cargo run -p sign-update -- check --source dist --key update.pub --current 0.0.0
```

`manifest` reads each package's platform and version from its name
(`<name>-<version>-<platform>.zip`), refuses names that disagree with
`--version`, and computes size and SHA-256. `sign` writes
`updates.json.sig` (`--append` adds a line for a second key during a
rotation). `check` runs the same core code as the apps against the folder or
URL: signature, manifest, download, hash, and unpacking into a temporary
folder, then reports what an app of version `--current` would be offered.
Upload the whole `dist/` folder to the update location, packages first and
`updates.json` and its signature last, so a client never sees a manifest
whose package is not there yet.

### Rotating a key

1. Generate the new key; publish its public key.
2. Sign releases with both keys (`sign` with the old key, then `sign
   --append` with the new one) for a while; users add the new key in
   Settings.
3. Sign with the new key only; users remove the old key.

A lost or leaked secret key cannot be revoked remotely: users remove it from
Settings. That is the price of having no central authority.

## Self-hoster workflow

To serve updates to your own computers (from the maintainer's builds or your
own):

1. Build or download the packages and the manifest. To trust the
   maintainer's signature, install the maintainer's public key and copy the
   four files as they are. To build and sign yourself, follow the maintainer
   workflow with your own key.
2. Put the files in a folder: a share every computer mounts, or a directory
   your web server serves (`https://`, or `http://` on your local network).
3. In each app's Settings, Updates: set the source to that folder or URL,
   add the public key, and press Check Now. Turn on Check automatically if
   you want the daily check.
4. `sign-update check --source <folder or URL> --key <public key file>
   --current <installed version>` from any computer shows what the apps will
   see, and why a check fails.

## macOS: Gatekeeper, the Keychain, and Local Network access

- **Quarantine.** Files the core writes are new files: their bytes are
  copied, not their extended attributes, and the app does not opt into file
  quarantine (`LSFileQuarantineEnabled`) and is not sandboxed. So a bundle
  unpacked from a quarantined zip or from a network share carries no
  `com.apple.quarantine` attribute, Gatekeeper does not assess it as a
  download, and it is not translocated when opened. What was measured, and
  how, is recorded in "Measured on macOS" below.
- **Keychain.** Saved device passwords are generic-password items whose
  access list trusts the app's designated requirement. A bundle signed with
  an Apple Development (or Developer ID) identity has a requirement naming
  the bundle ID and the certificate, which every later build signed with the
  same identity also meets: updated apps read the passwords without a
  prompt. An ad hoc signature's requirement is the build's own code hash,
  which every new build changes: after an update macOS asks once per saved
  password ("NVRView wants to use your confidential information...");
  Always Allow fixes it until the next update. Hosts that serve updates to
  others should sign with a stable identity.
- **Local Network.** Not measured yet (it needs a person to see the system
  prompt; "Testing by hand", step 4). Until then, assume the same as for the
  Keychain: a stable signing identity keeps the permission, an ad hoc
  signature may ask again.

### Measured on macOS

`scripts/test-update-macos.sh` (macOS 26, Apple silicon) builds a throwaway
app bundle (a small Mach-O, signed ad hoc) in a temporary folder, publishes
version 0.4.0 of it in a folder source whose manifest, signature, and zip
carry a browser-style `com.apple.quarantine` attribute, stages it with the
core's own code, and installs it with the real `NVRView --apply-update`
helper while the old version runs. Measured:

- the staged and the installed bundle carry **no quarantine attribute**,
  although every source file did;
- `codesign --verify --deep --strict` accepts the staged bundle (permissions
  and the ad hoc signature survive `ditto --norsrc --noextattr`, the zip, and
  the unpacking);
- the helper waited for the running process to exit, swapped the bundles,
  kept the previous one in `.NVRView-update/previous`, and `open` started
  the new version **from its own path, not translocated**;
- `spctl --assess` rejects the ad hoc bundle, as it rejects any bundle that
  is not notarized; that assessment only gates quarantined apps, so it did
  not stop the launch.

Not measured: a real SMB or AFP share as the source (a local folder with
quarantined files stood in for it; the core reads bytes either way and
writes new files), and the real NVRView.app (never replaced by the tests).
Local Network permission after an update needs a person to watch for the
system prompt; see "Testing by hand".

### Measured on Windows

`scripts/windows-app.sh update` (run by `scripts/windows-worker.sh` with
`NVRVIEW_APP_SELFTEST=1`) packages the freshly built app folder, signs it
with a key made for the run, stages it with the core (`sign-update check
--stage`, the same code as the app), moves it to `NvrView-<version>`, and
starts that copy with `--wait-for-pid` on a running stand-in process and
`--startup-test`: it must wait for the process, then start and pass.

### Testing by hand

1. Package two versions of the real app (`scripts/package-macos.sh` writes
   `build/NVRView-<version>-macos-arm64.zip`; bump `VERSION` for the second).
2. Publish the newer one in a folder with `sign-update` and a test key; put
   the older `NVRView.app` in a folder you own (not the copy you use daily).
3. In its Settings, Updates: set the folder, add the key, Check Now, accept,
   Restart Now. Expected: the newer version opens with the same devices,
   layouts, and passwords. With an Apple Development signature, no Keychain
   prompt; with an ad hoc signature, one prompt per saved password.
4. Watch whether macOS asks for Local Network access again after the update,
   for each kind of signature, and record the answer here.
