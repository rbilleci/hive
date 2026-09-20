//! Ports `application/configuration`'s two identity value types —
//! `TypedReference` and `ResourceIdentity`.

use std::sync::LazyLock;

static VALID_TOKEN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[a-z][a-z0-9-]{0,80}$").unwrap());
static VERSION_TOKEN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^v[1-9][0-9]*$").unwrap());

fn valid_token(value: &str) -> bool {
    VALID_TOKEN.is_match(value)
}

/// Ports `ResourceIdentity`.
pub mod resource_identity {
    use super::{valid_token, VERSION_TOKEN};

    pub fn from_display_name(name: &str) -> Option<String> {
        let lowercased = name.trim().to_lowercase();
        let mut normalized = String::with_capacity(lowercased.len());
        let mut previous_was_separator = false;
        for character in lowercased.chars() {
            if character.is_ascii_lowercase() || character.is_ascii_digit() {
                normalized.push(character);
                previous_was_separator = false;
            } else if !previous_was_separator {
                normalized.push('-');
                previous_was_separator = true;
            }
        }
        let trimmed = normalized.trim_matches('-').to_string();
        is_canonical(&trimmed).then_some(trimmed)
    }

    pub fn is_canonical(value: &str) -> bool {
        valid_token(value) && !value.contains("--") && !value.ends_with('-')
    }

    pub fn is_resource_kind(kind: &str) -> bool {
        matches!(kind, "prompt" | "policy" | "model-profile")
    }

    pub fn typed_kind(resource_kind: &str) -> Option<&'static str> {
        match resource_kind {
            "PROMPT" => Some("prompt"),
            "POLICY" => Some("policy"),
            "MODEL_PROFILE" => Some("model-profile"),
            _ => None,
        }
    }

    pub fn resource_kind(typed_kind: &str) -> Option<&'static str> {
        match typed_kind {
            "prompt" => Some("PROMPT"),
            "policy" => Some("POLICY"),
            "model-profile" => Some("MODEL_PROFILE"),
            _ => None,
        }
    }

    pub fn version(value: &str) -> Option<i64> {
        if !VERSION_TOKEN.is_match(value) {
            return None;
        }
        value[1..].parse().ok()
    }

    pub fn reference(resource_kind: &str, identity: &str, version: i64) -> Option<String> {
        Some(format!(
            "{}:{identity}@v{version}",
            typed_kind(resource_kind)?
        ))
    }
}

/// Ports `TypedReference`: a stable, typed dependency identity such as
/// `model:claude-sonnet-5@v3`. Display labels never participate in binding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypedReference {
    pub kind: String,
    pub identity: String,
    pub version: String,
}

impl TypedReference {
    fn new(kind: String, identity: String, version: String) -> Option<Self> {
        if !valid_token(&kind) || !valid_token(&identity) || !valid_token(&version) {
            return None;
        }
        if resource_identity::is_resource_kind(&kind)
            && (!resource_identity::is_canonical(&identity)
                || resource_identity::version(&version).is_none())
        {
            return None;
        }
        Some(Self {
            kind,
            identity,
            version,
        })
    }

    pub fn parse(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        let (head, version) = trimmed.split_once('@')?;
        if trimmed.matches('@').count() != 1 {
            return None;
        }
        let separator = head.find(':')?;
        if separator == 0 || separator == head.len() - 1 {
            return None;
        }
        Self::new(
            head[..separator].to_string(),
            head[separator + 1..].to_string(),
            version.to_string(),
        )
    }

    pub fn value(&self) -> String {
        format!("{}:{}@{}", self.kind, self.identity, self.version)
    }

    pub fn reusable_resource(&self) -> bool {
        resource_identity::is_resource_kind(&self.kind)
    }

    pub fn reusable_resource_version(&self) -> i64 {
        resource_identity::version(&self.version)
            .expect("constructor guarantees a resource-kind reference carries a valid version")
    }
}
