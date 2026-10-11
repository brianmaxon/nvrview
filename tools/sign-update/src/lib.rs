//! The work behind `sign-update` (docs/UPDATES.md, "Maintainer workflow"):
//! key generation, building the manifest from packages, signing it, and
//! checking a source with the same code the apps run.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use updates::source::SIGNATURE_NAME;
use updates::{
    Cancel, CheckOutcome, Config, Manifest, PublicKey, SecretKey, Source, Version, keys,
};

/// Generates a key pair. The secret key goes to `secret` (a new file, mode
/// 0600 on Unix, never inside a git working tree); the public key to
/// `public` when given. Returns the public key.
pub fn keygen(secret: &Path, public: Option<&Path>) -> Result<PublicKey, String> {
    refuse_in_repository(secret)?;
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("no random numbers from the system: {e}"))?;
    let key = SecretKey::from_seed(seed);
    seed.fill(0);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(secret).map_err(|e| {
        format!(
            "cannot create {} (it must not exist yet): {e}",
            secret.display()
        )
    })?;
    f.write_all(
        format!(
            "# nvrview update signing key (secret: keep it off any repository)\n{}",
            key.to_text()
        )
        .as_bytes(),
    )
    .and_then(|_| f.sync_all())
    .map_err(|e| format!("writing {}: {e}", secret.display()))?;
    let public_key = key.public();
    if let Some(p) = public {
        fs::write(p, format!("{}\n", public_key.to_text()))
            .map_err(|e| format!("writing {}: {e}", p.display()))?;
    }
    Ok(public_key)
}

/// Refuses a secret key path inside a git working tree (this repository or
/// any other), where it could be committed by accident.
fn refuse_in_repository(path: &Path) -> Result<(), String> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let parent = parent
        .canonicalize()
        .map_err(|e| format!("{}: {e}", parent.display()))?;
    for dir in parent.ancestors() {
        if dir.join(".git").exists() {
            return Err(format!(
                "{} is inside a git working tree ({}); keep the secret key outside any repository",
                path.display(),
                dir.display()
            ));
        }
    }
    Ok(())
}

pub fn read_secret(path: &Path) -> Result<SecretKey, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    SecretKey::parse(&text)
        .ok_or_else(|| format!("{} is not an nvrview secret key", path.display()))
}

/// A public key from a file, or from the text itself.
pub fn read_public(arg: &str) -> Result<PublicKey, String> {
    let text = if Path::new(arg).is_file() {
        fs::read_to_string(arg).map_err(|e| format!("reading {arg}: {e}"))?
    } else {
        arg.to_string()
    };
    PublicKey::parse(&text).ok_or_else(|| format!("{arg}: not an nvrview public key"))
}

/// The platform of a package named `<name>-<version>-<platform>.zip`.
pub fn package_platform(file_name: &str, version: &str) -> Result<String, String> {
    let stem = file_name
        .strip_suffix(".zip")
        .ok_or_else(|| format!("{file_name}: packages are .zip files"))?;
    let marker = format!("-{version}-");
    let at = stem
        .find(&marker)
        .ok_or_else(|| format!("{file_name}: the name does not carry version {version} (<name>-<version>-<platform>.zip)"))?;
    let platform = &stem[at + marker.len()..];
    if platform.is_empty() {
        return Err(format!("{file_name}: no platform after the version"));
    }
    Ok(platform.to_string())
}

/// Builds the manifest text for `packages` (each named
/// `<name>-<version>-<platform>.zip`), dated `published` (seconds since the
/// epoch; normally now) and optionally `expires`, and checks it parses.
pub fn build_manifest(
    version: &str,
    notes: &str,
    packages: &[PathBuf],
    published: u64,
    expires: Option<u64>,
) -> Result<String, String> {
    if expires.is_some_and(|e| e <= published) {
        return Err("the expiry must be after the publish time".into());
    }
    if Version::parse(version).is_none() {
        return Err(format!("{version}: not a semantic version"));
    }
    if packages.is_empty() {
        return Err("no packages given".into());
    }
    let mut entries = Vec::new();
    let mut platforms = std::collections::BTreeSet::new();
    for p in packages {
        let file = p
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{}: not a file name", p.display()))?;
        if !updates::manifest::valid_file_name(file) {
            return Err(format!(
                "{file}: package names are letters, digits, . _ - + and end in .zip"
            ));
        }
        let platform = package_platform(file, version)?;
        if !platforms.insert(platform.clone()) {
            return Err(format!("two packages for {platform}"));
        }
        let (size, sha) =
            updates::hash_file(p).map_err(|e| format!("reading {}: {e}", p.display()))?;
        entries.push(format!(
            "    {}: {{\n      \"file\": {},\n      \"size\": {size},\n      \"sha256\": \"{}\"\n    }}",
            json_string(&platform),
            json_string(file),
            keys::encode_hex(&sha)
        ));
    }
    let expires = expires
        .map(|e| format!("  \"expires\": \"{}\",\n", updates::time::format(e)))
        .unwrap_or_default();
    let text = format!(
        "{{\n  \"format\": {},\n  \"version\": {},\n  \"published\": \"{}\",\n{expires}  \"notes\": {},\n  \"packages\": {{\n{}\n  }}\n}}\n",
        updates::manifest::FORMAT,
        json_string(version),
        updates::time::format(published),
        json_string(notes.trim()),
        entries.join(",\n")
    );
    Manifest::parse(text.as_bytes()).map_err(|e| format!("the manifest does not validate: {e}"))?;
    Ok(text)
}

fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// Signs `manifest` with `secret`, writing `<manifest>.sig` next to it
/// (replacing it, or adding a line with `append`). Returns the signature
/// file's path.
pub fn sign(manifest: &Path, secret: &SecretKey, append: bool) -> Result<PathBuf, String> {
    let bytes = fs::read(manifest).map_err(|e| format!("reading {}: {e}", manifest.display()))?;
    Manifest::parse(&bytes).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let sig_path = manifest.with_file_name(SIGNATURE_NAME);
    if manifest.file_name().is_none_or(|n| n != "updates.json") {
        return Err("the manifest must be named updates.json".into());
    }
    let line = secret.sign_line(&bytes);
    let mut text = if append {
        fs::read_to_string(&sig_path).unwrap_or_default()
    } else {
        String::new()
    };
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&line);
    text.push('\n');
    let lines = keys::parse_signatures(&text).ok_or("the existing signature file is malformed")?;
    if keys::verify(&bytes, &lines, &[secret.public()]).is_none() {
        return Err("the new signature does not verify".into());
    }
    fs::write(&sig_path, text).map_err(|e| format!("writing {}: {e}", sig_path.display()))?;
    Ok(sig_path)
}

/// What `check` found, for printing.
pub struct CheckReport {
    pub lines: Vec<String>,
    /// Where the update was unpacked, when it was kept (`stage`).
    pub staged: Option<PathBuf>,
}

