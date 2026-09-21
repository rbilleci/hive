//! Computed fields of the generated administration
//! objects. `hive-api` attaches them to the generated objects.
//!
//! - `Organizations.assignableRoles` / `Projects.assignableRoles`: the role codes the requesting
//!   principal may assign there. Only a platform administrator is offered `DEPLOYMENT_APPROVER`.
//! - `Organizations.availablePrincipals` / `Projects.availablePrincipals`: the principals that
//!   hold or held a membership of the (owning) organization, the only ones a membership can be
//!   added for. Answered only to a holder of `ORGANIZATION_MEMBERSHIP.VIEW` /
//!   `PROJECT_MEMBERSHIP.VIEW` at that scope; empty otherwise. (The two fields are declared on
//!   the models in `crate::console`, next to `capabilities`, because a model takes one
//!   `#[CustomFields]` block.)
//! - `OrganizationMemberships.roleCodes` / `ProjectMemberships.roleCodes`: the membership's role
//!   codes, sorted.
//! - `OrganizationMemberships.projectAccessSummary`: one line per project of the organization the
//!   member has an active membership of, led by the all-projects line of an organization admin.
//! - `ProjectBudgetPolicies.currentVersion` and `status`: the version in force (none before the
//!   first), and the informational budget status against the newest frozen spend import batch
//!   that covers the current UTC month.
//! - `ProjectApprovalPolicies.currentVersion`; `ProjectApprovalPolicyVersions.rules`: the stored
//!   matrix as a list of cells.

#![allow(non_snake_case)] // a computed field is named after its method

use super::rows;
use crate::capability::{self, Scope};
use crate::console::requester;
use crate::entity::{
    frozen_spend_import_batches, organization_memberships, principals, project_approval_policies,
    project_approval_policy_versions, project_budget_policies, project_budget_policy_versions,
    project_memberships, projects,
};
use hive_application::administration::rules::{
    self, budget_status, utc_month_start, BudgetPolicyFacts, SpendBatchFacts,
};
use hive_application::administration::AdministrationScope;
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, QueryTrait,
};
// `#[CustomFields]` and `CustomOutputType` expand to paths that start with `async_graphql::`.
use seaography::async_graphql::{self, Context};
use seaography::{CustomFields, CustomOutputType};
use uuid::Uuid;

/// One cell of an approval policy matrix.
#[derive(CustomOutputType, Clone)]
pub struct ApprovalPolicyRule {
    /// `DEVELOPMENT_LOW` … `PRODUCTION_HIGH`.
    pub cell: String,
    pub requiredEvidence: Vec<String>,
    pub requiredApprovers: i32,
}

/// The informational budget status of a project; it never authorizes or blocks work.
#[derive(CustomOutputType, Clone)]
pub struct ProjectBudgetStatus {
    /// `NOT_CONFIGURED`, `UNKNOWN`, `NORMAL`, `WARNING` or `EXCEEDED`.
    pub state: String,
    pub reason: Option<String>,
    pub amountCents: Option<i32>,
    pub includesEstimates: bool,
    pub currency: Option<String>,
    pub periodStart: Option<String>,
    pub periodEnd: Option<String>,
    pub dataAsOf: Option<String>,
    pub lastSuccessfulImportAt: Option<String>,
}

pub async fn assignable_organization_roles() -> Vec<String> {
    rules::assignable_organization_roles()
}

pub async fn assignable_project_roles(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
) -> Result<Vec<String>, DbErr> {
    let platform_administrator = capability::is_platform_administrator(db, principal_id).await?;
    Ok(rules::assignable_project_roles(platform_administrator))
}

/// Every principal with a membership of the organization, current or ended, by name.
async fn organization_principals(
    db: &impl ConnectionTrait,
    organization_id: Uuid,
) -> Result<Vec<principals::Model>, DbErr> {
    principals::Entity::find()
        .filter(
            principals::Column::Id.in_subquery(
                organization_memberships::Entity::find()
                    .select_only()
                    .column(organization_memberships::Column::PrincipalId)
                    .filter(organization_memberships::Column::OrganizationId.eq(organization_id))
                    .into_query(),
            ),
        )
        .order_by_asc(principals::Column::DisplayName)
        .order_by_asc(principals::Column::Id)
        .all(db)
        .await
}

pub async fn available_organization_principals(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    organization_id: Uuid,
) -> Result<Vec<principals::Model>, DbErr> {
    let may_view = capability::has_capability(
        db,
        principal_id,
        "ORGANIZATION_MEMBERSHIP.VIEW",
        Scope::Organization(organization_id),
        false,
    )
    .await?;
    if !may_view {
        return Ok(Vec::new());
    }
    organization_principals(db, organization_id).await
}

pub async fn available_project_principals(
    db: &impl ConnectionTrait,
    principal_id: Uuid,
    project: &projects::Model,
) -> Result<Vec<principals::Model>, DbErr> {
    let may_view = capability::has_capability(
        db,
        principal_id,
        "PROJECT_MEMBERSHIP.VIEW",
        Scope::Project(project.id),
        false,
    )
    .await?;
    if !may_view {
        return Ok(Vec::new());
    }
    organization_principals(db, project.organization_id).await
}

