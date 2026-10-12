//! Export planners (docs/FFI-RECORDINGS.md, section 7a): what the user takes
//! out of the app, decided as data so both apps write the same thing.
//!
//! - [`trim`]: one output per camera cut to an in and an out point.
//! - [`range`]: the request, ID, folder and file names, and job description
//!   of a download of any time range of chosen cameras.
//! - [`still`]: which picture to save as an image, and what to call it.
//!
//! This module holds what they share: [`ExportError`], the validated
//! destination directory ([`validate_dest`], [`join_dest`]), file name
//! helpers, and the metadata the core's MP4 writer records
//! ([`mp4_metadata`], which is where "share-safe" meets the writer).
//!
//! Everything here is pure: the same request and the same finished-file
//! descriptions give the same plan, with no clock and no file system access.
//! The files' keyframes come from [`crate::sync_samples`], read by the
//! caller, and the apps decode, encode, and write.
//!
//! # Destination
//!
//! `dest` is an absolute directory the app chose (a save panel). The core
//! does not touch it; it only validates the string and joins names onto it.
//! Without `dest`, a plan puts files where section 7a says: next to the
//! download's files for edit lists and stills from a file, in
//! `downloads_dir` for everything else (the caller passes that directory).
//!
//! # Share-safe
//!
//! With `share_safe` every name that would identify the installation is
//! replaced by [`crate::naming::neutral_camera_name`] (in the request's
//! camera order), the user's name for a range is left out, no
//! `incident.json` is planned, labels carry no camera name, and
//! [`mp4_metadata`] gives the metadata a writer may record. The plan echoes
//! `share_safe` so the writer knows to add nothing of its own.

pub mod range;
pub mod still;
pub mod trim;

use serde::{Deserialize, Serialize};
use std::fmt;

/// Longest `dest` accepted, in characters.
pub const MAX_DEST_CHARS: usize = 1024;
/// Most source files one planner call looks at.
pub const MAX_SOURCES: usize = 256;

/// Why no plan could be made. The C interface maps [`ExportError::is_limit`]
/// to `NV_ERR_LIMIT`, [`ExportError::is_unknown`] to `NV_ERR_UNKNOWN_ID`,
/// and every other error to `NV_ERR_INVALID_ARGUMENT`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportError {
    /// `dest` is not an absolute directory string the core can join onto.
    InvalidDest,
    /// A trim with `in_ms >= out_ms`, or a download range that is empty or
    /// starts before the epoch.
    InvalidRange,
    /// The request names no camera.
    NoCameras,
    /// The same camera twice with different quality tiers (they would write
    /// the same file name).
    DuplicateCamera,
    /// More cameras than one range download carries (see
    /// [`range::MAX_CAMERAS`]).
    TooManyCameras,
    /// A range longer than [`range::MAX_RANGE_SECS`].
    RangeTooLong,
    /// More sources, tiles, or stills than one call handles.
    TooMany,
    /// None of the chosen cameras has a finished file.
    NoSources,
    /// The in and out points (or the time asked for) lie outside every
    /// chosen camera's file.
    OutsideFile,
    /// A `keyframe` cut or a keyframe still needs a keyframe and no chosen
    /// file has one.
    NoKeyframes,
    /// A playback tile that is not among the shown pictures.
    UnknownTile(u32),
    /// No requested tile has shown a picture yet.
    NothingShown,
}

impl ExportError {
    /// Whether this is a limit (`NV_ERR_LIMIT`) rather than a bad argument.
    pub fn is_limit(&self) -> bool {
        matches!(
            self,
            ExportError::TooManyCameras | ExportError::RangeTooLong | ExportError::TooMany
        )
    }

