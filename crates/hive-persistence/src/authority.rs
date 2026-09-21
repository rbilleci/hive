//! What a principal may read, loaded once per GraphQL request through the ORM and turned into the
//! row conditions Seaography's `entity_filter` hook applies to every generated query
//! (`docs/idiomatic-seaography-plan.md`, A3).
//!
//! Visibility rules (from the capability evaluator): an organization is visible to its active
//! members; a project and everything under it is visible to active members of its organization
//! (every project role also requires that membership); a platform administrator sees everything.
//! The catalog is one shared set of rows with no owner: `CATALOG.VIEW` is held at an organization
//! by its active members and by a platform administrator, so the catalog is visible to a principal
//! who is an active member of any organization, or a platform administrator.
//!
//! Administration rows follow the evaluator's view capabilities, not plain membership:
//! organization memberships and their roles need `ORGANIZATION_MEMBERSHIP.VIEW` (an active
//! `ORGANIZATION_ADMIN`); project memberships, their roles and the budget policy need
//! `PROJECT_MEMBERSHIP.VIEW` / `PROJECT_BUDGET.VIEW` (an active `ORGANIZATION_ADMIN` or `AUDITOR`
//! of the project's organization, or an active `PROJECT_ADMIN` or `AUDITOR` of the project); the
//! approval policy needs `PROJECT_APPROVAL_POLICY.VIEW` (the same, plus `DEPLOYMENT_APPROVER`). A
//! platform administrator holds all of them. Settings connections follow the project. A principal
//! row is visible to itself and to whoever may view a membership it holds.
//!
//! An audit event is visible to whoever holds `AUDIT.VIEW` at its scope and, for a project-level
//! grant, the view capability the event names (`required_capability`); see
//! `Authority::audit_event_projection`.
//!
//! Deployment approval rows follow `capability::deployment_approval_capabilities`: a requirement
//! needs `DEPLOYMENT_APPROVAL.VIEW` at its project (a platform administrator, an active
//! `ORGANIZATION_ADMIN`/`AUDITOR` of the owning organization, or an active
//! `PROJECT_ADMIN`/`DEPLOYMENT_APPROVER`/`AUDITOR` of the project), and a recorded decision follows
//! its requirement.
//!
//! Evaluation rows follow `capability::evaluation_capabilities` exactly: a definition needs
//! `EVALUATION_DEFINITION.VIEW` at its project, a run needs `EVALUATION_RUN.VIEW`, a target
//! projection needs `EVALUATION_RUN.RUN`, and everything under a definition or a run follows its
//! parent. The `effective_evaluation_capabilities` view is *not* the source of those sets: it
//! reads only `project_memberships`, so it ignores the platform role, the organization roles
//! (`ORGANIZATION_ADMIN` / `AUDITOR`), the project's lifecycle status and whether the owning
//! organization membership is still active — four conditions the evaluator decides on.

use crate::capability::queries::{active_organization_roles, active_project_roles};
use crate::capability::{
    AGENT_VIEW, CONFIGURATION_VIEW, DEPLOYMENT_VIEW, EVALUATION_DEFINITION_VIEW,
    EVALUATION_RUN_VIEW, ORGANIZATION_VIEW, PROJECT_VIEW,
};
use crate::entity::enums::{
    LifecycleStatus, OrganizationRoleCode, PlatformRoleCode, ProjectRoleCode,
};
use crate::entity::{
    agent_drafts, agent_operational_view_projection, agent_versions, agents,
    audit_event_projection, deployment_approval_decisions, deployment_approval_requirements,
    deployment_attempts, deployment_evidence_invalidations, deployment_evidence_snapshots,
    deployment_plan_review_facts, deployment_plan_versions, deployment_policy_snapshots,
    deployment_promotion_facts, deployment_runtime_health, deployment_stage_events, deployments,
    evaluation_artifact_metadata, evaluation_audit_events, evaluation_case_runs,
    evaluation_definition_drafts, evaluation_definition_versions, evaluation_definitions,
    evaluation_metric_results, evaluation_results, evaluation_runs, evaluation_target_projections,
    evaluation_target_snapshots, frozen_spend_import_batches, organization_membership_roles,
    organization_memberships, organizations, platform_role_assignments,
    principal_display_preferences, principals, project_approval_policies,
    project_approval_policy_versions, project_budget_policies, project_budget_policy_versions,
    project_dashboard_projection, project_membership_roles, project_memberships,
    project_settings_connections, project_tool_connections, projects, reusable_resource_drafts,
    reusable_resource_versions, reusable_resources,
};
use sea_orm::sea_query::{Expr, ExprTrait, SelectStatement};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect,
    QueryTrait,
};
use uuid::Uuid;

