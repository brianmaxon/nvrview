//! Update sources and the rules for them (docs/UPDATES.md, "Update
//! sources"): a folder, an `https://` URL, or an `http://` URL to a host on
//! the local network.
//!
//! URLs are parsed here by a small strict parser rather than a general URL
//! library: a source is an absolute `http(s)` URL naming a folder, and
//! everything unusual (user names, odd characters, `..` segments) is
//! refused rather than normalized.

use crate::{Reason, UpdateError};
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;

/// The manifest's file name in a source folder.
pub const MANIFEST_NAME: &str = "updates.json";
/// The signature file's name.
pub const SIGNATURE_NAME: &str = "updates.json.sig";
/// Longest source or redirect URL accepted.
const MAX_URL: usize = 2048;

/// Where updates come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A folder holding the manifest and the packages.
    Folder(PathBuf),
    /// A URL whose path ends in `/` (the folder).
    Url(Url),
}

impl Source {
    /// Parses what the user typed or picked. `http://` is accepted only for
    /// local hosts ([`Host::is_local`]); see docs/UPDATES.md.
    pub fn parse(text: &str) -> Result<Source, UpdateError> {
        let text = text.trim();
        let invalid = |why: &str| UpdateError::new(Reason::SourceInvalid, why);
        if text.is_empty() {
            return Err(invalid("no update source"));
        }
        let lower = text.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            let url = Url::parse(text, false)?;
            if !url.secure && !url.host.is_local() {
                return Err(UpdateError::new(
                    Reason::InsecureSource,
                    "http:// is allowed only for hosts on the local network; use https://",
                ));
            }
            return Ok(Source::Url(url.folder()));
        }
        if lower.starts_with("file://") {
            return Ok(Source::Folder(folder_path(file_url_path(&text[7..])?)));
        }
        if let Some((scheme, _)) = text.split_once("://")
            && !scheme.is_empty()
            && scheme.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(invalid(
                "unsupported URL scheme: use https://, http:// on the local network, or a \
                 folder (mount a network share first)",
            ));
        }
        let path = PathBuf::from(text);
        if !path.is_absolute() {
            return Err(invalid("a folder source must be an absolute path"));
        }
        Ok(Source::Folder(folder_path(path)))
    }

    /// A short form for status lines: the folder, or the URL.
    pub fn display(&self) -> String {
        match self {
            Source::Folder(p) => p.display().to_string(),
            Source::Url(u) => u.to_string(),
        }
    }

    /// `folder` for a folder, `https` or `http` for a URL.
    pub fn kind(&self) -> &'static str {
        match self {
            Source::Folder(_) => "folder",
            Source::Url(u) if u.secure => "https",
            Source::Url(_) => "http",
        }
    }
}

/// A path to `updates.json` itself means its folder.
fn folder_path(path: PathBuf) -> PathBuf {
    if path.file_name().is_some_and(|n| n == MANIFEST_NAME)
        && let Some(parent) = path.parent()
    {
        return parent.to_path_buf();
    }
    path
}

/// The path of a `file://` URL (the text after `file://`).
fn file_url_path(rest: &str) -> Result<PathBuf, UpdateError> {
    let invalid = |why: &str| UpdateError::new(Reason::SourceInvalid, why);
    if rest.contains(['?', '#']) {
        return Err(invalid("a file:// source cannot have a query or fragment"));
    }
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => return Err(invalid("a file:// URL needs a path")),
    };
    let decoded = percent_decode(path).ok_or_else(|| invalid("invalid %-escape in file:// URL"))?;
    let host = host.to_ascii_lowercase();
    let local = host.is_empty() || host == "localhost";
    let b = decoded.as_bytes();
    // file:///C:/folder on Windows.
    let windows_drive = b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':';
    let path = if !local {
        if !cfg!(windows) {
            return Err(invalid(
                "file:// URLs naming a server work only on Windows; mount the share and use its folder",
            ));
        }
        format!("//{host}{decoded}")
    } else if windows_drive && cfg!(windows) {
        decoded[1..].to_string()
    } else {
        decoded
    };
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(invalid("a file:// URL must name an absolute folder"));
    }
    Ok(path)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    let text = String::from_utf8(out).ok()?;
    // A decoded NUL is never a valid path.
    if text.contains('\0') {
        None
    } else {
        Some(text)
    }
}

