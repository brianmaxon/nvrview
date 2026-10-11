//! Checks, downloads, and unpacking against a local folder and a local
//! HTTP and HTTPS server (tests/common), with keys made at run time.

mod common;

use common::*;
use std::fs;
use std::path::Path;
use std::time::Duration;
use updates::{
    Cancel, CheckOutcome, Config, PublicKey, Reason, Source, UpdateError, Version, check, download,
};

const PKG: &str = "App-0.4.0-test-os.zip";

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn config(source: Source, keys: Vec<PublicKey>, current: &str) -> Config {
    let mut c = Config::new(source, keys, v(current));
    c.platform = PLATFORM.into();
    c.timeouts.connect = Duration::from_secs(2);
    c.timeouts.response = Duration::from_secs(2);
    c.timeouts.small_body = Duration::from_secs(5);
    c.timeouts.package_body = Duration::from_secs(10);
    c
}

fn folder_config(dir: &Path, keys: Vec<PublicKey>, current: &str) -> Config {
    config(Source::Folder(dir.to_path_buf()), keys, current)
}

fn fetch_staged(
    c: &Config,
    accepted: &str,
) -> (tempfile::TempDir, Result<updates::Staged, UpdateError>) {
    let work = tempfile::tempdir().unwrap();
    let staging = work.path().join(".App-update").join("staging-1");
    let mut seen = Vec::new();
    let r = download(c, &v(accepted), &staging, &Cancel::new(), &mut |a, b| {
        seen.push((a, b))
    });
    if r.is_ok() {
        assert_eq!(seen.first(), Some(&(0, seen[0].1)));
        assert_eq!(
            seen.last().map(|p| p.0),
            seen.last().map(|p| p.1),
            "progress ends at the size"
        );
    } else {
        // Nothing left behind on failure.
        assert!(!staging.exists(), "staging folder left after {r:?}");
    }
    (work, r)
}

fn reason<T: std::fmt::Debug>(r: Result<T, UpdateError>) -> Reason {
    r.unwrap_err().reason
}

fn tree(root: &Path) -> Vec<String> {
    updates::zip::list_tree(root).unwrap()
}

fn assert_app_unpacked(content: &Path) {
    let mut expected = vec![
        "App/",
        "App/bin/",
        "App/bin/app",
        "App/data.txt",
        "App/stored.bin",
    ];
    if cfg!(unix) {
        expected.insert(3, "App/current");
    }
    assert_eq!(tree(content), expected);
    assert_eq!(
        fs::read(content.join("App/data.txt")).unwrap(),
        b"data ".repeat(10_000)
    );
    assert_eq!(
        fs::read(content.join("App/stored.bin")).unwrap(),
        b"stored bytes"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &str| fs::metadata(content.join(p)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("App/bin/app"), 0o755);
        assert_eq!(mode("App/data.txt"), 0o644);
        assert_eq!(
            fs::read_link(content.join("App/current")).unwrap(),
            std::path::PathBuf::from("bin")
        );
    }
}

#[test]
fn folder_source_offers_and_stages_a_newer_version() {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    publish(src.path(), &key, "0.4.0", PKG, &app_zip());
    let c = folder_config(src.path(), vec![key.public()], "0.3.0");
    match check(&c, &Cancel::new()).unwrap() {
        CheckOutcome::Available(o) => {
            assert_eq!(o.version, v("0.4.0"));
            assert_eq!(o.notes, "Test release.");
            assert_eq!(o.package.file, PKG);
            assert_eq!(o.key_id, key.public().id());
        }
        other => panic!("{other:?}"),
    }
    let (_work, staged) = fetch_staged(&c, "0.4.0");
    let staged = staged.unwrap();
    assert_eq!(staged.version, v("0.4.0"));
    assert_app_unpacked(&staged.content);
    // The zip is gone; only the unpacked content stays.
    let staging = staged.content.parent().unwrap();
    assert_eq!(fs::read_dir(staging).unwrap().count(), 1);
}