#[CustomFields]
impl organization_memberships::Model {
    /// The membership's role codes, sorted.
    pub async fn roleCodes(&self, ctx: &Context<'_>) -> async_graphql::Result<Vec<String>> {
        let (_, db) = requester(ctx)?;
        Ok(rows::current_roles(db, AdministrationScope::Organization, self.id).await?)
    }

    /// The member's access to the organization's projects, one line each.
    pub async fn projectAccessSummary(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<String>> {
        let (_, db) = requester(ctx)?;
        let organization_roles =
            rows::current_roles(db, AdministrationScope::Organization, self.id).await?;
        let mut summaries = Vec::new();
        if organization_roles
            .iter()
            .any(|role| role == "ORGANIZATION_ADMIN")
        {
            summaries.push("All organization projects (ORGANIZATION_ADMIN)".to_string());
        }
        let memberships = project_memberships::Entity::find()
            .find_also_related(projects::Entity)
            .filter(project_memberships::Column::PrincipalId.eq(self.principal_id))
            .filter(projects::Column::OrganizationId.eq(self.organization_id))
            .filter(rows::started(project_memberships::Column::StartedAt))
            .filter(project_memberships::Column::EndedAt.is_null())
            .order_by_asc(projects::Column::DisplayName)
            .order_by_asc(projects::Column::Id)
            .all(db)
            .await?;
        for (membership, project) in memberships {
            let Some(project) = project else { continue };
            let roles =
                rows::current_roles(db, AdministrationScope::Project, membership.id).await?;
            summaries.push(format!("{} ({})", project.display_name, roles.join(", ")));
        }
        Ok(summaries)
    }
}

#[CustomFields]
impl project_memberships::Model {
    /// The membership's role codes, sorted.
    pub async fn roleCodes(&self, ctx: &Context<'_>) -> async_graphql::Result<Vec<String>> {
        let (_, db) = requester(ctx)?;
        Ok(rows::current_roles(db, AdministrationScope::Project, self.id).await?)
    }
}

fn instant(value: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    value.map(|at| at.to_rfc3339())
}

#[CustomFields]
impl project_budget_policies::Model {
    /// The policy version in force; none before the first version.
    pub async fn currentVersion(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<project_budget_policy_versions::Model>> {
        let (_, db) = requester(ctx)?;
        Ok(rows::budget_version(db, self.project_id, self.current_revision).await?)
    }

    /// The budget status for the current UTC month.
    pub async fn status(&self, ctx: &Context<'_>) -> async_graphql::Result<ProjectBudgetStatus> {
        let (_, db) = requester(ctx)?;
        let version = rows::budget_version(db, self.project_id, self.current_revision).await?;
        let now = chrono::Utc::now();
        let batch = match &version {
            None => None,
            Some(_) => {
                let month_start = utc_month_start(now);
                frozen_spend_import_batches::Entity::find()
                    .filter(frozen_spend_import_batches::Column::ProjectId.eq(self.project_id))
                    .filter(frozen_spend_import_batches::Column::PeriodStart.lte(month_start))
                    .filter(frozen_spend_import_batches::Column::PeriodEnd.gt(month_start))
                    .order_by_desc(frozen_spend_import_batches::Column::ImportedAt)
                    .one(db)
                    .await?
            }
        };
        let status = budget_status(
            version.as_ref().map(|version| BudgetPolicyFacts {
                currency: &version.currency,
                monthly_limit_cents: version.monthly_limit_cents,
                warning_threshold_cents: version.warning_threshold_cents,
            }),
            batch.map(|batch| SpendBatchFacts {
                period_start: batch.period_start.into(),
                period_end: batch.period_end.into(),
                currency: batch.currency,
                state: batch.state.to_value(),
                amount_cents: batch.amount_cents,
                includes_estimates: batch.includes_estimates,
                data_as_of: batch.data_as_of.map(Into::into),
                completed_at: batch.completed_at.map(Into::into),
            }),
            now,
        );
        Ok(ProjectBudgetStatus {
            state: status.state,
            reason: status.reason,
            amountCents: status.amount_cents,
            includesEstimates: status.includes_estimates,
            currency: status.currency,
            periodStart: instant(status.period_start),
            periodEnd: instant(status.period_end),
            dataAsOf: instant(status.data_as_of),
            lastSuccessfulImportAt: instant(status.last_successful_import_at),
        })
    }
}

#[CustomFields]
impl project_approval_policies::Model {
    /// The policy version in force.
    pub async fn currentVersion(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Option<project_approval_policy_versions::Model>> {
        let (_, db) = requester(ctx)?;
        Ok(
            project_approval_policy_versions::Entity::find_by_id((self.id, self.current_revision))
                .one(db)
                .await?,
        )
    }
}

#[CustomFields]
impl project_approval_policy_versions::Model {
    /// The matrix, one entry per cell, evidence sorted.
    pub async fn rules(
        &self,
        _ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<ApprovalPolicyRule>> {
        Ok(rows::stored_matrix(&self.matrix)?
            .into_iter()
            .map(|(cell, rule)| ApprovalPolicyRule {
                cell,
                requiredEvidence: rule.required_evidence,
                requiredApprovers: rule.required_approvers,
            })
            .collect())
    }
}
