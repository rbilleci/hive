//! The persistence boundary the administration commands are written against.

use crate::administration::models::{
    AdministrationMutationResult, AdministrationScope, ApprovalRule, BudgetPolicyInput,
    ProjectConnectionInput,
};
use crate::RepositoryError;
use async_trait::async_trait;
use std::collections::BTreeMap;
use uuid::Uuid;

/// The persistence boundary of the administration commands. Reads go through the generated API,
/// so this trait has none. Every command re-evaluates the current server capability, locks the
/// resource it changes, and answers with the stored row it left behind or a typed refusal.
#[async_trait]
pub trait AdministrationRepository: Send + Sync {
    /// The stored `organizations` row an organization command answers with.
    type Organization: Send;
    /// The stored `projects` row a project command answers with.
    type Project: Send;

    #[allow(clippy::too_many_arguments)]
    async fn create_project(
        &self,
        actor: Uuid,
        organization_id: Uuid,
        expected_revision: i64,
        slug: String,
        display_name: String,
        description: Option<String>,
    ) -> Result<Outcome<Self>, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn add_membership(
        &self,
        actor: Uuid,
        scope: AdministrationScope,
        scope_id: Uuid,
        member: Uuid,
        role_codes: Vec<String>,
        expected_scope_revision: i64,
    ) -> Result<Outcome<Self>, RepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn replace_membership(
        &self,
        actor: Uuid,
        scope: AdministrationScope,
        scope_id: Uuid,
        membership_id: Uuid,
        role_codes: Vec<String>,
        expected_revision: i64,
    ) -> Result<Outcome<Self>, RepositoryError>;

    async fn end_membership(
        &self,
        actor: Uuid,
        scope: AdministrationScope,
        scope_id: Uuid,
        membership_id: Uuid,
        expected_revision: i64,
        reason: String,
    ) -> Result<Outcome<Self>, RepositoryError>;

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
    ) -> Result<Outcome<Self>, RepositoryError>;

    async fn update_budget(
        &self,
        actor: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        values: &BudgetPolicyInput,
    ) -> Result<Outcome<Self>, RepositoryError>;

    async fn update_approval_policy(
        &self,
        actor: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        matrix: BTreeMap<String, ApprovalRule>,
        reason: String,
    ) -> Result<Outcome<Self>, RepositoryError>;

    async fn update_project_general(
        &self,
        actor: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        display_name: String,
        description: String,
    ) -> Result<Outcome<Self>, RepositoryError>;

    async fn save_project_connection(
        &self,
        actor: Uuid,
        project_id: Uuid,
        connection_id: Option<Uuid>,
        expected_revision: i64,
        values: &ProjectConnectionInput,
    ) -> Result<Outcome<Self>, RepositoryError>;
}

/// What a command answers with: the repository's stored rows, or a refusal.
pub type Outcome<R> = AdministrationMutationResult<
    <R as AdministrationRepository>::Organization,
    <R as AdministrationRepository>::Project,
>;
