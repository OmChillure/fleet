//! Field encryption for integration credentials (AES-256-GCM).
//!
//! Stored shape (JSONB):
//! ```json
//! { "_enc": "v1", "nonce": "<hex 12 bytes>", "ct": "<hex ciphertext+tag>" }
//! ```
//! Legacy plaintext blobs (no `_enc`) still decrypt as-is so existing rows keep
//! working until the next reconnect.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const ENC_MARKER: &str = "v1";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// 32-byte AES-256 key for credential envelopes.
#[derive(Clone)]
pub struct CredentialsKey {
    key: [u8; KEY_LEN],
}

impl std::fmt::Debug for CredentialsKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialsKey([REDACTED])")
    }
}

impl CredentialsKey {
    /// Build a key from env material.
    ///
    /// Accepts:
    /// - 64 hex chars (32 raw bytes)
    /// - base64-ish raw: any UTF-8 secret ≥ 32 chars (SHA-256 derived)
    ///
    /// Prefer a long random secret in production (`openssl rand -hex 32`).
    pub fn from_secret(secret: &str) -> anyhow::Result<Self> {
        let secret = secret.trim();
        if secret.len() < 32 {
            anyhow::bail!(
                "INTEGRATIONS_ENCRYPTION_KEY must be at least 32 characters \
                 (prefer 64 hex chars from `openssl rand -hex 32`)"
            );
        }

        let key = if secret.len() == 64 && secret.chars().all(|c| c.is_ascii_hexdigit()) {
            let bytes = hex::decode(secret)
                .map_err(|e| anyhow::anyhow!("invalid hex INTEGRATIONS_ENCRYPTION_KEY: {e}"))?;
            if bytes.len() != KEY_LEN {
                anyhow::bail!("INTEGRATIONS_ENCRYPTION_KEY hex must decode to 32 bytes");
            }
            let mut key = [0u8; KEY_LEN];
            key.copy_from_slice(&bytes);
            key
        } else {
            // Derive a fixed-length key from a passphrase-like secret.
            let digest = Sha256::digest(secret.as_bytes());
            let mut key = [0u8; KEY_LEN];
            key.copy_from_slice(&digest);
            key
        };

        Ok(Self { key })
    }

    /// Encrypt plaintext credentials JSON → envelope JSON for storage.
    pub fn encrypt(&self, plaintext: &Value) -> anyhow::Result<Value> {
        let bytes = serde_json::to_vec(plaintext).map_err(|e| {
            tracing::error!(error = %e, "credentials serialize failed");
            anyhow::anyhow!("credentials serialize: {e}")
        })?;

        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|e| {
            tracing::error!(error = %e, "aes key init failed (encrypt)");
            anyhow::anyhow!("aes key init: {e}")
        })?;

        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from(nonce_bytes);

        let ct = cipher.encrypt(&nonce, bytes.as_ref()).map_err(|_| {
            tracing::error!("credentials encrypt failed");
            anyhow::anyhow!("credentials encrypt failed")
        })?;

        Ok(json!({
            "_enc": ENC_MARKER,
            "nonce": hex::encode(nonce_bytes),
            "ct": hex::encode(ct),
        }))
    }

    /// Decrypt an envelope (or pass through legacy plaintext).
    pub fn decrypt(&self, stored: &Value) -> anyhow::Result<Value> {
        if !is_encrypted_envelope(stored) {
            // Pre-encryption rows: return as-is.
            tracing::debug!("credentials decrypt: legacy plaintext envelope");
            return Ok(stored.clone());
        }

        let nonce_hex = stored
            .get("nonce")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                tracing::error!("encrypted credentials missing nonce");
                anyhow::anyhow!("encrypted credentials missing nonce")
            })?;
        let ct_hex = stored.get("ct").and_then(|v| v.as_str()).ok_or_else(|| {
            tracing::error!("encrypted credentials missing ct");
            anyhow::anyhow!("encrypted credentials missing ct")
        })?;

        let nonce_bytes = hex::decode(nonce_hex).map_err(|e| {
            tracing::error!(error = %e, "credentials nonce hex decode failed");
            anyhow::anyhow!("credentials nonce hex: {e}")
        })?;
        let nonce_arr: [u8; NONCE_LEN] = nonce_bytes.as_slice().try_into().map_err(|_| {
            tracing::error!(
                len = nonce_bytes.len(),
                expected = NONCE_LEN,
                "bad credentials nonce length"
            );
            anyhow::anyhow!("credentials nonce must be {NONCE_LEN} bytes")
        })?;
        let ct = hex::decode(ct_hex).map_err(|e| {
            tracing::error!(error = %e, "credentials ct hex decode failed");
            anyhow::anyhow!("credentials ct hex: {e}")
        })?;

        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|e| {
            tracing::error!(error = %e, "aes key init failed (decrypt)");
            anyhow::anyhow!("aes key init: {e}")
        })?;
        let nonce = Nonce::from(nonce_arr);

        let plain = cipher.decrypt(&nonce, ct.as_ref()).map_err(|_| {
            tracing::error!(
                "credentials decrypt failed (wrong INTEGRATIONS_ENCRYPTION_KEY or corrupt data)"
            );
            anyhow::anyhow!("credentials decrypt failed (wrong key or corrupt data)")
        })?;

        serde_json::from_slice(&plain).map_err(|e| {
            tracing::error!(error = %e, "credentials plaintext is not JSON");
            anyhow::anyhow!("credentials plaintext is not JSON: {e}")
        })
    }
}

