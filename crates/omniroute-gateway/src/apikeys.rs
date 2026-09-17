//! API-key issuance.
//!
//! Mirrors what the JS dashboard stores for an operator key: a plaintext
//! `sk-` + 32 hex secret (the gateway matches it verbatim), an 11-char
//! `key_prefix` for masked display, a sha256 `key_hash` for JS-side lookup,
//! and a UUID v4 row id.

use sha2::{Digest, Sha256};

/// A freshly generated key: secret plus its derived metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedKey {
    /// UUID v4 row id.
    pub id: String,
    /// Full secret, shown once at creation and never stored elsewhere.
    pub secret: String,
    /// First 11 chars of the secret (`"sk-"` + 8 hex), for masked display.
    pub prefix: String,
    /// Hex sha256 of the secret, as the JS server records it.
    pub hash: String,
}

/// Generate a key. Retries are unnecessary — 128 bits of randomness do not
/// collide — so a failure here is a hard RNG error.
pub fn generate() -> Result<GeneratedKey, getrandom::Error> {
    let mut secret_bytes = [0u8; 16];
    getrandom::fill(&mut secret_bytes)?;
    let secret = format!("sk-{}", hex::encode(secret_bytes));

    let mut id_bytes = [0u8; 16];
    getrandom::fill(&mut id_bytes)?;
    // UUID v4: version and variant bits.
    id_bytes[6] = (id_bytes[6] & 0x0f) | 0x40;
    id_bytes[8] = (id_bytes[8] & 0x3f) | 0x80;
    let id = format!(
        "{}-{}-{}-{}-{}",
        hex::encode(&id_bytes[..4]),
        hex::encode(&id_bytes[4..6]),
        hex::encode(&id_bytes[6..8]),
        hex::encode(&id_bytes[8..10]),
        hex::encode(&id_bytes[10..16]),
    );

    let hash = hex::encode(Sha256::digest(secret.as_bytes()));
    let prefix = secret[..11].to_string();
    Ok(GeneratedKey {
        id,
        secret,
        prefix,
        hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_keys_match_js_shape() {
        let key = generate().unwrap();
        assert_eq!(key.secret.len(), 35);
        assert!(key.secret.starts_with("sk-"));
        assert_eq!(key.prefix.len(), 11);
        assert_eq!(&key.secret[..11], &key.prefix);
        assert_eq!(key.hash.len(), 64);
        assert_eq!(key.hash, hex::encode(Sha256::digest(key.secret.as_bytes())));
        // UUID v4 shape: 8-4-4-4-12 with version nibble 4.
        let parts: Vec<&str> = key.id.split('-').collect();
        assert_eq!(
            parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
    }

    #[test]
    fn generated_keys_do_not_repeat() {
        let mut secrets = HashSet::new();
        let mut ids = HashSet::new();
        for _ in 0..100 {
            let key = generate().unwrap();
            assert!(secrets.insert(key.secret));
            assert!(ids.insert(key.id));
        }
    }
}
