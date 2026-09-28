//! Dataset attestation and dataset-registry records.
//!
//! An `attestation` signs a dataset bundle's identity — the message string
//! `optcoil-dataset-attestation/v1|issuer|key_id|dataset_id|csv_sha256|issued_at`
//! binds the issuer's ed25519 signature to the exact measurement bytes
//! (`csv_sha256` already pins `csv_data` transitively through bundle
//! validation). The attestation carries no public key: keys are looked up in a
//! `DatasetRegistry` so a tampered bundle cannot supply its own trust anchor.
//!
//! Registry *entries* are individually countersigned by the registry issuer —
//! the file itself is unsigned because every entry is self-verifying:
//! `optcoil-registry-entry/v1|registry_issuer|dataset_id|bundle_sha256|attestation_sha256|status`.
//! `bundle_sha256` binds the exact attested-bundle file bytes; a reserialized
//! artifact is a different registry identity by design, matching how run
//! records bind raw input bytes.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ModelError;

pub const DATASET_ATTESTATION_SCHEMA: &str = "optcoil-dataset-attestation/v1";
pub const REGISTRY_ENTRY_SCHEMA: &str = "optcoil-registry-entry/v1";
pub const DATASET_REGISTRY_SCHEMA: &str = "optcoil-dataset-registry/v1";
pub const SIGNING_KEY_SCHEMA: &str = "optcoil-signing-key/v1";

fn invalid(message: &str) -> ModelError {
    ModelError::Invalid(message.into())
}

/// Issuer/key identifiers are restricted so they can never break the
/// pipe-delimited signed-message format.
fn validate_identifier(name: &str, value: &str) -> Result<(), ModelError> {
    let ok = !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    if ok {
        Ok(())
    } else {
        Err(invalid(&format!(
            "{name} must be 1-64 chars of [a-z0-9._-] starting with a letter or digit"
        )))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(text: &str) -> Result<Vec<u8>, ModelError> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("invalid hex encoding"));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|e| invalid(&e.to_string())))
        .collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

/// `ed25519:<64 hex chars>` encoding for public keys and signatures.
fn parse_ed25519_bytes<const N: usize>(field: &str, text: &str) -> Result<[u8; N], ModelError> {
    let hex = text
        .strip_prefix("ed25519:")
        .ok_or_else(|| invalid(&format!("{field} requires an ed25519: prefix")))?;
    let bytes = hex_decode(hex)?;
    bytes
        .try_into()
        .map_err(|_| invalid(&format!("{field} must be {N} bytes")))
}

/// The canonical signed message for a dataset attestation. Delimited text —
/// not JSON — so every implementation signs identical bytes.
pub fn attestation_message(
    issuer: &str,
    key_id: &str,
    dataset_id: &str,
    csv_sha256: &str,
    issued_at_unix_ms: u64,
) -> String {
    format!(
        "{DATASET_ATTESTATION_SCHEMA}|{issuer}|{key_id}|{dataset_id}|{csv_sha256}|{issued_at_unix_ms}"
    )
}

/// Canonical signed message for a registry countersignature.
pub fn registry_entry_message(
    registry_issuer: &str,
    dataset_id: &str,
    bundle_sha256: &str,
    attestation_sha256: &str,
    status: &str,
) -> String {
    format!(
        "{REGISTRY_ENTRY_SCHEMA}|{registry_issuer}|{dataset_id}|{bundle_sha256}|{attestation_sha256}|{status}"
    )
}

/// `attestation_sha256` pins a specific attestation inside a registry entry:
/// sha256 over `signed_message|signature` — delimited text, no JSON
/// canonicalization involved.
pub fn attestation_sha256(attestation: &DatasetAttestation) -> String {
    sha256_hex(format!("{}|{}", attestation.signed_message, attestation.signature).as_bytes())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetAttestation {
    pub issuer: String,
    pub key_id: String,
    pub issued_at_unix_ms: u64,
    pub signed_message: String,
    pub signature: String,
}

impl DatasetAttestation {
    pub fn create(
        issuer: &str,
        key_id: &str,
        dataset_id: &str,
        csv_sha256: &str,
        issued_at_unix_ms: u64,
        key: &SigningKey,
    ) -> Result<Self, ModelError> {
        validate_identifier("issuer", issuer)?;
        validate_identifier("key_id", key_id)?;
        let message =
            attestation_message(issuer, key_id, dataset_id, csv_sha256, issued_at_unix_ms);
        let signature = key.sign(message.as_bytes());
        Ok(Self {
            issuer: issuer.to_owned(),
            key_id: key_id.to_owned(),
            issued_at_unix_ms,
            signed_message: message,
            signature: format!("ed25519:{}", hex_encode(&signature.to_bytes())),
        })
    }

    /// Verifies structure, the declared message, and the signature against a
    /// caller-supplied public key. The caller decides which key is trusted —
    /// typically a registry lookup by `(issuer, key_id)`.
    pub fn verify(
        &self,
        dataset_id: &str,
        csv_sha256: &str,
        public_key: &VerifyingKey,
    ) -> Result<(), ModelError> {
        validate_identifier("issuer", &self.issuer)?;
        validate_identifier("key_id", &self.key_id)?;
        let expected = attestation_message(
            &self.issuer,
            &self.key_id,
            dataset_id,
            csv_sha256,
            self.issued_at_unix_ms,
        );
        if self.signed_message != expected {
            return Err(invalid(
                "attestation signed_message does not match its declared fields",
            ));
        }
        let signature_bytes = parse_ed25519_bytes::<64>("signature", &self.signature)?;
        let signature = Signature::from_bytes(&signature_bytes);
        public_key
            .verify(self.signed_message.as_bytes(), &signature)
            .map_err(|_| invalid("attestation signature does not verify"))
    }
}

/// An issuer's ed25519 keypair as stored on disk. `secret_key` must stay out
/// of version control; `public_key` is what registries publish.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SigningKeyFile {
    pub schema: String,
    pub issuer: String,
    pub key_id: String,
    pub public_key: String,
    pub secret_key: String,
}

