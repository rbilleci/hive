//! Ports `DeploymentCompiler`: pure compilation of tenant-scoped facts into a
//! frozen deployment request. No I/O — every input is already resolved by the
//! repository; this module only computes digests, risk, and canonical plan
//! text.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use uuid::Uuid;

pub const COMPILER_VERSION: &str = "local-m14-v1";

const CANONICAL_SECTIONS: &[&str] = &[
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
    "dependencies",
];
const EVIDENCE_KINDS: &[&str] = &[
    "PLAN_VALIDATED",
    "CHANGE_SUMMARY_READY",
    "EVALUATION_PASSED",
];

pub fn digest(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

/// Binds the compiler-derived P-05 risk to the frozen target facts without storing canonical source text in an adapter.
pub fn risk_verification_digest(risk: &str, binding_digest: &str) -> String {
    digest(&format!("{risk}|{binding_digest}"))
}

pub fn valid_strategy(value: &str) -> bool {
    matches!(value, "REPLACE" | "ROLLING" | "BLUE_GREEN" | "CANARY")
}

#[derive(Debug, Clone, PartialEq)]
pub struct VersionSource {
    pub id: Uuid,
    pub project_id: Uuid,
    pub agent_id: Uuid,
    pub agent_display_name: String,
    pub version_number: i64,
    pub content_digest: String,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub organization_id: Uuid,
    pub canonical_document: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvironmentDefinition {
    pub id: Uuid,
    pub stable_definition_id: String,
    pub version: String,
    pub display_name: String,
    pub logical_environment_class: String,
    pub catalog_release_id: String,
    pub catalog_release_digest: String,
    pub content_digest: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolicySource {
    pub id: Uuid,
    pub revision: i64,
    pub digest: String,
    pub matrix: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveTarget {
    pub target_digest: String,
    pub canonical_document: String,
    pub deployment_id: Uuid,
    pub agent_version_id: Uuid,
    pub agent_version_number: i64,
    pub requested_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PolicyRule {
    pub evidence: Vec<String>,
    pub approvers: i32,
}

/// Frozen browser-safe facts for review. The canonical plan remains an opaque compiler payload.
#[derive(Debug, Clone)]
pub struct CompilerReview {
    pub active_agent_version_number: Option<i64>,
    pub change_summary: String,
    pub requested_dependency_versions: Vec<String>,
    pub added_dependency_versions: Vec<String>,
    pub removed_dependency_versions: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CompiledRequest {
    pub version: VersionSource,
    pub environment: EnvironmentDefinition,
    pub policy: PolicySource,
    pub rule: PolicyRule,
    pub strategy: String,
    pub risk: String,
    pub target_digest: String,
    pub canonical_plan: String,
    pub plan_digest: String,
    pub package_digest: String,
    pub binding_digest: String,
    pub risk_verification_digest: String,
    pub current_target: Option<ActiveTarget>,
    pub review: CompilerReview,
    pub requirement_expires_at: DateTime<Utc>,
    pub evaluation_requirement_expires_at: Option<DateTime<Utc>>,
}

#[derive(Default)]
pub struct DeploymentCompiler;

impl DeploymentCompiler {
    pub fn new() -> Self {
        Self
    }

    pub fn compile(
        &self,
        version: &VersionSource,
        environment: &EnvironmentDefinition,
        policy: &PolicySource,
        current: Option<&ActiveTarget>,
        strategy: &str,
        requested_at: DateTime<Utc>,
    ) -> Option<CompiledRequest> {
        if !valid_strategy(strategy) {
            return None;
        }
        let target_digest = digest(&format!(
            "{}|{}|{}|{}",
            version.content_digest,
            environment.id,
            environment.content_digest,
            environment.catalog_release_digest
        ));
        let risk = frozen_risk(
            &target_digest,
            current.map(|value| value.target_digest.as_str()),
            current.map(|value| value.canonical_document.as_str()),
            &version.canonical_document,
        );
        let rule = rule(
            &policy.matrix,
            &environment.logical_environment_class,
            &risk,
        )?;

        let requested_document = classifier_document(&version.canonical_document);
        let active_document =
            current.and_then(|value| classifier_document(&value.canonical_document));
        let requested_dependencies = dependency_versions(requested_document.as_ref());
        let active_dependencies = dependency_versions(active_document.as_ref());
        let added_dependency_versions =
            dependency_difference(&requested_dependencies, &active_dependencies);
        let removed_dependency_versions =
            dependency_difference(&active_dependencies, &requested_dependencies);
        let change_summary = change_summary(version.version_number, current);

        let mut plan = Map::new();
        plan.insert(
            "agentContentDigest".to_string(),
            Value::String(version.content_digest.clone()),
        );
        plan.insert(
            "agentVersionId".to_string(),
            Value::String(version.id.to_string()),
        );
        plan.insert(
            "catalogReleaseDigest".to_string(),
            Value::String(version.catalog_release_digest.clone()),
        );
        plan.insert(
            "catalogReleaseId".to_string(),
            Value::String(version.catalog_release_id.clone()),
        );
        plan.insert(
            "compilerVersion".to_string(),
            Value::String(COMPILER_VERSION.to_string()),
        );
        plan.insert(
            "environmentDefinitionContentDigest".to_string(),
            Value::String(environment.content_digest.clone()),
        );
        plan.insert(
            "environmentDefinitionId".to_string(),
            Value::String(environment.stable_definition_id.clone()),
        );
        plan.insert(
            "environmentDefinitionVersion".to_string(),
            Value::String(environment.version.clone()),
        );
        plan.insert(
            "environmentDefinitionVersionId".to_string(),
            Value::String(environment.id.to_string()),
        );
        plan.insert(
            "logicalEnvironmentClass".to_string(),
            Value::String(environment.logical_environment_class.clone()),
        );
        plan.insert(
            "activeAgentVersionId".to_string(),
            current
                .map(|value| Value::String(value.agent_version_id.to_string()))
                .unwrap_or(Value::Null),
        );
        plan.insert(
            "activeAgentVersionNumber".to_string(),
            current
                .map(|value| Value::Number(value.agent_version_number.into()))
                .unwrap_or(Value::Null),
        );
        plan.insert(
            "activeTargetDigest".to_string(),
            current
                .map(|value| Value::String(value.target_digest.clone()))
                .unwrap_or(Value::Null),
        );
        plan.insert(
            "changeSummary".to_string(),
            Value::String(change_summary.clone()),
        );
        plan.insert(
            "requestedDependencyVersions".to_string(),
            Value::Array(
                requested_dependencies
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        plan.insert(
            "addedDependencyVersions".to_string(),
            Value::Array(
                added_dependency_versions
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        plan.insert(
            "removedDependencyVersions".to_string(),
            Value::Array(
                removed_dependency_versions
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        plan.insert("strategy".to_string(), Value::String(strategy.to_string()));
        plan.insert(
            "targetDigest".to_string(),
            Value::String(target_digest.clone()),
        );

        let canonical_plan = Value::Object(plan).to_string();
        let plan_digest = digest(&canonical_plan);
        let package_digest = digest(&format!("local-package|{plan_digest}"));
        let binding_digest = digest(&format!(
            "{}|{}|{}|{}|{}",
            version.id, environment.id, target_digest, plan_digest, package_digest
        ));

        // Only evaluation evidence expires. The approval requirement keeps its own independent
        // cycle expiry in the authoritative requirement relation.
        let requirement_expires_at = requested_at + chrono::Duration::hours(24);
        let evaluation_requirement_expires_at = rule
            .evidence
            .iter()
            .any(|value| value == "EVALUATION_PASSED")
            .then_some(requirement_expires_at);

        Some(CompiledRequest {
            version: version.clone(),
            environment: environment.clone(),
            policy: policy.clone(),
            risk_verification_digest: risk_verification_digest(&risk, &binding_digest),
            rule,
            strategy: strategy.to_string(),
            risk,
            target_digest,
            canonical_plan,
            plan_digest,
            package_digest,
            binding_digest,
            current_target: current.cloned(),
            review: CompilerReview {
                active_agent_version_number: current.map(|value| value.agent_version_number),
                change_summary,
                requested_dependency_versions: requested_dependencies,
                added_dependency_versions,
                removed_dependency_versions,
            },
            requirement_expires_at,
            evaluation_requirement_expires_at,
        })
    }
}

/// Recomputes a stored cycle's risk only from its captured request and active-target facts.
pub fn frozen_risk(
    requested_target_digest: &str,
    active_target_digest: Option<&str>,
    active_canonical_document: Option<&str>,
    requested_canonical_document: &str,
) -> String {
    let Some(active_target_digest) = active_target_digest else {
        return if classifier_document(requested_canonical_document).is_none() {
            "HIGH".to_string()
        } else {
            "MEDIUM".to_string()
        };
    };
    if requested_target_digest == active_target_digest {
        return "LOW".to_string();
    }
    let active_canonical_document = active_canonical_document.unwrap_or("");
    if high_risk_change(active_canonical_document, requested_canonical_document) {
        "HIGH".to_string()
    } else {
        "MEDIUM".to_string()
    }
}

fn rule(matrix: &str, environment_class: &str, risk: &str) -> Option<PolicyRule> {
    let root: Value = serde_json::from_str(matrix).ok()?;
    let cell = root
        .get(format!("{environment_class}_{risk}"))?
        .as_object()?;
    let keys: BTreeSet<&str> = cell.keys().map(String::as_str).collect();
    if keys != BTreeSet::from(["requiredEvidence", "requiredApprovers"]) {
        return None;
    }
    let raw_evidence = cell.get("requiredEvidence")?.as_array()?;
    let mut evidence = Vec::with_capacity(raw_evidence.len());
    for value in raw_evidence {
        let value = value.as_str()?;
        if !EVIDENCE_KINDS.contains(&value) {
            return None;
        }
        evidence.push(value.to_string());
    }
    evidence.sort();
    let distinct: BTreeSet<&String> = evidence.iter().collect();
    if distinct.len() != evidence.len() {
        return None;
    }
    let approvers = cell.get("requiredApprovers")?.as_i64()?;
    if !(0..=2).contains(&approvers) {
        return None;
    }
    Some(PolicyRule {
        evidence,
        approvers: approvers as i32,
    })
}

/// Every unknown nested shape is high risk. This predicate accepts the complete M12 local canonical document.
fn classifier_document(document: &str) -> Option<Value> {
    let root: Value = serde_json::from_str(document).ok()?;
    let object = root.as_object()?;
    if object.len() != CANONICAL_SECTIONS.len() {
        return None;
    }
    let names: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let sections: BTreeSet<&str> = CANONICAL_SECTIONS.iter().copied().collect();
    if names != sections || !root.get("dependencies")?.is_array() {
        return None;
    }
    for &section in CANONICAL_SECTIONS {
        if section != "dependencies" && !root.get(section)?.is_object() {
            return None;
        }
    }
    if !text_object(root.get("general")?, &["displayName", "description"]) {
        return None;
    }
    for section in [
        "instructions",
        "harness",
        "tools",
        "skills",
        "capabilities",
        "guardrails",
        "observability",
    ] {
        if !text_object(root.get(section)?, &["source", "language"]) {
            return None;
        }
    }
    if !text_object(root.get("model")?, &["reference"])
        || !text_object(root.get("memory")?, &["strategy"])
        || !text_object(root.get("identity")?, &["persona"])
    {
        return None;
    }
    if !exact_object(root.get("subagents")?, &["enabled"])
        || !root.get("subagents")?.get("enabled")?.is_boolean()
    {
        return None;
    }
    if !exact_object(root.get("limits")?, &["maxTokens"])
        || !root.get("limits")?.get("maxTokens")?.is_number()
    {
        return None;
    }
    if !exact_object(root.get("evaluations")?, &["required"])
        || !root.get("evaluations")?.get("required")?.is_boolean()
    {
        return None;
    }
    for value in root.get("dependencies")?.as_array()? {
        if !value.is_string() {
            return None;
        }
    }
    for value in root.get("limits")?.as_object()?.values() {
        if !value.is_number() {
            return None;
        }
    }
    Some(root)
}

fn exact_object(value: &Value, fields: &[&str]) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.len() != fields.len() {
        return false;
    }
    let names: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = fields.iter().copied().collect();
    names == expected
}

fn text_object(value: &Value, fields: &[&str]) -> bool {
    if !exact_object(value, fields) {
        return false;
    }
    fields
        .iter()
        .all(|field| value.get(field).is_some_and(Value::is_string))
}

fn high_risk_change(active_document: &str, requested_document: &str) -> bool {
    let (Some(active), Some(requested)) = (
        classifier_document(active_document),
        classifier_document(requested_document),
    ) else {
        return true;
    };
    if adds_text_value(&active["dependencies"], &requested["dependencies"]) {
        return true;
    }
    if runtime_limit_increased(&active["limits"], &requested["limits"]) {
        return true;
    }
    if active["model"] != requested["model"] {
        return true;
    }
    if active["tools"] != requested["tools"] || active["capabilities"] != requested["capabilities"]
    {
        return true;
    }
    if active["identity"] != requested["identity"] {
        return true;
    }
    let active_memory = memory_enabled(&active["memory"]);
    let requested_memory = memory_enabled(&requested["memory"]);
    if active_memory != requested_memory && requested_memory {
        return true;
    }
    let active_subagents = active["subagents"]["enabled"].as_bool().unwrap_or(false);
    let requested_subagents = requested["subagents"]["enabled"].as_bool().unwrap_or(false);
    if !active_subagents && requested_subagents {
        return true;
    }
    guardrails_weakened(&active["guardrails"], &requested["guardrails"])
}

fn memory_enabled(memory: &Value) -> bool {
    let strategy = memory["strategy"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_uppercase();
    !strategy.is_empty() && strategy != "NONE" && strategy != "DISABLED"
}

/// M13 has no semantic guardrail analyzer. Textual containment cannot prove that an appended
/// instruction preserves an existing restriction, so every guardrail change selects HIGH.
fn guardrails_weakened(active: &Value, requested: &Value) -> bool {
    active != requested
}

fn adds_text_value(earlier: &Value, later: &Value) -> bool {
    let (Some(earlier), Some(later)) = (earlier.as_array(), later.as_array()) else {
        return true;
    };
    let mut existing = BTreeSet::new();
    for value in earlier {
        let Some(text) = value.as_str() else {
            return true;
        };
        existing.insert(text.to_string());
    }
    for value in later {
        match value.as_str() {
            Some(text) if existing.contains(text) => {}
            _ => return true,
        }
    }
    false
}

fn dependency_versions(document: Option<&Value>) -> Vec<String> {
    let Some(document) = document else {
        return Vec::new();
    };
    let Some(dependencies) = document.get("dependencies").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut result: Vec<String> = dependencies
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect();
    result.sort();
    result
}

fn dependency_difference(first: &[String], second: &[String]) -> Vec<String> {
    let baseline: BTreeSet<&String> = second.iter().collect();
    first
        .iter()
        .filter(|value| !baseline.contains(value))
        .cloned()
        .collect()
}

fn change_summary(requested_version: i64, current: Option<&ActiveTarget>) -> String {
    match current {
        None => "First deployment to this environment.".to_string(),
        Some(current) if current.agent_version_number == requested_version => {
            "Redeployment of the active agent version.".to_string()
        }
        Some(current) => format!(
            "Deploy agent version {requested_version} over active version {}.",
            current.agent_version_number
        ),
    }
}

fn runtime_limit_increased(earlier: &Value, later: &Value) -> bool {
    let Some(later) = later.as_object() else {
        return false;
    };
    for (key, value) in later {
        let Some(before) = earlier.get(key).and_then(Value::as_f64) else {
            return true;
        };
        let Some(after) = value.as_f64() else {
            return true;
        };
        if after > before {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_document() -> Value {
        serde_json::json!({
            "general": {"displayName": "Agent", "description": "d"},
            "instructions": {"source": "s", "language": "l"},
            "harness": {"source": "s", "language": "l"},
            "model": {"reference": "model:a@v1"},
            "tools": {"source": "s", "language": "l"},
            "skills": {"source": "s", "language": "l"},
            "capabilities": {"source": "s", "language": "l"},
            "subagents": {"enabled": false},
            "memory": {"strategy": "NONE"},
            "guardrails": {"source": "s", "language": "l"},
            "identity": {"persona": "p"},
            "observability": {"source": "s", "language": "l"},
            "limits": {"maxTokens": 100},
            "evaluations": {"required": false},
            "dependencies": ["model:a@v1"],
        })
    }

    #[test]
    fn valid_strategy_accepts_the_four_known_strategies() {
        for strategy in ["REPLACE", "ROLLING", "BLUE_GREEN", "CANARY"] {
            assert!(valid_strategy(strategy));
        }
        assert!(!valid_strategy("UNKNOWN"));
    }

    #[test]
    fn classifier_document_accepts_a_complete_canonical_document() {
        let document = valid_document().to_string();
        assert!(classifier_document(&document).is_some());
    }

    #[test]
    fn classifier_document_rejects_a_missing_section() {
        let mut document = valid_document();
        document.as_object_mut().unwrap().remove("guardrails");
        assert!(classifier_document(&document.to_string()).is_none());
    }

    #[test]
    fn classifier_document_rejects_a_non_boolean_subagents_enabled() {
        let mut document = valid_document();
        document["subagents"]["enabled"] = Value::String("true".to_string());
        assert!(classifier_document(&document.to_string()).is_none());
    }

    #[test]
    fn frozen_risk_is_low_for_an_unchanged_target_digest() {
        let document = valid_document().to_string();
        let risk = frozen_risk("digest-a", Some("digest-a"), Some(&document), &document);
        assert_eq!(risk, "LOW");
    }

    #[test]
    fn frozen_risk_is_high_with_no_active_target_and_an_invalid_document() {
        let risk = frozen_risk("digest-a", None, None, "not json");
        assert_eq!(risk, "HIGH");
    }

    #[test]
    fn frozen_risk_is_medium_with_no_active_target_and_a_valid_document() {
        let document = valid_document().to_string();
        let risk = frozen_risk("digest-a", None, None, &document);
        assert_eq!(risk, "MEDIUM");
    }

    #[test]
    fn frozen_risk_is_high_when_a_dependency_is_added() {
        let active = valid_document().to_string();
        let mut requested = valid_document();
        requested["dependencies"] = serde_json::json!(["model:a@v1", "model:b@v1"]);
        let risk = frozen_risk(
            "digest-a",
            Some("digest-b"),
            Some(&active),
            &requested.to_string(),
        );
        assert_eq!(risk, "HIGH");
    }

    #[test]
    fn compile_produces_a_request_for_valid_inputs() {
        let document = valid_document().to_string();
        let version = VersionSource {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            agent_display_name: "Agent".to_string(),
            version_number: 1,
            content_digest: "content-digest".to_string(),
            catalog_release_id: "local-2026-08-10".to_string(),
            catalog_release_digest: "catalog-digest".to_string(),
            organization_id: Uuid::new_v4(),
            canonical_document: document,
        };
        let environment = EnvironmentDefinition {
            id: Uuid::new_v4(),
            stable_definition_id: "staging".to_string(),
            version: "v1".to_string(),
            display_name: "Staging".to_string(),
            logical_environment_class: "STAGING".to_string(),
            catalog_release_id: "local-2026-08-10".to_string(),
            catalog_release_digest: "catalog-digest".to_string(),
            content_digest: "env-digest".to_string(),
        };
        let policy = PolicySource {
            id: Uuid::new_v4(),
            revision: 1,
            digest: "policy-digest".to_string(),
            matrix: serde_json::json!({"STAGING_MEDIUM": {"requiredEvidence": ["PLAN_VALIDATED"], "requiredApprovers": 1}}).to_string(),
        };
        let compiler = DeploymentCompiler::new();
        let compiled =
            compiler.compile(&version, &environment, &policy, None, "REPLACE", Utc::now());
        let compiled = compiled.expect("a valid matrix cell resolves to a compiled request");
        assert_eq!(compiled.risk, "MEDIUM");
        assert_eq!(compiled.rule.approvers, 1);
        assert!(!compiled.plan_digest.is_empty());
    }

    #[test]
    fn compile_rejects_an_unknown_strategy() {
        let document = valid_document().to_string();
        let version = VersionSource {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            agent_display_name: "Agent".to_string(),
            version_number: 1,
            content_digest: "content-digest".to_string(),
            catalog_release_id: "local-2026-08-10".to_string(),
            catalog_release_digest: "catalog-digest".to_string(),
            organization_id: Uuid::new_v4(),
            canonical_document: document,
        };
        let environment = EnvironmentDefinition {
            id: Uuid::new_v4(),
            stable_definition_id: "staging".to_string(),
            version: "v1".to_string(),
            display_name: "Staging".to_string(),
            logical_environment_class: "STAGING".to_string(),
            catalog_release_id: "local-2026-08-10".to_string(),
            catalog_release_digest: "catalog-digest".to_string(),
            content_digest: "env-digest".to_string(),
        };
        let policy = PolicySource {
            id: Uuid::new_v4(),
            revision: 1,
            digest: "policy-digest".to_string(),
            matrix: "{}".to_string(),
        };
        let compiler = DeploymentCompiler::new();
        assert!(compiler
            .compile(&version, &environment, &policy, None, "UNKNOWN", Utc::now())
            .is_none());
    }
}