#[test]
fn the_source_may_name_the_manifest_itself() {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    publish(src.path(), &key, "0.4.0", PKG, &app_zip());
    let s = Source::parse(src.path().join("updates.json").to_str().unwrap()).unwrap();
    let c = config(s, vec![key.public()], "0.3.0");
    assert!(matches!(
        check(&c, &Cancel::new()).unwrap(),
        CheckOutcome::Available(_)
    ));
}

#[test]
fn keys_are_required_and_must_match() {
    let key = new_key();
    let other = new_key();
    let src = tempfile::tempdir().unwrap();
    publish(src.path(), &key, "0.4.0", PKG, &app_zip());
    let none = folder_config(src.path(), vec![], "0.3.0");
    assert_eq!(reason(check(&none, &Cancel::new())), Reason::NoTrustedKeys);
    let wrong = folder_config(src.path(), vec![other.public()], "0.3.0");
    assert_eq!(reason(check(&wrong, &Cancel::new())), Reason::BadSignature);
    let (_w, r) = fetch_staged(&wrong, "0.4.0");
    assert_eq!(reason(r), Reason::BadSignature);
    // Rotation: either of two trusted keys will do.
    let both = folder_config(src.path(), vec![other.public(), key.public()], "0.3.0");
    assert!(matches!(
        check(&both, &Cancel::new()).unwrap(),
        CheckOutcome::Available(_)
    ));
}

#[test]
fn a_tampered_or_unsigned_manifest_is_refused() {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    publish(src.path(), &key, "0.4.0", PKG, &app_zip());
    let c = folder_config(src.path(), vec![key.public()], "0.3.0");
    let original = fs::read_to_string(src.path().join("updates.json")).unwrap();
    // One changed byte (a version that would win), signature kept.
    fs::write(
        src.path().join("updates.json"),
        original.replace("0.4.0", "9.4.0"),
    )
    .unwrap();
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::BadSignature);
    // Whitespace counts too: the signature is over the exact bytes.
    fs::write(src.path().join("updates.json"), format!("{original} ")).unwrap();
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::BadSignature);
    fs::write(src.path().join("updates.json"), &original).unwrap();
    fs::remove_file(src.path().join("updates.json.sig")).unwrap();
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::BadSignature);
    fs::write(src.path().join("updates.json.sig"), "not a signature\n").unwrap();
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::BadSignature);
}

#[test]
fn a_tampered_package_is_never_unpacked() {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    let mut package = app_zip();
    publish(src.path(), &key, "0.4.0", PKG, &package);
    let c = folder_config(src.path(), vec![key.public()], "0.3.0");
    // Same size, one byte flipped.
    package[40] ^= 0x01;
    fs::write(src.path().join(PKG), &package).unwrap();
    let (_w, r) = fetch_staged(&c, "0.4.0");
    assert_eq!(reason(r), Reason::BadPackage);
    // Shorter than the manifest says.
    fs::write(src.path().join(PKG), &package[..100]).unwrap();
    let (_w, r) = fetch_staged(&c, "0.4.0");
    assert_eq!(reason(r), Reason::BadPackage);
    // Longer: refused before reading.
    let mut longer = app_zip();
    longer.push(0);
    fs::write(src.path().join(PKG), &longer).unwrap();
    let (_w, r) = fetch_staged(&c, "0.4.0");
    assert_eq!(reason(r), Reason::TooLarge);
    fs::remove_file(src.path().join(PKG)).unwrap();
    let (_w, r) = fetch_staged(&c, "0.4.0");
    assert_eq!(reason(r), Reason::NotFound);
}

#[test]
fn only_strictly_newer_versions_are_offered() {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    publish(src.path(), &key, "0.4.0", PKG, &app_zip());
    for current in ["0.4.0", "0.5.0", "1.0.0", "0.4.0+build"] {
        let c = folder_config(src.path(), vec![key.public()], current);
        assert_eq!(
            check(&c, &Cancel::new()).unwrap(),
            CheckOutcome::UpToDate {
                latest: v("0.4.0"),
                no_package: false
            },
            "{current}"
        );
    }
    // The manifest cannot push an older version through download either.
    let c = folder_config(src.path(), vec![key.public()], "0.5.0");
    let (_w, r) = fetch_staged(&c, "0.4.0");
    assert_eq!(reason(r), Reason::NotNewer);
    // A pre-release of 0.4.0 is older than 0.4.0.
    let c = folder_config(src.path(), vec![key.public()], "0.4.0-beta.1");
    assert!(matches!(
        check(&c, &Cancel::new()).unwrap(),
        CheckOutcome::Available(_)
    ));
}

