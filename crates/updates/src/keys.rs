//! Ed25519 keys and manifest signatures in their text forms
//! (docs/UPDATES.md, "The signature" and "Keys"):
//!
//! ```text
//! nvrview-ed25519-pub <64 hex digits>
//! nvrview-ed25519-secret <64 hex digits>
//! nvrview-ed25519-sig <key id> <128 hex digits>
//! ```
//!
//! A signature covers [`CONTEXT`] followed by the exact manifest bytes.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use std::fmt;

/// Prefixed to the manifest bytes before signing, so a signature made for
/// an update manifest means nothing anywhere else the key might be used.
pub const CONTEXT: &[u8] = b"nvrview-update-manifest-v1\0";

const PUBLIC_TAG: &str = "nvrview-ed25519-pub";
const SECRET_TAG: &str = "nvrview-ed25519-secret";
const SIGNATURE_TAG: &str = "nvrview-ed25519-sig";

/// Largest signature file read.
pub const MAX_SIGNATURE_FILE: u64 = 4 * 1024;
/// Most keys an app may trust at once (rotation needs two; a few spare).
pub const MAX_TRUSTED_KEYS: usize = 16;

/// A public key the user trusts.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicKey {
    key: VerifyingKey,
}

impl PublicKey {
    /// Parses a key as the apps accept it: a `nvrview-ed25519-pub <hex>`
    /// line or a bare 64-digit hex key, possibly inside a file whose other
    /// lines are blank or `#` comments. Weak (small-order) keys are refused.
    pub fn parse(text: &str) -> Option<PublicKey> {
        let mut found = None;
        for line in meaningful_lines(text) {
            if found.is_some() {
                return None; // one key per text
            }
            let hex_part = match line.split_once(char::is_whitespace) {
                Some((tag, rest)) if tag == PUBLIC_TAG => rest.trim(),
                Some(_) => return None,
                None => line,
            };
            let bytes: [u8; 32] = decode_hex(hex_part)?.try_into().ok()?;
            let key = VerifyingKey::from_bytes(&bytes).ok()?;
            if key.is_weak() {
                return None;
            }
            found = Some(PublicKey { key });
        }
        found
    }

    /// The first 8 bytes of the SHA-256 of the key, in hex.
    pub fn id(&self) -> String {
        let digest = Sha256::digest(self.key.as_bytes());
        encode_hex(&digest[..8])
    }

    /// The canonical text: `nvrview-ed25519-pub <hex>`.
    pub fn to_text(&self) -> String {
        format!("{PUBLIC_TAG} {}", encode_hex(self.key.as_bytes()))
    }

    /// Whether `signature` signs `manifest` (strict verification: no
    /// malleable or small-order forms).
    pub fn verifies(&self, manifest: &[u8], signature: &[u8; 64]) -> bool {
        let sig = Signature::from_bytes(signature);
        self.key
            .verify_strict(&signed_message(manifest), &sig)
            .is_ok()
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.id())
    }
}

/// A secret signing key (the 32-byte seed). Only the signing tool and tests
/// hold one; the apps never do.
pub struct SecretKey {
    key: SigningKey,
}

impl SecretKey {
    /// From 32 random bytes.
    pub fn from_seed(seed: [u8; 32]) -> SecretKey {
        SecretKey {
            key: SigningKey::from_bytes(&seed),
        }
    }

    /// Parses `nvrview-ed25519-secret <hex>` (comment lines allowed).
    pub fn parse(text: &str) -> Option<SecretKey> {
        let mut lines = meaningful_lines(text);
        let line = lines.next()?;
        if lines.next().is_some() {
            return None;
        }
        let (tag, rest) = line.split_once(char::is_whitespace)?;
        if tag != SECRET_TAG {
            return None;
        }
        let seed: [u8; 32] = decode_hex(rest.trim())?.try_into().ok()?;
        Some(SecretKey::from_seed(seed))
    }

    /// The text written to the secret key file.
    pub fn to_text(&self) -> String {
        format!("{SECRET_TAG} {}\n", encode_hex(&self.key.to_bytes()))
    }

    pub fn public(&self) -> PublicKey {
        PublicKey {
            key: self.key.verifying_key(),
        }
    }

    /// One signature line for `manifest`: `nvrview-ed25519-sig <id> <hex>`.
    pub fn sign_line(&self, manifest: &[u8]) -> String {
        let sig = self.key.sign(&signed_message(manifest));
        format!(
            "{SIGNATURE_TAG} {} {}",
            self.public().id(),
            encode_hex(&sig.to_bytes())
        )
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey(public {})", self.public().id())
    }
}

/// One line of a signature file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureLine {
    pub key_id: String,
    pub signature: [u8; 64],
}