    /// Whether this names something unknown (`NV_ERR_UNKNOWN_ID`).
    pub fn is_unknown(&self) -> bool {
        matches!(self, ExportError::UnknownTile(_))
    }
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::InvalidDest => f.write_str("the destination is not an absolute directory"),
            ExportError::InvalidRange => f.write_str("the time range is empty or not valid"),
            ExportError::NoCameras => f.write_str("no camera was chosen"),
            ExportError::DuplicateCamera => {
                f.write_str("a camera is listed twice with different tiers")
            }
            ExportError::TooManyCameras => f.write_str("too many cameras for one download"),
            ExportError::RangeTooLong => f.write_str("the time range is longer than 24 hours"),
            ExportError::TooMany => f.write_str("too many items in one request"),
            ExportError::NoSources => f.write_str("no chosen camera has a finished file"),
            ExportError::OutsideFile => f.write_str("the time lies outside the chosen files"),
            ExportError::NoKeyframes => f.write_str("no chosen file has a keyframe to cut on"),
            ExportError::UnknownTile(t) => write!(f, "unknown playback tile {t}"),
            ExportError::NothingShown => f.write_str("no tile has shown a picture yet"),
        }
    }
}

impl std::error::Error for ExportError {}

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

fn is_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

/// Whether `dest` is a UNC root such as `\\host\share` (two non-empty
/// components after the leading pair).
fn unc_ok(rest: &str) -> bool {
    rest.split(is_separator).filter(|c| !c.is_empty()).count() >= 2
}

/// Checks a destination directory and returns it unchanged.
///
/// Accepted: a POSIX absolute path (`/srv/...`), a drive path (`C:\...` or
/// `C:/...`), or a UNC path (`\\host\share\...`). Refused: an empty or
/// longer than [`MAX_DEST_CHARS`] string, control characters (including
/// NUL), a `.` or `..` component, a relative path, a bare drive (`C:`), and
/// a UNC path without both host and share. The core never touches the
/// directory; whether it exists and is writable is the app's business.
pub fn validate_dest(dest: &str) -> Result<&str, ExportError> {
    if dest.is_empty()
        || dest.chars().count() > MAX_DEST_CHARS
        || dest.chars().any(char::is_control)
    {
        return Err(ExportError::InvalidDest);
    }
    let bytes = dest.as_bytes();
    let absolute = if let Some(rest) = dest.strip_prefix("\\\\") {
        unc_ok(rest)
    } else if bytes[0] == b'/' || bytes[0] == b'\\' {
        // Rooted on the current drive or POSIX root.
        true
    } else {
        bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'/' || bytes[2] == b'\\')
    };
    if !absolute || dest.split(is_separator).any(|c| c == "." || c == "..") {
        return Err(ExportError::InvalidDest);
    }
    Ok(dest)
}

/// Joins a file or folder name onto a validated directory with the
/// separator the directory itself uses (`\` for a Windows path, `/`
/// otherwise), without doubling a trailing separator. `name` is joined as
/// given; callers pass names made safe by [`crate::naming::sanitize`].
pub fn join_dest(dir: &str, name: &str) -> String {
    let backslash_only = dir.contains('\\') && !dir.contains('/');
    let windows = backslash_only
        || (dir.as_bytes().get(1) == Some(&b':') && dir.as_bytes().get(2) == Some(&b'\\'));
    let sep = if windows { '\\' } else { '/' };
    let trimmed = dir.trim_end_matches(is_separator);
    // A root ("/" or "C:\") trims to "" or "C:"; the separator restores it.
    format!("{trimmed}{sep}{name}")
}

/// The directory part of a file path as the app gave it (split at the last
/// `/` or `\`), or `None` for a bare name.
pub fn parent_dir(file: &str) -> Option<&str> {
    let i = file.rfind(is_separator)?;
    Some(if i == 0 { &file[..1] } else { &file[..i] })
}

/// A path for `name`: in `dir` when known, else the bare name.
pub(crate) fn place(dir: Option<&str>, name: &str) -> String {
    match dir {
        Some(d) => join_dest(d, name),
        None => name.to_owned(),
    }
}

/// Makes names unique in order: a repeat gets ` (2)`, ` (3)`, and so on
/// before its extension. Comparison ignores case, as macOS and Windows
/// file systems do.
pub(crate) fn make_unique(names: &mut [String]) {
    let mut seen = std::collections::BTreeSet::new();
    for n in names.iter_mut() {
        let (stem, ext) = match n.rfind('.') {
            Some(i) if i > 0 => (n[..i].to_owned(), n[i..].to_owned()),
            _ => (n.clone(), String::new()),
        };
        let mut candidate = n.clone();
        let mut k = 2;
        while !seen.insert(candidate.to_lowercase()) {
            candidate = format!("{stem} ({k}){ext}");
            k += 1;
        }
        *n = candidate;
    }
}

