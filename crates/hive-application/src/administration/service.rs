use crate::administration::model::{
    AdministrationMutationResult, AdministrationProblem, AdministrationScope, ApprovalRule,
    OrganizationAdministration, ProjectAdministration,
};
use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

const ORGANIZATION_ROLES: [&str; 3] = ["ORGANIZATION_MEMBER", "ORGANIZATION_ADMIN", "AUDITOR"];
const PROJECT_ROLES: [&str; 5] = [
    "PROJECT_ADMIN",
    "AGENT_DEVELOPER",
    "OPERATOR",
    "DEPLOYMENT_APPROVER",
    "AUDITOR",
];
const EVIDENCE: [&str; 3] = [
    "PLAN_VALIDATED",
    "CHANGE_SUMMARY_READY",
    "EVALUATION_PASSED",
];
const CELLS: [&str; 9] = [
    "DEVELOPMENT_LOW",
    "DEVELOPMENT_MEDIUM",
    "DEVELOPMENT_HIGH",
    "STAGING_LOW",
    "STAGING_MEDIUM",
    "STAGING_HIGH",
    "PRODUCTION_LOW",
    "PRODUCTION_MEDIUM",
    "PRODUCTION_HIGH",
];
const ENVIRONMENTS: [&str; 3] = ["DEVELOPMENT", "STAGING", "PRODUCTION"];
const CREDENTIAL_STATUSES: [&str; 2] = ["UNBOUND", "REDACTED_BOUND"];
const CONNECTION_LIFECYCLE_STATUSES: [&str; 3] = ["ACTIVE", "DISABLED", "ARCHIVED"];

/// Port `AdministrationRepository`. Every command re-evaluates the current
/// server capability, locks the resource it changes, and returns a refreshed
/// projection or a typed refusal in the same round trip.
#[async_trait]
pub trait AdministrationRepository: Send + Sync {
    async fn find_organization(
        &self,
        principal_id: Uuid,
        organization_id: Uuid,
    ) -> Result<Option<OrganizationAdministration>, RepositoryError>;

    async fn find_project(
        &self,
        principal_id: Uuid,
        project_id: Uuid,
    ) -> Result<Option<ProjectAdministration>, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn create_project(
        &self,
        actor: Uuid,
        organization_id: Uuid,
        expected_revision: i64,
        slug: String,
        display_name: String,
        description: Option<String>,
    ) -> Result<AdministrationMutationResult, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn add_membership(
        &self,
        actor: Uuid,
        scope: AdministrationScope,
        scope_id: Uuid,
        member: Uuid,
        role_codes: Vec<String>,
        expected_scope_revision: i64,
    ) -> Result<AdministrationMutationResult, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn replace_membership(
        &self,
        actor: Uuid,
        scope: AdministrationScope,
        scope_id: Uuid,
        membership_id: Uuid,
        role_codes: Vec<String>,
        expected_revision: i64,
    ) -> Result<AdministrationMutationResult, RepositoryError>;

    async fn end_membership(
        &self,
        actor: Uuid,
        scope: AdministrationScope,
        scope_id: Uuid,
        membership_id: Uuid,
        expected_revision: i64,
        reason: String,
    ) -> Result<AdministrationMutationResult, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn lifecycle(
        &self,
        actor: Uuid,
        scope: AdministrationScope,
        scope_id: Uuid,
        expected_revision: i64,
        reason: Option<String>,
        confirmation: Option<String>,
        archive: bool,
    ) -> Result<AdministrationMutationResult, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn update_budget(
        &self,
        actor: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        currency: String,
        monthly_limit_cents: i32,
        warning_threshold_cents: i32,
        reason: String,
    ) -> Result<AdministrationMutationResult, RepositoryError>;

    async fn update_approval_policy(
        &self,
        actor: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        matrix: BTreeMap<String, ApprovalRule>,
        reason: String,
    ) -> Result<AdministrationMutationResult, RepositoryError>;

    async fn update_project_general(
        &self,
        actor: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        display_name: String,
        description: String,
    ) -> Result<AdministrationMutationResult, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn save_project_connection(
        &self,
        actor: Uuid,
        project_id: Uuid,
        connection_id: Option<Uuid>,
        expected_revision: i64,
        display_name: String,
        definition_version: String,
        environment: String,
        credential_status: String,
        lifecycle_status: String,
    ) -> Result<AdministrationMutationResult, RepositoryError>;
}

