//! Reading files from the update source: a folder, or HTTP(S) with the
//! redirect, timeout, and size rules of docs/UPDATES.md ("Fetching
//! rules").
//!
//! Redirects are followed here, not by the HTTP library, so every hop is
//! checked against the source rules before anything is sent to it.

use crate::source::{Source, Url};
use crate::{Cancel, Config, Reason, Timeouts, UpdateError};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Most redirects followed for one file.
pub const MAX_REDIRECTS: usize = 5;
const CHUNK: usize = 64 * 1024;

pub(crate) struct Fetcher {
    kind: Kind,
    timeouts: Timeouts,
}

enum Kind {
    Folder(PathBuf),
    Http {
        base: Url,
        roots: Option<Arc<Vec<ureq::tls::Certificate<'static>>>>,
    },
}

impl Fetcher {
    pub(crate) fn new(config: &Config) -> Result<Fetcher, UpdateError> {
        let kind = match &config.source {
            Source::Folder(p) => Kind::Folder(p.clone()),
            Source::Url(u) => {
                if u.secure && !crate::platform::https_supported() {
                    return Err(UpdateError::new(
                        Reason::UnsupportedCpu,
                        "HTTPS sources need a processor with AES-NI, AVX2, BMI2, and ADX; use a folder or a local http:// source",
                    ));
                }
                let roots = config.tls_roots.as_ref().map(|list| {
                    Arc::new(
                        list.iter()
                            .map(|der| ureq::tls::Certificate::from_der(der).to_owned())
                            .collect::<Vec<_>>(),
                    )
                });
                Kind::Http {
                    base: u.clone(),
                    roots,
                }
            }
        };
        Ok(Fetcher {
            kind,
            timeouts: config.timeouts,
        })
    }

    /// Reads a small file (the manifest or the signature) whole, refusing
    /// more than `max` bytes.
    pub(crate) fn read_small(
        &self,
        name: &str,
        max: u64,
        cancel: &Cancel,
    ) -> Result<Vec<u8>, UpdateError> {
        cancel.check()?;
        let (mut reader, _) = self.open(name, max, self.timeouts.small_body)?;
        let mut out = Vec::new();
        copy_limited(&mut *reader, max, cancel, &mut |chunk| {
            out.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(out)
    }

    /// Downloads a package into `out`, refusing more than `size` bytes.
    /// Returns the bytes received and their SHA-256.
    pub(crate) fn download(
        &self,
        name: &str,
        size: u64,
        out: &mut File,
        cancel: &Cancel,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(u64, [u8; 32]), UpdateError> {
        cancel.check()?;
        let (mut reader, _) = self.open(name, size, self.timeouts.package_body)?;
        let mut hasher = Sha256::new();
        let mut received = 0u64;
        progress(0, size);
        copy_limited(&mut *reader, size, cancel, &mut |chunk| {
            hasher.update(chunk);
            received += chunk.len() as u64;
            out.write_all(chunk).map_err(|e| {
                UpdateError::new(Reason::Staging, format!("writing the download: {e}"))
            })?;
            progress(received, size);
            Ok(())
        })?;
        Ok((received, hasher.finalize().into()))
    }

    /// Opens a file of the source for reading. A length known up front
    /// (file size, Content-Length) over `max` is refused before reading.
    fn open(
        &self,
        name: &str,
        max: u64,
        body_timeout: Duration,
    ) -> Result<(Box<dyn Read>, Option<u64>), UpdateError> {
        match &self.kind {
            Kind::Folder(dir) => {
                let path = dir.join(name);
                let file = File::open(&path).map_err(|e| match e.kind() {
                    ErrorKind::NotFound => {
                        if dir.is_dir() {
                            UpdateError::new(
                                Reason::NotFound,
                                format!("{name} is not in the update folder"),
                            )
                        } else {
                            UpdateError::new(
                                Reason::Unreachable,
                                "the update folder is not available (not mounted?)",
                            )
                        }
                    }
                    _ => UpdateError::new(Reason::Unreachable, format!("reading {name}: {e}")),
                })?;
                let len = file
                    .metadata()
                    .map_err(|e| {
                        UpdateError::new(Reason::Unreachable, format!("reading {name}: {e}"))
                    })?
                    .len();
                if len > max {
                    return Err(too_large(name, max));
                }
                Ok((Box::new(file), Some(len)))
            }
            Kind::Http { base, roots } => {
                let mut url = base.file(name);
                for _ in 0..=MAX_REDIRECTS {
                    check_hop(base, &url)?;
                    let agent = self.agent(&url, body_timeout, roots.clone());
                    let response = agent.get(&url.to_string()).call().map_err(map_error)?;
                    let status = response.status().as_u16();
                    match status {
                        200 => {
                            let length = response.body().content_length();
                            if length.is_some_and(|n| n > max) {
                                return Err(too_large(name, max));
                            }
                            let reader = response
                                .into_body()
                                .into_with_config()
                                .limit(max.saturating_add(1))
                                .reader();
                            return Ok((Box::new(reader), length));
                        }
                        301 | 302 | 303 | 307 | 308 => {
                            let location = response
                                .headers()
                                .get("location")
                                .and_then(|v| v.to_str().ok())
                                .ok_or_else(|| {
                                    UpdateError::new(
                                        Reason::RedirectRefused,
                                        "a redirect without a location",
                                    )
                                })?;
                            url = url
                                .join(location)
                                .map_err(|e| UpdateError::new(Reason::RedirectRefused, e.detail))?;
                        }
                        404 | 410 => {
                            return Err(UpdateError::new(
                                Reason::NotFound,
                                format!("{name}: HTTP {status}"),
                            ));
                        }
                        _ => {
                            return Err(UpdateError::new(
                                Reason::Unreachable,
                                format!("{name}: HTTP {status}"),
                            ));
                        }
                    }
                }
                Err(UpdateError::new(
                    Reason::RedirectRefused,
                    format!("more than {MAX_REDIRECTS} redirects"),
                ))
            }
        }
    }

    fn agent(
        &self,
        url: &Url,
        body_timeout: Duration,
        roots: Option<Arc<Vec<ureq::tls::Certificate<'static>>>>,
    ) -> ureq::Agent {
        let mut tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::Rustls)
            .unversioned_rustls_crypto_provider(Arc::new(rustls_graviola::default_provider()));
        if let Some(r) = roots {
            tls = tls.root_certs(ureq::tls::RootCerts::Specific(r));
        }
        // A proxy from the environment is used for hosts outside the local
        // network only.
        let proxy = if url.host.is_local() {
            None
        } else {
            ureq::Proxy::try_from_env()
        };
        let t = self.timeouts;
        ureq::Agent::config_builder()
            .tls_config(tls.build())
            .proxy(proxy)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .https_only(false)
            .user_agent(concat!("nvrview-updates/", env!("CARGO_PKG_VERSION")))
            .timeout_resolve(Some(t.connect))
            .timeout_connect(Some(t.connect))
            .timeout_send_request(Some(t.connect))
            .timeout_recv_response(Some(t.response))
            .timeout_recv_body(Some(body_timeout))
            .build()
            .into()
    }
}

/// Every request, the first and each redirect, follows the source rules:
/// an `https://` source never goes to `http://`, and `http://` only ever
/// goes to a local host.
pub fn check_hop(source: &Url, next: &Url) -> Result<(), UpdateError> {
    if next.secure {
        return Ok(());
    }
    if source.secure {
        return Err(UpdateError::new(
            Reason::RedirectRefused,
            "an https:// source redirected to http://",
        ));
    }
    if !next.host.is_local() {
        return Err(UpdateError::new(
            Reason::RedirectRefused,
            "a redirect to http:// outside the local network",
        ));
    }
    Ok(())
}

fn too_large(name: &str, max: u64) -> UpdateError {
    UpdateError::new(
        Reason::TooLarge,
        format!("{name} is larger than {max} bytes"),
    )
}

/// Reads to the end, handing chunks to `sink`; more than `max` bytes is
/// [`Reason::TooLarge`].
fn copy_limited(
    reader: &mut dyn Read,
    max: u64,
    cancel: &Cancel,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), UpdateError>,
) -> Result<(), UpdateError> {
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        cancel.check()?;
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(map_read_error(e)),
        };
        if n == 0 {
            return Ok(());
        }
        total += n as u64;
        if total > max {
            return Err(UpdateError::new(
                Reason::TooLarge,
                format!("more than {max} bytes"),
            ));
        }
        sink(&buf[..n])?;
    }
}