/// The metadata the core's MP4 writer records in a file it writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mp4Metadata {
    /// Creation and modification time in the movie, track, and media
    /// headers, seconds since the Unix epoch; 0 writes the 1904 epoch,
    /// which readers take as "unknown".
    pub creation_unix_secs: u64,
    /// Name in the video track's handler box.
    pub video_handler: &'static str,
    /// Name in the audio track's handler box.
    pub audio_handler: &'static str,
}

/// The generic handler names the writer uses (the values most muxers write,
/// naming no product or installation).
pub const GENERIC_VIDEO_HANDLER: &str = mp4_remux::VIDEO_HANDLER_NAME;
/// See [`GENERIC_VIDEO_HANDLER`].
pub const GENERIC_AUDIO_HANDLER: &str = mp4_remux::AudioHandler::Sound.name();

/// Every handler name a file the core writes may carry, with or without
/// `share_safe`: the two generic names above, and the fixed words that
/// title a call recording's audio tracks (`Conversation`, `Visitor`, `You`,
/// from [`mp4_remux::AudioHandler`]). They name no product, device,
/// person, or installation, so they need no share-safe replacement. The
/// writer takes names only from this set (an enum, not text), and reading
/// a file back maps any other name to `SoundHandler`.
pub const ALLOWED_HANDLER_NAMES: [&str; 5] = mp4_remux::HANDLER_NAMES;

/// Whether `name` is one of [`ALLOWED_HANDLER_NAMES`].
pub fn handler_name_allowed(name: &str) -> bool {
    ALLOWED_HANDLER_NAMES.contains(&name)
}

/// The metadata to write. Normally the creation time asked for (0 when the
/// caller has none) and the generic handler names; with `share_safe` always
/// zero times and the generic handler names, whatever time was asked for.
///
/// **Today's writer already complies**: `Mp4Finalizer` never sets a
/// creation time, so its headers carry the 1904 epoch, and the handler names
/// are the two constants above in every download (a recording with several
/// audio tracks uses names from [`ALLOWED_HANDLER_NAMES`] only). This
/// function is the one place that rule lives, so a later change that records
/// a creation time (or tags) is gated on `share_safe` here, and the tests
/// pin it.
pub fn mp4_metadata(share_safe: bool, creation_unix_secs: u64) -> Mp4Metadata {
    Mp4Metadata {
        creation_unix_secs: if share_safe { 0 } else { creation_unix_secs },
        video_handler: GENERIC_VIDEO_HANDLER,
        audio_handler: GENERIC_AUDIO_HANDLER,
    }
}

/// An image format for stills.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StillFormat {
    /// PNG (the default).
    #[default]
    Png,
    /// JPEG.
    Jpeg,
}

