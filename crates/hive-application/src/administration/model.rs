use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use uuid::Uuid;

/// A principal already known inside the authorized organization boundary. Ports
/// `AdministrationPrincipal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdministrationPrincipal {
    pub id: Uuid,
    pub display_name: String,
    pub email: String,
}

/// Temporal membership and its complete current role set. Ports
/// `AdministrationMembership`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdministrationMembership {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub display_name: String,
    pub email: String,
    pub role_codes: Vec<String>,
    pub project_access_summary: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub revision: i64,
}

/// An immutable current or historic local monthly budget policy version. Ports
/// `BudgetPolicy` (the SDL's `ProjectBudgetPolicy`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetPolicy {
    pub revision: i64,
    pub currency: String,
    pub monthly_limit_cents: i32,
    pub warning_threshold_cents: i32,
    pub change_reason: String,
    pub created_at: DateTime<Utc>,
}

/// Informational current-month budget state; it never authorizes or blocks work.
/// Ports `BudgetStatus` (the SDL's `ProjectBudgetStatus`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetStatus {
    pub state: String,
    pub reason: Option<String>,
    pub amount_cents: Option<i32>,
    pub includes_estimates: bool,
    pub currency: Option<String>,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    pub data_as_of: Option<DateTime<Utc>>,
    pub last_successful_import_at: Option<DateTime<Utc>>,
}

/// One fixed local P-05 environment/risk matrix cell. Ports `ApprovalRule`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRule {
    pub required_evidence: Vec<String>,
    pub required_approvers: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPolicyVersion {
    pub revision: i64,
    pub digest: String,
    pub matrix: BTreeMap<String, ApprovalRule>,
    pub change_reason: String,
    pub created_at: DateTime<Utc>,
}

/// Stable policy identity with an append-only immutable version history. Ports
/// `ApprovalPolicy` (the SDL's `ProjectApprovalPolicy`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPolicy {
    pub id: Uuid,
    pub revision: i64,
    pub digest: String,
    pub matrix: BTreeMap<String, ApprovalRule>,
    pub change_reason: String,
    pub created_at: DateTime<Utc>,
    pub history: Vec<ApprovalPolicyVersion>,
}

/// Non-secret, server-authorized project connection metadata. Credential values
/// never enter this model. Ports `ProjectSettingsConnection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSettingsConnection {
    pub id: Uuid,
    pub display_name: String,
    pub definition_version: String,
    pub environment: String,
    pub credential_status: String,
    pub lifecycle_status: String,
    pub agent_count: i32,
    pub revision: i64,
}

/// Server-authorized organization settings projection. Ports
/// `OrganizationAdministration`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationAdministration {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_status: String,
    pub revision: i64,
    pub memberships: Vec<AdministrationMembership>,
    pub available_principals: Vec<AdministrationPrincipal>,
    pub assignable_roles: Vec<String>,
    pub capabilities: Vec<String>,
}

/// Server-authorized project settings projection. Ports `ProjectAdministration`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAdministration {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub lifecycle_status: String,
    pub revision: i64,
    pub memberships: Vec<AdministrationMembership>,
    pub available_principals: Vec<AdministrationPrincipal>,
    pub assignable_roles: Vec<String>,
    pub budget_policy: Option<BudgetPolicy>,
    pub budget_history: Vec<BudgetPolicy>,
    pub budget_status: BudgetStatus,
    pub approval_policy: Option<ApprovalPolicy>,
    pub connections: Vec<ProjectSettingsConnection>,
    pub capabilities: Vec<String>,
}

/// One of the two scopes an administration command targets. Ports the `String
/// scope` ("ORGANIZATION"/"PROJECT") `AdministrationRepository`'s command
/// methods take; a two-variant enum makes the "must be one of these two
/// strings" validation `AdministrationService.commandScope` performs
/// unrepresentable as a bad state instead of a runtime check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdministrationScope {
    Organization,
    Project,
}

/// Ports `AdministrationProblem.Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdministrationProblemKind {
    NotFound,
    Forbidden,
    RevisionConflict,
    InvalidInput,
    ProtectedLifecycle,
    PolicyWeakening,
}

/// A deliberately non-disclosing refusal from an administration command. Ports
/// `AdministrationProblem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdministrationProblem {
    pub kind: AdministrationProblemKind,
    pub resource_id: Option<String>,
    pub expected_revision: i64,
    pub actual_revision: i64,
}

impl AdministrationProblem {
    pub fn unavailable() -> Self {
        Self {
            kind: AdministrationProblemKind::NotFound,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn forbidden() -> Self {
        Self {
            kind: AdministrationProblemKind::Forbidden,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn conflict(resource_id: String, expected_revision: i64, actual_revision: i64) -> Self {
        Self {
            kind: AdministrationProblemKind::RevisionConflict,
            resource_id: Some(resource_id),
            expected_revision,
            actual_revision,
        }
    }

    pub fn invalid() -> Self {
        Self {
            kind: AdministrationProblemKind::InvalidInput,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn protected_lifecycle() -> Self {
        Self {
            kind: AdministrationProblemKind::ProtectedLifecycle,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }

    pub fn weakening() -> Self {
        Self {
            kind: AdministrationProblemKind::PolicyWeakening,
            resource_id: None,
            expected_revision: -1,
            actual_revision: -1,
        }
    }
}

/// One successful administration projection or one typed refusal. Ports
/// `AdministrationMutationResult`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdministrationMutationResult {
    pub organization: Option<OrganizationAdministration>,
    pub project: Option<ProjectAdministration>,
    pub problem: Option<AdministrationProblem>,
}

impl AdministrationMutationResult {
    pub fn organization(value: OrganizationAdministration) -> Self {
        Self {
            organization: Some(value),
            project: None,
            problem: None,
        }
    }

    pub fn project(value: ProjectAdministration) -> Self {
        Self {
            organization: None,
            project: Some(value),
            problem: None,
        }
    }

    pub fn refused(problem: AdministrationProblem) -> Self {
        Self {
            organization: None,
            project: None,
            problem: Some(problem),
        }
    }
}
