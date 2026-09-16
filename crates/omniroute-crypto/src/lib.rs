//! Field-level encryption (AES-256-GCM).
//!
//! Ports `src/lib/db/encryption.ts`. Format:
//! `enc:v1:<iv_hex>:<ciphertext_hex>:<auth_tag_hex>` where the key is
//! `scrypt(secret, "omniroute-field-encryption-v1", 32)`. The legacy
//! dynamic-salt derivation (sha256(secret)[..16]) is tried as a fallback so
//! tokens encrypted by older versions still decrypt. Values without the
//! prefix are returned verbatim (passthrough mode).

use aes_gcm::aead::consts::{U12, U16};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::aes::Aes256;
use aes_gcm::{Aes256Gcm, AesGcm, Key, Nonce};
use scrypt::{Params, scrypt};
use sha2::{Digest, Sha256};

/// Field ciphertext prefix.
pub const PREFIX: &str = "enc:v1:";
/// Static salt for the primary key derivation.
pub const STATIC_SALT: &str = "omniroute-field-encryption-v1";
const KEY_LEN: usize = 32;
/// scrypt cost parameters. The JS `scryptSync` defaults are N=16384, r=8, p=1;
/// the derived key must match, so these are pinned rather than tuned.
const SCRYPT_LOG_N: u8 = 14;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;

/// A configured field-encryption key.
#[derive(Clone)]
pub struct FieldCrypto {
    primary: [u8; KEY_LEN],
    legacy: [u8; KEY_LEN],
}

impl std::fmt::Debug for FieldCrypto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FieldCrypto(<redacted>)")
    }
}

impl FieldCrypto {
    /// Derive both key variants from the shared secret.
    pub fn from_secret(secret: &str) -> Option<Self> {
        if secret.trim().is_empty() {
            return None;
        }

        let params = Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P).ok()?;
        let mut primary = [0u8; KEY_LEN];
        scrypt(
            secret.as_bytes(),
            STATIC_SALT.as_bytes(),
            &params,
            &mut primary,
        )
        .ok()?;

        // Legacy dynamic salt: first 16 bytes of sha256(secret).
        let digest = Sha256::digest(secret.as_bytes());
        let legacy_salt = &digest[..16];
        let mut legacy = [0u8; KEY_LEN];
        scrypt(secret.as_bytes(), legacy_salt, &params, &mut legacy).ok()?;

        Some(Self { primary, legacy })
    }

    /// Whether a stored value carries the encryption prefix.
    pub fn looks_encrypted(value: &str) -> bool {
        value.starts_with(PREFIX)
    }
    /// Decrypt a stored value. Non-prefixed values pass through unchanged.
    /// Returns `None` when a prefixed value cannot be authenticated.
    pub fn decrypt(&self, value: &str) -> Option<String> {
        if !Self::looks_encrypted(value) {
            return Some(value.to_string());
        }
        let body = &value[PREFIX.len()..];
        let parts: Vec<&str> = body.split(':').collect();
        if parts.len() != 3 {
            return None;
        }
        let iv = hex::decode(parts[0]).ok()?;
        let ciphertext = hex::decode(parts[1]).ok()?;
        let tag = hex::decode(parts[2]).ok()?;
        if tag.len() != 16 {
            return None;
        }

        for key in [&self.primary, &self.legacy] {
            if let Some(plaintext) = decrypt_with(key, &iv, &ciphertext, &tag) {
                return Some(plaintext);
            }
        }
        None
    }
}

// `from_slice` is deprecated in favour of `TryFrom`, but the slice lengths are
// already validated above, so the infallible path is the clearest here.
#[allow(deprecated)]
fn decrypt_with(key: &[u8; KEY_LEN], iv: &[u8], ciphertext: &[u8], tag: &[u8]) -> Option<String> {
    let mut payload = ciphertext.to_vec();
    payload.extend_from_slice(tag);
    // The JS writer uses a 16-byte IV; GCM with a non-96-bit IV is valid and
    // handled by the crate's GHASH-based derivation. A 12-byte IV (other
    // writers) uses the standard path.
    let plaintext = match iv.len() {
        16 => {
            let cipher = AesGcm::<Aes256, U16>::new(Key::<AesGcm<Aes256, U16>>::from_slice(key));
            let nonce = Nonce::<U16>::from_slice(iv);
            cipher.decrypt(nonce, payload.as_ref()).ok()?
        }
        12 => {
            let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
            let nonce = Nonce::<U12>::from_slice(iv);
            cipher.decrypt(nonce, payload.as_ref()).ok()?
        }
        _ => return None,
    };
    String::from_utf8(plaintext).ok()
}

