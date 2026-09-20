//! Ports `AgentDraftCanonicalDocument`: canonical local agent document rules.
//! No client-supplied digest or validation result is ever trusted.

use crate::configuration::TypedReference;
use serde_json::{Map, Value};
use std::collections::HashSet;

pub const SECTIONS: &[&str] = &[
    "general",
    "instructions",
    "harness",
    "model",
    "tools",
    "skills",
    "capabilities",
    "subagents",
    "memory",
    "guardrails",
    "identity",
    "observability",
    "limits",
    "evaluations",
];

const SUPPORTED_LANGUAGES: &[&str] = &[
    "markdown",
    "python",
    "shell",
    "json",
    "javascript",
    "typescript",
    "xml",
];

/// Ports `AgentDraftDiagnostic`: one server-produced validation result. `path`
/// locates the editor section a client should surface it against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDraftDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub path: Vec<String>,
}

fn error(code: &str, message: &str, path: &[&str]) -> AgentDraftDiagnostic {
    AgentDraftDiagnostic {
        code: code.to_string(),
        severity: "ERROR".to_string(),
        message: message.to_string(),
        path: path.iter().map(|value| value.to_string()).collect(),
    }
}

fn warning(code: &str, message: &str, path: &[&str]) -> AgentDraftDiagnostic {
    AgentDraftDiagnostic {
        code: code.to_string(),
        severity: "WARNING".to_string(),
        message: message.to_string(),
        path: path.iter().map(|value| value.to_string()).collect(),
    }
}

/// `None` for a `null`/absent source, a document over 250,000 characters, text
/// that fails to parse as JSON, or a parsed value that is not a JSON object.
pub fn canonicalize(source: Option<&str>) -> Option<String> {
    let source = source?;
    if source.len() > 250_000 {
        return None;
    }
    let parsed: Value = serde_json::from_str(source).ok()?;
    if !parsed.is_object() {
        return None;
    }
    Some(serde_json::to_string(&sorted(parsed)).expect("a parsed JSON value always re-serializes"))
}

pub fn default_document(display_name: &str) -> String {
    let mut root = Map::new();
    root.insert("general".to_string(), serde_json::json!({"displayName": display_name, "description": "Describe this agent draft."}));
    root.insert("instructions".to_string(), serde_json::json!({"source": "Follow the project instructions and respond helpfully.", "language": "markdown"}));
    root.insert(
        "harness".to_string(),
        serde_json::json!({"source": "def run(input):\n    return input\n", "language": "python"}),
    );
    root.insert("model".to_string(), serde_json::json!({"reference": ""}));
    root.insert(
        "tools".to_string(),
        serde_json::json!({"source": "{}\n", "language": "json"}),
    );
    root.insert(
        "skills".to_string(),
        serde_json::json!({"source": "export const skills = [];\n", "language": "javascript"}),
    );
    root.insert("capabilities".to_string(), serde_json::json!({"source": "export type Capability = string;\n", "language": "typescript"}));
    root.insert(
        "subagents".to_string(),
        serde_json::json!({"enabled": false}),
    );
    root.insert(
        "memory".to_string(),
        serde_json::json!({"strategy": "project"}),
    );
    root.insert(
        "guardrails".to_string(),
        serde_json::json!({"source": "# local guardrail checks\n", "language": "shell"}),
    );
    root.insert("identity".to_string(), serde_json::json!({"persona": ""}));
    root.insert(
        "observability".to_string(),
        serde_json::json!({"source": "<observability/>\n", "language": "xml"}),
    );
    root.insert("limits".to_string(), serde_json::json!({"maxTokens": 2048}));
    root.insert(
        "evaluations".to_string(),
        serde_json::json!({"required": false}),
    );
    root.insert("dependencies".to_string(), Value::Array(vec![]));
    serde_json::to_string(&sorted(Value::Object(root)))
        .expect("the default agent document always serializes")
}

pub fn digest(canonical_document: &str) -> String {
    crate::configuration::digest(canonical_document)
}