/// A host as written in a URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Host {
    /// A DNS name, lowercase.
    Name(String),
    V4(Ipv4Addr),
    V6(Ipv6Addr),
}

impl Host {
    /// Whether plain `http://` may be used with this host: a loopback,
    /// private (RFC 1918; IPv6 unique local), or link-local address,
    /// `localhost` (and names under `.localhost`), or a `.local` name.
    /// Judged by the host as written; names are not resolved.
    pub fn is_local(&self) -> bool {
        match self {
            Host::V4(a) => v4_is_local(*a),
            Host::V6(a) => {
                if let Some(v4) = a.to_ipv4_mapped() {
                    return v4_is_local(v4);
                }
                let first = a.segments()[0];
                a.is_loopback()
                    || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
                    || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
            }
            Host::Name(n) => n == "localhost" || n.ends_with(".localhost") || n.ends_with(".local"),
        }
    }
}

fn v4_is_local(a: Ipv4Addr) -> bool {
    a.is_loopback() || a.is_private() || a.is_link_local()
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Host::Name(n) => f.write_str(n),
            Host::V4(a) => write!(f, "{a}"),
            Host::V6(a) => write!(f, "[{a}]"),
        }
    }
}

/// An absolute `http` or `https` URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub secure: bool,
    pub host: Host,
    pub port: Option<u16>,
    /// Starts with `/`; may end in `?query` (redirect targets only).
    pub path: String,
}

impl Url {
    /// Parses an absolute URL. A source (`allow_query` false) may not have
    /// a query; a redirect target may (download servers sign them). A
    /// fragment, a user name or password, an empty host, and characters
    /// outside RFC 3986's sets are refused.
    pub fn parse(text: &str, allow_query: bool) -> Result<Url, UpdateError> {
        let invalid = |why: &str| UpdateError::new(Reason::SourceInvalid, why);
        if text.len() > MAX_URL {
            return Err(invalid("URL too long"));
        }
        let (scheme, rest) = text.split_once("://").ok_or_else(|| invalid("not a URL"))?;
        let secure = match scheme.to_ascii_lowercase().as_str() {
            "https" => true,
            "http" => false,
            _ => return Err(invalid("only http:// and https:// URLs")),
        };
        let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, mut path) = (&rest[..split], rest[split..].to_string());
        if authority.contains('@') {
            return Err(invalid(
                "URLs with a user name or password are not supported",
            ));
        }
        let (host, port) =
            parse_authority(authority).ok_or_else(|| invalid("invalid host or port"))?;
        if path.contains('#') {
            return Err(invalid("a URL with a fragment (#) is not supported"));
        }
        if path.is_empty() || path.starts_with('?') {
            path.insert(0, '/');
        }
        let (only_path, query) = match path.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (path.as_str(), None),
        };
        if query.is_some() && !allow_query {
            return Err(invalid("an update source cannot have a query (?)"));
        }
        if !only_path.bytes().all(path_byte) || query.is_some_and(|q| !q.bytes().all(query_byte)) {
            return Err(invalid("invalid characters in URL"));
        }
        // A server decodes `%2e` to `.` before it resolves segments, so
        // every spelling of `.` and `..` (`%2e`, `.%2E`, ...) counts.
        if only_path.split('/').any(|seg| {
            let decoded = seg.to_ascii_lowercase().replace("%2e", ".");
            decoded == "." || decoded == ".."
        }) {
            return Err(invalid("a URL path cannot have . or .. segments"));
        }
        Ok(Url {
            secure,
            host,
            port,
            path,
        })
    }

    /// The folder this URL names: a path ending in `/updates.json` means
    /// its folder; any other path is taken as a folder.
    fn folder(mut self) -> Url {
        if let Some(stripped) = self.path.strip_suffix(MANIFEST_NAME)
            && stripped.ends_with('/')
        {
            self.path = stripped.to_string();
        }
        if !self.path.ends_with('/') {
            self.path.push('/');
        }
        self
    }

    /// A file in this folder URL (`name` is a validated bare file name).
    pub fn file(&self, name: &str) -> Url {
        let mut u = self.clone();
        u.path.push_str(name);
        u
    }

    /// Resolves a redirect's `Location` against this URL: an absolute URL,
    /// `//host/path`, `/path`, or a path relative to this URL's folder.
    pub fn join(&self, location: &str) -> Result<Url, UpdateError> {
        let location = location.trim();
        let lower = location.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return Url::parse(location, true);
        }
        if location.contains("://") {
            return Err(UpdateError::new(
                Reason::RedirectRefused,
                "redirect to a scheme other than http or https",
            ));
        }
        let scheme = if self.secure { "https" } else { "http" };
        let absolute = if let Some(rest) = location.strip_prefix("//") {
            format!("{scheme}://{rest}")
        } else if location.starts_with('/') {
            format!("{scheme}://{}{location}", self.authority())
        } else {
            let dir = match self.path.split('?').next().unwrap_or("/").rfind('/') {
                Some(i) => &self.path[..=i],
                None => "/",
            };
            format!("{scheme}://{}{dir}{location}", self.authority())
        };
        Url::parse(&absolute, true)
    }

    fn authority(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{p}", self.host),
            None => self.host.to_string(),
        }
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.secure { "https" } else { "http" };
        write!(f, "{scheme}://{}{}", self.authority(), self.path)
    }
}