/// Runs the apps' check against `source` at time `now` (seconds since the
/// epoch; the system clock in the tool); when a newer version is offered,
/// also downloads, verifies, and unpacks it, into `stage` (kept) or a
/// temporary folder (removed).
pub fn check(
    source: &str,
    keys: Vec<PublicKey>,
    current: &str,
    platform: Option<&str>,
    stage: Option<&Path>,
    now: u64,
) -> Result<CheckReport, String> {
    let source = Source::parse(source).map_err(|e| e.to_string())?;
    let current =
        Version::parse(current).ok_or_else(|| format!("{current}: not a semantic version"))?;
    let mut config = Config::new(source, keys, current.clone());
    if let Some(p) = platform {
        config.platform = p.to_string();
    }
    config.now = now;
    let mut lines = vec![format!(
        "source: {} ({}); platform {}; current version {current}",
        config.source.display(),
        config.source.kind(),
        config.platform
    )];
    let cancel = Cancel::new();
    let offer = match updates::check(&config, &cancel).map_err(|e| e.to_string())? {
        CheckOutcome::UpToDate {
            latest, no_package, ..
        } => {
            lines.push(if no_package {
                format!(
                    "signed manifest for {latest}, but no package for {}",
                    config.platform
                )
            } else {
                format!("up to date: the source offers {latest}")
            });
            return Ok(CheckReport {
                lines,
                staged: None,
            });
        }
        CheckOutcome::Available(o) => o,
    };
    lines.push(format!(
        "available: {} ({}, {} bytes), signed by key {}, {}",
        offer.version,
        offer.package.file,
        offer.package.size,
        offer.key_id,
        match offer.published {
            Some(t) => format!("published {}", updates::time::format(t)),
            None => "undated (no published time: the apps cannot detect a replay of it)".into(),
        }
    ));
    if !offer.notes.is_empty() {
        lines.push(format!("notes: {}", offer.notes));
    }
    let (staging, keep) = match stage {
        Some(dir) => (dir.join(format!("staging-{}", offer.version)), true),
        None => {
            let mut seed = [0u8; 8];
            getrandom::fill(&mut seed).map_err(|e| e.to_string())?;
            (
                std::env::temp_dir().join(format!("sign-update-check-{}", keys::encode_hex(&seed))),
                false,
            )
        }
    };
    let staged = updates::download(&config, &offer.version, &staging, &cancel, &mut |_, _| {})
        .map_err(|e| e.to_string())?;
    let files = updates::zip::list_tree(&staged.content).map_err(|e| e.to_string())?;
    lines.push(format!(
        "downloaded, SHA-256 matched, unpacked {} entries{}",
        files.len(),
        if keep {
            format!(" into {}", staged.content.display())
        } else {
            String::new()
        }
    ));
    let kept = if keep {
        Some(staged.content)
    } else {
        let _ = fs::remove_dir_all(&staging);
        None
    };
    Ok(CheckReport {
        lines,
        staged: kept,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests' "now": 2026-10-11T12:00:00Z; no test reads the clock.
    const NOW: u64 = 1_791_720_000;

    #[test]
    fn keygen_writes_a_private_file_once_and_never_in_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("update.key");
        let public = dir.path().join("update.pub");
        let key = keygen(&secret, Some(&public)).unwrap();
        assert_eq!(read_secret(&secret).unwrap().public(), key);
        assert_eq!(read_public(public.to_str().unwrap()).unwrap(), key);
        assert_eq!(read_public(&key.to_text()).unwrap(), key);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&secret).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // Never overwrites.
        let before = fs::read(&secret).unwrap();
        assert!(keygen(&secret, None).is_err());
        assert_eq!(fs::read(&secret).unwrap(), before);
        // Refuses a git working tree, at any depth.
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir(repo.path().join(".git")).unwrap();
        fs::create_dir(repo.path().join("sub")).unwrap();
        let inside = repo.path().join("sub").join("update.key");
        assert!(
            keygen(&inside, None)
                .unwrap_err()
                .contains("git working tree")
        );
        assert!(!inside.exists());
    }

    #[test]
    fn manifest_from_packages() {
        let dir = tempfile::tempdir().unwrap();
        let mac = dir.path().join("NVRView-0.4.0-macos-arm64.zip");
        let win = dir.path().join("NvrView-0.4.0-win-x64.zip");
        fs::write(&mac, b"mac package").unwrap();
        fs::write(&win, b"windows package").unwrap();
        let text = build_manifest(
            "0.4.0",
            "Fixes \"quoted\".\n",
            &[mac.clone(), win.clone()],
            NOW,
            Some(NOW + 86_400),
        )
        .unwrap();
        let m = Manifest::parse(text.as_bytes()).unwrap();
        assert_eq!(m.notes, "Fixes \"quoted\".");
        assert_eq!(m.published, Some(NOW));
        assert_eq!(m.expires, Some(NOW + 86_400));
        assert!(build_manifest("0.4.0", "", std::slice::from_ref(&mac), NOW, Some(NOW)).is_err());
        assert_eq!(m.packages["macos-arm64"].size, 11);
        assert_eq!(m.packages["win-x64"].file, "NvrView-0.4.0-win-x64.zip");
        assert!(build_manifest("0.5.0", "", std::slice::from_ref(&mac), NOW, None).is_err());
        assert!(build_manifest("0.4", "", std::slice::from_ref(&mac), NOW, None).is_err());
        assert!(build_manifest("0.4.0", "", &[mac.clone(), mac], NOW, None).is_err());
        assert_eq!(
            package_platform("NVRView-1.2.3-beta.1-macos-arm64.zip", "1.2.3-beta.1").unwrap(),
            "macos-arm64"
        );
    }

    #[test]
    fn sign_then_check_a_folder() {
        let keys_dir = tempfile::tempdir().unwrap();
        let old_key = keygen(&keys_dir.path().join("old.key"), None).unwrap();
        let new_key = keygen(&keys_dir.path().join("new.key"), None).unwrap();
        let dist = tempfile::tempdir().unwrap();
        // A real (stored) zip holding one file, written by hand.
        let package = dist.path().join("App-0.4.0-test-os.zip");
        fs::write(
            &package,
            updates::zip::write_stored(&[("hello.txt", b"hello")]),
        )
        .unwrap();
        let manifest = dist.path().join("updates.json");
        fs::write(
            &manifest,
            build_manifest(
                "0.4.0",
                "notes",
                std::slice::from_ref(&package),
                NOW - 60,
                None,
            )
            .unwrap(),
        )
        .unwrap();
        let old = read_secret(&keys_dir.path().join("old.key")).unwrap();
        let new = read_secret(&keys_dir.path().join("new.key")).unwrap();
        sign(&manifest, &old, false).unwrap();
        sign(&manifest, &new, true).unwrap();
        let sig = fs::read_to_string(dist.path().join("updates.json.sig")).unwrap();
        assert_eq!(sig.lines().count(), 2);
        for key in [old_key, new_key] {
            let source = dist.path().to_str().unwrap();
            let r = check(
                source,
                vec![key.clone()],
                "0.3.0",
                Some("test-os"),
                None,
                NOW,
            )
            .unwrap();
            assert!(
                r.lines.iter().any(|l| l.starts_with("available: 0.4.0")
                    && l.contains("published 2026-10-11T11:59:00Z")),
                "{:?}",
                r.lines
            );
            assert!(
                r.lines.iter().any(|l| l.contains("unpacked 1 entries")),
                "{:?}",
                r.lines
            );
            let r = check(source, vec![key], "0.4.0", Some("test-os"), None, NOW).unwrap();
            assert!(r.lines.iter().any(|l| l.starts_with("up to date")));
        }
        let stage = tempfile::tempdir().unwrap();
        let source = dist.path().to_str().unwrap();
        let r = check(
            source,
            vec![
                read_secret(&keys_dir.path().join("new.key"))
                    .unwrap()
                    .public(),
            ],
            "0.3.0",
            Some("test-os"),
            Some(stage.path()),
            NOW,
        )
        .unwrap();
        let kept = r.staged.unwrap();
        assert_eq!(fs::read(kept.join("hello.txt")).unwrap(), b"hello");
    }
}