#[test]
fn no_package_for_this_platform() {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    publish(src.path(), &key, "0.4.0", PKG, &app_zip());
    let mut c = folder_config(src.path(), vec![key.public()], "0.3.0");
    c.platform = "other-os".into();
    assert_eq!(
        check(&c, &Cancel::new()).unwrap(),
        CheckOutcome::UpToDate {
            latest: v("0.4.0"),
            no_package: true
        }
    );
}

#[test]
fn the_version_must_not_change_between_check_and_download() {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    publish(src.path(), &key, "0.5.0", "App-0.5.0.zip", &app_zip());
    let c = folder_config(src.path(), vec![key.public()], "0.3.0");
    let (_w, r) = fetch_staged(&c, "0.4.0");
    assert_eq!(reason(r), Reason::VersionChanged);
}

#[test]
fn an_existing_staging_folder_is_left_alone() {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    publish(src.path(), &key, "0.4.0", PKG, &app_zip());
    let c = folder_config(src.path(), vec![key.public()], "0.3.0");
    let work = tempfile::tempdir().unwrap();
    let staging = work.path().join("staging");
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("keep.txt"), b"mine").unwrap();
    let r = download(&c, &v("0.4.0"), &staging, &Cancel::new(), &mut |_, _| {});
    assert_eq!(reason(r), Reason::Staging);
    assert_eq!(fs::read(staging.join("keep.txt")).unwrap(), b"mine");
    let relative = download(
        &c,
        &v("0.4.0"),
        Path::new("rel"),
        &Cancel::new(),
        &mut |_, _| {},
    );
    assert_eq!(reason(relative), Reason::Staging);
}

#[test]
fn canceling_stops_and_cleans_up() {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    publish(src.path(), &key, "0.4.0", PKG, &app_zip());
    let c = folder_config(src.path(), vec![key.public()], "0.3.0");
    let cancel = Cancel::new();
    let work = tempfile::tempdir().unwrap();
    let staging = work.path().join("staging");
    let r = download(&c, &v("0.4.0"), &staging, &cancel, &mut |received, _| {
        if received > 0 {
            cancel.cancel();
        }
    });
    assert_eq!(reason(r), Reason::Canceled);
    assert!(!staging.exists());
    let pre = Cancel::new();
    pre.cancel();
    assert_eq!(reason(check(&c, &pre)), Reason::Canceled);
}

#[test]
fn oversized_manifest_in_a_folder() {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    let padded = format!(
        "{}{}",
        manifest("0.4.0", PKG, &app_zip()),
        " ".repeat(70 * 1024)
    );
    write_signed(src.path(), &key, &padded);
    let c = folder_config(src.path(), vec![key.public()], "0.3.0");
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::TooLarge);
}

#[test]
fn a_missing_folder_is_unreachable() {
    let key = new_key();
    let gone = tempfile::tempdir().unwrap().path().join("not-mounted");
    let c = folder_config(&gone, vec![key.public()], "0.3.0");
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::Unreachable);
    let empty = tempfile::tempdir().unwrap();
    let c = folder_config(empty.path(), vec![key.public()], "0.3.0");
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::NotFound);
}

// ---- unsafe archives ---------------------------------------------------

/// Publishes `package` and tries to stage it; returns the result and
/// checks that nothing appeared next to the staging folder.
fn stage_package(package: &[u8]) -> Result<updates::Staged, UpdateError> {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    publish(src.path(), &key, "0.4.0", PKG, package);
    let c = folder_config(src.path(), vec![key.public()], "0.3.0");
    let work = tempfile::tempdir().unwrap();
    let parent = work.path().join("a").join("b");
    let staging = parent.join("staging");
    let r = download(&c, &v("0.4.0"), &staging, &Cancel::new(), &mut |_, _| {});
    if r.is_err() {
        assert!(!staging.exists());
        // Nothing escaped into the folders above.
        assert_eq!(tree(work.path()), ["a/", "a/b/"]);
    }
    r.map(|s| {
        // Keep the folder alive for the caller's checks.
        let kept = tempfile::tempdir().unwrap().keep();
        let dest = kept.join("content");
        fs::rename(&s.content, &dest).unwrap();
        updates::Staged {
            version: s.version,
            content: dest,
        }
    })
}

