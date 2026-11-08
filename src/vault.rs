// Copyright (c) 2026 - Matus Hubinsky

// Licensed under the EUPL-1.2-only
// You may not use this work except in compliance with the Licence.
// You may obtain a copy of the Licence at:

// https://joinup.ec.europa.eu/software/page/eupl

// Unless required by applicable law or agreed to in writing, software
// distributed under the Licence is distributed on an "AS IS" basis,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the Licence for the specific language governing permissions and
// limitations under the Licence.

//! Encrypts saved device passwords with a user-supplied master password:
//! Argon2id derives a 256-bit key, AES-256-GCM seals the secret (the GCM tag
//! makes a wrong master password fail decryption instead of returning garbage).

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::random;

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 16;

/// Environment variable holding the master password until the TUI asks for
/// it interactively.
pub const MASTER_PASSWORD_ENV: &str = "BEHE_VTY_MASTER_PASSWORD";

/// The master password, if provided via the environment.
pub fn master_password() -> Option<String> {
    std::env::var(MASTER_PASSWORD_ENV)
        .ok()
        .filter(|p| !p.is_empty())
}

/// Derives the sealing key from the master password and a per-credential salt.
pub fn derive_key(master: &str, salt: &[u8]) -> Result<[u8; KEY_LEN], String> {
    // OWASP-recommended Argon2id defaults (19 MiB, 2 passes, 1 thread).
    let params = Params::new(19 * 1024, 2, 1, Some(KEY_LEN))
        .map_err(|e| format!("argon2 params: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = [0u8; KEY_LEN];
    argon
        .hash_password_into(master.as_bytes(), salt, &mut key)
        .map_err(|e| format!("key derivation failed: {e}"))?;
    Ok(key)
}

/// A fresh random salt for a credential.
pub fn random_salt() -> Vec<u8> {
    random::<[u8; SALT_LEN]>().to_vec()
}

/// Seals `plaintext`, returning `(nonce, ciphertext)`.
pub fn seal(key: &[u8; KEY_LEN], plaintext: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(*key));

    let nonce_bytes = random::<[u8; NONCE_LEN]>();

    let ciphertext = cipher
        .encrypt(&Nonce::from(nonce_bytes), plaintext.as_bytes())
        .map_err(|_| "encryption failed".to_string())?;

    Ok((nonce_bytes.to_vec(), ciphertext))
}

/// Opens a `(nonce, ciphertext)` pair sealed with the same master password.
pub fn open(key: &[u8; KEY_LEN], nonce: &[u8], ciphertext: &[u8]) -> Result<String, String> {
    if nonce.len() != NONCE_LEN {
        return Err("corrupted credential: bad nonce".to_string());
    }

    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(*key));
    let nonce_bytes: [u8; NONCE_LEN] = nonce
        .try_into()
        .map_err(|_| "corrupted credential: bad nonce".to_string())?;
    let plaintext = cipher
        .decrypt(&Nonce::from(nonce_bytes), ciphertext)
        .map_err(|_| "wrong master password or corrupted credential".to_string())?;

    String::from_utf8(plaintext).map_err(|_| "corrupted credential".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seal_open_roundtrip() {
        let salt = random_salt();
        let key = derive_key("s3cret!", &salt).unwrap();
        let (nonce, ciphertext) = seal(&key, "admin123").unwrap();

        assert_eq!(open(&key, &nonce, &ciphertext).unwrap(), "admin123");
        assert_ne!(&ciphertext[..], b"admin123");
    }

    #[test]
    fn test_wrong_master_password_fails() {
        let salt = random_salt();
        let key = derive_key("correct horse", &salt).unwrap();
        let (nonce, ciphertext) = seal(&key, "admin123").unwrap();

        let wrong = derive_key("battery staple", &salt).unwrap();
        assert!(open(&wrong, &nonce, &ciphertext).is_err());
    }

    #[test]
    fn test_salts_and_nonces_differ_per_call() {
        let key = derive_key("master", &random_salt()).unwrap();
        let (n1, c1) = seal(&key, "same").unwrap();
        let (n2, c2) = seal(&key, "same").unwrap();
        assert_ne!(n1, n2);
        assert_ne!(c1, c2);
    }
}