/// The requesting principal's read authority, inserted into the GraphQL request data by the
/// `/graphql` handler. `None` when it could not be loaded. It lives here, not in `hive-api`,
/// because the entities' computed fields read it too.
pub struct RequestAuthority(pub Option<Authority>);

#[derive(Debug, Clone)]
pub struct Authority {
    pub principal_id: Uuid,
    pub platform_admin: bool,
    pub organization_ids: Vec<Uuid>,
}

impl Authority {
    pub async fn load(db: &impl ConnectionTrait, principal_id: Uuid) -> Result<Self, DbErr> {
        let platform_admin = platform_role_assignments::Entity::find_by_id((
            principal_id,
            PlatformRoleCode::PlatformAdmin,
        ))
        .one(db)
        .await?
        .is_some();
        let organization_ids = organization_memberships::Entity::find()
            .select_only()
            .column(organization_memberships::Column::OrganizationId)
            .filter(organization_memberships::Column::PrincipalId.eq(principal_id))
            .filter(
                Expr::col(organization_memberships::Column::StartedAt)
                    .lte(Expr::current_timestamp()),
            )
            .filter(organization_memberships::Column::EndedAt.is_null())
            .into_tuple::<Uuid>()
            .all(db)
            .await?;
        Ok(Self {
            principal_id,
            platform_admin,
            organization_ids,
        })
    }

    /// The row condition for a generated read of `entity` (Seaography's GraphQL type name), or
    /// `None` when the entity has no rule. Callers must treat `None` as deny.
    pub fn read_condition(&self, entity: &str) -> Option<Condition> {
        let condition = match entity {
            "Organizations" => self.organizations(),
            "Projects" => self.projects(),
            "Agents" => self.agents(),
            "AgentVersions" => self.agent_versions(),
            "AgentDrafts" => self.agent_drafts(),
            "ProjectDashboardProjection" => self.project_dashboard_projection(),
            "AgentOperationalViewProjection" => self.agent_operational_view_projection(),
            "Principals" => self.principals(),
            "PrincipalDisplayPreferences" => self.principal_display_preferences(),
            "CatalogReleases"
            | "CatalogDefinitions"
            | "CatalogEnvironments"
            | "CatalogProjectionHeads" => self.catalog(),
            "ReusableResources" => self.reusable_resources(),
            "ReusableResourceDrafts" => self.reusable_resource_drafts(),
            "ReusableResourceVersions" => self.reusable_resource_versions(),
            "ProjectToolConnections" => self.project_tool_connections(),
            "OrganizationMemberships" => self.organization_memberships(),
            "OrganizationMembershipRoles" => self.organization_membership_roles(),
            "ProjectMemberships" => self.project_memberships(),
            "ProjectMembershipRoles" => self.project_membership_roles(),
            "ProjectBudgetPolicies" => self.project_budget_policies(),
            "ProjectBudgetPolicyVersions" => self.project_budget_policy_versions(),
            "ProjectApprovalPolicies" => self.project_approval_policies(),
            "ProjectApprovalPolicyVersions" => self.project_approval_policy_versions(),
            "ProjectSettingsConnections" => self.project_settings_connections(),
            "AuditEventProjection" => self.audit_event_projection(),
            "EvaluationDefinitions" => self.evaluation_definitions(),
            "EvaluationDefinitionDrafts" => self.evaluation_definition_drafts(),
            "EvaluationDefinitionVersions" => self.evaluation_definition_versions(),
            "EvaluationRuns" => self.evaluation_runs(),
            "EvaluationCaseRuns" => self.evaluation_case_runs(),
            "EvaluationMetricResults" => self.evaluation_metric_results(),
            "EvaluationArtifactMetadata" => self.evaluation_artifact_metadata(),
            "EvaluationAuditEvents" => self.evaluation_audit_events(),
            "EvaluationTargetSnapshots" => self.evaluation_target_snapshots(),
            "EvaluationTargetProjections" => self.evaluation_target_projections(),
            "Deployments" => self.deployments(),
            "DeploymentAttempts" => self.deployment_attempts(),
            "DeploymentPlanVersions" => self.deployment_plan_versions(),
            "DeploymentPlanReviewFacts" => self.deployment_plan_review_facts(),
            "DeploymentPolicySnapshots" => self.deployment_policy_snapshots(),
            "DeploymentRuntimeHealth" => self.deployment_runtime_health(),
            "DeploymentEvidenceSnapshots" => self.deployment_evidence_snapshots(),
            "DeploymentApprovalRequirements" => self.deployment_approval_requirements(),
            "DeploymentApprovalDecisions" => self.deployment_approval_decisions(),
            "DeploymentStageEvents" => self.deployment_stage_events(),
            "DeploymentPromotionFacts" => self.deployment_promotion_facts(),
            "DeploymentEvidenceInvalidations" => self.deployment_evidence_invalidations(),
            "EvaluationResults" => self.evaluation_results(),
            "FrozenSpendImportBatches" => self.frozen_spend_import_batches(),
            "EnvironmentDefinitionVersions" => self.catalog(),
            _ => return None,
        };
        Some(condition)
    }