fn map_read_error(e: std::io::Error) -> UpdateError {
    // ureq reports its body limit and timeouts as io errors wrapping its
    // own error.
    if let Some(inner) = e.get_ref().and_then(|i| i.downcast_ref::<ureq::Error>()) {
        return map_error_ref(inner);
    }
    UpdateError::new(Reason::Unreachable, format!("reading: {e}"))
}

fn map_error(e: ureq::Error) -> UpdateError {
    map_error_ref(&e)
}

fn map_error_ref(e: &ureq::Error) -> UpdateError {
    match e {
        ureq::Error::BodyExceedsLimit(n) => {
            UpdateError::new(Reason::TooLarge, format!("more than {n} bytes"))
        }
        ureq::Error::Timeout(t) => {
            UpdateError::new(Reason::Unreachable, format!("timed out ({t:?})"))
        }
        ureq::Error::HostNotFound => UpdateError::new(Reason::Unreachable, "host not found"),
        ureq::Error::ConnectionFailed => UpdateError::new(Reason::Unreachable, "connection failed"),
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            UpdateError::new(Reason::RedirectRefused, e.to_string())
        }
        other => UpdateError::new(Reason::Unreachable, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s, true).unwrap()
    }

    #[test]
    fn hops_follow_the_source_rules() {
        let https = u("https://updates.example.com/u/");
        let http_local = u("http://nas.local/u/");
        assert!(check_hop(&https, &u("https://cdn.example.com/x")).is_ok());
        assert_eq!(
            check_hop(&https, &u("http://updates.example.com/u/"))
                .unwrap_err()
                .reason,
            Reason::RedirectRefused
        );
        assert_eq!(
            check_hop(&https, &u("http://nas.local/u/"))
                .unwrap_err()
                .reason,
            Reason::RedirectRefused
        );
        assert!(check_hop(&http_local, &u("http://nas.local/x")).is_ok());
        assert!(check_hop(&http_local, &u("https://cdn.example.com/x")).is_ok());
        assert_eq!(
            check_hop(&http_local, &u("http://203.0.113.9/x"))
                .unwrap_err()
                .reason,
            Reason::RedirectRefused
        );
    }
}
