//! Wrappers for values that must never appear in logs in full.

use std::fmt;

/// The mask shown in place of a redacted value.
pub const MASK: &str = "[redacted]";

/// Hides any value behind a fixed mask in both `Display` and `Debug`.
///
/// ```
/// use viewer_log::Redacted;
/// assert_eq!(format!("{}", Redacted("hunter2")), "[redacted]");
/// ```
#[derive(Clone, Copy)]
pub struct Redacted<T>(pub T);

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

/// Shows only the last four characters of an identifier such as a device UID.
///
/// Values of four characters or fewer are fully masked, so a short value is
/// never revealed.
///
/// ```
/// use viewer_log::RedactedTail;
/// assert_eq!(format!("{}", RedactedTail("UID-EXAMPLE-1234")), "****1234");
/// assert_eq!(format!("{}", RedactedTail("abc")), "****");
/// ```
#[derive(Clone, Copy)]
pub struct RedactedTail<'a>(pub &'a str);

impl fmt::Display for RedactedTail<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.0.chars().count();
        f.write_str("****")?;
        if count > 4 {
            let tail: String = self.0.chars().skip(count - 4).collect();
            f.write_str(&tail)?;
        }
        Ok(())
    }
}

impl fmt::Debug for RedactedTail<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_is_fixed() {
        assert_eq!(Redacted("secret").to_string(), MASK);
        assert_eq!(format!("{:?}", Redacted(12345)), MASK);
        assert!(!format!("{:?}", Some(Redacted("secret"))).contains("secret"));
    }

    #[test]
    fn tail_shows_last_four() {
        assert_eq!(RedactedTail("UID-EXAMPLE-9876").to_string(), "****9876");
        assert_eq!(
            format!("{:?}", RedactedTail("UID-EXAMPLE-9876")),
            "****9876"
        );
    }

    #[test]
    fn short_values_fully_masked() {
        assert_eq!(RedactedTail("").to_string(), "****");
        assert_eq!(RedactedTail("1234").to_string(), "****");
        assert_eq!(RedactedTail("12345").to_string(), "****2345");
    }

    #[test]
    fn tail_handles_multibyte() {
        assert_eq!(
            RedactedTail("ab\u{e9}\u{e9}\u{e9}\u{e9}").to_string(),
            "****\u{e9}\u{e9}\u{e9}\u{e9}"
        );
    }
}