    fn organizations(&self) -> Condition {
        self.unless_platform_admin(|| {
            organizations::Column::Id.is_in(self.organization_ids.clone())
        })
    }

    fn projects(&self) -> Condition {
        self.unless_platform_admin(|| {
            projects::Column::OrganizationId.is_in(self.organization_ids.clone())
        })
    }

    fn agents(&self) -> Condition {
        self.unless_platform_admin(|| agents::Column::ProjectId.in_subquery(self.project_ids()))
    }

    fn agent_versions(&self) -> Condition {
        self.unless_platform_admin(|| {
            agent_versions::Column::AgentId.in_subquery(
                agents::Entity::find()
                    .select_only()
                    .column(agents::Column::Id)
                    .filter(agents::Column::ProjectId.in_subquery(self.project_ids()))
                    .into_query(),
            )
        })
    }

    /// A draft is visible with its agent.
    fn agent_drafts(&self) -> Condition {
        self.unless_platform_admin(|| {
            agent_drafts::Column::AgentId.in_subquery(
                agents::Entity::find()
                    .select_only()
                    .column(agents::Column::Id)
                    .filter(agents::Column::ProjectId.in_subquery(self.project_ids()))
                    .into_query(),
            )
        })
    }

    fn project_dashboard_projection(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_dashboard_projection::Column::OrganizationId
                .is_in(self.organization_ids.clone())
        })
    }

    fn agent_operational_view_projection(&self) -> Condition {
        self.unless_platform_admin(|| {
            agent_operational_view_projection::Column::OrganizationId
                .is_in(self.organization_ids.clone())
        })
    }

    /// A principal reads its own row, and the row of a principal whose membership it may view:
    /// a member (current or former) of an organization where it holds
    /// `ORGANIZATION_MEMBERSHIP.VIEW`, or of a project where it holds `PROJECT_MEMBERSHIP.VIEW`.
    /// That is what the administration member lists show (name, email, last seen). A platform
    /// administrator holds both capabilities everywhere, so it reads every principal that has a
    /// membership; a principal with no membership stays visible to itself only. The principals an
    /// administrator may add are the computed `availablePrincipals` field, not this rule.
    fn principals(&self) -> Condition {
        let organization_members = organization_memberships::Entity::find()
            .select_only()
            .column(organization_memberships::Column::PrincipalId);
        let project_members = project_memberships::Entity::find()
            .select_only()
            .column(project_memberships::Column::PrincipalId);
        let (organization_members, project_members) = if self.platform_admin {
            (organization_members, project_members)
        } else {
            (
                organization_members.filter(
                    organization_memberships::Column::OrganizationId
                        .in_subquery(self.administered_organization_ids()),
                ),
                project_members.filter(
                    project_memberships::Column::ProjectId
                        .in_subquery(self.membership_view_project_ids()),
                ),
            )
        };
        Condition::any()
            .add(principals::Column::Id.eq(self.principal_id))
            .add(principals::Column::Id.in_subquery(organization_members.into_query()))
            .add(principals::Column::Id.in_subquery(project_members.into_query()))
    }

    fn principal_display_preferences(&self) -> Condition {
        Condition::all()
            .add(principal_display_preferences::Column::PrincipalId.eq(self.principal_id))
    }

    /// The catalog has no owner; whoever holds `CATALOG.VIEW` at any organization reads all of it.
    fn catalog(&self) -> Condition {
        if self.platform_admin || !self.organization_ids.is_empty() {
            Condition::all()
        } else {
            deny_all()
        }
    }

    fn reusable_resources(&self) -> Condition {
        self.unless_platform_admin(|| {
            reusable_resources::Column::ProjectId.in_subquery(self.project_ids())
        })
    }

    /// A draft revision is visible with its resource.
    fn reusable_resource_drafts(&self) -> Condition {
        self.unless_platform_admin(|| {
            reusable_resource_drafts::Column::ResourceId.in_subquery(self.resource_ids())
        })
    }

    /// A published version is visible with its resource.
    fn reusable_resource_versions(&self) -> Condition {
        self.unless_platform_admin(|| {
            reusable_resource_versions::Column::ResourceId.in_subquery(self.resource_ids())
        })
    }

    /// The MCP server descriptors of a project.
    fn project_tool_connections(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_tool_connections::Column::ProjectId.in_subquery(self.project_ids())
        })
    }

    /// `ORGANIZATION_MEMBERSHIP.VIEW`.
    fn organization_memberships(&self) -> Condition {
        self.unless_platform_admin(|| {
            organization_memberships::Column::OrganizationId
                .in_subquery(self.administered_organization_ids())
        })
    }

    /// A role is visible with its membership.
    fn organization_membership_roles(&self) -> Condition {
        self.unless_platform_admin(|| {
            organization_membership_roles::Column::MembershipId.in_subquery(
                organization_memberships::Entity::find()
                    .select_only()
                    .column(organization_memberships::Column::Id)
                    .filter(
                        organization_memberships::Column::OrganizationId
                            .in_subquery(self.administered_organization_ids()),
                    )
                    .into_query(),
            )
        })
    }

    /// `PROJECT_MEMBERSHIP.VIEW`.
    fn project_memberships(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_memberships::Column::ProjectId.in_subquery(self.membership_view_project_ids())
        })
    }

    /// A role is visible with its membership.
    fn project_membership_roles(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_membership_roles::Column::MembershipId.in_subquery(
                project_memberships::Entity::find()
                    .select_only()
                    .column(project_memberships::Column::Id)
                    .filter(
                        project_memberships::Column::ProjectId
                            .in_subquery(self.membership_view_project_ids()),
                    )
                    .into_query(),
            )
        })
    }

    /// `PROJECT_BUDGET.VIEW`, which the evaluator grants to the same roles as
    /// `PROJECT_MEMBERSHIP.VIEW`.
    fn project_budget_policies(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_budget_policies::Column::ProjectId
                .in_subquery(self.membership_view_project_ids())
        })
    }

    fn project_budget_policy_versions(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_budget_policy_versions::Column::ProjectId
                .in_subquery(self.membership_view_project_ids())
        })
    }

    /// `PROJECT_APPROVAL_POLICY.VIEW`.
    fn project_approval_policies(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_approval_policies::Column::ProjectId
                .in_subquery(self.approval_policy_view_project_ids())
        })
    }

    /// A policy version is visible with its policy.
    fn project_approval_policy_versions(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_approval_policy_versions::Column::PolicyId.in_subquery(
                project_approval_policies::Entity::find()
                    .select_only()
                    .column(project_approval_policies::Column::Id)
                    .filter(
                        project_approval_policies::Column::ProjectId
                            .in_subquery(self.approval_policy_view_project_ids()),
                    )
                    .into_query(),
            )
        })
    }

    /// Settings connections carry no secret and have no capability of their own; they are
    /// visible with their project, as `projectAdministration.connections` was.
    fn project_settings_connections(&self) -> Condition {
        self.unless_platform_admin(|| {
            project_settings_connections::Column::ProjectId.in_subquery(self.project_ids())
        })
    }

    /// An event is visible to whoever the evaluator grants `AUDIT.VIEW` at the event's scope,
    /// exactly as the deleted `auditEvents` query decided it:
    ///
    /// - a platform administrator reads every event;
    /// - an active `ORGANIZATION_ADMIN` or `AUDITOR` of the event's organization reads every
    ///   event of it, its projects' events included (`AUDIT.VIEW` on an organization is the
    ///   descendant-project grant);
    /// - any active project role grants `AUDIT.VIEW` on the project, and then the event also
    ///   needs the view capability it names. `PROJECT.VIEW`, `AGENT.VIEW`, `CONFIGURATION.VIEW`
    ///   and `DEPLOYMENT.VIEW` come with every project role. `EVALUATION_DEFINITION.VIEW` and
    ///   `EVALUATION_RUN.VIEW` come with `AUDITOR` and `DEPLOYMENT_APPROVER`, and with
    ///   `PROJECT_ADMIN` and `AGENT_DEVELOPER` while the project is active; an `OPERATOR` of an
    ///   active project holds `EVALUATION_RUN.VIEW` only. An organization's own events
    ///   (`ORGANIZATION.VIEW`) have no project, so no project role reaches them.
    ///
    /// An event that names no known capability is visible to nobody, as before.
    fn audit_event_projection(&self) -> Condition {
        use audit_event_projection::Column;
        let known = Column::RequiredCapability.is_in([
            ORGANIZATION_VIEW,
            PROJECT_VIEW,
            AGENT_VIEW,
            CONFIGURATION_VIEW,
            DEPLOYMENT_VIEW,
            EVALUATION_DEFINITION_VIEW,
            EVALUATION_RUN_VIEW,
        ]);
        if self.platform_admin {
            return Condition::all().add(known);
        }
        let evaluation = [EVALUATION_DEFINITION_VIEW, EVALUATION_RUN_VIEW];
        let through_organization =
            Column::OrganizationId.in_subquery(self.organization_ids_with_role(&[
                OrganizationRoleCode::OrganizationAdmin,
                OrganizationRoleCode::Auditor,
            ]));
        let through_project = Condition::all()
            .add(Column::ProjectId.in_subquery(self.project_ids_with_role(
                &[
                    ProjectRoleCode::ProjectAdmin,
                    ProjectRoleCode::AgentDeveloper,
                    ProjectRoleCode::Operator,
                    ProjectRoleCode::DeploymentApprover,
                    ProjectRoleCode::Auditor,
                ],
                false,
            )))
            .add(
                Condition::any()
                    .add(Column::RequiredCapability.is_in([
                        PROJECT_VIEW,
                        AGENT_VIEW,
                        CONFIGURATION_VIEW,
                        DEPLOYMENT_VIEW,
                    ]))
                    .add(
                        Condition::all()
                            .add(Column::RequiredCapability.is_in(evaluation))
                            .add(Column::ProjectId.in_subquery(self.project_ids_with_role(
                                &[
                                    ProjectRoleCode::Auditor,
                                    ProjectRoleCode::DeploymentApprover,
                                ],
                                false,
                            ))),
                    )
                    .add(
                        Condition::all()
                            .add(Column::RequiredCapability.is_in(evaluation))
                            .add(Column::ProjectId.in_subquery(self.project_ids_with_role(
                                &[
                                    ProjectRoleCode::ProjectAdmin,
                                    ProjectRoleCode::AgentDeveloper,
                                ],
                                true,
                            ))),
                    )
                    .add(
                        Condition::all()
                            .add(Column::RequiredCapability.eq(EVALUATION_RUN_VIEW))
                            .add(Column::ProjectId.in_subquery(
                                self.project_ids_with_role(&[ProjectRoleCode::Operator], true),
                            )),
                    ),
            );
        Condition::all().add(known).add(
            Condition::any()
                .add(through_organization)
                .add(through_project),
        )
    }

    /// A definition needs `EVALUATION_DEFINITION.VIEW` at its project.
    fn evaluation_definitions(&self) -> Condition {
        Condition::all().add(
            evaluation_definitions::Column::ProjectId
                .in_subquery(self.evaluation_definition_view_project_ids()),
        )
    }

    /// The draft is the definition's own content; it is visible with the definition.
    fn evaluation_definition_drafts(&self) -> Condition {
        Condition::all().add(
            evaluation_definition_drafts::Column::DefinitionId
                .in_subquery(self.visible_evaluation_definition_ids()),
        )
    }

    /// A published version is visible with its definition.
    fn evaluation_definition_versions(&self) -> Condition {
        Condition::all().add(
            evaluation_definition_versions::Column::DefinitionId
                .in_subquery(self.visible_evaluation_definition_ids()),
        )
    }

    /// A run needs `EVALUATION_RUN.VIEW` at its project.
    fn evaluation_runs(&self) -> Condition {
        Condition::all().add(
            evaluation_runs::Column::ProjectId.in_subquery(self.evaluation_run_view_project_ids()),
        )
    }

    fn evaluation_case_runs(&self) -> Condition {
        Condition::all()
            .add(evaluation_case_runs::Column::RunId.in_subquery(self.visible_evaluation_run_ids()))
    }

    fn evaluation_metric_results(&self) -> Condition {
        Condition::all().add(
            evaluation_metric_results::Column::RunId.in_subquery(self.visible_evaluation_run_ids()),
        )
    }

    fn evaluation_artifact_metadata(&self) -> Condition {
        Condition::all().add(
            evaluation_artifact_metadata::Column::RunId
                .in_subquery(self.visible_evaluation_run_ids()),
        )
    }

    /// A run's audit trail is visible with the run. A definition-scoped event carries no `run_id`
    /// and is read through `auditEventProjection`, as it was before this entity was generated.
    fn evaluation_audit_events(&self) -> Condition {
        Condition::all().add(
            evaluation_audit_events::Column::RunId.in_subquery(self.visible_evaluation_run_ids()),
        )
    }

    /// The frozen target of a run, visible with it.
    fn evaluation_target_snapshots(&self) -> Condition {
        Condition::all().add(
            evaluation_target_snapshots::Column::RunId
                .in_subquery(self.visible_evaluation_run_ids()),
        )
    }

    /// A candidate target row is what `EVALUATION_RUN.RUN` may queue against, which is the
    /// capability the deleted `evaluationTargets` query required.
    fn evaluation_target_projections(&self) -> Condition {
        Condition::all().add(
            evaluation_target_projections::Column::ProjectId
                .in_subquery(self.evaluation_run_project_ids()),
        )
    }

    /// A deployment needs `DEPLOYMENT.VIEW` at its project.
    fn deployments(&self) -> Condition {
        Condition::all()
            .add(deployments::Column::ProjectId.in_subquery(self.deployment_view_project_ids()))
    }

    /// An execution attempt is visible with its deployment.
    fn deployment_attempts(&self) -> Condition {
        Condition::all().add(
            deployment_attempts::Column::DeploymentId.in_subquery(self.visible_deployment_ids()),
        )
    }

    /// The frozen plan is visible with its deployment.
    fn deployment_plan_versions(&self) -> Condition {
        Condition::all().add(
            deployment_plan_versions::Column::DeploymentId
                .in_subquery(self.visible_deployment_ids()),
        )
    }

    /// The retained review facts are visible with their plan.
    fn deployment_plan_review_facts(&self) -> Condition {
        Condition::all().add(
            deployment_plan_review_facts::Column::PlanId.in_subquery(
                deployment_plan_versions::Entity::find()
                    .select_only()
                    .column(deployment_plan_versions::Column::Id)
                    .filter(
                        deployment_plan_versions::Column::DeploymentId
                            .in_subquery(self.visible_deployment_ids()),
                    )
                    .into_query(),
            ),
        )
    }

    /// The frozen policy is visible with its deployment.
    fn deployment_policy_snapshots(&self) -> Condition {
        Condition::all().add(
            deployment_policy_snapshots::Column::DeploymentId
                .in_subquery(self.visible_deployment_ids()),
        )
    }

    /// Observed runtime health is visible with its deployment.
    fn deployment_runtime_health(&self) -> Condition {
        Condition::all().add(
            deployment_runtime_health::Column::DeploymentId
                .in_subquery(self.visible_deployment_ids()),
        )
    }

    /// A frozen evidence snapshot is visible with its deployment.
    fn deployment_evidence_snapshots(&self) -> Condition {
        Condition::all().add(
            deployment_evidence_snapshots::Column::DeploymentId
                .in_subquery(self.visible_deployment_ids()),
        )
    }

    /// A stage event is visible with the attempt it belongs to, and so with its deployment.
    fn deployment_stage_events(&self) -> Condition {
        Condition::all().add(
            deployment_stage_events::Column::DeploymentAttemptId.in_subquery(
                deployment_attempts::Entity::find()
                    .select_only()
                    .column(deployment_attempts::Column::Id)
                    .filter(
                        deployment_attempts::Column::DeploymentId
                            .in_subquery(self.visible_deployment_ids()),
                    )
                    .into_query(),
            ),
        )
    }

    /// A promotion fact is visible with its deployment.
    fn deployment_promotion_facts(&self) -> Condition {
        Condition::all().add(
            deployment_promotion_facts::Column::DeploymentId
                .in_subquery(self.visible_deployment_ids()),
        )
    }

    /// An evidence invalidation is visible with the snapshot it invalidates, and so with its
    /// deployment.
    fn deployment_evidence_invalidations(&self) -> Condition {
        Condition::all().add(
            deployment_evidence_invalidations::Column::EvidenceSnapshotId.in_subquery(
                deployment_evidence_snapshots::Entity::find()
                    .select_only()
                    .column(deployment_evidence_snapshots::Column::Id)
                    .filter(
                        deployment_evidence_snapshots::Column::DeploymentId
                            .in_subquery(self.visible_deployment_ids()),
                    )
                    .into_query(),
            ),
        )
    }

    /// A run's terminal result row is visible with the run.
    fn evaluation_results(&self) -> Condition {
        Condition::all()
            .add(evaluation_results::Column::RunId.in_subquery(self.visible_evaluation_run_ids()))
    }

    /// An imported spend batch is money, so it follows `PROJECT_BUDGET.VIEW`, the same rule the
    /// budget policy and its versions use — and the same rows the computed
    /// `ProjectBudgetPolicies.status` already reports in aggregate.
    fn frozen_spend_import_batches(&self) -> Condition {
        self.unless_platform_admin(|| {
            frozen_spend_import_batches::Column::ProjectId
                .in_subquery(self.membership_view_project_ids())
        })
    }

    /// An approval requirement needs `DEPLOYMENT_APPROVAL.VIEW` at its project. The deleted
    /// `approvalInbox` union over the approval scope caches was a *candidate generator*, not a
    /// visibility rule: every candidate it produced was rechecked with
    /// `capability::deployment_approval_capabilities`, which is exactly this condition, so the
    /// union adds no row this rule does not grant and loses none it does.
    fn deployment_approval_requirements(&self) -> Condition {
        Condition::all().add(
            deployment_approval_requirements::Column::ProjectId
                .in_subquery(self.deployment_approval_view_project_ids()),
        )
    }

    /// A recorded decision is visible with the requirement it was recorded against.
    fn deployment_approval_decisions(&self) -> Condition {
        Condition::all().add(
            deployment_approval_decisions::Column::ApprovalRequirementId.in_subquery(
                deployment_approval_requirements::Entity::find()
                    .select_only()
                    .column(deployment_approval_requirements::Column::Id)
                    .filter(
                        deployment_approval_requirements::Column::ProjectId
                            .in_subquery(self.deployment_approval_view_project_ids()),
                    )
                    .into_query(),
            ),
        )
    }

    /// The projects where the principal holds `DEPLOYMENT_APPROVAL.VIEW`, reproducing
    /// `capability::deployment_approval_capabilities`: a platform administrator everywhere; an
    /// active `ORGANIZATION_ADMIN` or `AUDITOR` of the owning organization; or an active
    /// `PROJECT_ADMIN`, `DEPLOYMENT_APPROVER` or `AUDITOR` of the project, while the membership of
    /// the owning organization is active too. Narrower than `DEPLOYMENT.VIEW`, which also comes
    /// with `AGENT_DEVELOPER` and `OPERATOR`.
    fn deployment_approval_view_project_ids(&self) -> SelectStatement {
        if self.platform_admin {
            return self.projects_where(Condition::all());
        }
        self.project_ids_viewed_through(&[
            ProjectRoleCode::ProjectAdmin,
            ProjectRoleCode::DeploymentApprover,
            ProjectRoleCode::Auditor,
        ])
    }

    fn visible_deployment_ids(&self) -> SelectStatement {
        deployments::Entity::find()
            .select_only()
            .column(deployments::Column::Id)
            .filter(deployments::Column::ProjectId.in_subquery(self.deployment_view_project_ids()))
            .into_query()
    }

    /// The projects where the principal holds `DEPLOYMENT.VIEW`, reproducing the reader half of
    /// `capability::deployment_capabilities`: a platform administrator everywhere; an active
    /// `ORGANIZATION_ADMIN` or `AUDITOR` of the owning organization; or any of the five project
    /// roles, while the membership of the owning organization is active too. The project's own
    /// lifecycle status does not narrow the view capability.
    fn deployment_view_project_ids(&self) -> SelectStatement {
        if self.platform_admin {
            return self.projects_where(Condition::all());
        }
        self.project_ids_viewed_through(&[
            ProjectRoleCode::ProjectAdmin,
            ProjectRoleCode::AgentDeveloper,
            ProjectRoleCode::Operator,
            ProjectRoleCode::DeploymentApprover,
            ProjectRoleCode::Auditor,
        ])
    }

    fn visible_evaluation_definition_ids(&self) -> SelectStatement {
        evaluation_definitions::Entity::find()
            .select_only()
            .column(evaluation_definitions::Column::Id)
            .filter(
                evaluation_definitions::Column::ProjectId
                    .in_subquery(self.evaluation_definition_view_project_ids()),
            )
            .into_query()
    }

    fn visible_evaluation_run_ids(&self) -> SelectStatement {
        evaluation_runs::Entity::find()
            .select_only()
            .column(evaluation_runs::Column::Id)
            .filter(
                evaluation_runs::Column::ProjectId
                    .in_subquery(self.evaluation_run_view_project_ids()),
            )
            .into_query()
    }

    /// The projects where the principal holds `EVALUATION_DEFINITION.VIEW`.
    fn evaluation_definition_view_project_ids(&self) -> SelectStatement {
        self.projects_where(self.evaluation_view_condition(false))
    }

    /// The projects where the principal holds `EVALUATION_RUN.VIEW`: the definition-view set plus
    /// an active project's `OPERATOR`, who holds the run capabilities and no definition capability.
    fn evaluation_run_view_project_ids(&self) -> SelectStatement {
        self.projects_where(self.evaluation_view_condition(true))
    }

    /// Ports the view half of `capability::evaluation_capabilities`: a platform administrator sees
    /// every project; an active `ORGANIZATION_ADMIN` or `AUDITOR` sees its organization's
    /// projects; an active project `AUDITOR` or `DEPLOYMENT_APPROVER` sees that project whatever
    /// its lifecycle status; `PROJECT_ADMIN`, `AGENT_DEVELOPER` (and, for runs, `OPERATOR`) only
    /// while the project is active.
    fn evaluation_view_condition(&self, include_operator: bool) -> Condition {
        if self.platform_admin {
            return Condition::all();
        }
        let mut held = Condition::any()
            .add(
                projects::Column::OrganizationId.in_subquery(self.organization_ids_with_role(&[
                    OrganizationRoleCode::OrganizationAdmin,
                    OrganizationRoleCode::Auditor,
                ])),
            )
            .add(projects::Column::Id.in_subquery(self.project_ids_with_role(
                &[
                    ProjectRoleCode::Auditor,
                    ProjectRoleCode::DeploymentApprover,
                ],
                false,
            )))
            .add(projects::Column::Id.in_subquery(self.project_ids_with_role(
                &[
                    ProjectRoleCode::ProjectAdmin,
                    ProjectRoleCode::AgentDeveloper,
                ],
                true,
            )));
        if include_operator {
            held = held.add(
                projects::Column::Id
                    .in_subquery(self.project_ids_with_role(&[ProjectRoleCode::Operator], true)),
            );
        }
        held
    }

    /// The projects where the principal holds `EVALUATION_RUN.RUN`. The evaluator grants no
    /// evaluation write capability on an inactive project, a platform administrator included.
    fn evaluation_run_project_ids(&self) -> SelectStatement {
        let active = projects::Column::LifecycleStatus.eq(LifecycleStatus::Active);
        let held = if self.platform_admin {
            Condition::all().add(active)
        } else {
            Condition::all()
                .add(active)
                .add(projects::Column::Id.in_subquery(self.project_ids_with_role(
                    &[
                        ProjectRoleCode::ProjectAdmin,
                        ProjectRoleCode::AgentDeveloper,
                        ProjectRoleCode::Operator,
                    ],
                    true,
                )))
        };
        self.projects_where(held)
    }

    fn projects_where(&self, condition: Condition) -> SelectStatement {
        projects::Entity::find()
            .select_only()
            .column(projects::Column::Id)
            .filter(condition)
            .into_query()
    }

    /// The projects where the principal actively holds one of `roles`, optionally only the
    /// projects that are active themselves.
    fn project_ids_with_role(
        &self,
        roles: &[ProjectRoleCode],
        active_only: bool,
    ) -> SelectStatement {
        let mut held = active_project_roles(self.principal_id)
            .select_only()
            .column(project_memberships::Column::ProjectId)
            .filter(project_membership_roles::Column::RoleCode.is_in(roles.iter().copied()));
        if active_only {
            held = held.filter(projects::Column::LifecycleStatus.eq(LifecycleStatus::Active));
        }
        held.into_query()
    }

    /// The organizations where the principal is an active `ORGANIZATION_ADMIN`.
    fn administered_organization_ids(&self) -> SelectStatement {
        self.organization_ids_with_role(&[OrganizationRoleCode::OrganizationAdmin])
    }

    fn organization_ids_with_role(&self, roles: &[OrganizationRoleCode]) -> SelectStatement {
        active_organization_roles(self.principal_id)
            .select_only()
            .column(organization_memberships::Column::OrganizationId)
            .filter(organization_membership_roles::Column::RoleCode.is_in(roles.iter().copied()))
            .into_query()
    }

    /// The projects where the principal holds one of `roles` directly or is an active
    /// `ORGANIZATION_ADMIN` or `AUDITOR` of the owning organization. A project role counts only
    /// while the membership of the owning organization is active too, as in the evaluator.
    fn project_ids_viewed_through(&self, roles: &[ProjectRoleCode]) -> SelectStatement {
        let through_project = active_project_roles(self.principal_id)
            .select_only()
            .column(project_memberships::Column::ProjectId)
            .filter(project_membership_roles::Column::RoleCode.is_in(roles.iter().copied()))
            .into_query();
        let through_organization = self.organization_ids_with_role(&[
            OrganizationRoleCode::OrganizationAdmin,
            OrganizationRoleCode::Auditor,
        ]);
        projects::Entity::find()
            .select_only()
            .column(projects::Column::Id)
            .filter(
                Condition::any()
                    .add(projects::Column::OrganizationId.in_subquery(through_organization))
                    .add(projects::Column::Id.in_subquery(through_project)),
            )
            .into_query()
    }

    fn membership_view_project_ids(&self) -> SelectStatement {
        self.project_ids_viewed_through(&[ProjectRoleCode::ProjectAdmin, ProjectRoleCode::Auditor])
    }

    fn approval_policy_view_project_ids(&self) -> SelectStatement {
        self.project_ids_viewed_through(&[
            ProjectRoleCode::ProjectAdmin,
            ProjectRoleCode::Auditor,
            ProjectRoleCode::DeploymentApprover,
        ])
    }

    /// The ids of every reusable resource in a project the principal can see.
    fn resource_ids(&self) -> SelectStatement {
        reusable_resources::Entity::find()
            .select_only()
            .column(reusable_resources::Column::Id)
            .filter(reusable_resources::Column::ProjectId.in_subquery(self.project_ids()))
            .into_query()
    }

    /// The ids of every project in an organization the principal is an active member of.
    fn project_ids(&self) -> SelectStatement {
        projects::Entity::find()
            .select_only()
            .column(projects::Column::Id)
            .filter(projects::Column::OrganizationId.is_in(self.organization_ids.clone()))
            .into_query()
    }

    fn unless_platform_admin(&self, rule: impl FnOnce() -> Expr) -> Condition {
        if self.platform_admin {
            Condition::all()
        } else {
            Condition::all().add(rule())
        }
    }
}

/// Matches no row: the condition for an entity with no rule, or a request with no authority.
pub fn deny_all() -> Condition {
    Condition::all().add(Expr::val(false))
}
