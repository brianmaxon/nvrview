//! The update manifest, `updates.json` (docs/UPDATES.md, "The manifest").
//! Parsed only after its signature verified; validated strictly anyway.

use crate::version::Version;
use crate::{Reason, UpdateError};
use serde::Deserialize;
use std::collections::BTreeMap;

/// The manifest format this core reads.
pub const FORMAT: u64 = 1;
/// Largest manifest read.
pub const MAX_MANIFEST: u64 = 64 * 1024;
/// Largest package a manifest may name.
pub const MAX_PACKAGE: u64 = 1024 * 1024 * 1024;
/// Longest release notes, in characters.
pub const MAX_NOTES: usize = 2000;
/// Longest package file name.
pub const MAX_FILE_NAME: usize = 128;
/// Most platform entries.
const MAX_PACKAGES: usize = 32;

/// One platform's package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    /// A bare file name next to the manifest (see [`valid_file_name`]).
    pub file: String,
    pub size: u64,
    /// The SHA-256 of the package.
    pub sha256: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub version: Version,
    pub notes: String,
    /// When the manifest was signed, seconds since the Unix epoch
    /// (`published`; optional but recommended, see docs/UPDATES.md).
    pub published: Option<u64>,
    /// After this time the manifest is refused (`expires`; optional).
    pub expires: Option<u64>,
    /// By platform name (`macos-arm64`, `win-x64`, ...).
    pub packages: BTreeMap<String, Package>,
}

#[derive(Deserialize)]
struct RawManifest {
    format: u64,
    version: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    published: Option<String>,
    #[serde(default)]
    expires: Option<String>,
    packages: BTreeMap<String, RawPackage>,
}

#[derive(Deserialize)]
struct RawPackage {
    file: String,
    size: u64,
    sha256: String,
}

impl Manifest {
    /// Parses and validates manifest bytes. Unknown members are ignored;
    /// a newer `format` is refused.
    pub fn parse(bytes: &[u8]) -> Result<Manifest, UpdateError> {
        let bad = |what: &str| UpdateError::new(Reason::BadManifest, what);
        if bytes.len() as u64 > MAX_MANIFEST {
            return Err(UpdateError::new(Reason::TooLarge, "manifest over 64 KiB"));
        }
        // Read the format on its own first, so a newer format with a
        // different shape is reported as such rather than as malformed.
        #[derive(Deserialize)]
        struct FormatOnly {
            format: Option<u64>,
        }
        let f: FormatOnly =
            serde_json::from_slice(bytes).map_err(|e| bad(&format!("not JSON: {e}")))?;
        match f.format {
            Some(FORMAT) => {}
            Some(n) if n > FORMAT => {
                return Err(UpdateError::new(
                    Reason::UnsupportedFormat,
                    format!("manifest format {n}; this version reads format {FORMAT}"),
                ));
            }
            _ => return Err(bad("missing or invalid format")),
        }
        let raw: RawManifest =
            serde_json::from_slice(bytes).map_err(|e| bad(&format!("invalid manifest: {e}")))?;
        debug_assert_eq!(raw.format, FORMAT);
        let version = Version::parse(&raw.version).ok_or_else(|| bad("invalid version"))?;
        let date = |text: &Option<String>, what: &str| -> Result<Option<u64>, UpdateError> {
            match text {
                None => Ok(None),
                Some(t) => crate::time::parse(t)
                    .map(Some)
                    .ok_or_else(|| bad(&format!("invalid {what} time (YYYY-MM-DDTHH:MM:SSZ)"))),
            }
        };
        let published = date(&raw.published, "published")?;
        let expires = date(&raw.expires, "expires")?;
        if let (Some(p), Some(e)) = (published, expires)
            && e <= p
        {
            return Err(bad("expires is not after published"));
        }
        if raw.notes.chars().count() > MAX_NOTES {
            return Err(bad("notes longer than 2000 characters"));
        }
        if raw.packages.len() > MAX_PACKAGES {
            return Err(bad("too many packages"));
        }
        let mut packages = BTreeMap::new();
        for (platform, p) in raw.packages {
            if !valid_platform(&platform) {
                return Err(bad("invalid platform name"));
            }
            if !valid_file_name(&p.file) {
                return Err(bad("invalid package file name"));
            }
            if p.size == 0 || p.size > MAX_PACKAGE {
                return Err(bad("package size out of range"));
            }
            let sha256 = parse_sha256(&p.sha256).ok_or_else(|| bad("invalid sha256"))?;
            packages.insert(
                platform,
                Package {
                    file: p.file,
                    size: p.size,
                    sha256,
                },
            );
        }
        Ok(Manifest {
            version,
            notes: raw.notes,
            published,
            expires,
            packages,
        })
    }
}