/// Read the shared secret from the environment or `<data_dir>/.env`,
/// mirroring the JS `ensureSecretLoaded` precedence.
pub fn load_secret(data_dir: Option<&std::path::Path>) -> Option<String> {
    if let Ok(secret) = std::env::var("STORAGE_ENCRYPTION_KEY")
        && !secret.trim().is_empty()
    {
        return Some(secret);
    }
    let dir = data_dir?;
    let content = std::fs::read_to_string(dir.join(".env")).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("STORAGE_ENCRYPTION_KEY=") {
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ground truth generated by the JS `encrypt()` with SECRET and a fixed
    // 16-byte IV (see scripts note in the commit): plaintext "sk-test-PLAINTEXT".
    const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const VECTOR: &str = "enc:v1:00112233445566778899aabbccddeeff:9e6c5a3e28f85441d2a61974970c800299:66db18a9b980d005606496f6f158ef1e";
    const LEGACY_VECTOR: &str = "enc:v1:00112233445566778899aabbccddeeff:f0a87f4a002e9b13c3aecba4493887c288:ea1c1e308f82a748cf91a88852cebabe";
    const PLAINTEXT: &str = "sk-test-PLAINTEXT";

    #[test]
    fn decrypts_js_static_salt_vector() {
        let crypto = FieldCrypto::from_secret(SECRET).unwrap();
        assert_eq!(crypto.decrypt(VECTOR).as_deref(), Some(PLAINTEXT));
    }

    #[test]
    fn decrypts_legacy_dynamic_salt_vector() {
        let crypto = FieldCrypto::from_secret(SECRET).unwrap();
        assert_eq!(crypto.decrypt(LEGACY_VECTOR).as_deref(), Some(PLAINTEXT));
    }

    #[test]
    fn wrong_secret_fails_authentication() {
        let other = FieldCrypto::from_secret("ffffffffffffffffffffffffffffffff").unwrap();
        assert!(other.decrypt(VECTOR).is_none());
    }

    #[test]
    fn plaintext_passes_through() {
        let crypto = FieldCrypto::from_secret(SECRET).unwrap();
        assert_eq!(crypto.decrypt("sk-plain").as_deref(), Some("sk-plain"));
    }

    #[test]
    fn empty_secret_is_rejected() {
        assert!(FieldCrypto::from_secret("").is_none());
        assert!(FieldCrypto::from_secret("   ").is_none());
    }

    #[test]
    fn malformed_prefixed_values_fail() {
        let crypto = FieldCrypto::from_secret(SECRET).unwrap();
        assert!(crypto.decrypt("enc:v1:only-two:parts").is_none());
        assert!(crypto.decrypt("enc:v1:zz:zz:zz").is_none());
    }

    #[test]
    fn detects_prefix() {
        assert!(FieldCrypto::looks_encrypted(VECTOR));
        assert!(!FieldCrypto::looks_encrypted("plain"));
    }
}

#[cfg(test)]
mod loader_tests {
    use super::*;

    #[test]
    fn loads_secret_from_data_dir_env_file() {
        let dir =
            std::env::temp_dir().join(format!("omniroute-crypto-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join(".env"),
            "# comment\nOTHER=1\nSTORAGE_ENCRYPTION_KEY=\"abc123\"\n",
        )
        .unwrap();

        // The process env takes precedence; ensure it is not set for this check.
        let previous = std::env::var("STORAGE_ENCRYPTION_KEY").ok();
        unsafe { std::env::remove_var("STORAGE_ENCRYPTION_KEY") };
        assert_eq!(load_secret(Some(&dir)).as_deref(), Some("abc123"));
        if let Some(previous) = previous {
            unsafe { std::env::set_var("STORAGE_ENCRYPTION_KEY", previous) };
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_secret_returns_none() {
        let previous = std::env::var("STORAGE_ENCRYPTION_KEY").ok();
        unsafe { std::env::remove_var("STORAGE_ENCRYPTION_KEY") };
        assert!(load_secret(None).is_none());
        if let Some(previous) = previous {
            unsafe { std::env::set_var("STORAGE_ENCRYPTION_KEY", previous) };
        }
    }
}
