use chrono::{DateTime, Utc};

/// Informational current-month budget state; it never authorizes or blocks work. The GraphQL
/// schema exposes it as `ProjectBudgetStatus`.
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

/// One fixed local P-05 environment/risk matrix cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRule {
    pub required_evidence: Vec<String>,
    pub required_approvers: i32,
}

/// One of the two scopes an administration command targets. The persisted and wire form is the
/// string "ORGANIZATION" or "PROJECT"; this enum makes any other value unrepresentable, so no
/// caller has to revalidate one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdministrationScope {
    Organization,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdministrationProblemKind {
    NotFound,
    Forbidden,
    RevisionConflict,
    InvalidInput,
    ProtectedLifecycle,
    PolicyWeakening,
}

/// A deliberately non-disclosing refusal from an administration command.
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

/// The stored organization or project row a command left behind, or one typed refusal. `O` and
/// `P` are the persistence layer's `organizations` and `projects` rows; the GraphQL payload
/// exposes them as the generated types the reads use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdministrationMutationResult<O, P> {
    pub organization: Option<O>,
    pub project: Option<P>,
    pub problem: Option<AdministrationProblem>,
}

impl<O, P> AdministrationMutationResult<O, P> {
    pub fn organization(value: O) -> Self {
        Self {
            organization: Some(value),
            project: None,
            problem: None,
        }
    }

    pub fn project(value: P) -> Self {
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

/// The submitted values of a project settings connection, named rather than positional.
///
/// All five are strings and all five sit next to each other in the request, so a caller that
/// transposed two of them would compile and then write a connection whose environment is its
/// credential status. The names are the only thing that can carry that distinction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectConnectionInput {
    pub display_name: String,
    pub definition_version: String,
    pub environment: String,
    pub credential_status: String,
    pub lifecycle_status: String,
}

/// The submitted values of a project budget policy, named rather than positional.
///
/// The two amounts are adjacent `i32`s that mean different things, and the rule between them —
/// the warning threshold sits below the monthly limit — is exactly what a transposition would
/// invert.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BudgetPolicyInput {
    pub currency: String,
    pub monthly_limit_cents: i32,
    pub warning_threshold_cents: i32,
    pub reason: String,
}