fn unsafe_reason(entries: &[ZEntry]) -> Reason {
    reason(stage_package(&zip(entries)))
}

#[test]
fn path_traversal_is_refused() {
    let ok = ZEntry::file("App/ok.txt", b"ok");
    for name in [
        "../evil.txt",
        "App/../../evil.txt",
        "/tmp/evil.txt",
        "\\evil.txt",
        "App\\..\\..\\evil.txt",
        "C:/evil.txt",
        "App/evil.txt:stream",
        "App//evil.txt",
        "App/./evil.txt",
        "App/CON",
        "App/evil. ",
        "App/caf\u{e9}.txt",
    ] {
        let r = unsafe_reason(&[ok.clone(), ZEntry::file(name, b"x")]);
        assert_eq!(r, Reason::UnsafeArchive, "{name:?}");
    }
    // The local header naming a different file than the directory.
    let mut sneaky = ZEntry::file("App/fine.txt", b"x");
    sneaky.local_name = Some("../../evil.txt".into());
    assert_eq!(unsafe_reason(&[sneaky]), Reason::UnsafeArchive);
}

#[cfg(unix)]
#[test]
fn links_must_stay_inside() {
    let base = || vec![ZEntry::dir("App/"), ZEntry::file("App/a.txt", b"a")];
    for (link, target) in [
        ("App/out", "../../outside"),
        ("App/abs", "/etc"),
        ("App/up", "../.."),
        ("App/deep", "../App/../.."),
        ("App/empty", ""),
        ("App/back", "a\\..\\.."),
    ] {
        let mut e = base();
        e.push(ZEntry::symlink(link, target));
        assert_eq!(
            unsafe_reason(&e),
            Reason::UnsafeArchive,
            "{link} -> {target}"
        );
    }
    // Out through another link: `here` is the package's root, so `here/..`
    // is above it.
    let mut e = base();
    e.push(ZEntry::symlink("here", "."));
    e.push(ZEntry::symlink("escape", "here/.."));
    assert_eq!(unsafe_reason(&e), Reason::UnsafeArchive);
    // Writing through a link, even one pointing inside.
    let mut e = base();
    e.push(ZEntry::symlink("App/link", "."));
    e.push(ZEntry::file("App/link/x.txt", b"x"));
    assert_eq!(unsafe_reason(&e), Reason::UnsafeArchive);
    // A link to the package's own root is inside.
    let mut e = base();
    e.push(ZEntry::symlink("App/root", ".."));
    assert!(stage_package(&zip(&e)).is_ok());
    // Links inside are kept: a framework's usual layout.
    let staged = stage_package(&zip(&[
        ZEntry::dir("F.framework/Versions/A/"),
        ZEntry::exec("F.framework/Versions/A/F", b"binary"),
        ZEntry::symlink("F.framework/Versions/Current", "A"),
        ZEntry::symlink("F.framework/F", "Versions/Current/F"),
    ]))
    .unwrap();
    assert_eq!(
        fs::read(staged.content.join("F.framework/F")).unwrap(),
        b"binary"
    );
    let _ = fs::remove_dir_all(staged.content.parent().unwrap());
}

#[cfg(not(unix))]
#[test]
fn links_are_refused_where_unsupported() {
    let e = [
        ZEntry::file("App/a.txt", b"a"),
        ZEntry::symlink("App/l", "a.txt"),
    ];
    assert_eq!(unsafe_reason(&e), Reason::UnsafeArchive);
}

