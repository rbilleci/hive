//! The one problem type every command payload lists its refusals with. A payload carries the
//! entity the command left behind plus `problems`; a refused command carries only `problems`.
//! `code` is the stable, machine-readable reason. The revision fields are set on a
//! `REVISION_CONFLICT` only.

use seaography::CustomOutputType;

#[derive(CustomOutputType, Clone, Debug, PartialEq, Eq)]
#[allow(non_snake_case)]
pub struct Problem {
    pub code: String,
    pub message: String,
    pub resourceId: Option<String>,
    pub expectedRevision: Option<i32>,
    pub actualRevision: Option<i32>,
}

impl Problem {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.to_string(),
            message: message.to_string(),
            resourceId: None,
            expectedRevision: None,
            actualRevision: None,
        }
    }

    pub fn revision_conflict(
        message: &str,
        resource_id: Option<String>,
        expected_revision: i64,
        actual_revision: i64,
    ) -> Self {
        Self {
            resourceId: resource_id,
            expectedRevision: Some(expected_revision as i32),
            actualRevision: Some(actual_revision as i32),
            ..Self::new("REVISION_CONFLICT", message)
        }
    }
}

pub fn register(builder: &mut seaography::Builder) {
    builder.register_custom_output::<Problem>();
}
