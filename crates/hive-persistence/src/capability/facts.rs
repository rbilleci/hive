//! The primitive reads the rule makes, answered once per evaluation.
//!
//! Each method here mirrors one `queries` function and takes the same `lock` flag the rule passes.
//! With `lock` set the call goes straight through, so the locked evaluation still issues every
//! statement and takes every row lock in the order `queries` documents. With `lock` unset the
//! answer is remembered, and the role checks are answered from one statement per scope rather than
//! one per role code: the console asks for the whole code set at one scope per row, and a set of
//! codes reads the same rows over and over.
//!
//! Remembering an unlocked answer narrows the snapshot a single evaluation sees, it does not widen
//! it: without this the codes of one set were read across as many snapshots as there were codes.

use super::queries::{self, ScopeKind};
use super::AuthorityAssignment;
use crate::entity::enums::{OrganizationRoleCode, ProjectRoleCode};
use sea_orm::{ConnectionTrait, DbErr};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub(crate) struct Facts<'a, C: ConnectionTrait> {
    db: &'a C,
    principal_id: Uuid,
    known_principal: Option<bool>,
    platform_admin: Option<bool>,
    scope_exists: HashMap<(ScopeKind, Uuid), bool>,
    active_project: HashMap<Uuid, bool>,
    project_organization: HashMap<Uuid, Option<Uuid>>,
    organization_roles: HashMap<Uuid, HashSet<OrganizationRoleCode>>,
    project_roles: HashMap<Uuid, HashSet<ProjectRoleCode>>,
    organization_membership: HashMap<Uuid, bool>,
    organization_membership_for_project: HashMap<Uuid, bool>,
    legacy_console_assignment: HashMap<Uuid, bool>,
    approval_authority: HashMap<Uuid, Vec<AuthorityAssignment>>,
}

impl<'a, C: ConnectionTrait> Facts<'a, C> {
    pub(crate) fn new(db: &'a C, principal_id: Uuid) -> Self {
        Self {
            db,
            principal_id,
            known_principal: None,
            platform_admin: None,
            scope_exists: HashMap::new(),
            active_project: HashMap::new(),
            project_organization: HashMap::new(),
            organization_roles: HashMap::new(),
            project_roles: HashMap::new(),
            organization_membership: HashMap::new(),
            organization_membership_for_project: HashMap::new(),
            legacy_console_assignment: HashMap::new(),
            approval_authority: HashMap::new(),
        }
    }

    pub(crate) fn principal_id(&self) -> Uuid {
        self.principal_id
    }

