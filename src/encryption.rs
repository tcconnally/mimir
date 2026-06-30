use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::RngCore;

/// Manages AES-256-GCM encryption for entity body_json.
pub struct EncryptionManager {
    cipher: Aes256Gcm,
}

/// Result of attempting to decrypt a stored `body_json` in a possibly-mixed DB
/// (one where encryption was enabled after some rows were already written plain).
pub enum BodyDecrypt {
    /// Ciphertext that authenticated and decrypted successfully.
    Plaintext(String),
    /// The stored value is not Mimir ciphertext at all (a legacy plaintext row);
    /// it is safe to use as-is. JSON bodies always start with `{`, which is not in
    /// the base64 alphabet, so real plaintext is reliably classified here.
    LegacyPlaintext(String),
    /// The value WAS well-formed ciphertext but failed authentication — wrong key
    /// or tampered / AAD-mismatched data. The raw bytes MUST NOT be returned to the
    /// caller; doing so would silently defeat the AES-256-GCM integrity guarantee.
    AuthFailed(String),
}

impl EncryptionManager {
    /// Load an encryption key from a base64-encoded key file.
    /// Supports `~` expansion for home directory paths.
    pub fn from_key_file(path: &str) -> Result<Self, String> {
        let expanded = if path.starts_with("~/") {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| "/root".to_string());
            path.replacen("~", &home, 1)
        } else {
            path.to_string()
        };

        let key_b64 = std::fs::read_to_string(&expanded)
            .map_err(|e| format!("Cannot read key file {}: {}", expanded, e))?
            .trim()
            .to_string();

        let key_bytes = B64
            .decode(&key_b64)
            .map_err(|e| format!("Invalid base64 key in {}: {}", expanded, e))?;

        if key_bytes.len() != 32 {
            return Err(format!(
                "Invalid key length: expected 32 bytes (256-bit), got {}",
                key_bytes.len()
            ));
        }

        let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(key);

        Ok(Self { cipher })
    }

    /// Generate a new 256-bit key and return it as a base64 string.
    pub fn generate_key() -> String {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        B64.encode(key)
    }

    /// Encrypt plaintext with AAD (additional authenticated data) and return
    /// base64-encoded ciphertext (nonce prepended).
    /// AAD binds the ciphertext to the provided context (e.g. category + key) so
    /// that swapping encrypted payloads between entities is detected on decryption.
    pub fn encrypt(&self, plaintext: &str, aad: &[u8]) -> Result<String, String> {
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let payload = aes_gcm::aead::Payload {
            msg: plaintext.as_bytes(),
            aad: if aad.is_empty() { b"" } else { aad },
        };
        let ciphertext = self
            .cipher
            .encrypt(nonce, payload)
            .map_err(|e| format!("Encryption failed: {}", e))?;

        // Prepend nonce for decryption
        let mut combined = nonce_bytes.to_vec();
        combined.extend(&ciphertext);
        Ok(B64.encode(&combined))
    }

    /// Mixed-DB-aware decrypt for stored bodies. Distinguishes a legacy plaintext
    /// row (not ciphertext at all -> safe to pass through) from authentic-looking
    /// ciphertext that fails GCM authentication (wrong key or tampering -> the raw
    /// value must NOT be used). This is the variant read paths should use: the old
    /// `decrypt(...).unwrap_or(raw)` pattern silently returned ciphertext on an
    /// auth failure, nullifying the AAD tamper-detection guarantee.
    pub fn decrypt_body(&self, encoded: &str, aad: &[u8]) -> BodyDecrypt {
        let combined = match B64.decode(encoded) {
            Ok(c) => c,
            // Not base64 -> cannot be our ciphertext -> legacy plaintext row.
            Err(_) => return BodyDecrypt::LegacyPlaintext(encoded.to_string()),
        };
        // Mimir ciphertext is nonce(12) + GCM tag(16) + body(>=0) = >= 28 bytes.
        // Anything shorter is not our ciphertext.
        if combined.len() < 12 + 16 {
            return BodyDecrypt::LegacyPlaintext(encoded.to_string());
        }
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);
        let payload = aes_gcm::aead::Payload {
            msg: ciphertext,
            aad: if aad.is_empty() { b"" } else { aad },
        };
        match self.cipher.decrypt(nonce, payload) {
            Ok(pt) => match String::from_utf8(pt) {
                Ok(s) => BodyDecrypt::Plaintext(s),
                Err(e) => BodyDecrypt::AuthFailed(format!("decrypted bytes not UTF-8: {}", e)),
            },
            Err(e) => BodyDecrypt::AuthFailed(format!("authentication failed: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr() -> EncryptionManager {
        let key = Key::<Aes256Gcm>::from_slice(&[7u8; 32]);
        EncryptionManager {
            cipher: Aes256Gcm::new(key),
        }
    }

    #[test]
    fn decrypt_body_roundtrip_is_plaintext() {
        let m = mgr();
        let ct = m.encrypt("{\"note\":\"hello\"}", b"cat:key").unwrap();
        match m.decrypt_body(&ct, b"cat:key") {
            BodyDecrypt::Plaintext(s) => assert_eq!(s, "{\"note\":\"hello\"}"),
            _ => panic!("expected Plaintext"),
        }
    }

    #[test]
    fn legacy_plaintext_passes_through() {
        // A real JSON body starts with '{' (not base64) -> classified legacy plaintext.
        let m = mgr();
        match m.decrypt_body("{\"note\":\"legacy unencrypted row\"}", b"cat:key") {
            BodyDecrypt::LegacyPlaintext(s) => assert!(s.contains("legacy")),
            _ => panic!("expected LegacyPlaintext"),
        }
    }

    #[test]
    fn tampered_ciphertext_is_authfailed_not_returned() {
        let m = mgr();
        let ct = m.encrypt("{\"secret\":\"x\"}", b"cat:key").unwrap();
        // Flip a byte in the base64 ciphertext body (after the nonce region).
        let mut bytes = ct.into_bytes();
        let i = bytes.len() - 4;
        bytes[i] = if bytes[i] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();
        match m.decrypt_body(&tampered, b"cat:key") {
            BodyDecrypt::AuthFailed(_) => {}
            BodyDecrypt::Plaintext(_) => panic!("tampered ciphertext authenticated (GCM broken?)"),
            BodyDecrypt::LegacyPlaintext(s) => {
                panic!("tampered ciphertext returned as plaintext: {}", s)
            }
        }
    }

    #[test]
    fn wrong_aad_is_authfailed() {
        let m = mgr();
        let ct = m.encrypt("{\"a\":1}", b"cat:key").unwrap();
        match m.decrypt_body(&ct, b"different:aad") {
            BodyDecrypt::AuthFailed(_) => {}
            _ => panic!("AAD mismatch must fail authentication"),
        }
    }

    #[test]
    fn wrong_key_is_authfailed() {
        let m = mgr();
        let ct = m.encrypt("{\"a\":1}", b"cat:key").unwrap();
        let other = EncryptionManager {
            cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&[9u8; 32])),
        };
        match other.decrypt_body(&ct, b"cat:key") {
            BodyDecrypt::AuthFailed(_) => {}
            _ => panic!("wrong key must fail authentication"),
        }
    }
}