fn parsed(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value).ok()
}

fn blank(value: &str) -> bool {
    value.trim().is_empty()
}

fn valid_roles(scope: AdministrationScope, roles: &[String]) -> bool {
    if roles.is_empty() {
        return false;
    }
    let unique: BTreeSet<&String> = roles.iter().collect();
    if unique.len() != roles.len() {
        return false;
    }
    let allowed: &[&str] = match scope {
        AdministrationScope::Organization => &ORGANIZATION_ROLES,
        AdministrationScope::Project => &PROJECT_ROLES,
    };
    roles.iter().all(|role| allowed.contains(&role.as_str()))
}

fn sorted(mut roles: Vec<String>) -> Vec<String> {
    roles.sort();
    roles
}

fn valid_matrix(matrix: &BTreeMap<String, ApprovalRule>) -> bool {
    matrix.values().all(|rule| {
        rule.required_approvers >= 0
            && rule.required_approvers <= 2
            && !rule.required_evidence.is_empty()
            && rule
                .required_evidence
                .iter()
                .all(|evidence| EVIDENCE.contains(&evidence.as_str()))
            && {
                let unique: BTreeSet<&String> = rule.required_evidence.iter().collect();
                unique.len() == rule.required_evidence.len()
            }
    })
}

fn command_scope(scope: &str, scope_id: &str) -> Option<(AdministrationScope, Uuid)> {
    let scope = match scope {
        "ORGANIZATION" => AdministrationScope::Organization,
        "PROJECT" => AdministrationScope::Project,
        _ => return None,
    };
    parsed(scope_id).map(|id| (scope, id))
}

/// Validates command shapes before the repository rechecks current capability
/// inside its transaction. Ports `AdministrationService`.
pub struct AdministrationService<R: AdministrationRepository> {
    repository: R,
}