    pub(crate) fn connection(&self) -> &'a C {
        self.db
    }

    pub(crate) async fn known_principal(&mut self, lock: bool) -> Result<bool, DbErr> {
        if lock {
            return queries::known_principal(self.db, self.principal_id, true).await;
        }
        match self.known_principal {
            Some(known) => Ok(known),
            None => {
                let known = queries::known_principal(self.db, self.principal_id, false).await?;
                self.known_principal = Some(known);
                Ok(known)
            }
        }
    }

    pub(crate) async fn scope_exists(
        &mut self,
        kind: ScopeKind,
        id: Uuid,
        lock: bool,
    ) -> Result<bool, DbErr> {
        if lock {
            return queries::scope_exists(self.db, kind, id, true).await;
        }
        match self.scope_exists.get(&(kind, id)) {
            Some(exists) => Ok(*exists),
            None => {
                let exists = queries::scope_exists(self.db, kind, id, false).await?;
                self.scope_exists.insert((kind, id), exists);
                Ok(exists)
            }
        }
    }

    pub(crate) async fn has_platform_admin(&mut self, lock: bool) -> Result<bool, DbErr> {
        if lock {
            return queries::has_platform_admin(self.db, self.principal_id, true).await;
        }
        match self.platform_admin {
            Some(administrator) => Ok(administrator),
            None => {
                let administrator =
                    queries::has_platform_admin(self.db, self.principal_id, false).await?;
                self.platform_admin = Some(administrator);
                Ok(administrator)
            }
        }
    }

    pub(crate) async fn active_organization_membership(
        &mut self,
        organization_id: Uuid,
        lock: bool,
    ) -> Result<bool, DbErr> {
        if lock {
            return queries::active_organization_membership(
                self.db,
                self.principal_id,
                organization_id,
                true,
            )
            .await;
        }
        match self.organization_membership.get(&organization_id) {
            Some(member) => Ok(*member),
            None => {
                let member = queries::active_organization_membership(
                    self.db,
                    self.principal_id,
                    organization_id,
                    false,
                )
                .await?;
                self.organization_membership.insert(organization_id, member);
                Ok(member)
            }
        }
    }

    pub(crate) async fn has_active_organization_role(
        &mut self,
        organization_id: Uuid,
        role: OrganizationRoleCode,
        lock: bool,
    ) -> Result<bool, DbErr> {
        if lock {
            return queries::has_active_organization_role(
                self.db,
                self.principal_id,
                organization_id,
                role,
                true,
            )
            .await;
        }
        if !self.organization_roles.contains_key(&organization_id) {
            let codes = queries::active_organization_role_codes(
                self.db,
                self.principal_id,
                organization_id,
            )
            .await?;
            self.organization_roles.insert(organization_id, codes);
        }
        Ok(self.organization_roles[&organization_id].contains(&role))
    }

    pub(crate) async fn has_active_project_role(
        &mut self,
        project_id: Uuid,
        role: ProjectRoleCode,
        lock: bool,
    ) -> Result<bool, DbErr> {
        if lock {
            return queries::has_active_project_role(
                self.db,
                self.principal_id,
                project_id,
                role,
                true,
            )
            .await;
        }
        if !self.project_roles.contains_key(&project_id) {
            let codes =
                queries::active_project_role_codes(self.db, self.principal_id, project_id).await?;
            self.project_roles.insert(project_id, codes);
        }
        Ok(self.project_roles[&project_id].contains(&role))
    }

    /// The membership of the project's organization, read through the project row.
    async fn active_organization_for_project(
        &mut self,
        project_id: Uuid,
        lock: bool,
    ) -> Result<bool, DbErr> {
        if lock {
            return queries::active_organization_for_project(
                self.db,
                self.principal_id,
                project_id,
                true,
            )
            .await;
        }
        match self.organization_membership_for_project.get(&project_id) {
            Some(member) => Ok(*member),
            None => {
                let member = queries::active_organization_for_project(
                    self.db,
                    self.principal_id,
                    project_id,
                    false,
                )
                .await?;
                self.organization_membership_for_project
                    .insert(project_id, member);
                Ok(member)
            }
        }
    }

    pub(crate) async fn project_visible(
        &mut self,
        project_id: Uuid,
        lock: bool,
    ) -> Result<bool, DbErr> {
        Ok(self
            .active_organization_for_project(project_id, lock)
            .await?
            || self
                .has_active_project_role(project_id, ProjectRoleCode::ProjectAdmin, lock)
                .await?
            || self
                .has_active_project_role(project_id, ProjectRoleCode::AgentDeveloper, lock)
                .await?
            || self
                .has_active_project_role(project_id, ProjectRoleCode::Operator, lock)
                .await?
            || self
                .has_active_project_role(project_id, ProjectRoleCode::DeploymentApprover, lock)
                .await?
            || self
                .has_active_project_role(project_id, ProjectRoleCode::Auditor, lock)
                .await?
            || self.has_platform_admin(lock).await?)
    }

    pub(crate) async fn organization_visible(
        &mut self,
        organization_id: Uuid,
        lock: bool,
    ) -> Result<bool, DbErr> {
        Ok(self
            .active_organization_membership(organization_id, lock)
            .await?
            || self.has_platform_admin(lock).await?)
    }

    pub(crate) async fn legacy_or_developer(
        &mut self,
        project_id: Uuid,
        lock: bool,
    ) -> Result<bool, DbErr> {
        Ok(self.legacy_console_assignment(project_id, lock).await?
            || self
                .has_active_project_role(project_id, ProjectRoleCode::ProjectAdmin, lock)
                .await?
            || self
                .has_active_project_role(project_id, ProjectRoleCode::AgentDeveloper, lock)
                .await?)
    }

    async fn legacy_console_assignment(
        &mut self,
        project_id: Uuid,
        lock: bool,
    ) -> Result<bool, DbErr> {
        if lock {
            return queries::legacy_console_assignment(
                self.db,
                self.principal_id,
                project_id,
                true,
            )
            .await;
        }
        match self.legacy_console_assignment.get(&project_id) {
            Some(assigned) => Ok(*assigned),
            None => {
                let assigned = queries::legacy_console_assignment(
                    self.db,
                    self.principal_id,
                    project_id,
                    false,
                )
                .await?;
                self.legacy_console_assignment.insert(project_id, assigned);
                Ok(assigned)
            }
        }
    }

    pub(crate) async fn project_organization(
        &mut self,
        project_id: Uuid,
        lock: bool,
    ) -> Result<Option<Uuid>, DbErr> {
        if lock {
            return queries::project_organization(self.db, project_id, true).await;
        }
        match self.project_organization.get(&project_id) {
            Some(organization) => Ok(*organization),
            None => {
                let organization =
                    queries::project_organization(self.db, project_id, false).await?;
                self.project_organization.insert(project_id, organization);
                Ok(organization)
            }
        }
    }

    pub(crate) async fn active_project(
        &mut self,
        project_id: Uuid,
        lock: bool,
    ) -> Result<bool, DbErr> {
        if lock {
            return queries::active_project(self.db, project_id, true).await;
        }
        match self.active_project.get(&project_id) {
            Some(active) => Ok(*active),
            None => {
                let active = queries::active_project(self.db, project_id, false).await?;
                self.active_project.insert(project_id, active);
                Ok(active)
            }
        }
    }

    /// The principal's approval authority scoped to one project, never locked: `queries` has no
    /// locking form of it.
    pub(crate) async fn approval_authority(
        &mut self,
        project_id: Uuid,
    ) -> Result<&[AuthorityAssignment], DbErr> {
        if !self.approval_authority.contains_key(&project_id) {
            let assignments =
                queries::authority_assignments(self.db, self.principal_id, None, Some(project_id))
                    .await?;
            self.approval_authority.insert(project_id, assignments);
        }
        Ok(&self.approval_authority[&project_id])
    }
}