impl SigningKeyFile {
    pub fn new(issuer: &str, key_id: &str, secret_bytes: [u8; 32]) -> Result<Self, ModelError> {
        validate_identifier("issuer", issuer)?;
        validate_identifier("key_id", key_id)?;
        let signing = SigningKey::from_bytes(&secret_bytes);
        Ok(Self {
            schema: SIGNING_KEY_SCHEMA.to_owned(),
            issuer: issuer.to_owned(),
            key_id: key_id.to_owned(),
            public_key: format!("ed25519:{}", hex_encode(signing.verifying_key().as_bytes())),
            secret_key: format!("ed25519:{}", hex_encode(&secret_bytes)),
        })
    }

    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let file: Self = serde_json::from_str(json)?;
        if file.schema != SIGNING_KEY_SCHEMA {
            return Err(invalid(&format!(
                "unsupported signing key schema: {}",
                file.schema
            )));
        }
        let key = file.signing_key()?;
        if hex_encode(key.verifying_key().as_bytes())
            != file
                .public_key
                .strip_prefix("ed25519:")
                .ok_or_else(|| invalid("public_key requires an ed25519: prefix"))?
        {
            return Err(invalid("signing key file: secret and public key disagree"));
        }
        Ok(file)
    }

    pub fn signing_key(&self) -> Result<SigningKey, ModelError> {
        let bytes = parse_ed25519_bytes::<32>("secret_key", &self.secret_key)?;
        Ok(SigningKey::from_bytes(&bytes))
    }

    pub fn verifying_key(&self) -> Result<VerifyingKey, ModelError> {
        parse_verifying_key(&self.public_key)
    }
}