fn parse_authority(a: &str) -> Option<(Host, Option<u16>)> {
    let (host_text, port_text) = if let Some(rest) = a.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':')?),
        };
        (h, port)
    } else {
        match a.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (a, None),
        }
    };
    let port = match port_text {
        None => None,
        Some(p) => {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let n: u16 = p.parse().ok()?;
            if n == 0 {
                return None;
            }
            Some(n)
        }
    };
    let host = if a.starts_with('[') {
        Host::V6(host_text.parse().ok()?)
    } else if let Ok(v4) = host_text.parse::<Ipv4Addr>() {
        Host::V4(v4)
    } else {
        let name = host_text.to_ascii_lowercase();
        let ok = !name.is_empty()
            && name.len() <= 253
            && name.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            // All-numeric dotted names are malformed addresses, not names.
            && !name.bytes().all(|b| b.is_ascii_digit() || b == b'.');
        if !ok {
            return None;
        }
        Host::Name(name)
    };
    Some((host, port))
}

/// RFC 3986 `pchar` plus `/`, with `%` for escapes.
fn path_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/%".contains(&b)
}

fn query_byte(b: u8) -> bool {
    path_byte(b) || b == b'?'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        match Source::parse(s).unwrap() {
            Source::Url(u) => u,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn https_sources() {
        let u = url("https://Updates.Example.com/nvrview");
        assert_eq!(u.to_string(), "https://updates.example.com/nvrview/");
        assert_eq!(
            url("https://updates.example.com/nvrview/updates.json").to_string(),
            "https://updates.example.com/nvrview/"
        );
        assert_eq!(
            url("https://updates.example.com").to_string(),
            "https://updates.example.com/"
        );
        assert_eq!(url("https://203.0.113.5:8443/u/").port, Some(8443));
        assert_eq!(
            url("https://updates.example.com/a/")
                .file("updates.json")
                .to_string(),
            "https://updates.example.com/a/updates.json"
        );
        assert_eq!(
            url("https://[2001:db8::1]/x").host,
            Host::V6("2001:db8::1".parse().unwrap())
        );
    }

    #[test]
    fn http_only_on_the_local_network() {
        for ok in [
            "http://127.0.0.1:8000/u/",
            "http://localhost/u",
            "http://nas.local/updates/",
            "http://10.1.2.3/",       // range test case, sanitize:allow
            "http://172.16.0.1/",     // range test case, sanitize:allow
            "http://172.31.255.255/", // range test case, sanitize:allow
            "http://192.168.1.10/u",  // range test case, sanitize:allow
            "http://169.254.10.10/",  // range test case, sanitize:allow
            "http://[::1]:8080/",
            "http://[fe80::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:192.168.1.1]/", // range test case, sanitize:allow
            "http://dev.localhost/",
        ] {
            assert!(Source::parse(ok).is_ok(), "{ok}");
        }
        for public in [
            "http://updates.example.com/u/",
            "http://203.0.113.5/",
            "http://172.32.0.1/", // range test case, sanitize:allow
            "http://198.51.100.7/",
            "http://[2001:db8::1]/",
            "http://[::ffff:203.0.113.5]/",
            "http://local/",
            "http://example.local.example.com/",
        ] {
            let e = Source::parse(public).unwrap_err();
            assert_eq!(e.reason, Reason::InsecureSource, "{public}");
        }
    }

    #[test]
    fn refuses_odd_urls() {
        for bad in [
            "https://user:pass@updates.example.com/",
            "https://user@updates.example.com/",
            "https://updates.example.com/u?token=1",
            "https://updates.example.com/u#frag",
            "https://updates.example.com/a/../b/",
            "https://updates.example.com/a/./b/",
            "https://updates.example.com/a/%2e%2e/b/",
            "https://updates.example.com/a/.%2E/b/",
            "https://updates.example.com/a/%2e./b/",
            "https://updates.example.com/a/%2E/b/",
            "https://updates.example.com/a b/",
            "https://updates.example.com:0/",
            "https://updates.example.com:99999/",
            "https://updates.example.com:/",
            "https:///path",
            "https://-bad-.example.com/",
            "https://exa_mple.com/",
            "https://999.1.1.1/",
            "https://[::1/",
            "ftp://updates.example.com/",
            "smb://server/share",
            "relative/folder",
        ] {
            let e = Source::parse(bad).unwrap_err();
            assert_eq!(e.reason, Reason::SourceInvalid, "{bad}");
        }
    }

    #[test]
    fn folders() {
        #[cfg(unix)]
        {
            assert_eq!(
                Source::parse("/Volumes/share/nvrview").unwrap(),
                Source::Folder("/Volumes/share/nvrview".into())
            );
            assert_eq!(
                Source::parse("/srv/u/updates.json").unwrap(),
                Source::Folder("/srv/u".into())
            );
            assert_eq!(
                Source::parse("file:///Volumes/My%20Share/u").unwrap(),
                Source::Folder("/Volumes/My Share/u".into())
            );
            assert_eq!(
                Source::parse("file://localhost/srv/u").unwrap(),
                Source::Folder("/srv/u".into())
            );
            assert!(Source::parse("file://server/share/u").is_err());
            assert!(Source::parse(r"\\server\share\u").is_err());
        }
        #[cfg(windows)]
        {
            assert_eq!(
                Source::parse(r"\\server\share\u").unwrap(),
                Source::Folder(r"\\server\share\u".into())
            );
            assert_eq!(
                Source::parse(r"D:\updates").unwrap(),
                Source::Folder(r"D:\updates".into())
            );
            assert_eq!(
                Source::parse("file:///D:/updates").unwrap(),
                Source::Folder("D:/updates".into())
            );
            assert_eq!(
                Source::parse("file://server/share/u").unwrap(),
                Source::Folder("//server/share/u".into())
            );
        }
        assert!(Source::parse("file:///u%zz").is_err());
        assert!(Source::parse("file:///u%00").is_err());
        assert!(Source::parse("").is_err());
    }

    #[test]
    fn redirects_resolve() {
        let base = Url::parse("https://updates.example.com/a/updates.json", false).unwrap();
        let j = |l: &str| base.join(l).unwrap().to_string();
        assert_eq!(
            j("https://cdn.example.com/x.zip?sig=1"),
            "https://cdn.example.com/x.zip?sig=1"
        );
        assert_eq!(j("//cdn.example.com/x"), "https://cdn.example.com/x");
        assert_eq!(
            j("/b/updates.json"),
            "https://updates.example.com/b/updates.json"
        );
        assert_eq!(j("other.json"), "https://updates.example.com/a/other.json");
        assert!(base.join("ftp://x/").is_err());
        assert!(base.join("javascript://x").is_err());
        assert!(base.join("/a/../../etc").is_err());
        assert!(base.join("/a/.%2e/.%2e/etc").is_err());
        assert!(base.join("%2e%2e/x.zip").is_err());
        let downgrade = base.join("http://updates.example.com/a/").unwrap();
        assert!(!downgrade.secure);
    }
}