#[test]
fn malformed_archives_are_refused() {
    // Two names that differ only in case.
    assert_eq!(
        unsafe_reason(&[
            ZEntry::file("App/x.txt", b"1"),
            ZEntry::file("app/X.TXT", b"2")
        ]),
        Reason::UnsafeArchive
    );
    // A file used as a folder.
    assert_eq!(
        unsafe_reason(&[ZEntry::file("App/x", b"1"), ZEntry::file("App/x/y", b"2")]),
        Reason::UnsafeArchive
    );
    // Wrong CRC, wrong size, encryption flag.
    let mut bad_crc = ZEntry::file("App/x", b"hello");
    bad_crc.crc = Some(1);
    assert_eq!(unsafe_reason(&[bad_crc]), Reason::UnsafeArchive);
    let mut bad_size = ZEntry::file("App/x", b"hello");
    bad_size.size = Some(3);
    assert_eq!(unsafe_reason(&[bad_size]), Reason::UnsafeArchive);
    let mut encrypted = ZEntry::file("App/x", b"hello");
    encrypted.flags = 1;
    assert_eq!(unsafe_reason(&[encrypted]), Reason::UnsafeArchive);
    // A device file.
    let mut fifo = ZEntry::file("App/fifo", b"");
    fifo.mode = Some(0o010_644);
    assert_eq!(unsafe_reason(&[fifo]), Reason::UnsafeArchive);
    // Not a zip at all.
    assert_eq!(
        reason(stage_package(b"definitely not a zip file at all")),
        Reason::UnsafeArchive
    );
    // A truncated archive (its end record cut off).
    let whole = app_zip();
    assert_eq!(
        reason(stage_package(&whole[..whole.len() - 10])),
        Reason::UnsafeArchive
    );
}

#[test]
fn unpacked_size_is_limited() {
    let key = new_key();
    let src = tempfile::tempdir().unwrap();
    // 4 MiB of zeros deflates to a few KiB.
    let bomb = zip(&[ZEntry::file("App/zeros", &vec![0u8; 4 << 20])]);
    assert!(bomb.len() < 64 * 1024);
    publish(src.path(), &key, "0.4.0", PKG, &bomb);
    let mut c = folder_config(src.path(), vec![key.public()], "0.3.0");
    c.archive.max_total = 1 << 20;
    let (_w, r) = fetch_staged(&c, "0.4.0");
    assert_eq!(reason(r), Reason::UnsafeArchive);
    c.archive.max_total = 8 << 20;
    c.archive.max_entries = 0;
    let (_w, r) = fetch_staged(&c, "0.4.0");
    assert_eq!(reason(r), Reason::UnsafeArchive);
}

#[test]
fn finder_resource_forks_are_skipped() {
    let staged = stage_package(&zip(&[
        ZEntry::file("App/a.txt", b"a"),
        ZEntry::dir("__MACOSX/"),
        ZEntry::file("__MACOSX/App/._a.txt", b"resource fork"),
        ZEntry::file("App/._a.txt", b"attributes beside the file"),
    ]))
    .unwrap();
    assert_eq!(tree(&staged.content), ["App/", "App/a.txt"]);
    let _ = fs::remove_dir_all(staged.content.parent().unwrap());
}

