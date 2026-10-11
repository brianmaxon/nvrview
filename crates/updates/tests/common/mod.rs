//! Test helpers: a zip writer that can also write the malformed archives
//! the unpacker must refuse, a publisher for update folders, and a small
//! HTTP/HTTPS server on loopback. Every key (the update signing key and the
//! TLS certificate authority) is generated when the test runs.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use updates::SecretKey;
use updates::keys::encode_hex;

pub const PLATFORM: &str = "test-os";

/// A fresh random signing key.
pub fn new_key() -> SecretKey {
    let mut seed = [0u8; 32];
    graviola::random::fill(&mut seed).unwrap();
    SecretKey::from_seed(seed)
}

// ---- zip writing -------------------------------------------------------

#[derive(Clone)]
pub struct ZEntry {
    pub name: String,
    pub data: Vec<u8>,
    /// Unix mode with its type bits; `None` writes a DOS-made entry.
    pub mode: Option<u32>,
    pub deflate: bool,
    /// Overrides for malformed archives.
    pub crc: Option<u32>,
    pub size: Option<u32>,
    pub local_name: Option<String>,
    pub flags: u16,
}

impl ZEntry {
    pub fn file(name: &str, data: &[u8]) -> ZEntry {
        ZEntry {
            name: name.into(),
            data: data.into(),
            mode: Some(0o100_644),
            deflate: true,
            crc: None,
            size: None,
            local_name: None,
            flags: 0,
        }
    }

    pub fn exec(name: &str, data: &[u8]) -> ZEntry {
        ZEntry {
            mode: Some(0o100_755),
            ..ZEntry::file(name, data)
        }
    }

    pub fn dir(name: &str) -> ZEntry {
        ZEntry {
            mode: Some(0o040_755),
            deflate: false,
            ..ZEntry::file(name, b"")
        }
    }

    pub fn symlink(name: &str, target: &str) -> ZEntry {
        ZEntry {
            mode: Some(0o120_755),
            deflate: false,
            ..ZEntry::file(name, target.as_bytes())
        }
    }

