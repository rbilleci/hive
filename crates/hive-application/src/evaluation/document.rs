//! The local prompt-case contract and every definition digest computed on the server.
//! `serde_json::Map` is a `BTreeMap` in this workspace (the `preserve_order` feature stays off),
//! so it already serializes object keys in sorted order; only the four named arrays
//! `canonical` lists need an explicit sort by value.

use crate::configuration::canonical;
use serde_json::{Map, Value};
use std::collections::HashSet;

pub const SCHEMA_VERSION: &str = "hive.evaluation-definition/v1";
pub const LOCAL_RUNNER: &str = "LOCAL_PROMPT_CASE_V1";
pub const SUPPORTED_METRICS: &[&str] = &["EXACT_MATCH_RATE"];

const MAX_DOCUMENT_LENGTH: usize = 250_000;
const MAX_CASES: usize = 100;
const MAX_TEXT_LENGTH: usize = 20_000;

const ROOT_FIELDS: &[&str] = &[
    "schemaVersion",
    "cases",
    "metrics",
    "requiredArtifacts",
    "compatibleTargetKinds",
    "compatibleLogicalEnvironmentClasses",
    "localRunner",
];
const CASE_FIELDS: &[&str] = &["key", "prompt", "expectedOutput", "fixture"];
const FIXTURE_FIELDS: &[&str] = &["output", "targetFailureCode"];
const METRIC_FIELDS: &[&str] = &["code", "threshold"];
const RUNNER_FIELDS: &[&str] = &["adapter", "failureFixture"];

/// One validation result, in the same shape as `AgentDraftDiagnostic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub path: Vec<String>,
}

fn error(code: &str, message: &str, path: &[String]) -> EvaluationDiagnostic {
    EvaluationDiagnostic {
        code: code.to_string(),
        severity: "ERROR".to_string(),
        message: message.to_string(),
        path: path.to_vec(),
    }
}