/// A zip made by the system's own tool (ditto on macOS), with an
/// executable and a link, unpacks to the same tree.
#[cfg(target_os = "macos")]
#[test]
fn a_ditto_zip_unpacks_faithfully() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let work = tempfile::tempdir().unwrap();
    let app = work.path().join("Test.app");
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::write(app.join("Contents/Info.plist"), b"<plist/>").unwrap();
    fs::write(app.join("Contents/MacOS/Test"), b"#!/bin/sh\n").unwrap();
    fs::set_permissions(
        app.join("Contents/MacOS/Test"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::os::unix::fs::symlink("MacOS/Test", app.join("Contents/Link")).unwrap();
    for extra in [&[][..], &["--sequesterRsrc"][..]] {
        let zip_path = work.path().join("t.zip");
        let _ = fs::remove_file(&zip_path);
        let status = Command::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .args(extra)
            .arg(&app)
            .arg(&zip_path)
            .status()
            .unwrap();
        assert!(status.success());
        let staged = stage_package(&fs::read(&zip_path).unwrap()).unwrap();
        assert_eq!(
            tree(&staged.content),
            [
                "Test.app/",
                "Test.app/Contents/",
                "Test.app/Contents/Info.plist",
                "Test.app/Contents/Link",
                "Test.app/Contents/MacOS/",
                "Test.app/Contents/MacOS/Test",
            ]
        );
        let mode = fs::metadata(staged.content.join("Test.app/Contents/MacOS/Test"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        let _ = fs::remove_dir_all(staged.content.parent().unwrap());
    }
}

// ---- HTTP --------------------------------------------------------------

fn http_source(server: &Server, path: &str) -> Source {
    Source::parse(&server.url("http", path)).unwrap()
}

fn published_server(
    key: &updates::SecretKey,
    https: Option<&TestCa>,
) -> (Server, tempfile::TempDir) {
    let server = match https {
        Some(ca) => Server::https(ca),
        None => Server::http(),
    };
    let src = tempfile::tempdir().unwrap();
    publish(src.path(), key, "0.4.0", PKG, &app_zip());
    server.routes.serve_folder("/u/", src.path());
    (server, src)
}

#[test]
fn local_http_source_works() {
    let key = new_key();
    let (server, _src) = published_server(&key, None);
    let c = config(http_source(&server, "/u/"), vec![key.public()], "0.3.0");
    assert!(matches!(
        check(&c, &Cancel::new()).unwrap(),
        CheckOutcome::Available(_)
    ));
    let (_w, staged) = fetch_staged(&c, "0.4.0");
    assert_app_unpacked(&staged.unwrap().content);
    let hits = server.hits.lock().unwrap().clone();
    assert!(hits.contains(&format!("/u/{PKG}")), "{hits:?}");
}

#[test]
fn http_to_a_public_host_is_refused() {
    let e = Source::parse("http://updates.example.com/u/").unwrap_err();
    assert_eq!(e.reason, Reason::InsecureSource);
    let e = Source::parse("http://203.0.113.10/u/").unwrap_err();
    assert_eq!(e.reason, Reason::InsecureSource);
}

#[test]
fn oversized_http_responses_are_cut_off() {
    let key = new_key();
    let (server, src) = published_server(&key, None);
    let c = config(http_source(&server, "/u/"), vec![key.public()], "0.3.0");
    let manifest = fs::read(src.path().join("updates.json")).unwrap();
    // Announced too large: refused before reading.
    server.routes.set(
        "/u/updates.json",
        Route::Declared(10 << 20, manifest.clone()),
    );
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::TooLarge);
    // No length announced, and more arrives than allowed.
    server
        .routes
        .set("/u/updates.json", Route::OkUnsized(vec![b' '; 200 * 1024]));
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::TooLarge);
    server.routes.set("/u/updates.json", Route::Ok(manifest));
    // A package larger than the manifest says, with and without a length.
    let mut big = app_zip();
    big.extend_from_slice(&[0; 1000]);
    server
        .routes
        .set(&format!("/u/{PKG}"), Route::Ok(big.clone()));
    let (_w, r) = fetch_staged(&c, "0.4.0");
    assert_eq!(reason(r), Reason::TooLarge);
    server
        .routes
        .set(&format!("/u/{PKG}"), Route::OkUnsized(big));
    let (_w, r) = fetch_staged(&c, "0.4.0");
    assert_eq!(reason(r), Reason::TooLarge);
}

#[test]
fn redirects_follow_the_rules() {
    let key = new_key();
    let (server, src) = published_server(&key, None);
    // A local redirect to another path of the same server is followed.
    let moved = server.url("http", "/u/updates.json");
    server
        .routes
        .set("/old/updates.json", Route::Redirect(301, moved));
    server.routes.set(
        "/old/updates.json.sig",
        Route::Redirect(302, "/u/updates.json.sig".into()),
    );
    let c = config(http_source(&server, "/old/"), vec![key.public()], "0.3.0");
    assert!(matches!(
        check(&c, &Cancel::new()).unwrap(),
        CheckOutcome::Available(_)
    ));

    // A redirect to plain HTTP on a public host is refused before any
    // connection is attempted there.
    server.routes.set(
        "/pub/updates.json",
        Route::Redirect(302, "http://203.0.113.10/updates.json".into()),
    );
    let c = config(http_source(&server, "/pub/"), vec![key.public()], "0.3.0");
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::RedirectRefused);

    // Other schemes and endless loops.
    server.routes.set(
        "/ftp/updates.json",
        Route::Redirect(302, "ftp://nas.local/x".into()),
    );
    let c = config(http_source(&server, "/ftp/"), vec![key.public()], "0.3.0");
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::RedirectRefused);
    server.routes.set(
        "/loop/updates.json",
        Route::Redirect(302, "/loop/updates.json".into()),
    );
    let c = config(http_source(&server, "/loop/"), vec![key.public()], "0.3.0");
    let before = server.hit_count();
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::RedirectRefused);
    assert_eq!(
        server.hit_count() - before,
        1 + updates::fetch::MAX_REDIRECTS
    );
    drop(src);
}

