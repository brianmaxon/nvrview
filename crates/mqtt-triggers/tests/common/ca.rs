//! A certificate authority and server certificate made at run time (no key
//! is ever committed), copied in shape from the update client's tests.

#![allow(dead_code)]

use std::sync::Arc;

// ---- a certificate authority made at run time ---------------------------

use graviola::hashing::Sha256;
use graviola::key_agreement::p256::StaticPrivateKey;
use graviola::signing::ecdsa::{P256, SigningKey};

const OID_EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const OID_SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
const OID_EXT_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25];
const OID_SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else if n < 0x100 {
        out.extend_from_slice(&[0x81, n as u8]);
    } else {
        out.extend_from_slice(&[0x82, (n >> 8) as u8, n as u8]);
    }
    out.extend_from_slice(content);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    der(0x30, &parts.concat())
}

fn oid(o: &[u8]) -> Vec<u8> {
    der(0x06, o)
}

fn name(cn: &str) -> Vec<u8> {
    seq(&[der(
        0x31,
        &seq(&[oid(OID_COMMON_NAME), der(0x0c, cn.as_bytes())]),
    )])
}

fn extension(id: &[u8], critical: bool, value: Vec<u8>) -> Vec<u8> {
    let mut parts = vec![oid(id)];
    if critical {
        parts.push(der(0x01, &[0xff]));
    }
    parts.push(der(0x04, &value));
    seq(&parts)
}

fn new_signing_key() -> SigningKey<P256> {
    loop {
        let mut b = [0u8; 32];
        graviola::random::fill(&mut b).unwrap();
        if let Ok(k) = StaticPrivateKey::from_bytes(&b) {
            return SigningKey { private_key: k };
        }
    }
}

fn spki(key: &SigningKey<P256>) -> Vec<u8> {
    let mut buf = [0u8; 256];
    key.to_spki_der(&mut buf).unwrap().to_vec()
}

fn certificate(
    serial: u8,
    issuer: &str,
    subject: &str,
    subject_key: &SigningKey<P256>,
    signer: &SigningKey<P256>,
    extensions: Vec<Vec<u8>>,
) -> Vec<u8> {
    let alg = seq(&[oid(OID_ECDSA_SHA256)]);
    let tbs = seq(&[
        der(0xa0, &der(0x02, &[2])),
        der(0x02, &[serial]),
        alg.clone(),
        name(issuer),
        seq(&[der(0x17, b"200101000000Z"), der(0x18, b"20991231235959Z")]),
        name(subject),
        spki(subject_key),
        der(0xa3, &seq(&extensions)),
    ]);
    let mut sig = [0u8; 128];
    let sig = signer.sign_asn1::<Sha256>(&[&tbs], &mut sig).unwrap();
    let mut bits = vec![0u8];
    bits.extend_from_slice(sig);
    seq(&[tbs, alg, der(0x03, &bits)])
}

/// A certificate authority and a server certificate for 127.0.0.1 and
/// `localhost`, both made now.
pub struct TestCa {
    pub ca_der: Vec<u8>,
    server_der: Vec<u8>,
    server_key_pkcs8: Vec<u8>,
}

impl TestCa {
    pub fn new() -> TestCa {
        TestCa::with_san(seq(&[der(0x82, b"localhost"), der(0x87, &[127, 0, 0, 1])]))
    }

    /// A server certificate for the single DNS name `name` only, so a
    /// client that dials loopback by address sees a name mismatch.
    pub fn for_other_name(name: &str) -> TestCa {
        TestCa::with_san(seq(&[der(0x82, name.as_bytes())]))
    }

    fn with_san(san: Vec<u8>) -> TestCa {
        let ca_key = new_signing_key();
        let ca_der = certificate(
            1,
            "nvrview test CA",
            "nvrview test CA",
            &ca_key,
            &ca_key,
            vec![
                extension(OID_BASIC_CONSTRAINTS, true, seq(&[der(0x01, &[0xff])])),
                extension(OID_KEY_USAGE, true, der(0x03, &[0x01, 0x06])),
            ],
        );
        let server_key = new_signing_key();
        let server_der = certificate(
            2,
            "nvrview test CA",
            "localhost",
            &server_key,
            &ca_key,
            vec![
                extension(OID_KEY_USAGE, true, der(0x03, &[0x07, 0x80])),
                extension(OID_EXT_KEY_USAGE, false, seq(&[oid(OID_SERVER_AUTH)])),
                extension(OID_SUBJECT_ALT_NAME, false, san),
            ],
        );
        let mut buf = [0u8; 512];
        let server_key_pkcs8 = server_key.to_pkcs8_der(&mut buf).unwrap().to_vec();
        TestCa {
            ca_der,
            server_der,
            server_key_pkcs8,
        }
    }

    pub fn server_config(&self) -> Arc<rustls::ServerConfig> {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls_graviola::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(self.server_der.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.server_key_pkcs8.clone())),
        )
        .unwrap();
        Arc::new(cfg)
    }
}

impl TestCa {
    /// The authority certificate as PEM text.
    pub fn ca_pem(&self) -> String {
        const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for chunk in self.ca_der.chunks(3) {
            let n =
                chunk.iter().fold(0u32, |a, b| (a << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
            for k in 0..4 {
                if k <= chunk.len() {
                    s.push(B64[((n >> (18 - 6 * k)) & 63) as usize] as char);
                } else {
                    s.push('=');
                }
            }
        }
        let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
        for line in s.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(line).unwrap());
            out.push('\n');
        }
        out.push_str("-----END CERTIFICATE-----\n");
        out
    }
}
