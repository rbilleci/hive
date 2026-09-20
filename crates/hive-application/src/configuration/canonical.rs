//! Ports `CanonicalConfiguration`: deterministic, locally serialized canonical
//! documents and their SHA-256 digest, used for immutable configuration
//! facts. Hand-rolled string building, not a JSON library — the exact key
//! order and escaping are the contract every stored digest depends on.

use super::identity::TypedReference;

fn quote(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Ports `CanonicalConfiguration.document`. Dependencies are sorted by
/// `TypedReference::value()` string order, not the struct's derived `Ord`
/// (which disagrees with `value()` order across kinds where one kind is a
/// prefix of another, e.g. `model` vs `model-profile`).
pub fn document(
    kind: &str,
    name: &str,
    identity: &str,
    content: &str,
    dependencies: &[TypedReference],
) -> String {
    let mut references: Vec<String> = dependencies.iter().map(TypedReference::value).collect();
    references.sort();
    let joined = references
        .iter()
        .map(|value| format!("\"{}\"", quote(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"content\":\"{}\",\"dependencies\":[{joined}],\"identity\":\"{}\",\"kind\":\"{}\",\"name\":\"{}\"}}", quote(content), quote(identity), quote(kind), quote(name))
}

/// Ports `CanonicalConfiguration.digest`.
pub fn digest(canonical_document: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(canonical_document.as_bytes());
    hex::encode(hasher.finalize())
}

/// Ports `CanonicalConfiguration.sorted`: by `value()` string order, matching
/// `document`'s dependency ordering.
pub fn sorted(mut values: Vec<TypedReference>) -> Vec<TypedReference> {
    values.sort_by_key(TypedReference::value);
    values
}