#[test]
fn http_errors_and_timeouts() {
    let key = new_key();
    let (server, _src) = published_server(&key, None);
    server.routes.set("/err/updates.json", Route::Status(500));
    let c = config(http_source(&server, "/err/"), vec![key.public()], "0.3.0");
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::Unreachable);
    let c = config(
        http_source(&server, "/missing/"),
        vec![key.public()],
        "0.3.0",
    );
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::NotFound);
    server.routes.set("/slow/updates.json", Route::Hang);
    let mut c = config(http_source(&server, "/slow/"), vec![key.public()], "0.3.0");
    c.timeouts.response = Duration::from_millis(500);
    let started = std::time::Instant::now();
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::Unreachable);
    assert!(started.elapsed() < Duration::from_secs(5));
    let port = closed_port();
    let s = Source::parse(&format!("http://127.0.0.1:{port}/u/")).unwrap();
    let c = config(s, vec![key.public()], "0.3.0");
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::Unreachable);
}

// ---- HTTPS -------------------------------------------------------------

fn https_config(server: &Server, path: &str, ca: &TestCa, key: &updates::SecretKey) -> Config {
    let s = Source::parse(&server.url("https", path)).unwrap();
    let mut c = config(s, vec![key.public()], "0.3.0");
    c.tls_roots = Some(vec![ca.ca_der.clone()]);
    c
}

#[test]
fn https_source_works_and_is_verified() {
    if !updates::platform::https_supported() {
        eprintln!("this processor cannot run the HTTPS crypto; skipped");
        return;
    }
    let key = new_key();
    let ca = TestCa::new();
    let (server, _src) = published_server(&key, Some(&ca));
    let c = https_config(&server, "/u/", &ca, &key);
    let (_w, staged) = fetch_staged(&c, "0.4.0");
    assert_app_unpacked(&staged.unwrap().content);

    // The built-in roots do not know the test authority.
    let mut untrusted = c.clone();
    untrusted.tls_roots = None;
    let e = check(&untrusted, &Cancel::new()).unwrap_err();
    assert_eq!(e.reason, Reason::Unreachable);
    assert!(e.detail.contains("UnknownIssuer"), "{e}");
    // Another authority's roots do not either.
    let mut other = c.clone();
    other.tls_roots = Some(vec![TestCa::new().ca_der]);
    let e = check(&other, &Cancel::new()).unwrap_err();
    assert!(
        e.detail.contains("UnknownIssuer") || e.detail.contains("BadSignature"),
        "{e}"
    );
}

#[test]
fn https_never_redirects_to_http() {
    if !updates::platform::https_supported() {
        eprintln!("this processor cannot run the HTTPS crypto; skipped");
        return;
    }
    let key = new_key();
    let ca = TestCa::new();
    let (server, _src) = published_server(&key, Some(&ca));
    let (plain, _src2) = published_server(&key, None);
    // Even to a local host, where an http:// source would be allowed.
    server.routes.set(
        "/down/updates.json",
        Route::Redirect(302, plain.url("http", "/u/updates.json")),
    );
    let c = https_config(&server, "/down/", &ca, &key);
    let before = plain.hit_count();
    assert_eq!(reason(check(&c, &Cancel::new())), Reason::RedirectRefused);
    assert_eq!(plain.hit_count(), before, "nothing was requested over http");
}
