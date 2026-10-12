//! File names for pictures, live recordings, and call recordings
//! (docs/CAPTURE.md, "Save picture", "Files").
//!
//! The pieces come from `recordings`: [`sanitize`] (characters Windows
//! forbids become `-`, whitespace collapses, trailing dots and spaces go,
//! at most 120 characters), `second_stamp` and `millis_stamp` (device-local
//! naive time, `2026-10-11 17.02.03`), `neutral_camera_name` (`Camera A`),
//! and the numbering helper `numbered_folder` (`name`, `name (2)`, ...).
//! Nothing here touches the file system: callers pass a `taken` predicate
//! that answers for a file name in the destination folder.
//!
//! # Rules
//!
//! - **Live recording:** `YYYY-MM-DD HH.MM.SS <camera>.mp4`, the camera
//!   `Camera A` when share-safe. The stem (time and name together) is at
//!   most 120 characters. ` (2)`, ` (3)` when the name is taken: a name is
//!   taken when either `<stem>.mp4` or `<stem>.partial.mp4` exists. Later
//!   segments are `<stem> (part 2).mp4`, built from the first segment's
//!   final stem so a set of parts sorts and moves together.
//! - **Picture:** the still-image rule (`recordings::export::still::still_name`):
//!   `YYYY-MM-DD HH.MM.SS.mmm <camera>.png` or `.jpg`; ` (2)` before the
//!   extension when taken.
//! - **Call recording:** the stem is the call transcript's: the start
//!   time and the camera's own name (share-safe names do not apply to
//!   calls), the name cut to 60 characters, ` 2`, ` 3` when taken. The
//!   stem counts as taken when any of `.txt`, `.mp4`, `.partial.mp4` with
//!   it exists, so the recording and the transcript always share one.
//!   Later parts are `<stem> (part 2).mp4`.
//! - **Partial files:** `<stem>.partial.mp4`, in the same folder.

use recordings::Secs;
pub use recordings::export::StillFormat;
use recordings::export::range::numbered_folder;
use recordings::naming::{neutral_camera_name, sanitize, second_stamp};

/// The longest call stem name part, in characters (the transcript's rule).
pub const CALL_NAME_CHARS: usize = 60;

/// Names tried before giving up.
pub use recordings::export::range::MAX_FOLDER_TRIES as MAX_NAME_TRIES;

/// No free name was found within [`MAX_NAME_TRIES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooManyNames;

impl std::fmt::Display for TooManyNames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("too many files with the same name")
    }
}

impl std::error::Error for TooManyNames {}

/// The camera's name as it appears in a file name: its own, or `Camera A`
/// when share-safe.
pub fn camera_label(camera: &str, share_safe: bool) -> String {
    if share_safe {
        neutral_camera_name(0)
    } else {
        camera.to_owned()
    }
}

/// The first part of a live recording's file name, before numbering.
pub fn live_base_stem(start: Secs, camera: &str, share_safe: bool) -> String {
    sanitize(&format!(
        "{} {}",
        second_stamp(start),
        camera_label(camera, share_safe)
    ))
}

/// `<stem>.mp4`.
pub fn final_file_name(stem: &str) -> String {
    format!("{stem}.mp4")
}

/// `<stem>.partial.mp4`, the file written while recording.
pub fn partial_file_name(stem: &str) -> String {
    format!("{stem}.partial.mp4")
}

/// The stem of the `part`th segment (2 and up) of a recording whose first
/// segment has stem `stem`; part 1 is the stem itself.
pub fn part_stem(stem: &str, part: u32) -> String {
    if part <= 1 {
        stem.to_owned()
    } else {
        format!("{stem} (part {part})")
    }
}

/// The first stem, `base` or `base (2)` and so on, for which neither
/// `<stem>.mp4` nor `<stem>.partial.mp4` is taken.
pub fn unique_stem(base: &str, taken: impl Fn(&str) -> bool) -> Result<String, TooManyNames> {
    numbered_folder(base, |s| {
        taken(&final_file_name(s)) || taken(&partial_file_name(s))
    })
    .map_err(|_| TooManyNames)
}

