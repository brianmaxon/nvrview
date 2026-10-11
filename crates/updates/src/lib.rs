//! Self-update from a location the user hosts (docs/UPDATES.md): the
//! source rules, fetching from a folder or over HTTP(S), the signed
//! manifest, version comparison, the package download, and unpacking into
//! a staging folder. Platform neutral: moving the staged version into place
//! and relaunching are the apps' part.
//!
//! Everything here blocks; callers run it on a thread of their own and
//! cancel it with a [`Cancel`].

pub mod fetch;
pub mod keys;
pub mod manifest;
pub mod platform;
pub mod source;
pub mod version;
pub mod zip;

pub use keys::{PublicKey, SecretKey};
pub use manifest::{Manifest, Package};
pub use source::Source;
pub use version::Version;

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Why a check or download failed; the `reason` of the `update` event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The source is not a folder or URL this core accepts.
    SourceInvalid,
    /// `http://` to a host that is not on the local network.
    InsecureSource,
    /// No trusted key is installed.
    NoTrustedKeys,
    /// The folder or server could not be read (not mounted, no answer,
    /// TLS failure, an HTTP error status).
    Unreachable,
    /// The manifest, signature, or package is not there.
    NotFound,
    /// A response or file is larger than allowed.
    TooLarge,
    /// A redirect broke the source rules, or there were too many.
    RedirectRefused,
    /// No signature line verified with any trusted key.
    BadSignature,
    /// The manifest is malformed (after its signature verified).
    BadManifest,
    /// The manifest's format is newer than this core reads.
    UnsupportedFormat,
    /// The source no longer offers the version the user accepted.
    VersionChanged,
    /// The version asked for is not newer than the running one.
    NotNewer,
    /// The package's size or SHA-256 does not match the manifest.
    BadPackage,
    /// The package would unpack outside its folder or is malformed.
    UnsafeArchive,
    /// The staging folder could not be created or written.
    Staging,
    /// Canceled by the app.
    Canceled,
    /// HTTPS needs CPU features this processor lacks.
    UnsupportedCpu,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::SourceInvalid => "source_invalid",
            Reason::InsecureSource => "insecure_source",
            Reason::NoTrustedKeys => "no_trusted_keys",
            Reason::Unreachable => "unreachable",
            Reason::NotFound => "not_found",
            Reason::TooLarge => "too_large",
            Reason::RedirectRefused => "redirect_refused",
            Reason::BadSignature => "bad_signature",
            Reason::BadManifest => "bad_manifest",
            Reason::UnsupportedFormat => "unsupported_format",
            Reason::VersionChanged => "version_changed",
            Reason::NotNewer => "not_newer",
            Reason::BadPackage => "bad_package",
            Reason::UnsafeArchive => "unsafe_archive",
            Reason::Staging => "staging",
            Reason::Canceled => "canceled",
            Reason::UnsupportedCpu => "unsupported_cpu",
        }
    }
}

/// A failure: a [`Reason`] for programs and a sentence for people. The
/// detail may name the source's host or folder, so it is logged at debug
/// level only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateError {
    pub reason: Reason,
    pub detail: String,
}

impl UpdateError {
    pub fn new(reason: Reason, detail: impl Into<String>) -> UpdateError {
        UpdateError {
            reason,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason.as_str(), self.detail)
    }
}

impl std::error::Error for UpdateError {}

/// Cancels a running check or download from another thread. A blocking
/// network read notices at its next chunk or when its timeout ends.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_canceled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub(crate) fn check(&self) -> Result<(), UpdateError> {
        if self.is_canceled() {
            Err(UpdateError::new(Reason::Canceled, "canceled"))
        } else {
            Ok(())
        }
    }
}

/// Network timeouts (docs/UPDATES.md, "Fetching rules").
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// Resolving the host and connecting.
    pub connect: Duration,
    /// Until a response begins.
    pub response: Duration,
    /// Reading the manifest or the signature.
    pub small_body: Duration,
    /// Reading a package.
    pub package_body: Duration,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts {
            connect: Duration::from_secs(10),
            response: Duration::from_secs(20),
            small_body: Duration::from_secs(30),
            package_body: Duration::from_secs(30 * 60),
        }
    }
}

