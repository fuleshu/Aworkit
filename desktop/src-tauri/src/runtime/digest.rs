//! The one digest rule for durable artifacts.
//!
//! Every hash this runtime pins into durable evidence is RFC 8785 (the JSON
//! Canonicalization Scheme) over the value's JSON, SHA-256, rendered as
//! `sha256:<lowercase hex>` (or bare hex where the value is used as an
//! identifier rather than as evidence).
//!
//! The rule is the *only* implementation: `history`, `pipeline`, `tool_loop`
//! and `approvals` all reach this module, so a stored hash can never depend on
//! which copy of the rule ran. Object keys are sorted by the rule itself, so a
//! digest never depends on struct field declaration order or on whether the
//! `serde_json` in the dependency graph happens to preserve insertion order.
//!
//! Two things deliberately keep their existing bytes and are *not* this rule:
//! `service::command_fingerprint` streams the command's struct-declaration-order
//! JSON into SHA-256, and the bytes it produces are already stored inside
//! frozen Chat contexts (`startCommandHash`), so changing the rule would
//! invalidate every started-but-uncommitted draft. See the P2 report; removing
//! that sensitivity needs a stored-record migration, not a digest change.

use serde::Serialize;
use sha2::{Digest, Sha256};

/// `sha256:<hex>` of the RFC 8785 canonical JSON of `value`.
///
/// The error is the raw serializer message so each caller can keep the
/// sentence it already showed for its own artifact.
pub(crate) fn canonical_hash(value: &impl Serialize) -> Result<String, String> {
    let digest = canonical_digest(value)?;
    Ok(format!("sha256:{digest}"))
}

/// The same rule without the algorithm prefix, for identifiers that are not
/// themselves evidence of an algorithm.
pub(crate) fn canonical_digest(value: &impl Serialize) -> Result<String, String> {
    let bytes = serde_jcs::to_vec(value).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// SHA-256 of raw, non-JSON material (an opaque string or byte run).
pub(crate) fn digest_bytes(material: &[u8]) -> String {
    format!("{:x}", Sha256::digest(material))
}

/// Whether `value` is written in the durable hash shape: `sha256:` plus 64 hex
/// digits. Shape only; comparing bytes is what proves a record.
pub(crate) fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The order of object keys is decided by the rule, never by the producer.
    #[test]
    fn object_key_order_never_changes_the_digest() {
        assert_eq!(
            canonical_hash(&json!({"executable": "python", "env": {"a": "1", "b": "2"}}))
                .unwrap(),
            canonical_hash(&json!({"env": {"b": "2", "a": "1"}, "executable": "python"}))
                .unwrap()
        );
    }

    /// A struct's field declaration order is invisible to the digest.
    #[test]
    fn struct_field_declaration_order_never_changes_the_digest() {
        #[derive(Serialize)]
        struct First {
            zebra: u32,
            alpha: &'static str,
        }
        #[derive(Serialize)]
        struct Second {
            alpha: &'static str,
            zebra: u32,
        }
        assert_eq!(
            canonical_hash(&First {
                zebra: 7,
                alpha: "a"
            })
            .unwrap(),
            canonical_hash(&Second {
                alpha: "a",
                zebra: 7
            })
            .unwrap()
        );
    }

    #[test]
    fn the_hash_carries_its_algorithm_and_matches_the_bare_digest() {
        let value = json!({"a": 1});
        let hash = canonical_hash(&value).unwrap();
        assert!(is_sha256(&hash));
        assert_eq!(hash, format!("sha256:{}", canonical_digest(&value).unwrap()));
        assert!(!is_sha256("sha256:xyz"));
        assert!(!is_sha256(&hash[..70]));
    }
}
