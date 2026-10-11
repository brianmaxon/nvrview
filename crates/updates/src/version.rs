//! Semantic versions (`MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]`), compared by
//! semver precedence. Only what the manifest and the apps' `VERSION` file
//! need: no ranges, no requirements.

use std::cmp::Ordering;
use std::fmt;

/// Longest version text accepted.
const MAX_LEN: usize = 64;

/// A parsed version. Build metadata is kept for display but ignored when
/// comparing, as semver says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Dot-separated pre-release identifiers; empty for a release.
    pub pre: Vec<String>,
    /// Build metadata after `+`, or empty.
    pub build: String,
}

impl Version {
    /// Parses `1.2.3`, `1.2.3-beta.1`, `1.2.3+build.5`. Leading zeros in
    /// numbers are refused, as semver requires.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_LEN {
            return None;
        }
        let (rest, build) = match text.split_once('+') {
            Some((r, b)) => {
                if b.is_empty() || !b.split('.').all(valid_identifier) {
                    return None;
                }
                (r, b.to_string())
            }
            None => (text, String::new()),
        };
        let (core, pre) = match rest.split_once('-') {
            Some((c, p)) => {
                if p.is_empty() || !p.split('.').all(valid_identifier) {
                    return None;
                }
                for id in p.split('.') {
                    if is_numeric(id) && id.len() > 1 && id.starts_with('0') {
                        return None;
                    }
                }
                (c, p.split('.').map(str::to_string).collect())
            }
            None => (rest, Vec::new()),
        };
        let mut parts = core.split('.');
        let major = number(parts.next()?)?;
        let minor = number(parts.next()?)?;
        let patch = number(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
            pre,
            build,
        })
    }

    /// Whether this version is strictly newer than `other`.
    pub fn is_newer_than(&self, other: &Version) -> bool {
        self.cmp_precedence(other) == Ordering::Greater
    }

    /// Semver precedence: numbers first, then a release above any of its
    /// pre-releases, then pre-release identifiers left to right (numeric
    /// ones numerically and below alphanumeric ones; a shorter list of
    /// equal identifiers first). Build metadata is ignored.
    pub fn cmp_precedence(&self, other: &Version) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => compare_pre(&self.pre, &other.pre),
            })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        if !self.build.is_empty() {
            write!(f, "+{}", self.build)?;
        }
        Ok(())
    }
}

fn compare_pre(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let o = match (is_numeric(x), is_numeric(y)) {
            // Lengths first: no leading zeros, so a longer number is larger
            // (and this never overflows).
            (true, true) => x.len().cmp(&y.len()).then_with(|| x.cmp(y)),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => x.cmp(y),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    a.len().cmp(&b.len())
}

fn number(s: &str) -> Option<u64> {
    if !is_numeric(s) || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.parse().ok()
}

fn is_numeric(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn valid_identifier(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn parses_and_prints() {
        assert_eq!(v("0.3.0").to_string(), "0.3.0");
        assert_eq!(v(" 1.2.3\n").to_string(), "1.2.3");
        assert_eq!(v("1.2.3-beta.1+b.5").to_string(), "1.2.3-beta.1+b.5");
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4", // a version string, sanitize:allow
            "01.2.3",
            "1.02.3",
            "1.2.3-",
            "1.2.3+",
            "1.2.3-01",
            "1.2.3-a..b",
            "v1.2.3",
            "1.2.x",
            "-1.2.3",
            "1.2.3-a_b",
            "1. 2.3",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
        assert!(Version::parse(&format!("1.2.3-{}", "a".repeat(80))).is_none());
    }

    #[test]
    fn precedence_follows_semver() {
        // The order from the semver specification, section 11.
        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
            "10.0.0",
        ];
        for (i, a) in ordered.iter().enumerate() {
            for (j, b) in ordered.iter().enumerate() {
                assert_eq!(v(a).cmp_precedence(&v(b)), i.cmp(&j), "{a} vs {b}");
            }
        }
        assert_eq!(v("1.0.0+a").cmp_precedence(&v("1.0.0+b")), Ordering::Equal);
        assert!(!v("1.0.0+b").is_newer_than(&v("1.0.0")));
        assert!(v("0.4.0").is_newer_than(&v("0.3.0")));
        assert!(!v("0.3.0").is_newer_than(&v("0.3.0")));
        assert!(!v("0.2.9").is_newer_than(&v("0.3.0")));
    }
}