pub fn parse_verifying_key(text: &str) -> Result<VerifyingKey, ModelError> {
    let bytes = parse_ed25519_bytes::<32>("public key", text)?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| invalid(&format!("invalid public key: {e}")))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryKey {
    pub issuer: String,
    pub key_id: String,
    pub public_key: String,
    /// `current` or `revoked`.
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryEntry {
    pub dataset_id: String,
    /// sha256 over the exact attested-bundle file bytes.
    pub bundle_sha256: String,
    pub attestation_sha256: String,
    /// `current`, `superseded` or `revoked`.
    pub status: String,
    pub note: String,
    pub countersigned_message: String,
    pub countersignature: String,
}

impl RegistryEntry {
    pub fn countersigned(
        registry_issuer: &str,
        dataset_id: &str,
        bundle_sha256: &str,
        attestation_sha256: &str,
        status: &str,
        note: &str,
        key: &SigningKey,
    ) -> Result<Self, ModelError> {
        if !matches!(status, "current" | "superseded" | "revoked") {
            return Err(invalid(
                "registry status must be current, superseded or revoked",
            ));
        }
        let message = registry_entry_message(
            registry_issuer,
            dataset_id,
            bundle_sha256,
            attestation_sha256,
            status,
        );
        let signature = key.sign(message.as_bytes());
        Ok(Self {
            dataset_id: dataset_id.to_owned(),
            bundle_sha256: bundle_sha256.to_owned(),
            attestation_sha256: attestation_sha256.to_owned(),
            status: status.to_owned(),
            note: note.to_owned(),
            countersigned_message: message,
            countersignature: format!("ed25519:{}", hex_encode(&signature.to_bytes())),
        })
    }

    pub fn verify_countersignature(
        &self,
        registry_issuer: &str,
        registry_key: &VerifyingKey,
    ) -> Result<(), ModelError> {
        let expected = registry_entry_message(
            registry_issuer,
            &self.dataset_id,
            &self.bundle_sha256,
            &self.attestation_sha256,
            &self.status,
        );
        if self.countersigned_message != expected {
            return Err(invalid(
                "registry countersigned_message does not match its declared fields",
            ));
        }
        let bytes = parse_ed25519_bytes::<64>("countersignature", &self.countersignature)?;
        registry_key
            .verify(
                self.countersigned_message.as_bytes(),
                &Signature::from_bytes(&bytes),
            )
            .map_err(|_| invalid("registry countersignature does not verify"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetRegistry {
    pub schema: String,
    pub registry_issuer: String,
    pub registry_key_id: String,
    pub registry_public_key: String,
    pub issued_at_unix_ms: u64,
    pub keys: Vec<RegistryKey>,
    pub entries: Vec<RegistryEntry>,
}

impl DatasetRegistry {
    pub fn empty(registry_issuer: &str, key_file: &SigningKeyFile, issued_at_unix_ms: u64) -> Self {
        Self {
            schema: DATASET_REGISTRY_SCHEMA.to_owned(),
            registry_issuer: registry_issuer.to_owned(),
            registry_key_id: key_file.key_id.clone(),
            registry_public_key: key_file.public_key.clone(),
            issued_at_unix_ms,
            keys: Vec::new(),
            entries: Vec::new(),
        }
    }

    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let registry: Self = serde_json::from_str(json)?;
        if registry.schema != DATASET_REGISTRY_SCHEMA {
            return Err(invalid(&format!(
                "unsupported dataset registry schema: {}",
                registry.schema
            )));
        }
        registry.verifying_key()?;
        Ok(registry)
    }

    pub fn verifying_key(&self) -> Result<VerifyingKey, ModelError> {
        parse_verifying_key(&self.registry_public_key)
    }

    pub fn find_key(&self, issuer: &str, key_id: &str) -> Option<&RegistryKey> {
        self.keys
            .iter()
            .find(|k| k.issuer == issuer && k.key_id == key_id)
    }

    pub fn find_entry(&self, dataset_id: &str) -> Option<&RegistryEntry> {
        self.entries.iter().find(|e| e.dataset_id == dataset_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> SigningKeyFile {
        SigningKeyFile::new("test-issuer", "k1", [7u8; 32]).unwrap()
    }

    #[test]
    fn attestation_roundtrips() {
        let key = test_key();
        let att = DatasetAttestation::create(
            "test-issuer",
            "k1",
            "dataset-a",
            &"ab".repeat(64),
            1_770_000_000_000,
            &key.signing_key().unwrap(),
        )
        .unwrap();
        assert!(
            att.verify("dataset-a", &"ab".repeat(64), &key.verifying_key().unwrap())
                .is_ok()
        );
        // wrong dataset identity, wrong csv hash, wrong key all fail closed
        assert!(
            att.verify("dataset-b", &"ab".repeat(64), &key.verifying_key().unwrap())
                .is_err()
        );
        assert!(
            att.verify("dataset-a", &"cd".repeat(64), &key.verifying_key().unwrap())
                .is_err()
        );
        let other = SigningKeyFile::new("test-issuer", "k1", [9u8; 32]).unwrap();
        assert!(
            att.verify(
                "dataset-a",
                &"ab".repeat(64),
                &other.verifying_key().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn attestation_rejects_message_substitution() {
        let key = test_key();
        let mut att = DatasetAttestation::create(
            "test-issuer",
            "k1",
            "dataset-a",
            &"ab".repeat(64),
            1,
            &key.signing_key().unwrap(),
        )
        .unwrap();
        att.issued_at_unix_ms = 2; // signature still valid, message no longer matches fields
        assert!(
            att.verify("dataset-a", &"ab".repeat(64), &key.verifying_key().unwrap())
                .is_err()
        );
    }

    #[test]
    fn identifiers_reject_delimiter_injection() {
        let key = test_key();
        assert!(
            DatasetAttestation::create(
                "evil|issuer",
                "k1",
                "d",
                &"ab".repeat(64),
                1,
                &key.signing_key().unwrap(),
            )
            .is_err()
        );
        assert!(
            DatasetAttestation::create(
                "test-issuer",
                "k1|extra",
                "d",
                &"ab".repeat(64),
                1,
                &key.signing_key().unwrap(),
            )
            .is_err()
        );
    }

    #[test]
    fn key_file_detects_secret_public_mismatch() {
        let mut key = test_key();
        key.secret_key = format!("ed25519:{}", "11".repeat(32));
        assert!(SigningKeyFile::from_json(&serde_json::to_string(&key).unwrap()).is_err());
    }

    #[test]
    fn registry_entry_countersigns_and_rejects_tamper() {
        let key = test_key();
        let entry = RegistryEntry::countersigned(
            "test-issuer",
            "dataset-a",
            &"aa".repeat(64),
            &"bb".repeat(64),
            "current",
            "note",
            &key.signing_key().unwrap(),
        )
        .unwrap();
        let vk = key.verifying_key().unwrap();
        assert!(entry.verify_countersignature("test-issuer", &vk).is_ok());
        let mut forged = entry.clone();
        forged.status = "revoked".into();
        assert!(forged.verify_countersignature("test-issuer", &vk).is_err());
        assert!(
            RegistryEntry::countersigned(
                "test-issuer",
                "d",
                "x",
                "y",
                "bogus",
                "",
                &key.signing_key().unwrap(),
            )
            .is_err()
        );
    }
}