fn is_encrypted_envelope(v: &Value) -> bool {
    v.get("_enc")
        .and_then(|m| m.as_str())
        .map(|m| m == ENC_MARKER)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip() {
        let key = CredentialsKey::from_secret("dev-test-encryption-key-32chars!!").unwrap();
        let plain = json!({
            "access_token": "tok_abc",
            "refresh_token": "ref_xyz",
            "token_type": "Bearer",
        });
        let sealed = key.encrypt(&plain).unwrap();
        assert_eq!(sealed["_enc"], "v1");
        assert!(sealed.get("access_token").is_none());
        let opened = key.decrypt(&sealed).unwrap();
        assert_eq!(opened, plain);
    }

    #[test]
    fn legacy_plaintext_passthrough() {
        let key = CredentialsKey::from_secret("dev-test-encryption-key-32chars!!").unwrap();
        let legacy = json!({ "access_token": "old" });
        let out = key.decrypt(&legacy).unwrap();
        assert_eq!(out["access_token"], "old");
    }

    #[test]
    fn wrong_key_fails() {
        let a = CredentialsKey::from_secret("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let b = CredentialsKey::from_secret("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        let secret = "tok_wrong_key_must_not_leak";
        let sealed = a.encrypt(&json!({ "access_token": secret })).unwrap();
        let err = b.decrypt(&sealed).unwrap_err().to_string();
        assert!(
            !err.contains(secret),
            "decrypt error must not leak plaintext: {err}"
        );
    }

    #[test]
    fn garbled_ciphertext_fails() {
        let key = CredentialsKey::from_secret("dev-test-encryption-key-32chars!!").unwrap();
        let secret = "tok_garbled_must_not_leak";
        let mut sealed = key.encrypt(&json!({ "access_token": secret })).unwrap();
        let ct = sealed["ct"].as_str().unwrap().to_string();
        let last = ct.chars().last().unwrap();
        let flipped = format!(
            "{}{}",
            &ct[..ct.len() - 1],
            if last == '0' { '1' } else { '0' }
        );
        sealed["ct"] = json!(flipped);
        let err = key.decrypt(&sealed).unwrap_err().to_string();
        assert!(
            !err.contains(secret),
            "decrypt error must not leak plaintext: {err}"
        );
    }

    #[test]
    fn empty_ciphertext_fails() {
        let key = CredentialsKey::from_secret("dev-test-encryption-key-32chars!!").unwrap();
        let sealed = json!({
            "_enc": "v1",
            "nonce": hex::encode([0u8; NONCE_LEN]),
            "ct": "",
        });
        assert!(key.decrypt(&sealed).is_err());
    }

    #[test]
    fn integrations_encryption_key_from_secret_rejects_short() {
        for secret in ["short", "iek-thirty-one-chars-exactly-31"] {
            let err = CredentialsKey::from_secret(secret)
                .expect_err("INTEGRATIONS_ENCRYPTION_KEY shorter than 32 must fail");
            let msg = format!("{err:#}");
            assert!(!msg.contains(secret), "error must not print the key");
            assert!(
                msg.contains("INTEGRATIONS_ENCRYPTION_KEY") && msg.contains("32"),
                "short INTEGRATIONS_ENCRYPTION_KEY must mention the var and 32, got {msg}"
            );
        }
        assert_eq!("iek-thirty-one-chars-exactly-31".len(), 31);
    }
}