fn append(path: &[String], names: &[&str]) -> Vec<String> {
    let mut result = path.to_vec();
    result.extend(names.iter().map(|value| value.to_string()));
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseDefinition {
    pub ordinal: i32,
    pub key: String,
    pub prompt: String,
    pub expected_output: String,
    pub fixture_output: Option<String>,
    pub target_failure_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetricDefinition {
    pub code: String,
    pub threshold: f64,
}

/// `None` for a document over 250,000 characters, text that fails to parse as
/// JSON, or a parsed value that is not a JSON object.
pub fn canonicalize(source: &str) -> Option<String> {
    if source.len() > MAX_DOCUMENT_LENGTH {
        return None;
    }
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    let parsed: Value = serde_json::from_str(&normalized).ok()?;
    if !parsed.is_object() {
        return None;
    }
    Some(
        serde_json::to_string(&canonical(parsed, &[]))
            .expect("a parsed JSON value always re-serializes"),
    )
}

pub fn digest(canonical_document: &str) -> String {
    canonical::digest(canonical_document)
}

pub fn default_document() -> String {
    let document = serde_json::json!({
        "schemaVersion": SCHEMA_VERSION,
        "cases": [{
            "key": "smoke",
            "prompt": "Return the word ready.",
            "expectedOutput": "ready",
            "fixture": {"output": "ready"},
        }],
        "metrics": [{"code": "EXACT_MATCH_RATE", "threshold": 1}],
        "requiredArtifacts": ["LOCAL_SUMMARY"],
        "compatibleTargetKinds": ["AGENT_VERSION", "DEPLOYMENT"],
        "compatibleLogicalEnvironmentClasses": ["DEVELOPMENT", "STAGING", "PRODUCTION"],
        "localRunner": {"adapter": LOCAL_RUNNER},
    });
    serde_json::to_string(&canonical(document, &[]))
        .expect("the default document always re-serializes")
}

pub fn validate(source: &str) -> Vec<EvaluationDiagnostic> {
    let Some(canonical_document) = canonicalize(source) else {
        return vec![error(
            "DOCUMENT_PARSE_FAILED",
            "The definition must be a JSON object within the document bound.",
            &[],
        )];
    };
    let Ok(root) = serde_json::from_str::<Value>(&canonical_document) else {
        return vec![error(
            "DOCUMENT_PARSE_FAILED",
            "The definition cannot be read after canonicalization.",
            &[],
        )];
    };
    let mut diagnostics = Vec::new();
    unknown_fields(&root, ROOT_FIELDS, &mut diagnostics, &[]);
    if root.get("schemaVersion").and_then(Value::as_str) != Some(SCHEMA_VERSION) {
        diagnostics.push(error(
            "SCHEMA_VERSION_INVALID",
            "schemaVersion must identify the supported local evaluation document.",
            &["schemaVersion".to_string()],
        ));
    }
    validate_cases(root.get("cases").unwrap_or(&Value::Null), &mut diagnostics);
    validate_metrics(
        root.get("metrics").unwrap_or(&Value::Null),
        &mut diagnostics,
    );
    validate_set(
        root.get("requiredArtifacts").unwrap_or(&Value::Null),
        "requiredArtifacts",
        &["LOCAL_SUMMARY"],
        &mut diagnostics,
        true,
    );
    validate_set(
        root.get("compatibleTargetKinds").unwrap_or(&Value::Null),
        "compatibleTargetKinds",
        &["AGENT_VERSION", "DEPLOYMENT"],
        &mut diagnostics,
        true,
    );
    validate_set(
        root.get("compatibleLogicalEnvironmentClasses")
            .unwrap_or(&Value::Null),
        "compatibleLogicalEnvironmentClasses",
        &["DEVELOPMENT", "STAGING", "PRODUCTION"],
        &mut diagnostics,
        true,
    );
    validate_runner(
        root.get("localRunner").unwrap_or(&Value::Null),
        &mut diagnostics,
    );
    reject_sensitive_fields(&root, &mut diagnostics, &[]);
    diagnostics
}

pub fn cases(canonical_document: &str) -> Vec<CaseDefinition> {
    let Ok(root) = serde_json::from_str::<Value>(canonical_document) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    let mut ordinal = 1;
    if let Some(entries) = root.get("cases").and_then(Value::as_array) {
        for value in entries {
            let fixture = value.get("fixture").cloned().unwrap_or(Value::Null);
            values.push(CaseDefinition {
                ordinal,
                key: text(value, "key"),
                prompt: text(value, "prompt"),
                expected_output: raw_text(value, "expectedOutput"),
                fixture_output: fixture.get("output").map(|_| raw_text(&fixture, "output")),
                target_failure_code: fixture
                    .get("targetFailureCode")
                    .map(|_| text(&fixture, "targetFailureCode")),
            });
            ordinal += 1;
        }
    }
    values
}

pub fn metrics(canonical_document: &str) -> Vec<MetricDefinition> {
    let Ok(root) = serde_json::from_str::<Value>(canonical_document) else {
        return Vec::new();
    };
    root.get("metrics")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|value| MetricDefinition {
                    code: text(value, "code"),
                    threshold: value
                        .get("threshold")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn target_kinds(canonical_document: &str) -> HashSet<String> {
    text_set(canonical_document, "compatibleTargetKinds")
}

pub fn environment_classes(canonical_document: &str) -> HashSet<String> {
    text_set(canonical_document, "compatibleLogicalEnvironmentClasses")
}

pub fn runner_failure_fixture(canonical_document: &str) -> bool {
    serde_json::from_str::<Value>(canonical_document)
        .ok()
        .map(|root| {
            text(
                root.get("localRunner").unwrap_or(&Value::Null),
                "failureFixture",
            )
        })
        .as_deref()
        == Some("RUNNER_FAILURE")
}

fn validate_cases(cases: &Value, diagnostics: &mut Vec<EvaluationDiagnostic>) {
    let Some(entries) = cases
        .as_array()
        .filter(|entries| !entries.is_empty() && entries.len() <= MAX_CASES)
    else {
        diagnostics.push(error(
            "CASES_INVALID",
            "cases must contain the permitted local execution cases.",
            &["cases".to_string()],
        ));
        return;
    };
    let mut keys = HashSet::new();
    for (index, value) in entries.iter().enumerate() {
        let path = vec!["cases".to_string(), index.to_string()];
        if !value.is_object() {
            diagnostics.push(error("CASE_INVALID", "Each case must be an object.", &path));
            continue;
        }
        unknown_fields(value, CASE_FIELDS, diagnostics, &path);
        let key = text(value, "key");
        if !valid_case_key(&key) || !keys.insert(key) {
            diagnostics.push(error(
                "CASE_KEY_INVALID",
                "Each case key must be unique and stable.",
                &append(&path, &["key"]),
            ));
        }
        required_text(value, "prompt", diagnostics, &path);
        required_text(value, "expectedOutput", diagnostics, &path);
        let fixture = value.get("fixture").cloned().unwrap_or(Value::Null);
        if !fixture.is_object() {
            diagnostics.push(error(
                "FIXTURE_INVALID",
                "Each case needs one local fixture outcome.",
                &append(&path, &["fixture"]),
            ));
            continue;
        }
        unknown_fields(
            &fixture,
            FIXTURE_FIELDS,
            diagnostics,
            &append(&path, &["fixture"]),
        );
        let output = fixture.get("output").is_some_and(Value::is_string);
        let failure = fixture
            .get("targetFailureCode")
            .and_then(Value::as_str)
            .is_some_and(valid_failure_code);
        if output == failure {
            diagnostics.push(error(
                "FIXTURE_OUTCOME_INVALID",
                "A fixture contains exactly one output or target-failure code.",
                &append(&path, &["fixture"]),
            ));
        }
        if output
            && fixture
                .get("output")
                .and_then(Value::as_str)
                .is_some_and(|value| value.len() > MAX_TEXT_LENGTH)
        {
            diagnostics.push(error(
                "FIXTURE_OUTPUT_TOO_LARGE",
                "Fixture output exceeds the local document bound.",
                &append(&path, &["fixture", "output"]),
            ));
        }
    }
}

fn valid_case_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && key.len() <= 120
        && chars.all(|value| value.is_ascii_alphanumeric() || value == '_' || value == '-')
}

fn valid_failure_code(code: &str) -> bool {
    let mut chars = code.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && (3..=81).contains(&code.len())
        && chars.all(|value| value.is_ascii_uppercase() || value.is_ascii_digit() || value == '_')
}

fn validate_metrics(metrics: &Value, diagnostics: &mut Vec<EvaluationDiagnostic>) {
    let Some(entries) = metrics.as_array().filter(|entries| !entries.is_empty()) else {
        diagnostics.push(error(
            "METRICS_INVALID",
            "metrics must contain a supported aggregate metric.",
            &["metrics".to_string()],
        ));
        return;
    };
    let mut codes = HashSet::new();
    for (index, value) in entries.iter().enumerate() {
        let path = vec!["metrics".to_string(), index.to_string()];
        if !value.is_object() {
            diagnostics.push(error(
                "METRIC_INVALID",
                "Each metric must be an object.",
                &path,
            ));
            continue;
        }
        unknown_fields(value, METRIC_FIELDS, diagnostics, &path);
        let code = text(value, "code");
        if !SUPPORTED_METRICS.contains(&code.as_str()) || !codes.insert(code) {
            diagnostics.push(error(
                "METRIC_CODE_INVALID",
                "Metric codes must use the supported local metric set once.",
                &append(&path, &["code"]),
            ));
        }
        let threshold = value.get("threshold").and_then(Value::as_f64);
        if !matches!(threshold, Some(value) if (0.0..=1.0).contains(&value)) {
            diagnostics.push(error(
                "METRIC_THRESHOLD_INVALID",
                "Metric threshold must be in the inclusive unit interval.",
                &append(&path, &["threshold"]),
            ));
        }
    }
}

fn validate_set(
    value: &Value,
    name: &str,
    allowed: &[&str],
    diagnostics: &mut Vec<EvaluationDiagnostic>,
    required: bool,
) {
    let Some(entries) = value
        .as_array()
        .filter(|entries| !required || !entries.is_empty())
    else {
        diagnostics.push(error(
            &format!("{}_INVALID", name.to_uppercase()),
            &format!("{name} must be a nonempty array."),
            &[name.to_string()],
        ));
        return;
    };
    let mut seen = HashSet::new();
    for entry in entries {
        let matches = entry
            .as_str()
            .is_some_and(|value| allowed.contains(&value) && seen.insert(value.to_string()));
        if !matches {
            diagnostics.push(error(
                &format!("{}_INVALID", name.to_uppercase()),
                &format!("{name} contains an unsupported or duplicate value."),
                &[name.to_string()],
            ));
        }
    }
}

fn validate_runner(runner: &Value, diagnostics: &mut Vec<EvaluationDiagnostic>) {
    if !runner.is_object() {
        diagnostics.push(error(
            "LOCAL_RUNNER_INVALID",
            "localRunner must name the local adapter.",
            &["localRunner".to_string()],
        ));
        return;
    }
    unknown_fields(
        runner,
        RUNNER_FIELDS,
        diagnostics,
        &["localRunner".to_string()],
    );
    if text(runner, "adapter") != LOCAL_RUNNER {
        diagnostics.push(error(
            "LOCAL_RUNNER_INVALID",
            "localRunner.adapter must select LOCAL_PROMPT_CASE_V1.",
            &["localRunner".to_string(), "adapter".to_string()],
        ));
    }
    if runner.get("failureFixture").is_some() && text(runner, "failureFixture") != "RUNNER_FAILURE"
    {
        diagnostics.push(error(
            "RUNNER_FIXTURE_INVALID",
            "The only local runner failure fixture is RUNNER_FAILURE.",
            &["localRunner".to_string(), "failureFixture".to_string()],
        ));
    }
}

fn required_text(
    parent: &Value,
    name: &str,
    diagnostics: &mut Vec<EvaluationDiagnostic>,
    path: &[String],
) {
    let value = text(parent, name);
    if value.trim().is_empty() || value.len() > MAX_TEXT_LENGTH {
        diagnostics.push(error(
            &format!("{}_INVALID", name.to_uppercase()),
            &format!("{name} must be nonblank within the local document bound."),
            &append(path, &[name]),
        ));
    }
}

fn unknown_fields(
    object: &Value,
    allowed: &[&str],
    diagnostics: &mut Vec<EvaluationDiagnostic>,
    path: &[String],
) {
    let Some(map) = object.as_object() else {
        return;
    };
    for name in map.keys() {
        if !allowed.contains(&name.as_str()) {
            diagnostics.push(error(
                "UNKNOWN_FIELD",
                "The canonical document does not permit this field.",
                &append(path, &[name]),
            ));
        }
    }
}

fn reject_sensitive_fields(
    value: &Value,
    diagnostics: &mut Vec<EvaluationDiagnostic>,
    path: &[String],
) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                let lower = key.to_lowercase();
                if ["credential", "secret", "token", "storage", "uri", "url"]
                    .iter()
                    .any(|needle| lower.contains(needle))
                {
                    diagnostics.push(error(
                        "SENSITIVE_FIELD_FORBIDDEN",
                        "The local definition stores no credential or storage reference.",
                        &append(path, &[key]),
                    ));
                }
                reject_sensitive_fields(entry, diagnostics, &append(path, &[key]));
            }
        }
        Value::Array(items) => {
            for (index, entry) in items.iter().enumerate() {
                reject_sensitive_fields(entry, diagnostics, &append(path, &[&index.to_string()]));
            }
        }
        _ => {}
    }
}