/// Exactly 64 lowercase hex digits.
fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || text.bytes().any(|b| b.is_ascii_uppercase()) {
        return None;
    }
    crate::keys::decode_hex(text)?.try_into().ok()
}

fn valid_platform(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// A package name: starts with a letter or digit, then letters, digits,
/// `.`, `_`, `-`, `+`; ends in `.zip`; at most 128 characters. No path
/// separators, so no `..` path either.
pub fn valid_file_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_FILE_NAME
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'+'))
        && name.ends_with(".zip")
        && name.len() > ".zip".len()
        && !name.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn manifest(packages: &str) -> String {
        format!(r#"{{"format":1,"version":"0.4.0","notes":"Fixes.","packages":{{{packages}}}}}"#)
    }

    #[test]
    fn parses_a_valid_manifest() {
        let text = manifest(&format!(
            r#""macos-arm64":{{"file":"NVRView-0.4.0-macos-arm64.zip","size":10,"sha256":"{SHA}"}},
               "win-x64":{{"file":"NvrView-0.4.0-win-x64.zip","size":20,"sha256":"{SHA}","later":true}}"#
        ));
        let m = Manifest::parse(text.as_bytes()).unwrap();
        assert_eq!(m.version.to_string(), "0.4.0");
        assert_eq!(m.notes, "Fixes.");
        assert_eq!(m.packages["win-x64"].size, 20);
        assert_eq!(m.packages["macos-arm64"].sha256[1], 0x11);
        assert_eq!((m.published, m.expires), (None, None));
        let dated = r#"{"format":1,"version":"0.4.0","published":"2026-10-11T12:00:00Z",
            "expires":"2026-11-10T12:00:00Z","packages":{}}"#;
        let m = Manifest::parse(dated.as_bytes()).unwrap();
        assert_eq!(m.published, crate::time::parse("2026-10-11T12:00:00Z"));
        assert_eq!(m.expires, crate::time::parse("2026-11-10T12:00:00Z"));
    }

    #[test]
    fn refuses_invalid_manifests() {
        let pkg = |file: &str, size: u64, sha: &str| {
            manifest(&format!(
                r#""win-x64":{{"file":"{file}","size":{size},"sha256":"{sha}"}}"#
            ))
        };
        let cases = [
            pkg("../x.zip", 1, SHA),
            pkg("a/b.zip", 1, SHA),
            pkg("a\\\\b.zip", 1, SHA),
            pkg("..zip", 1, SHA),
            pkg(".zip", 1, SHA),
            pkg("x.tar", 1, SHA),
            pkg("-x.zip", 1, SHA),
            pkg("x y.zip", 1, SHA),
            pkg("x.zip", 0, SHA),
            pkg("x.zip", MAX_PACKAGE + 1, SHA),
            pkg("x.zip", 1, &SHA.to_uppercase()),
            pkg("x.zip", 1, &SHA[..63]),
            pkg(&format!("{}.zip", "a".repeat(125)), 1, SHA),
            r#"{"format":1,"version":"0.4","packages":{}}"#.to_string(),
            r#"{"format":"1","version":"0.4.0","packages":{}}"#.to_string(),
            r#"{"version":"0.4.0","packages":{}}"#.to_string(),
            r#"{"format":1,"version":"0.4.0"}"#.to_string(),
            r#"[1]"#.to_string(),
            r#"{"format":1,"version":"0.4.0","published":"2026-10-11","packages":{}}"#.to_string(),
            r#"{"format":1,"version":"0.4.0","published":1792000000,"packages":{}}"#.to_string(),
            r#"{"format":1,"version":"0.4.0","expires":"tomorrow","packages":{}}"#.to_string(),
            r#"{"format":1,"version":"0.4.0","published":"2026-10-11T12:00:00Z","expires":"2026-10-11T12:00:00Z","packages":{}}"#.to_string(),
            format!(
                r#"{{"format":1,"version":"0.4.0","notes":"{}","packages":{{}}}}"#,
                "n".repeat(MAX_NOTES + 1)
            ),
            manifest(&format!(
                r#""Win":{{"file":"x.zip","size":1,"sha256":"{SHA}"}}"#
            )),
        ];
        for c in cases {
            let e = Manifest::parse(c.as_bytes()).unwrap_err();
            assert_eq!(e.reason, Reason::BadManifest, "{c}: {e}");
        }
    }

    #[test]
    fn newer_format_and_size() {
        let e = Manifest::parse(br#"{"format":2,"anything":"else"}"#).unwrap_err();
        assert_eq!(e.reason, Reason::UnsupportedFormat);
        let big = vec![b' '; MAX_MANIFEST as usize + 1];
        assert_eq!(Manifest::parse(&big).unwrap_err().reason, Reason::TooLarge);
    }
}
