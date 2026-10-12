//! The network seam: a blocking [`Http`] trait, its ureq implementation,
//! and the token wrapper that keeps the secret out of logs.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

/// A long-lived access token. Held in memory only (the host reads it from
/// the keychain); `Debug` is redacted and there is no `Display` or
/// `Serialize`.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    pub fn new(token: impl Into<String>) -> Token {
        Token(token.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// One request. The token is attached as `Authorization: Bearer` by the
/// implementation; `Debug` shows no secret.
pub struct HttpRequest<'a> {
    pub method: Method,
    pub url: &'a str,
    pub token: &'a Token,
    /// A JSON body for POST.
    pub body: Option<&'a [u8]>,
}

impl fmt::Debug for HttpRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("token", &"[redacted]")
            .finish()
    }
}

#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// A request that got no HTTP response. Carries no detail text: library
/// error messages can quote URLs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetError {
    Timeout,
    /// Name lookup, connect, or TLS (including certificate) failure.
    Connect,
    Other,
}

pub trait Http: Send + Sync {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, NetError>;
}

const MAX_BODY: u64 = 1 << 20;

/// The real client: ureq on rustls with certificate checks, no redirects
/// (a redirect would carry the token elsewhere), a proxy from the
/// environment only for hosts outside the local network.
pub struct UreqHttp {
    local: ureq::Agent,
    remote: ureq::Agent,
}

impl UreqHttp {
    pub fn new(timeout: Duration) -> UreqHttp {
        UreqHttp {
            local: agent(timeout, false),
            remote: agent(timeout, true),
        }
    }
}

fn agent(timeout: Duration, proxy_from_env: bool) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .unversioned_rustls_crypto_provider(Arc::new(rustls_graviola::default_provider()))
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .proxy(if proxy_from_env {
            ureq::Proxy::try_from_env()
        } else {
            None
        })
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .https_only(false)
        .user_agent(concat!("nvrview-ha-output/", env!("CARGO_PKG_VERSION")))
        .timeout_global(Some(timeout))
        .build()
        .into()
}

impl Http for UreqHttp {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, NetError> {
        let host = req
            .url
            .split_once("://")
            .map(|(_, r)| r.split(['/', ':']).next().unwrap_or(""))
            .unwrap_or("");
        let agent = if host_is_local(host.trim_matches(['[', ']'])) {
            &self.local
        } else {
            &self.remote
        };
        let auth = format!("Bearer {}", req.token.expose());
        let sent = match req.method {
            Method::Get => agent.get(req.url).header("Authorization", &auth).call(),
            Method::Post => agent
                .post(req.url)
                .header("Authorization", &auth)
                .content_type("application/json")
                .send(req.body.unwrap_or(b"{}")),
        };
        let mut resp = sent.map_err(map_error)?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_vec()
            .map_err(map_error)?;
        Ok(HttpResponse { status, body })
    }
}

fn map_error(e: ureq::Error) -> NetError {
    match e {
        ureq::Error::Timeout(_) => NetError::Timeout,
        ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Tls(_)
        | ureq::Error::Rustls(_)
        | ureq::Error::Io(_) => NetError::Connect,
        _ => NetError::Other,
    }
}

/// Whether plain `http://` may be used with this host as written (names are
/// not resolved): loopback, private (RFC 1918, IPv6 unique local), and
/// link-local addresses, `localhost`, and `.local`, `.lan`, `.home.arpa`, or
/// single-label names.
pub fn host_is_local(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if let Ok(a) = host.parse::<Ipv4Addr>() {
        return v4_local(a);
    }
    if let Ok(a) = host.parse::<Ipv6Addr>() {
        if let Some(v4) = a.to_ipv4_mapped() {
            return v4_local(v4);
        }
        let first = a.segments()[0];
        return a.is_loopback() || (first & 0xffc0) == 0xfe80 || (first & 0xfe00) == 0xfc00;
    }
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".lan")
        || host.ends_with(".home.arpa")
        || (!host.is_empty() && !host.contains('.'))
}

fn v4_local(a: Ipv4Addr) -> bool {
    a.is_loopback() || a.is_private() || a.is_link_local()
}

/// Whether graviola (the HTTPS crypto) runs on this processor; it stops the
/// process on one without the features, so HTTPS is refused up front.
pub(crate) fn https_supported() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("aes")
            && std::arch::is_x86_feature_detected!("pclmulqdq")
            && std::arch::is_x86_feature_detected!("ssse3")
            && std::arch::is_x86_feature_detected!("avx")
            && std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("bmi1")
            && std::arch::is_x86_feature_detected!("bmi2")
            && std::arch::is_x86_feature_detected!("adx")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("neon")
            && std::arch::is_aarch64_feature_detected!("aes")
            && std::arch::is_aarch64_feature_detected!("pmull")
            && std::arch::is_aarch64_feature_detected!("sha2")
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}
