//! Obfuscation, hashing, and encryption primitives.

use aes::Aes128;
use cfb_mode::cipher::KeyIvInit;
use md5::{Digest, Md5};
use std::fmt::{self, Write};

/// Fixed AES IV (`0123456789abcdef` as ASCII), restarted for every unit.
pub const PROTOCOL_IV: [u8; 16] = *b"0123456789abcdef";

const XOR_KEY: [u8; 8] = [0x1f, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78, 0xff];

/// XOR-obfuscates (or de-obfuscates; the operation is its own inverse) `data`
/// in place, as used for the handshake and login bodies.
///
/// Byte `i` is XORed with `key[(message_id + i) mod 8] ^ (message_id mod 256)`.
pub fn xor_obfuscate(message_id: u32, data: &mut [u8]) {
    let salt = (message_id & 0xff) as u8;
    for (i, b) in data.iter_mut().enumerate() {
        let idx = (message_id as usize).wrapping_add(i) % XOR_KEY.len();
        *b ^= XOR_KEY[idx] ^ salt;
    }
}

/// Uppercase hex MD5 of `input`, truncated to 31 characters.
pub fn md5_hex_upper_31(input: &str) -> String {
    let mut s = md5_hex_upper(input);
    s.truncate(31);
    s
}

fn md5_hex_upper(input: &str) -> String {
    let digest = Md5::digest(input.as_bytes());
    let mut s = String::with_capacity(32);
    for b in digest.iter() {
        let _ = write!(s, "{b:02X}");
    }
    s
}

/// Salted hash of a user name or password for the login body:
/// `md5_hex_upper(value + nonce)[..31]`.
pub fn credential_hash(value: &str, nonce: &str) -> String {
    let mut salted = String::with_capacity(value.len() + nonce.len());
    salted.push_str(value);
    salted.push_str(nonce);
    md5_hex_upper_31(&salted)
}

/// AES-128-CFB (128-bit feedback) encryption in place with an explicit key and IV.
pub fn aes128_cfb_encrypt(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
    cfb_mode::Encryptor::<Aes128>::new(key.into(), iv.into()).encrypt(data);
}

/// AES-128-CFB (128-bit feedback) decryption in place with an explicit key and IV.
pub fn aes128_cfb_decrypt(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
    cfb_mode::Decryptor::<Aes128>::new(key.into(), iv.into()).decrypt(data);
}

/// The per-session AES key derived from the nonce and password.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionKey([u8; 16]);

impl SessionKey {
    /// Derives the key: the first 16 characters of
    /// `md5_hex_upper(nonce + "-" + password)`, as ASCII bytes.
    pub fn derive(nonce: &str, password: &str) -> SessionKey {
        let hex = md5_hex_upper(&format!("{nonce}-{password}"));
        let mut key = [0u8; 16];
        key.copy_from_slice(&hex.as_bytes()[..16]);
        SessionKey(key)
    }

    /// The raw key bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Encrypts one unit in place with the protocol IV.
    pub fn encrypt(&self, data: &mut [u8]) {
        aes128_cfb_encrypt(&self.0, &PROTOCOL_IV, data);
    }

    /// Decrypts one unit in place with the protocol IV.
    pub fn decrypt(&self, data: &mut [u8]) {
        aes128_cfb_decrypt(&self.0, &PROTOCOL_IV, data);
    }
}

impl fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionKey(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn xor_hand_computed() {
        let mut z = [0u8; 8];
        xor_obfuscate(0, &mut z);
        assert_eq!(z, XOR_KEY);
        let mut z = [0u8; 8];
        xor_obfuscate(1, &mut z);
        assert_eq!(z, [0x2c, 0x3d, 0x4a, 0x5b, 0x68, 0x79, 0xfe, 0x1e]);
        let mut z = [0u8; 1];
        xor_obfuscate(258, &mut z);
        assert_eq!(z, [0x3e]);
    }

    #[test]
    fn xor_is_involution() {
        let original: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        for id in [0u32, 1, 7, 8, 255, 256, 1_000_003, u32::MAX] {
            let mut data = original.clone();
            xor_obfuscate(id, &mut data);
            assert_ne!(data, original);
            xor_obfuscate(id, &mut data);
            assert_eq!(data, original);
        }
    }

    #[test]
    fn md5_known_values() {
        // MD5("") = d41d8cd98f00b204e9800998ecf8427e
        assert_eq!(md5_hex_upper(""), "D41D8CD98F00B204E9800998ECF8427E");
        assert_eq!(md5_hex_upper_31(""), "D41D8CD98F00B204E9800998ECF8427");
        assert_eq!(credential_hash("a", "b"), md5_hex_upper_31("ab"));
    }

    #[test]
    fn session_key_shape() {
        let key = SessionKey::derive("example-nonce", "example-password");
        let expected = &md5_hex_upper("example-nonce-example-password")[..16];
        assert_eq!(key.as_bytes(), expected.as_bytes());
        assert_eq!(format!("{key:?}"), "SessionKey(<redacted>)");
    }

    // NIST SP 800-38A, F.3.13 / F.3.14: CFB128-AES128 encrypt and decrypt.
    #[test]
    fn cfb128_nist_vector() {
        let key: [u8; 16] = unhex("2b7e151628aed2a6abf7158809cf4f3c")
            .try_into()
            .unwrap();
        let iv: [u8; 16] = unhex("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        let plain = unhex(concat!(
            "6bc1bee22e409f96e93d7e117393172a",
            "ae2d8a571e03ac9c9eb76fac45af8e51",
            "30c81c46a35ce411e5fbc1191a0a52ef",
            "f69f2445df4f9b17ad2b417be66c3710"
        ));
        let cipher = unhex(concat!(
            "3b3fd92eb72dad20333449f8e83cfb4a",
            "c8a64537a0b3a93fcde3cdad9f1ce58b",
            "26751f67a3cbb140b1808cf187a4f4df",
            "c04b05357c5d1c0eeac4c66f9ff7f2e6"
        ));
        let mut buf = plain.clone();
        aes128_cfb_encrypt(&key, &iv, &mut buf);
        assert_eq!(buf, cipher);
        aes128_cfb_decrypt(&key, &iv, &mut buf);
        assert_eq!(buf, plain);
    }

    #[test]
    fn cfb_partial_blocks_round_trip() {
        let key = SessionKey::derive("n", "p");
        for len in [0usize, 1, 15, 16, 17, 33, 1024] {
            let original: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let mut data = original.clone();
            key.encrypt(&mut data);
            if len > 0 {
                assert_ne!(data, original);
            }
            key.decrypt(&mut data);
            assert_eq!(data, original);
        }
    }
}