/// A picture's file name, numbered when taken (` (2)` before the
/// extension).
pub fn picture_file_name(
    ms: i64,
    camera: &str,
    share_safe: bool,
    format: StillFormat,
    taken: impl Fn(&str) -> bool,
) -> Result<String, TooManyNames> {
    let first =
        recordings::export::still::still_name(ms, &camera_label(camera, share_safe), format);
    if !taken(&first) {
        return Ok(first);
    }
    let ext = format.extension();
    let stem = first.strip_suffix(&format!(".{ext}")).unwrap_or(&first);
    (2..=MAX_NAME_TRIES)
        .map(|n| format!("{stem} ({n}).{ext}"))
        .find(|c| !taken(c))
        .ok_or(TooManyNames)
}

/// A call recording's stem before numbering: the start and the camera's
/// name cut to [`CALL_NAME_CHARS`].
pub fn call_base_stem(start: Secs, camera: &str) -> String {
    let name: String = sanitize(camera).chars().take(CALL_NAME_CHARS).collect();
    let name = name.trim_end_matches(['.', ' ']);
    let name = if name.is_empty() { "_" } else { name };
    format!("{} {name}", second_stamp(start))
}

/// Reserves a call stem: `base`, `base 2`, `base 3`, ... the first for
/// which none of `.txt`, `.mp4`, `.partial.mp4` is taken. The recording
/// and its transcript share the result.
pub fn reserve_call_stem(
    start: Secs,
    camera: &str,
    taken: impl Fn(&str) -> bool,
) -> Result<String, TooManyNames> {
    let base = call_base_stem(start, camera);
    let is_taken = |s: &str| {
        taken(&format!("{s}.txt")) || taken(&final_file_name(s)) || taken(&partial_file_name(s))
    };
    if !is_taken(&base) {
        return Ok(base);
    }
    (2..=MAX_NAME_TRIES)
        .map(|n| format!("{base} {n}"))
        .find(|c| !is_taken(c))
        .ok_or(TooManyNames)
}

/// The stem of a final file name (`<stem>.mp4`), if it is one and not a
/// partial.
pub fn stem_of_final(file_name: &str) -> Option<&str> {
    let stem = file_name.strip_suffix(".mp4")?;
    (!stem.ends_with(".partial") && !stem.is_empty()).then_some(stem)
}

/// The final file name for a partial file name (`<stem>.partial.mp4`).
pub fn final_for_partial(partial_name: &str) -> Option<String> {
    let stem = partial_name.strip_suffix(".partial.mp4")?;
    (!stem.is_empty()).then(|| final_file_name(stem))
}

#[cfg(test)]
mod tests {
    use super::*;
    use recordings::DateTime;