impl<R: AdministrationRepository> AdministrationService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub async fn find_organization(
        &self,
        principal_id: Uuid,
        organization_id: &str,
    ) -> Result<Option<OrganizationAdministration>, RepositoryError> {
        let Ok(id) = Uuid::parse_str(organization_id) else {
            return Ok(None);
        };
        self.repository.find_organization(principal_id, id).await
    }

    pub async fn find_project(
        &self,
        principal_id: Uuid,
        project_id: &str,
    ) -> Result<Option<ProjectAdministration>, RepositoryError> {
        let Ok(id) = Uuid::parse_str(project_id) else {
            return Ok(None);
        };
        self.repository.find_project(principal_id, id).await
    }

    pub async fn create_project(
        &self,
        actor: Uuid,
        organization_id: &str,
        expected_revision: i64,
        slug: String,
        display_name: String,
        description: Option<String>,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        let Some(id) = parsed(organization_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::unavailable(),
            ));
        };
        self.repository
            .create_project(
                actor,
                id,
                expected_revision,
                slug,
                display_name,
                description,
            )
            .await
    }

    pub async fn add_membership(
        &self,
        actor: Uuid,
        scope: &str,
        scope_id: &str,
        member_id: &str,
        roles: Vec<String>,
        expected_scope_revision: i64,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        let Some((scope, id)) = command_scope(scope, scope_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        };
        let Some(member) = parsed(member_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::unavailable(),
            ));
        };
        if !valid_roles(scope, &roles) {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        self.repository
            .add_membership(
                actor,
                scope,
                id,
                member,
                sorted(roles),
                expected_scope_revision,
            )
            .await
    }

    pub async fn replace_membership(
        &self,
        actor: Uuid,
        scope: &str,
        scope_id: &str,
        membership_id: &str,
        roles: Vec<String>,
        expected_revision: i64,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        let Some((scope, id)) = command_scope(scope, scope_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        };
        let Some(membership) = parsed(membership_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::unavailable(),
            ));
        };
        if !valid_roles(scope, &roles) {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        self.repository
            .replace_membership(
                actor,
                scope,
                id,
                membership,
                sorted(roles),
                expected_revision,
            )
            .await
    }

    pub async fn end_membership(
        &self,
        actor: Uuid,
        scope: &str,
        scope_id: &str,
        membership_id: &str,
        expected_revision: i64,
        reason: &str,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        let Some((scope, id)) = command_scope(scope, scope_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        };
        let Some(membership) = parsed(membership_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::unavailable(),
            ));
        };
        if blank(reason) {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        self.repository
            .end_membership(
                actor,
                scope,
                id,
                membership,
                expected_revision,
                reason.trim().to_string(),
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn lifecycle(
        &self,
        actor: Uuid,
        scope: &str,
        scope_id: &str,
        expected_revision: i64,
        reason: Option<&str>,
        confirmation: Option<&str>,
        archive: bool,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        let Some((scope, id)) = command_scope(scope, scope_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        };
        if archive && reason.map(blank).unwrap_or(true) {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        let reason = if archive {
            reason.map(|value| value.trim().to_string())
        } else {
            None
        };
        self.repository
            .lifecycle(
                actor,
                scope,
                id,
                expected_revision,
                reason,
                confirmation.map(str::to_string),
                archive,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_budget(
        &self,
        actor: Uuid,
        project_id: &str,
        expected_revision: i64,
        currency: Option<&str>,
        monthly_limit_cents: i32,
        warning_threshold_cents: i32,
        reason: &str,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        let Some(id) = parsed(project_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::unavailable(),
            ));
        };
        let currency_valid = currency.is_some_and(|value| {
            value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_uppercase())
        });
        if !currency_valid
            || monthly_limit_cents <= 0
            || warning_threshold_cents <= 0
            || warning_threshold_cents >= monthly_limit_cents
            || blank(reason)
        {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        self.repository
            .update_budget(
                actor,
                id,
                expected_revision,
                currency.unwrap().to_string(),
                monthly_limit_cents,
                warning_threshold_cents,
                reason.trim().to_string(),
            )
            .await
    }

    pub async fn update_approval_policy(
        &self,
        actor: Uuid,
        project_id: &str,
        expected_revision: i64,
        matrix: BTreeMap<String, ApprovalRule>,
        reason: &str,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        let Some(id) = parsed(project_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::unavailable(),
            ));
        };
        let cells: BTreeSet<&str> = matrix.keys().map(String::as_str).collect();
        let expected_cells: BTreeSet<&str> = CELLS.iter().copied().collect();
        if blank(reason) || cells != expected_cells || !valid_matrix(&matrix) {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        self.repository
            .update_approval_policy(
                actor,
                id,
                expected_revision,
                matrix,
                reason.trim().to_string(),
            )
            .await
    }

    pub async fn update_project_general(
        &self,
        actor: Uuid,
        project_id: &str,
        expected_revision: i64,
        display_name: &str,
        description: Option<&str>,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        let Some(id) = parsed(project_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::unavailable(),
            ));
        };
        if blank(display_name) {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        let description = description
            .map(|value| value.trim().to_string())
            .unwrap_or_default();
        self.repository
            .update_project_general(
                actor,
                id,
                expected_revision,
                display_name.trim().to_string(),
                description,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn save_project_connection(
        &self,
        actor: Uuid,
        project_id: &str,
        connection_id: Option<&str>,
        expected_revision: i64,
        display_name: &str,
        definition_version: &str,
        environment: &str,
        credential_status: &str,
        lifecycle_status: &str,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        let Some(project) = parsed(project_id) else {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::unavailable(),
            ));
        };
        let connection_id = connection_id.filter(|value| !value.is_empty());
        let connection = match connection_id {
            None => None,
            Some(value) => match parsed(value) {
                Some(id) => Some(id),
                None => {
                    return Ok(AdministrationMutationResult::refused(
                        AdministrationProblem::invalid(),
                    ))
                }
            },
        };
        if expected_revision < 0
            || blank(display_name)
            || blank(definition_version)
            || !ENVIRONMENTS.contains(&environment)
            || !CREDENTIAL_STATUSES.contains(&credential_status)
            || !CONNECTION_LIFECYCLE_STATUSES.contains(&lifecycle_status)
        {
            return Ok(AdministrationMutationResult::refused(
                AdministrationProblem::invalid(),
            ));
        }
        self.repository
            .save_project_connection(
                actor,
                project,
                connection,
                expected_revision,
                display_name.trim().to_string(),
                definition_version.trim().to_string(),
                environment.to_string(),
                credential_status.to_string(),
                lifecycle_status.to_string(),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::administration::model::AdministrationProblemKind;

    struct StubRepository;

    #[async_trait]
    impl AdministrationRepository for StubRepository {
        async fn find_organization(
            &self,
            _principal_id: Uuid,
            _organization_id: Uuid,
        ) -> Result<Option<OrganizationAdministration>, RepositoryError> {
            Ok(None)
        }

        async fn find_project(
            &self,
            _principal_id: Uuid,
            _project_id: Uuid,
        ) -> Result<Option<ProjectAdministration>, RepositoryError> {
            Ok(None)
        }

        async fn create_project(
            &self,
            _actor: Uuid,
            _organization_id: Uuid,
            _expected_revision: i64,
            _slug: String,
            _display_name: String,
            _description: Option<String>,
        ) -> Result<AdministrationMutationResult, RepositoryError> {
            Ok(AdministrationMutationResult::default())
        }

        async fn add_membership(
            &self,
            _actor: Uuid,
            _scope: AdministrationScope,
            _scope_id: Uuid,
            _member: Uuid,
            _role_codes: Vec<String>,
            _expected_scope_revision: i64,
        ) -> Result<AdministrationMutationResult, RepositoryError> {
            Ok(AdministrationMutationResult::default())
        }

        async fn replace_membership(
            &self,
            _actor: Uuid,
            _scope: AdministrationScope,
            _scope_id: Uuid,
            _membership_id: Uuid,
            _role_codes: Vec<String>,
            _expected_revision: i64,
        ) -> Result<AdministrationMutationResult, RepositoryError> {
            Ok(AdministrationMutationResult::default())
        }

        async fn end_membership(
            &self,
            _actor: Uuid,
            _scope: AdministrationScope,
            _scope_id: Uuid,
            _membership_id: Uuid,
            _expected_revision: i64,
            _reason: String,
        ) -> Result<AdministrationMutationResult, RepositoryError> {
            Ok(AdministrationMutationResult::default())
        }

        async fn lifecycle(
            &self,
            _actor: Uuid,
            _scope: AdministrationScope,
            _scope_id: Uuid,
            _expected_revision: i64,
            _reason: Option<String>,
            _confirmation: Option<String>,
            _archive: bool,
        ) -> Result<AdministrationMutationResult, RepositoryError> {
            Ok(AdministrationMutationResult::default())
        }

        async fn update_budget(
            &self,
            _actor: Uuid,
            _project_id: Uuid,
            _expected_revision: i64,
            _currency: String,
            _monthly_limit_cents: i32,
            _warning_threshold_cents: i32,
            _reason: String,
        ) -> Result<AdministrationMutationResult, RepositoryError> {
            Ok(AdministrationMutationResult::default())
        }

        async fn update_approval_policy(
            &self,
            _actor: Uuid,
            _project_id: Uuid,
            _expected_revision: i64,
            _matrix: BTreeMap<String, ApprovalRule>,
            _reason: String,
        ) -> Result<AdministrationMutationResult, RepositoryError> {
            Ok(AdministrationMutationResult::default())
        }

        async fn update_project_general(
            &self,
            _actor: Uuid,
            _project_id: Uuid,
            _expected_revision: i64,
            _display_name: String,
            _description: String,
        ) -> Result<AdministrationMutationResult, RepositoryError> {
            Ok(AdministrationMutationResult::default())
        }

        async fn save_project_connection(
            &self,
            _actor: Uuid,
            _project_id: Uuid,
            _connection_id: Option<Uuid>,
            _expected_revision: i64,
            _display_name: String,
            _definition_version: String,
            _environment: String,
            _credential_status: String,
            _lifecycle_status: String,
        ) -> Result<AdministrationMutationResult, RepositoryError> {
            Ok(AdministrationMutationResult::default())
        }
    }

    fn service() -> AdministrationService<StubRepository> {
        AdministrationService::new(StubRepository)
    }

    #[tokio::test]
    async fn an_unparseable_organization_id_is_not_found_rather_than_an_error() {
        let service = service();
        assert!(service
            .find_organization(Uuid::new_v4(), "not-a-uuid")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn an_unparseable_project_id_is_not_found_rather_than_an_error() {
        let service = service();
        assert!(service
            .find_project(Uuid::new_v4(), "not-a-uuid")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn add_membership_rejects_a_project_only_role_at_organization_scope() {
        let service = service();
        let result = service
            .add_membership(
                Uuid::new_v4(),
                "ORGANIZATION",
                &Uuid::new_v4().to_string(),
                &Uuid::new_v4().to_string(),
                vec!["PROJECT_ADMIN".to_string()],
                1,
            )
            .await
            .unwrap();
        assert_eq!(
            result.problem.unwrap().kind,
            AdministrationProblemKind::InvalidInput
        );
    }

    #[tokio::test]
    async fn add_membership_rejects_a_duplicate_role() {
        let service = service();
        let result = service
            .add_membership(
                Uuid::new_v4(),
                "PROJECT",
                &Uuid::new_v4().to_string(),
                &Uuid::new_v4().to_string(),
                vec!["AUDITOR".to_string(), "AUDITOR".to_string()],
                1,
            )
            .await
            .unwrap();
        assert_eq!(
            result.problem.unwrap().kind,
            AdministrationProblemKind::InvalidInput
        );
    }

    #[tokio::test]
    async fn add_membership_rejects_an_unrecognized_scope() {
        let service = service();
        let result = service
            .add_membership(
                Uuid::new_v4(),
                "PRINCIPAL",
                &Uuid::new_v4().to_string(),
                &Uuid::new_v4().to_string(),
                vec!["AUDITOR".to_string()],
                1,
            )
            .await
            .unwrap();
        assert_eq!(
            result.problem.unwrap().kind,
            AdministrationProblemKind::InvalidInput
        );
    }

    #[tokio::test]
    async fn update_budget_rejects_a_warning_threshold_at_or_above_the_limit() {
        let service = service();
        let result = service
            .update_budget(
                Uuid::new_v4(),
                &Uuid::new_v4().to_string(),
                1,
                Some("USD"),
                1000,
                1000,
                "reason",
            )
            .await
            .unwrap();
        assert_eq!(
            result.problem.unwrap().kind,
            AdministrationProblemKind::InvalidInput
        );
    }

    #[tokio::test]
    async fn update_budget_rejects_a_lowercase_currency_code() {
        let service = service();
        let result = service
            .update_budget(
                Uuid::new_v4(),
                &Uuid::new_v4().to_string(),
                1,
                Some("usd"),
                1000,
                500,
                "reason",
            )
            .await
            .unwrap();
        assert_eq!(
            result.problem.unwrap().kind,
            AdministrationProblemKind::InvalidInput
        );
    }

    fn default_matrix() -> BTreeMap<String, ApprovalRule> {
        CELLS
            .iter()
            .map(|cell| {
                (
                    cell.to_string(),
                    ApprovalRule {
                        required_evidence: vec!["PLAN_VALIDATED".to_string()],
                        required_approvers: 0,
                    },
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn update_approval_policy_rejects_a_matrix_missing_a_cell() {
        let service = service();
        let mut matrix = default_matrix();
        matrix.remove("PRODUCTION_HIGH");
        let result = service
            .update_approval_policy(
                Uuid::new_v4(),
                &Uuid::new_v4().to_string(),
                1,
                matrix,
                "reason",
            )
            .await
            .unwrap();
        assert_eq!(
            result.problem.unwrap().kind,
            AdministrationProblemKind::InvalidInput
        );
    }

    #[tokio::test]
    async fn update_approval_policy_rejects_an_out_of_range_approver_count() {
        let service = service();
        let mut matrix = default_matrix();
        matrix.insert(
            "PRODUCTION_HIGH".to_string(),
            ApprovalRule {
                required_evidence: vec!["PLAN_VALIDATED".to_string()],
                required_approvers: 3,
            },
        );
        let result = service
            .update_approval_policy(
                Uuid::new_v4(),
                &Uuid::new_v4().to_string(),
                1,
                matrix,
                "reason",
            )
            .await
            .unwrap();
        assert_eq!(
            result.problem.unwrap().kind,
            AdministrationProblemKind::InvalidInput
        );
    }

    #[tokio::test]
    async fn save_project_connection_rejects_an_unrecognized_environment() {
        let service = service();
        let result = service
            .save_project_connection(
                Uuid::new_v4(),
                &Uuid::new_v4().to_string(),
                None,
                0,
                "name",
                "v1",
                "PREPROD",
                "UNBOUND",
                "ACTIVE",
            )
            .await
            .unwrap();
        assert_eq!(
            result.problem.unwrap().kind,
            AdministrationProblemKind::InvalidInput
        );
    }
}