/// What a check or download needs.
#[derive(Clone, Debug)]
pub struct Config {
    pub source: Source,
    /// The keys the user trusts; empty means nothing is accepted.
    pub keys: Vec<PublicKey>,
    /// The running app's version.
    pub current: Version,
    /// This app's platform name ([`platform::current`] by default).
    pub platform: String,
    pub timeouts: Timeouts,
    pub archive: zip::Limits,
    /// Trust anchors (DER certificates) used instead of the built-in
    /// Mozilla root list. For tests with a local HTTPS server; `None` in
    /// the apps.
    pub tls_roots: Option<Vec<Vec<u8>>>,
}

impl Config {
    pub fn new(source: Source, keys: Vec<PublicKey>, current: Version) -> Config {
        Config {
            source,
            keys,
            current,
            platform: platform::current().to_string(),
            timeouts: Timeouts::default(),
            archive: zip::Limits::default(),
            tls_roots: None,
        }
    }
}

/// A newer version the source offers for this platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub version: Version,
    pub notes: String,
    pub package: Package,
    /// The ID of the trusted key that verified the manifest.
    pub key_id: String,
}

/// What a check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Nothing to offer. `latest` is the manifest's version;
    /// `no_package` is true when it is newer but has no package for this
    /// platform.
    UpToDate {
        latest: Version,
        no_package: bool,
    },
    Available(Offer),
}

/// A downloaded, verified, unpacked update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub version: Version,
    /// The unpacked package: `<staging>/content`.
    pub content: PathBuf,
}

/// Fetches the manifest and its signature, verifies the signature with
/// the trusted keys, and parses the manifest.
pub fn fetch_manifest(config: &Config, cancel: &Cancel) -> Result<(Manifest, String), UpdateError> {
    if config.keys.is_empty() {
        return Err(UpdateError::new(
            Reason::NoTrustedKeys,
            "no trusted key is installed; add the update publisher's public key",
        ));
    }
    let fetcher = fetch::Fetcher::new(config)?;
    let manifest_bytes =
        fetcher.read_small(source::MANIFEST_NAME, manifest::MAX_MANIFEST, cancel)?;
    let signature_bytes =
        match fetcher.read_small(source::SIGNATURE_NAME, keys::MAX_SIGNATURE_FILE, cancel) {
            Err(e) if e.reason == Reason::NotFound => {
                return Err(UpdateError::new(
                    Reason::BadSignature,
                    "the manifest is not signed (no updates.json.sig)",
                ));
            }
            other => other?,
        };
    let lines = std::str::from_utf8(&signature_bytes)
        .ok()
        .and_then(keys::parse_signatures)
        .ok_or_else(|| UpdateError::new(Reason::BadSignature, "malformed signature file"))?;
    let key = keys::verify(&manifest_bytes, &lines, &config.keys).ok_or_else(|| {
        UpdateError::new(
            Reason::BadSignature,
            "the manifest's signature does not verify with any trusted key",
        )
    })?;
    let manifest = Manifest::parse(&manifest_bytes)?;
    Ok((manifest, key.id()))
}

/// An error after the operation was canceled is reported as the cancel
/// (a read that timed out because nothing came back is not news then).
fn or_canceled<T>(r: Result<T, UpdateError>, cancel: &Cancel) -> Result<T, UpdateError> {
    match r {
        Err(_) if cancel.is_canceled() => Err(UpdateError::new(Reason::Canceled, "canceled")),
        other => other,
    }
}