fn text(root: &Value, section: &str, field: &str) -> String {
    root.get(section)
        .and_then(|value| value.get(field))
        .and_then(|value| value.as_str())
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

pub fn validate(canonical_document: &str) -> Vec<AgentDraftDiagnostic> {
    let root: Value = match serde_json::from_str(canonical_document) {
        Ok(value) => value,
        Err(_) => {
            return vec![error(
                "DOCUMENT_PARSE_FAILED",
                "The saved draft document cannot be read.",
                &["review"],
            )]
        }
    };
    let mut diagnostics = Vec::new();

    if text(&root, "general", "displayName").is_empty() {
        diagnostics.push(error(
            "DISPLAY_NAME_REQUIRED",
            "Display name is required.",
            &["general", "displayName"],
        ));
    }
    if text(&root, "instructions", "source").is_empty()
        && text(&root, "instructions", "system").is_empty()
    {
        diagnostics.push(error(
            "SYSTEM_INSTRUCTIONS_REQUIRED",
            "System instructions are required.",
            &["instructions", "source"],
        ));
    }
    match root
        .get("limits")
        .and_then(|limits| limits.get("maxTokens"))
        .and_then(|value| value.as_f64())
    {
        Some(value) if (value as i64) > 0 => {
            if (value as i64) > 128_000 {
                diagnostics.push(warning(
                    "MAX_TOKENS_HIGH",
                    "Maximum tokens exceed the recommended local limit.",
                    &["limits", "maxTokens"],
                ));
            }
        }
        _ => diagnostics.push(error(
            "MAX_TOKENS_POSITIVE",
            "Maximum tokens must be positive.",
            &["limits", "maxTokens"],
        )),
    }
    if text(&root, "general", "description").is_empty() {
        diagnostics.push(warning(
            "DESCRIPTION_RECOMMENDED",
            "Add a description to help collaborators identify this draft.",
            &["general", "description"],
        ));
    }
    if text(&root, "model", "reference").is_empty() && text(&root, "model", "profile").is_empty() {
        diagnostics.push(warning(
            "MODEL_REFERENCE_RECOMMENDED",
            "Choose an approved exact model reference before publication.",
            &["model", "reference"],
        ));
    }
    for section in [
        "harness",
        "tools",
        "skills",
        "capabilities",
        "guardrails",
        "observability",
    ] {
        let language = text(&root, section, "language");
        if !language.is_empty() && !SUPPORTED_LANGUAGES.contains(&language.as_str()) {
            diagnostics.push(error(
                "UNSUPPORTED_FILE_LANGUAGE",
                "Select one of the supported local editor languages.",
                &[section, "language"],
            ));
        }
    }

    match root.get("dependencies") {
        None => {}
        Some(Value::Array(items)) => {
            let mut seen: Vec<&str> = Vec::new();
            for item in items {
                match item
                    .as_str()
                    .filter(|text| TypedReference::parse(text).is_some())
                {
                    Some(text) => seen.push(text),
                    None => {
                        diagnostics.push(error(
                            "DEPENDENCY_REFERENCE_INVALID",
                            "Dependencies use kind:identity@version references.",
                            &["review", "dependencies"],
                        ));
                        break;
                    }
                }
            }
            let unique: HashSet<&&str> = seen.iter().collect();
            if unique.len() != seen.len() {
                diagnostics.push(error(
                    "DEPENDENCY_DUPLICATE",
                    "Each exact dependency may be selected only once.",
                    &["review", "dependencies"],
                ));
            }
        }
        Some(_) => diagnostics.push(error(
            "DEPENDENCIES_MALFORMED",
            "Dependencies must be an array of exact typed references.",
            &["review", "dependencies"],
        )),
    }

    let model_reference = text(&root, "model", "reference");
    if !model_reference.is_empty() {
        match TypedReference::parse(&model_reference) {
            Some(model) if model.kind == "model" => {
                let bound = dependencies(canonical_document)
                    .iter()
                    .any(|dependency| dependency.value() == model.value());
                if !bound {
                    diagnostics.push(error(
                        "MODEL_REFERENCE_UNBOUND",
                        "The selected model must also be captured as an exact dependency.",
                        &["model", "reference"],
                    ));
                }
            }
            _ => diagnostics.push(error(
                "MODEL_REFERENCE_INVALID",
                "Select an exact approved model reference.",
                &["model", "reference"],
            )),
        }
    }

    diagnostics
}

pub fn dependencies(canonical_document: &str) -> Vec<TypedReference> {
    let Ok(root) = serde_json::from_str::<Value>(canonical_document) else {
        return Vec::new();
    };
    let Some(Value::Array(items)) = root.get("dependencies") else {
        return Vec::new();
    };
    let mut values: Vec<TypedReference> = items
        .iter()
        .filter_map(|value| value.as_str())
        .filter_map(TypedReference::parse)
        .collect();
    values.sort_by_key(|a| a.value());
    values
}

pub fn changed_sections(older_document: &str, newer_document: &str) -> Vec<String> {
    let (Ok(older), Ok(newer)) = (
        serde_json::from_str::<Value>(older_document),
        serde_json::from_str::<Value>(newer_document),
    ) else {
        return vec!["review".to_string()];
    };
    let mut changed = Vec::new();
    for section in SECTIONS {
        if older.get(*section).unwrap_or(&Value::Null)
            != newer.get(*section).unwrap_or(&Value::Null)
        {
            changed.push(section.to_string());
        }
    }
    if older.get("dependencies").unwrap_or(&Value::Null)
        != newer.get("dependencies").unwrap_or(&Value::Null)
    {
        changed.push("review".to_string());
    }
    changed
}

pub fn display_name(canonical_document: &str) -> String {
    match serde_json::from_str::<Value>(canonical_document) {
        Ok(root) => text(&root, "general", "displayName"),
        Err(_) => String::new(),
    }
}

/// `serde_json::Map` is a `BTreeMap` in this workspace (the `preserve_order`
/// feature is not enabled), so it always serializes its entries in key order —
/// inserting in any order below is sufficient to canonicalize object key
/// order. Only `dependencies` gets a value-based sort; every other array
/// preserves the order the client supplied.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, entry) in map {
                let transformed = if key == "dependencies" {
                    match entry {
                        Value::Array(items) => sorted_dependencies(items),
                        other => sorted(other),
                    }
                } else {
                    sorted(entry)
                };
                result.insert(key, transformed);
            }
            Value::Object(result)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

