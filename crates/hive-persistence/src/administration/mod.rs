//! Ports `PostgresAdministrationRepository` in full: `findOrganization`/`findProject` (in
//! `queries`) and the nine write commands (`createProject`, `addMembership`, `replaceMembership`,
//! `endMembership`, `lifecycle`, `updateBudget`, `updateApprovalPolicy`, `updateProjectGeneral`,
//! `saveProjectConnection`, in `mutations`), including the deployment/approval-domain scope-cache
//! maintenance and archive cascade Administration's own writes must keep current, even though the
//! Deployment/approval feature area itself is not yet ported. `rows` holds the scope-to-table-name
//! encodings, the approval policy matrix's JSON/digest encoding, and the locked single-row
//! fetchers both `queries` and `mutations` share.
//!
//! See `mutations`'s module doc comment for the transaction-scoped capability re-check invariant
//! every write command relies on, and for why an early return before `tx.commit()` needs no
//! explicit `tx.rollback().await`.

mod mutations;
mod queries;
mod rows;

use async_trait::async_trait;
use hive_application::administration::{
    AdministrationMutationResult, AdministrationRepository,
    AdministrationRepositoryError as RepositoryError, AdministrationScope, ApprovalRule,
    OrganizationAdministration, ProjectAdministration,
};
use sea_orm::DatabaseConnection;
use std::collections::BTreeMap;
use uuid::Uuid;

pub struct PgAdministrationRepository {
    db: DatabaseConnection,
}

impl PgAdministrationRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AdministrationRepository for PgAdministrationRepository {
    async fn find_organization(
        &self,
        principal: Uuid,
        organization_id: Uuid,
    ) -> Result<Option<OrganizationAdministration>, RepositoryError> {
        queries::find_organization(&self.db, principal, organization_id).await
    }

    async fn find_project(
        &self,
        principal: Uuid,
        project_id: Uuid,
    ) -> Result<Option<ProjectAdministration>, RepositoryError> {
        queries::find_project(&self.db, principal, project_id).await
    }

    async fn create_project(
        &self,
        actor: Uuid,
        organization_id: Uuid,
        expected_revision: i64,
        slug: String,
        display_name: String,
        description: Option<String>,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        mutations::create_project(
            &self.db,
            actor,
            organization_id,
            expected_revision,
            slug,
            display_name,
            description,
        )
        .await
    }

    async fn add_membership(
        &self,
        actor: Uuid,
        scope: AdministrationScope,
        scope_id: Uuid,
        member: Uuid,
        role_codes: Vec<String>,
        expected_scope_revision: i64,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        mutations::add_membership(
            &self.db,
            actor,
            scope,
            scope_id,
            member,
            role_codes,
            expected_scope_revision,
        )
        .await
    }

    async fn replace_membership(
        &self,
        actor: Uuid,
        scope: AdministrationScope,
        scope_id: Uuid,
        membership_id: Uuid,
        role_codes: Vec<String>,
        expected_revision: i64,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        mutations::replace_membership(
            &self.db,
            actor,
            scope,
            scope_id,
            membership_id,
            role_codes,
            expected_revision,
        )
        .await
    }

    async fn end_membership(
        &self,
        actor: Uuid,
        scope: AdministrationScope,
        scope_id: Uuid,
        membership_id: Uuid,
        expected_revision: i64,
        reason: String,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        mutations::end_membership(
            &self.db,
            actor,
            scope,
            scope_id,
            membership_id,
            expected_revision,
            reason,
        )
        .await
    }

    async fn lifecycle(
        &self,
        actor: Uuid,
        scope: AdministrationScope,
        scope_id: Uuid,
        expected_revision: i64,
        reason: Option<String>,
        confirmation: Option<String>,
        archive: bool,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        mutations::lifecycle(
            &self.db,
            actor,
            scope,
            scope_id,
            expected_revision,
            reason,
            confirmation,
            archive,
        )
        .await
    }

    async fn update_budget(
        &self,
        actor: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        currency: String,
        monthly_limit_cents: i32,
        warning_threshold_cents: i32,
        reason: String,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        mutations::update_budget(
            &self.db,
            actor,
            project_id,
            expected_revision,
            currency,
            monthly_limit_cents,
            warning_threshold_cents,
            reason,
        )
        .await
    }

    async fn update_approval_policy(
        &self,
        actor: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        matrix: BTreeMap<String, ApprovalRule>,
        reason: String,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        mutations::update_approval_policy(
            &self.db,
            actor,
            project_id,
            expected_revision,
            matrix,
            reason,
        )
        .await
    }

    async fn update_project_general(
        &self,
        actor: Uuid,
        project_id: Uuid,
        expected_revision: i64,
        display_name: String,
        description: String,
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        mutations::update_project_general(
            &self.db,
            actor,
            project_id,
            expected_revision,
            display_name,
            description,
        )
        .await
    }

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
    ) -> Result<AdministrationMutationResult, RepositoryError> {
        mutations::save_project_connection(
            &self.db,
            actor,
            project_id,
            connection_id,
            expected_revision,
            display_name,
            definition_version,
            environment,
            credential_status,
            lifecycle_status,
        )
        .await
    }
}