/// Checks the source: is there a strictly newer version for this platform?
pub fn check(config: &Config, cancel: &Cancel) -> Result<CheckOutcome, UpdateError> {
    let (manifest, key_id) = or_canceled(fetch_manifest(config, cancel), cancel)?;
    if !manifest.version.is_newer_than(&config.current) {
        return Ok(CheckOutcome::UpToDate {
            latest: manifest.version,
            no_package: false,
        });
    }
    match manifest.packages.get(&config.platform) {
        None => Ok(CheckOutcome::UpToDate {
            latest: manifest.version,
            no_package: true,
        }),
        Some(p) => Ok(CheckOutcome::Available(Offer {
            version: manifest.version,
            notes: manifest.notes,
            package: p.clone(),
            key_id,
        })),
    }
}

/// Downloads and unpacks the version the user accepted into `staging`
/// (a folder that must not exist yet; its parent is created if needed).
/// The manifest is fetched and verified again, and must still offer
/// `accepted`. `progress` receives (bytes so far, package size). On any
/// failure everything created under `staging` is removed.
pub fn download(
    config: &Config,
    accepted: &Version,
    staging: &Path,
    cancel: &Cancel,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Staged, UpdateError> {
    or_canceled(
        download_inner(config, accepted, staging, cancel, progress),
        cancel,
    )
}

fn download_inner(
    config: &Config,
    accepted: &Version,
    staging: &Path,
    cancel: &Cancel,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Staged, UpdateError> {
    if !accepted.is_newer_than(&config.current) {
        return Err(UpdateError::new(
            Reason::NotNewer,
            format!("{accepted} is not newer than {}", config.current),
        ));
    }
    if !staging.is_absolute() {
        return Err(UpdateError::new(
            Reason::Staging,
            "the staging folder must be an absolute path",
        ));
    }
    let offer = match check(config, cancel)? {
        CheckOutcome::Available(o) if o.version == *accepted => o,
        CheckOutcome::Available(o) => {
            return Err(UpdateError::new(
                Reason::VersionChanged,
                format!("the source now offers {} instead of {accepted}", o.version),
            ));
        }
        CheckOutcome::UpToDate { latest, .. } => {
            return Err(UpdateError::new(
                Reason::VersionChanged,
                format!("the source now offers {latest}, not {accepted}"),
            ));
        }
    };
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            UpdateError::new(Reason::Staging, format!("creating the update folder: {e}"))
        })?;
    }
    fs::create_dir(staging).map_err(|e| {
        UpdateError::new(Reason::Staging, format!("creating the staging folder: {e}"))
    })?;
    let mut guard = RemoveOnDrop(Some(staging.to_path_buf()));

    let fetcher = fetch::Fetcher::new(config)?;
    let zip_path = staging.join(&offer.package.file);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&zip_path)
        .map_err(|e| UpdateError::new(Reason::Staging, format!("creating the download: {e}")))?;
    let (received, digest) = fetcher.download(
        &offer.package.file,
        offer.package.size,
        &mut file,
        cancel,
        progress,
    )?;
    file.sync_all()
        .map_err(|e| UpdateError::new(Reason::Staging, format!("writing the download: {e}")))?;
    drop(file);
    if received != offer.package.size {
        return Err(UpdateError::new(
            Reason::BadPackage,
            format!(
                "the package is {received} bytes; the manifest says {}",
                offer.package.size
            ),
        ));
    }
    if digest != offer.package.sha256 {
        return Err(UpdateError::new(
            Reason::BadPackage,
            "the package's SHA-256 does not match the manifest",
        ));
    }
    let content = staging.join("content");
    zip::extract(&zip_path, &content, config.archive, cancel)?;
    let _ = fs::remove_file(&zip_path);
    guard.0 = None;
    Ok(Staged {
        version: offer.version,
        content,
    })
}

/// SHA-256 and size of a file (for the signing tool).
pub fn hash_file(path: &Path) -> std::io::Result<(u64, [u8; 32])> {
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((total, hasher.finalize().into()))
}

/// Removes a folder (and what is inside) when dropped, unless disarmed by
/// setting it to `None`.
struct RemoveOnDrop(Option<PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = fs::remove_dir_all(p);
        }
    }
}