    pub fn stored(mut self) -> ZEntry {
        self.deflate = false;
        self
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// Writes a zip as common tools do (local headers, central directory, end
/// record), with the overrides each entry asks for.
pub fn zip(entries: &[ZEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let offset = out.len() as u32;
        let packed = if e.deflate {
            miniz_oxide::deflate::compress_to_vec(&e.data, 6)
        } else {
            e.data.clone()
        };
        let method: u16 = if e.deflate { 8 } else { 0 };
        let crc = e.crc.unwrap_or_else(|| crc32(&e.data));
        let size = e.size.unwrap_or(e.data.len() as u32);
        let local_name = e.local_name.as_deref().unwrap_or(&e.name);
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&e.flags.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0x21u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(local_name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(local_name.as_bytes());
        out.extend_from_slice(&packed);

        let (made_by, external) = match e.mode {
            Some(m) => ((3u16 << 8) | 20, m << 16),
            None => (20u16, if e.name.ends_with('/') { 0x10 } else { 0 }),
        };
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&made_by.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&e.flags.to_le_bytes());
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x21u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 8]); // extra, comment, disk, internal
        central.extend_from_slice(&external.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(e.name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// A small app-like package: a folder, an executable, a data file, and
/// (where links work) a link inside the package.
pub fn app_zip() -> Vec<u8> {
    let mut entries = vec![
        ZEntry::dir("App/"),
        ZEntry::dir("App/bin/"),
        ZEntry::exec("App/bin/app", b"#!/bin/sh\necho app\n"),
        ZEntry::file("App/data.txt", &b"data ".repeat(10_000)),
        ZEntry::file("App/stored.bin", b"stored bytes").stored(),
    ];
    if cfg!(unix) {
        entries.push(ZEntry::symlink("App/current", "bin"));
    }
    zip(&entries)
}

// ---- publishing --------------------------------------------------------

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    encode_hex(&sha2::Sha256::digest(data))
}

/// The manifest text for one package on [`PLATFORM`].
pub fn manifest(version: &str, file: &str, package: &[u8]) -> String {
    format!(
        r#"{{"format":1,"version":"{version}","notes":"Test release.","packages":{{"{PLATFORM}":{{"file":"{file}","size":{},"sha256":"{}"}}}}}}"#,
        package.len(),
        sha256_hex(package)
    )
}

/// Writes manifest, signature, and package into `dir`.
pub fn publish(dir: &Path, key: &SecretKey, version: &str, file: &str, package: &[u8]) {
    let m = manifest(version, file, package);
    write_signed(dir, key, &m);
    std::fs::write(dir.join(file), package).unwrap();
}

pub fn write_signed(dir: &Path, key: &SecretKey, manifest: &str) {
    std::fs::write(dir.join("updates.json"), manifest).unwrap();
    std::fs::write(
        dir.join("updates.json.sig"),
        format!("{}\n", key.sign_line(manifest.as_bytes())),
    )
    .unwrap();
}

// ---- HTTP(S) server ----------------------------------------------------

#[derive(Clone)]
pub enum Route {
    /// 200 with Content-Length.
    Ok(Vec<u8>),
    /// 200 without Content-Length (the body ends when the connection closes).
    OkUnsized(Vec<u8>),
    /// 200 announcing a length other than the body's.
    Declared(u64, Vec<u8>),
    /// A redirect with this status and Location.
    Redirect(u16, String),
    Status(u16),
    /// Accepts the request and never answers.
    Hang,
}

#[derive(Clone, Default)]
pub struct Routes(Arc<Mutex<HashMap<String, Route>>>);

impl Routes {
    pub fn set(&self, path: &str, route: Route) {
        self.0.lock().unwrap().insert(path.into(), route);
    }

    fn get(&self, path: &str) -> Option<Route> {
        self.0.lock().unwrap().get(path).cloned()
    }

    /// Serves a published folder's three files under `prefix`.
    pub fn serve_folder(&self, prefix: &str, dir: &Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            self.set(
                &format!("{prefix}{name}"),
                Route::Ok(std::fs::read(entry.path()).unwrap()),
            );
        }
    }
}

pub struct Server {
    pub port: u16,
    pub routes: Routes,
    /// Paths requested, in order.
    pub hits: Arc<Mutex<Vec<String>>>,
}

impl Server {
    /// A plain HTTP server on 127.0.0.1.
    pub fn http() -> Server {
        Server::start(None)
    }

    /// An HTTPS server on 127.0.0.1 with a certificate from `ca`.
    pub fn https(ca: &TestCa) -> Server {
        Server::start(Some(ca.server_config()))
    }

    fn start(tls: Option<Arc<rustls::ServerConfig>>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes = Routes::default();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let (r, h) = (routes.clone(), hits.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (r, h, tls) = (r.clone(), h.clone(), tls.clone());
                thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    match tls {
                        None => serve(stream, &r, &h),
                        Some(cfg) => {
                            let Ok(conn) = rustls::ServerConnection::new(cfg) else {
                                return;
                            };
                            serve(rustls::StreamOwned::new(conn, stream), &r, &h);
                        }
                    }
                });
            }
        });
        Server { port, routes, hits }
    }

    pub fn url(&self, scheme: &str, path: &str) -> String {
        format!("{scheme}://127.0.0.1:{}{path}", self.port)
    }

    pub fn hit_count(&self) -> usize {
        self.hits.lock().unwrap().len()
    }
}

fn serve<S: Read + Write>(stream: S, routes: &Routes, hits: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) | Err(_) => return,
            Ok(_) if header == "\r\n" => break,
            Ok(_) => {}
        }
    }
    hits.lock().unwrap().push(path.clone());
    let mut stream = reader.into_inner();
    let response = |status: &str, headers: &str, body: &[u8]| {
        let mut r = format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").into_bytes();
        r.extend_from_slice(body);
        r
    };
    let bytes = match routes.get(&path) {
        None => response("404 Not Found", "Content-Length: 0\r\n", b""),
        Some(Route::Ok(body)) => response(
            "200 OK",
            &format!("Content-Length: {}\r\n", body.len()),
            &body,
        ),
        Some(Route::OkUnsized(body)) => response("200 OK", "", &body),
        Some(Route::Declared(n, body)) => {
            response("200 OK", &format!("Content-Length: {n}\r\n"), &body)
        }
        Some(Route::Redirect(code, to)) => response(
            &format!("{code} Redirect"),
            &format!("Location: {to}\r\nContent-Length: 0\r\n"),
            b"",
        ),
        Some(Route::Status(code)) => {
            response(&format!("{code} Status"), "Content-Length: 0\r\n", b"")
        }
        Some(Route::Hang) => {
            thread::sleep(Duration::from_secs(20));
            return;
        }
    };
    let _ = stream.write_all(&bytes);
    let _ = stream.flush();
}

/// A connection that is accepted and then reset or left silent, for
/// connection-level failures.
pub fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

pub fn connect_ok(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

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
        let san = seq(&[der(0x82, b"localhost"), der(0x87, &[127, 0, 0, 1])]);
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

    fn server_config(&self) -> Arc<rustls::ServerConfig> {
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