fn sorted_dependencies(mut items: Vec<Value>) -> Value {
    items.sort_by_key(dependency_sort_key);
    Value::Array(items.into_iter().map(sorted).collect())
}

fn dependency_sort_key(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalization_sorts_object_keys_without_reordering_dependencies() {
        let source = r#"{"zebra":1,"alpha":2,"dependencies":["model:z@v2","model:a@v1"]}"#;
        let canonical = canonicalize(Some(source)).unwrap();
        assert_eq!(
            canonical,
            r#"{"alpha":2,"dependencies":["model:a@v1","model:z@v2"],"zebra":1}"#
        );
    }

    #[test]
    fn canonicalize_rejects_a_non_object_root() {
        assert!(canonicalize(Some("[1,2,3]")).is_none());
        assert!(canonicalize(Some("not json")).is_none());
        assert!(canonicalize(None).is_none());
    }

    #[test]
    fn the_default_document_has_no_blocking_errors() {
        let document = default_document("My Agent");
        let diagnostics = validate(&document);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity != "ERROR"),
            "{diagnostics:?}"
        );
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "MODEL_REFERENCE_RECOMMENDED"));
    }

    #[test]
    fn an_unsupported_language_is_a_blocking_error() {
        let document = default_document("My Agent")
            .replace("\"language\":\"python\"", "\"language\":\"cobol\"");
        let diagnostics = validate(&document);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "UNSUPPORTED_FILE_LANGUAGE"
                && diagnostic.path == vec!["harness", "language"]));
    }

    #[test]
    fn changed_sections_are_bounded_to_the_editor_sections_plus_review() {
        let older = default_document("A");
        let newer = canonicalize(Some(
            r#"{"general":{"displayName":"B"},"dependencies":["model:a@v1"]}"#,
        ))
        .unwrap();
        let mut sections = changed_sections(&older, &newer);
        sections.sort();
        let mut expected: Vec<String> =
            SECTIONS.iter().map(|section| section.to_string()).collect();
        expected.push("review".to_string());
        expected.sort();
        assert_eq!(sections, expected);
    }

    #[test]
    fn a_model_reference_must_also_be_a_declared_dependency() {
        let document = canonicalize(Some(r#"{"general":{"displayName":"A"},"model":{"reference":"model:claude@v1"},"dependencies":[]}"#)).unwrap();
        let diagnostics = validate(&document);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "MODEL_REFERENCE_UNBOUND"));
    }
}
