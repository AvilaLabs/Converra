//! Validated intake for a material metadata JSON and its measurement CSV.

use sha2::{Digest, Sha256};

use crate::{ModelError, material::MaterialBundle, material::MaterialDataset};

const MAX_METADATA_BYTES: usize = 1024 * 1024;
const MAX_CSV_BYTES: usize = 32 * 1024 * 1024;

/// Parse an attributed metadata/CSV pair into the same validated bundle
/// representation used by the single-file dataset loader. A missing or
/// null `csv_sha256` is filled from the exact CSV bytes. A supplied hash
/// must match; it is never silently replaced.
pub fn material_bundle_from_pair(
    metadata_json: &str,
    csv_bytes: &[u8],
) -> Result<MaterialBundle, ModelError> {
    if metadata_json.len() > MAX_METADATA_BYTES {
        return Err(ModelError::Invalid(
            "material metadata JSON exceeds 1 MiB".into(),
        ));
    }
    if csv_bytes.len() > MAX_CSV_BYTES {
        return Err(ModelError::Invalid("material CSV exceeds 32 MiB".into()));
    }
    let mut metadata: serde_json::Value = serde_json::from_str(metadata_json)?;
    let object = metadata
        .as_object_mut()
        .ok_or_else(|| ModelError::Invalid("material metadata must be a JSON object".into()))?;
    let actual_sha = format!("{:x}", Sha256::digest(csv_bytes));
    match object.get("csv_sha256").cloned() {
        None | Some(serde_json::Value::Null) => {
            object.insert("csv_sha256".into(), serde_json::Value::String(actual_sha));
        }
        Some(serde_json::Value::String(declared)) if declared.is_empty() => {
            return Err(ModelError::Invalid(
                "material metadata csv_sha256 must be omitted or null when not supplied".into(),
            ));
        }
        Some(serde_json::Value::String(declared)) if declared == actual_sha => {}
        Some(serde_json::Value::String(_)) => {
            return Err(ModelError::Invalid(
                "material CSV SHA-256 does not match its metadata".into(),
            ));
        }
        Some(_) => {
            return Err(ModelError::Invalid(
                "material metadata csv_sha256 must be a string, null, or omitted".into(),
            ));
        }
    }
    let csv_data = String::from_utf8(csv_bytes.to_vec())
        .map_err(|_| ModelError::Invalid("material CSV must be UTF-8".into()))?;
    let normalized_metadata = serde_json::to_string(&metadata)?;
    let dataset = MaterialDataset::from_csv(&normalized_metadata, csv_bytes)?;
    MaterialBundle::from_parts(dataset, csv_data, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::{SUPERPOWER_CSV, SUPERPOWER_METADATA};

    fn metadata_with_hash(value: serde_json::Value) -> serde_json::Value {
        let mut metadata: serde_json::Value = serde_json::from_str(SUPERPOWER_METADATA).unwrap();
        metadata["csv_sha256"] = value;
        metadata
    }

    #[test]
    fn validates_pair_and_computes_missing_hash() {
        let metadata = metadata_with_hash(serde_json::Value::Null);
        let bundle = material_bundle_from_pair(&metadata.to_string(), SUPERPOWER_CSV).unwrap();
        assert_eq!(
            bundle.dataset.metadata.csv_sha256,
            format!("{:x}", Sha256::digest(SUPERPOWER_CSV))
        );
        assert_eq!(bundle.csv_data.as_bytes(), SUPERPOWER_CSV);
        assert!(bundle.attestation.is_none());
        let canonical = bundle.to_json().unwrap();
        let loaded = MaterialBundle::from_json(&canonical).unwrap();
        assert_eq!(loaded.dataset.metadata.id, bundle.dataset.metadata.id);
        assert_eq!(
            loaded.dataset.metadata.authors,
            bundle.dataset.metadata.authors
        );
        assert_eq!(
            loaded.dataset.metadata.source_doi,
            bundle.dataset.metadata.source_doi
        );
        assert_eq!(loaded.csv_data.as_bytes(), SUPERPOWER_CSV);

        let mut omitted: serde_json::Value = serde_json::from_str(SUPERPOWER_METADATA).unwrap();
        omitted.as_object_mut().unwrap().remove("csv_sha256");
        assert!(material_bundle_from_pair(&omitted.to_string(), SUPERPOWER_CSV).is_ok());
    }

    #[test]
    fn rejects_wrong_hash_and_malformed_metadata() {
        let wrong = metadata_with_hash(serde_json::Value::String("0".repeat(64)));
        assert!(material_bundle_from_pair(&wrong.to_string(), SUPERPOWER_CSV).is_err());
        assert!(material_bundle_from_pair("[]", SUPERPOWER_CSV).is_err());
        assert!(material_bundle_from_pair("{", SUPERPOWER_CSV).is_err());
        let empty = metadata_with_hash(serde_json::Value::String(String::new()));
        assert!(material_bundle_from_pair(&empty.to_string(), SUPERPOWER_CSV).is_err());
        let malformed_csv = b"not,a,material,dataset\n";
        let malformed = metadata_with_hash(serde_json::Value::String(format!(
            "{:x}",
            Sha256::digest(malformed_csv)
        )));
        assert!(material_bundle_from_pair(&malformed.to_string(), malformed_csv).is_err());
    }
}