/// Object keys need no explicit sort, because `serde_json::Map` is a `BTreeMap` here; only these
/// four named arrays are sorted by value.
fn canonical(value: Value, path: &[String]) -> Value {
    match value {
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, entry) in map {
                let mut child_path = path.to_vec();
                child_path.push(key.clone());
                result.insert(key, canonical(entry, &child_path));
            }
            Value::Object(result)
        }
        Value::Array(items) => {
            let sortable = matches!(
                path,
                [only] if only == "requiredArtifacts"
                    || only == "compatibleTargetKinds"
                    || only == "compatibleLogicalEnvironmentClasses"
                    || only == "metrics"
            );
            let mut values = items;
            if sortable {
                values.sort_by_key(sort_key);
            }
            Value::Array(
                values
                    .into_iter()
                    .map(|entry| canonical(entry, path))
                    .collect(),
            )
        }
        other => other,
    }
}

fn sort_key(value: &Value) -> String {
    if value.is_object() {
        text(value, "code")
    } else {
        value.as_str().unwrap_or_default().to_string()
    }
}

fn text_set(document: &str, field: &str) -> HashSet<String> {
    serde_json::from_str::<Value>(document)
        .ok()
        .and_then(|root| root.get(field).and_then(Value::as_array).cloned())
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn text(parent: &Value, name: &str) -> String {
    parent
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

fn raw_text(parent: &Value, name: &str) -> String {
    parent
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_document_validates_clean() {
        let document = default_document();
        let diagnostics = validate(&document);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn canonicalize_rejects_a_non_object_root() {
        assert_eq!(canonicalize("[1, 2, 3]"), None);
        assert_eq!(canonicalize("not json"), None);
    }

    #[test]
    fn canonicalize_sorts_named_arrays_by_value_not_object_key_order() {
        let source = r#"{"schemaVersion":"hive.evaluation-definition/v1","cases":[],"metrics":[{"code":"EXACT_MATCH_RATE","threshold":1}],
            "requiredArtifacts":["LOCAL_SUMMARY"],"compatibleTargetKinds":["DEPLOYMENT","AGENT_VERSION"],
            "compatibleLogicalEnvironmentClasses":["STAGING","DEVELOPMENT"],"localRunner":{"adapter":"LOCAL_PROMPT_CASE_V1"}}"#;
        let canonical = canonicalize(source).unwrap();
        let parsed: Value = serde_json::from_str(&canonical).unwrap();
        assert_eq!(
            parsed["compatibleTargetKinds"],
            serde_json::json!(["AGENT_VERSION", "DEPLOYMENT"])
        );
        assert_eq!(
            parsed["compatibleLogicalEnvironmentClasses"],
            serde_json::json!(["DEVELOPMENT", "STAGING"])
        );
    }

    #[test]
    fn digest_is_deterministic_for_equal_canonical_documents() {
        let document = default_document();
        assert_eq!(digest(&document), digest(&document));
        assert_ne!(digest(&document), digest("{}"));
    }

    #[test]
    fn validate_rejects_an_unknown_root_field() {
        let source = r#"{"schemaVersion":"hive.evaluation-definition/v1","cases":[{"key":"a","prompt":"p","expectedOutput":"e","fixture":{"output":"e"}}],
            "metrics":[{"code":"EXACT_MATCH_RATE","threshold":1}],"requiredArtifacts":["LOCAL_SUMMARY"],
            "compatibleTargetKinds":["AGENT_VERSION"],"compatibleLogicalEnvironmentClasses":["DEVELOPMENT"],
            "localRunner":{"adapter":"LOCAL_PROMPT_CASE_V1"},"unexpectedField":true}"#;
        let diagnostics = validate(source);
        assert!(diagnostics
            .iter()
            .any(|value| value.code == "UNKNOWN_FIELD"));
    }

    #[test]
    fn validate_rejects_a_sensitive_field_by_substring() {
        let source = r#"{"schemaVersion":"hive.evaluation-definition/v1","cases":[{"key":"a","prompt":"p","expectedOutput":"e",
            "fixture":{"output":"e"}}],"metrics":[{"code":"EXACT_MATCH_RATE","threshold":1}],"requiredArtifacts":["LOCAL_SUMMARY"],
            "compatibleTargetKinds":["AGENT_VERSION"],"compatibleLogicalEnvironmentClasses":["DEVELOPMENT"],
            "localRunner":{"adapter":"LOCAL_PROMPT_CASE_V1","apiToken":"secret"}}"#;
        let diagnostics = validate(source);
        assert!(diagnostics
            .iter()
            .any(|value| value.code == "SENSITIVE_FIELD_FORBIDDEN"));
    }

    #[test]
    fn cases_extracts_ordinal_and_fixture_fields() {
        let document = default_document();
        let values = cases(&document);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].ordinal, 1);
        assert_eq!(values[0].key, "smoke");
        assert_eq!(values[0].fixture_output.as_deref(), Some("ready"));
        assert_eq!(values[0].target_failure_code, None);
    }

    #[test]
    fn runner_failure_fixture_detects_the_configured_flag() {
        let with_flag = r#"{"localRunner":{"adapter":"LOCAL_PROMPT_CASE_V1","failureFixture":"RUNNER_FAILURE"}}"#;
        let without_flag = r#"{"localRunner":{"adapter":"LOCAL_PROMPT_CASE_V1"}}"#;
        assert!(runner_failure_fixture(with_flag));
        assert!(!runner_failure_fixture(without_flag));
    }

    #[test]
    fn target_kinds_and_environment_classes_extract_from_the_default_document() {
        let document = default_document();
        let kinds = target_kinds(&document);
        assert!(kinds.contains("AGENT_VERSION"));
        assert!(kinds.contains("DEPLOYMENT"));
        let environments = environment_classes(&document);
        assert!(environments.contains("DEVELOPMENT"));
        assert!(environments.contains("STAGING"));
        assert!(environments.contains("PRODUCTION"));
    }
}