    fn t(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Secs {
        DateTime {
            year: y as _,
            month: mo as _,
            day: d as _,
            hour: h as _,
            minute: mi as _,
            second: s as _,
        }
        .to_naive_secs()
    }

    #[test]
    fn live_stems() {
        let start = t(2026, 10, 11, 17, 2, 3);
        let table = [
            ("Front door", false, "2026-10-11 17.02.03 Front door"),
            ("Front door", true, "2026-10-11 17.02.03 Camera A"),
            ("a/b:c", false, "2026-10-11 17.02.03 a-b-c"),
            ("  Gate.  ", false, "2026-10-11 17.02.03 Gate"),
            ("", false, "2026-10-11 17.02.03"),
        ];
        for (name, safe, want) in table {
            assert_eq!(live_base_stem(start, name, safe), want, "{name:?}");
        }
        assert_eq!(final_file_name("x"), "x.mp4");
        assert_eq!(partial_file_name("x"), "x.partial.mp4");
    }

    #[test]
    fn a_long_name_is_capped_at_120_characters() {
        let stem = live_base_stem(t(2026, 10, 11, 17, 2, 3), &"x".repeat(300), false);
        assert_eq!(stem.chars().count(), 120);
    }

    #[test]
    fn parts_and_numbering() {
        assert_eq!(part_stem("a", 1), "a");
        assert_eq!(part_stem("a", 2), "a (part 2)");
        assert_eq!(part_stem("a (2)", 3), "a (2) (part 3)");
        let taken = |n: &str| ["a.mp4", "a (2).partial.mp4"].contains(&n);
        assert_eq!(unique_stem("b", taken).unwrap(), "b");
        assert_eq!(unique_stem("a", taken).unwrap(), "a (3)");
        assert_eq!(unique_stem("a", |_| true), Err(TooManyNames));
    }

    #[test]
    fn pictures() {
        let ms = t(2026, 10, 11, 17, 2, 3) * 1000 + 45;
        let none = |_: &str| false;
        assert_eq!(
            picture_file_name(ms, "Front", false, StillFormat::Png, none).unwrap(),
            "2026-10-11 17.02.03.045 Front.png"
        );
        assert_eq!(
            picture_file_name(ms, "Front", true, StillFormat::Jpeg, none).unwrap(),
            "2026-10-11 17.02.03.045 Camera A.jpg"
        );
        let taken = |n: &str| n == "2026-10-11 17.02.03.045 Front.png" || n.ends_with("(2).png");
        assert_eq!(
            picture_file_name(ms, "Front", false, StillFormat::Png, taken).unwrap(),
            "2026-10-11 17.02.03.045 Front (3).png"
        );
        assert_eq!(
            picture_file_name(ms, "Front", false, StillFormat::Png, |_| true),
            Err(TooManyNames)
        );
        // Negative milliseconds are not special (a clock before 1970 is
        // still a valid stamp).
        assert!(picture_file_name(-1, "F", false, StillFormat::Png, none).is_ok());
    }

    #[test]
    fn call_stems_follow_the_transcript_rule() {
        let start = t(2026, 10, 11, 17, 2, 3);
        assert_eq!(
            call_base_stem(start, "Front door"),
            "2026-10-11 17.02.03 Front door"
        );
        // The name is cut at 60 characters; a trailing space after the cut goes.
        let long = format!("{} {}", "a".repeat(59), "b".repeat(20));
        let stem = call_base_stem(start, &long);
        assert_eq!(stem, format!("2026-10-11 17.02.03 {}", "a".repeat(59)));
        assert_eq!(call_base_stem(start, ""), "2026-10-11 17.02.03 _");
        // Share-safe does not apply: the real name is used.
        assert!(call_base_stem(start, "Porch").ends_with("Porch"));
    }

    #[test]
    fn a_call_stem_is_taken_by_any_of_its_files() {
        let start = t(2026, 10, 11, 17, 2, 3);
        let base = "2026-10-11 17.02.03 Door";
        let free = reserve_call_stem(start, "Door", |_| false).unwrap();
        assert_eq!(free, base);
        for taken_name in [
            format!("{base}.txt"),
            format!("{base}.mp4"),
            format!("{base}.partial.mp4"),
        ] {
            let s = reserve_call_stem(start, "Door", |n| n == taken_name).unwrap();
            assert_eq!(s, format!("{base} 2"), "{taken_name}");
        }
        let two = |n: &str| n == format!("{base}.txt") || n == format!("{base} 2.mp4");
        assert_eq!(
            reserve_call_stem(start, "Door", two).unwrap(),
            format!("{base} 3")
        );
        assert_eq!(
            reserve_call_stem(start, "Door", |_| true),
            Err(TooManyNames)
        );
    }

    #[test]
    fn partial_and_final_names_map_to_each_other() {
        assert_eq!(
            final_for_partial("a b.partial.mp4").as_deref(),
            Some("a b.mp4")
        );
        assert_eq!(final_for_partial("a.mp4"), None);
        assert_eq!(final_for_partial(".partial.mp4"), None);
        assert_eq!(stem_of_final("a b.mp4"), Some("a b"));
        assert_eq!(stem_of_final("a.partial.mp4"), None);
        assert_eq!(stem_of_final("a.txt"), None);
    }
}