/// Parses a signature file: one or more `nvrview-ed25519-sig` lines (blank
/// and `#` lines ignored). Any other line makes the whole file invalid.
pub fn parse_signatures(text: &str) -> Option<Vec<SignatureLine>> {
    let mut out = Vec::new();
    for line in meaningful_lines(text) {
        let mut parts = line.split_whitespace();
        if parts.next()? != SIGNATURE_TAG {
            return None;
        }
        let key_id = parts.next()?;
        if key_id.len() != 16 || decode_hex(key_id).is_none() {
            return None;
        }
        let signature: [u8; 64] = decode_hex(parts.next()?)?.try_into().ok()?;
        if parts.next().is_some() {
            return None;
        }
        out.push(SignatureLine {
            key_id: key_id.to_ascii_lowercase(),
            signature,
        });
    }
    if out.is_empty() { None } else { Some(out) }
}

/// The trusted key that verified the manifest, if any. Every line is tried
/// with every key (the key ID is a hint for people, not trusted).
pub fn verify<'a>(
    manifest: &[u8],
    signatures: &[SignatureLine],
    keys: &'a [PublicKey],
) -> Option<&'a PublicKey> {
    keys.iter().find(|k| {
        signatures
            .iter()
            .any(|s| k.verifies(manifest, &s.signature))
    })
}

fn signed_message(manifest: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(CONTEXT.len() + manifest.len());
    m.extend_from_slice(CONTEXT);
    m.extend_from_slice(manifest);
    m
}

fn meaningful_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// Lowercase hex.
pub fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    s
}

/// Hex in either case; None for odd lengths or other characters.
pub fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let b = text.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let digit = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    b.chunks(2)
        .map(|p| Some(digit(p[0])? << 4 | digit(p[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> SecretKey {
        SecretKey::from_seed([n; 32])
    }

    #[test]
    fn round_trips() {
        let k = key(7);
        let p = k.public();
        assert_eq!(PublicKey::parse(&p.to_text()), Some(p.clone()));
        let bare = p.to_text().split_once(' ').unwrap().1.to_uppercase();
        assert_eq!(PublicKey::parse(&bare), Some(p.clone()));
        let file = format!("# update key\n\n  {}  \n", p.to_text());
        assert_eq!(PublicKey::parse(&file), Some(p.clone()));
        assert_eq!(p.id().len(), 16);
        let back = SecretKey::parse(&k.to_text()).unwrap();
        assert_eq!(back.public(), p);
    }

    #[test]
    fn refuses_malformed_keys() {
        let p = key(7).public().to_text();
        let hex = p.split_once(' ').unwrap().1.to_string();
        for bad in [
            String::new(),
            "# only a comment".into(),
            hex[..62].to_string(),
            format!("{hex}00"),
            format!("nvrview-ed25519-secret {hex}"),
            format!("{p}\n{p}"),
            hex.replace('a', "g"),
            // The identity point: a small-order (weak) key.
            format!("01{}", "0".repeat(62)),
        ] {
            assert!(PublicKey::parse(&bad).is_none(), "{bad:?}");
        }
        assert!(SecretKey::parse(&hex).is_none());
        assert!(SecretKey::parse(&p).is_none());
    }

    #[test]
    fn signs_and_verifies() {
        let k = key(1);
        let other = key(2);
        let manifest = b"{\"format\":1}";
        let line = k.sign_line(manifest);
        let sigs = parse_signatures(&line).unwrap();
        assert_eq!(sigs[0].key_id, k.public().id());
        let trusted = [other.public(), k.public()];
        assert_eq!(verify(manifest, &sigs, &trusted), Some(&trusted[1]));
        assert_eq!(verify(manifest, &sigs, &[other.public()]), None);
        assert_eq!(verify(b"{\"format\":2}", &sigs, &trusted), None);
        assert_eq!(verify(manifest, &sigs, &[]), None);
        // Without the context prefix the same bytes do not verify.
        let raw = k.key.sign(manifest).to_bytes();
        let raw_line = SignatureLine {
            key_id: k.public().id(),
            signature: raw,
        };
        assert_eq!(verify(manifest, &[raw_line], &trusted), None);
    }

    #[test]
    fn several_signature_lines() {
        let old = key(1);
        let new = key(2);
        let manifest = b"manifest bytes";
        let file = format!(
            "# rotation\n{}\n\n{}\n",
            old.sign_line(manifest),
            new.sign_line(manifest)
        );
        let sigs = parse_signatures(&file).unwrap();
        assert_eq!(sigs.len(), 2);
        assert!(verify(manifest, &sigs, &[new.public()]).is_some());
        assert!(verify(manifest, &sigs, &[old.public()]).is_some());
        assert!(parse_signatures("").is_none());
        assert!(parse_signatures(&format!("{file}\nsomething else")).is_none());
        assert!(parse_signatures("nvrview-ed25519-sig 0011 00").is_none());
    }
}