impl StillFormat {
    /// The file extension: `png` or `jpg`.
    pub fn extension(self) -> &'static str {
        match self {
            StillFormat::Png => "png",
            StillFormat::Jpeg => "jpg",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dest_must_be_an_absolute_directory() {
        for ok in [
            "/",
            "/srv/export/clips",
            "/a/b/",
            "C:\\Exports\\Videos",
            "C:/Videos",
            "c:\\",
            "\\\\server\\share",
            "\\\\server\\share\\sub dir",
        ] {
            assert_eq!(validate_dest(ok), Ok(ok), "{ok}");
        }
        for bad in [
            "",
            "relative/dir",
            "./x",
            "~/Movies",
            "C:",
            "C:folder",
            "\\\\server",
            "\\\\\\\\",
            "/a/../b",
            "/a/./b",
            "C:\\a\\..\\b",
            "/a\0b",
            "/a\nb",
        ] {
            assert_eq!(validate_dest(bad), Err(ExportError::InvalidDest), "{bad:?}");
        }
        assert!(validate_dest(&format!("/{}", "x".repeat(MAX_DEST_CHARS - 1))).is_ok());
        assert!(validate_dest(&format!("/{}", "x".repeat(MAX_DEST_CHARS))).is_err());
        // Dots inside a name are fine.
        assert!(validate_dest("/a/.hidden/b..c").is_ok());
    }

    #[test]
    fn join_uses_the_directory_separator() {
        assert_eq!(join_dest("/a/b", "f.mp4"), "/a/b/f.mp4");
        assert_eq!(join_dest("/a/b/", "f.mp4"), "/a/b/f.mp4");
        assert_eq!(join_dest("/", "f.mp4"), "/f.mp4");
        assert_eq!(join_dest("C:\\Videos", "f.mp4"), "C:\\Videos\\f.mp4");
        assert_eq!(join_dest("C:\\", "f.mp4"), "C:\\f.mp4");
        assert_eq!(join_dest("C:/Videos", "f.mp4"), "C:/Videos/f.mp4");
        assert_eq!(join_dest("\\\\h\\s", "f.mp4"), "\\\\h\\s\\f.mp4");
    }

    #[test]
    fn parent_of_a_file() {
        assert_eq!(parent_dir("/a/b/f.mp4"), Some("/a/b"));
        assert_eq!(parent_dir("C:\\a\\f.mp4"), Some("C:\\a"));
        assert_eq!(parent_dir("/f.mp4"), Some("/"));
        assert_eq!(parent_dir("f.mp4"), None);
    }

    #[test]
    fn repeated_names_get_a_counter_before_the_extension() {
        let mut v: Vec<String> = ["a.mp4", "A.mp4", "b.mp4", "a.mp4", "noext", "noext"]
            .map(String::from)
            .into();
        make_unique(&mut v);
        assert_eq!(
            v,
            [
                "a.mp4",
                "A (2).mp4",
                "b.mp4",
                "a (3).mp4",
                "noext",
                "noext (2)"
            ]
        );
    }

    #[test]
    fn metadata_is_stripped_when_share_safe() {
        let normal = mp4_metadata(false, 1_790_000_000);
        assert_eq!(normal.creation_unix_secs, 1_790_000_000);
        let safe = mp4_metadata(true, 1_790_000_000);
        assert_eq!(safe.creation_unix_secs, 0);
        assert_eq!(safe.video_handler, "VideoHandler");
        assert_eq!(safe.audio_handler, "SoundHandler");
        assert_eq!(mp4_metadata(false, 0).creation_unix_secs, 0);
        // The same call gives the same answer.
        assert_eq!(mp4_metadata(true, 5), mp4_metadata(true, 99));
    }

    #[test]
    fn handler_names_are_a_fixed_set_of_generic_words() {
        assert_eq!(
            ALLOWED_HANDLER_NAMES,
            [
                "VideoHandler",
                "SoundHandler",
                "Conversation",
                "Visitor",
                "You"
            ]
        );
        for share_safe in [false, true] {
            let m = mp4_metadata(share_safe, 1_790_000_000);
            assert!(handler_name_allowed(m.video_handler));
            assert!(handler_name_allowed(m.audio_handler));
        }
        for h in mp4_remux::AudioHandler::ALL {
            assert!(handler_name_allowed(h.name()), "{h:?}");
        }
        assert!(handler_name_allowed(mp4_remux::VIDEO_HANDLER_NAME));
        assert!(!handler_name_allowed("Camera 1"));
        assert!(!handler_name_allowed(""));
    }

    #[test]
    fn errors_classify_for_the_c_interface() {
        assert!(ExportError::TooManyCameras.is_limit());
        assert!(ExportError::RangeTooLong.is_limit());
        assert!(!ExportError::InvalidRange.is_limit());
        assert!(ExportError::UnknownTile(3).is_unknown());
        assert!(!ExportError::NothingShown.is_unknown());
        assert!(!ExportError::NoSources.to_string().is_empty());
    }

    #[test]
    fn still_formats() {
        assert_eq!(StillFormat::default().extension(), "png");
        assert_eq!(StillFormat::Jpeg.extension(), "jpg");
        assert_eq!(
            serde_json::from_str::<StillFormat>("\"jpeg\"").unwrap(),
            StillFormat::Jpeg
        );
    }
}
