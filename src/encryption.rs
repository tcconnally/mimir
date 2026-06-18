use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::RngCore;

/// Manages AES-256-GCM encryption for entity body_json.
pub struct EncryptionManager {
    cipher: Aes256Gcm,
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

    /// Encrypt plaintext and return base64-encoded ciphertext (nonce prepended).
    pub fn encrypt(&self, plaintext: &str) -> Result<String, String> {
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| format!("Encryption failed: {}", e))?;

        // Prepend nonce for decryption
        let mut combined = nonce_bytes.to_vec();
        combined.extend(&ciphertext);
        Ok(B64.encode(&combined))
    }

    /// Decrypt a base64-encoded ciphertext (nonce prepended) back to plaintext.
    pub fn decrypt(&self, encoded: &str) -> Result<String, String> {
        let combined = B64
            .decode(encoded)
            .map_err(|e| format!("Invalid base64 ciphertext: {}", e))?;

        if combined.len() < 12 {
            return Err("Ciphertext too short".to_string());
        }

        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| format!("Decryption failed: incorrect key or corrupted data ({})", e))?;

        String::from_utf8(plaintext)
            .map_err(|e| format!("Decrypted data is not valid UTF-8: {}", e))
    }
}
