use cowboy_workflow_core::ObjectKind;
use serde::Serialize;

use crate::Result;

/// Serialize a typed object into the canonical envelope bytes used for hashing.
///
/// The envelope includes object kind and format version so identical payloads of
/// different object kinds do not collide semantically.
pub fn canonical_object_bytes<T: Serialize>(kind: ObjectKind, value: &T) -> Result<Vec<u8>> {
    let envelope = serde_json::json!({
        "kind": kind,
        "version": 1u32,
        "payload": value,
    });
    Ok(serde_json::to_vec(&envelope)?)
}

/// Compute the BLAKE3 content hash for a typed object.
pub fn object_hash<T: Serialize>(kind: ObjectKind, value: &T) -> Result<String> {
    let bytes = canonical_object_bytes(kind, value)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_with_kind() {
        let value = serde_json::json!({"x": 1});
        let step = object_hash(ObjectKind::StepRecord, &value).unwrap();
        let turn = object_hash(ObjectKind::TurnRecord, &value).unwrap();
        assert_ne!(step, turn);
    }

    #[test]
    fn hash_is_stable_for_same_value() {
        let value = serde_json::json!({"x": 1});
        let a = object_hash(ObjectKind::StepRecord, &value).unwrap();
        let b = object_hash(ObjectKind::StepRecord, &value).unwrap();
        assert_eq!(a, b);
    }
}
