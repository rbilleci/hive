//! Ports `PostgresConsoleRepository`'s `findContext`: the visible-organizations
//! query (active membership OR platform admin), then a large fan-out of
//! `capability::has_capability`/`evaluation_capabilities`/`deployment_capabilities_many`
//! calls across every visible organization and project, exactly matching Java's
//! `addAdministrationCapabilities`/`addAuditCapabilities`/`addConfigurationCapabilities`
//! plus the direct `PREFERENCES_UPDATE`/`ORGANIZATION_VIEW`/`PROJECT_VIEW`/`AGENT_VIEW`/
//! `AGENT_DRAFT_UPDATE` checks. `JpaConsoleRepository`'s preference read/write
//! (`findPreferences`/`updatePreferences`) is ported alongside it as one repository,
//! matching the two Java classes' combined public surface.
//!
//! `GSR-PERSISTENCE`: every statement here runs through `sea_orm::ConnectionTrait` via
//! `Statement::from_sql_and_values` + `query_one_raw`/`query_all_raw`/`execute_raw` — the same
//! raw-SQL-through-`ConnectionTrait` idiom the migrator (`GSR-PHASE-1`) and `capability`
//! (`GSR-PHASE-P5`) established, preserving every hand-written query's text (and therefore its
//! query plan) verbatim rather than re-deriving it in `Select`/`Condition` builder form. This
//! module's generated entities (`organizations`, `organization_memberships`, `projects`,
//! `platform_role_assignments`, `principals`, `principal_display_preferences`) all declare an
//! empty `Relation` enum (`entity/*.rs` are `sea-orm-codegen`-generated without foreign-key
//! relations configured), so a builder-based join would need the same manual `Select::join`
//! wiring anyway with none of the byte-for-byte parity a preserved raw statement gives for free.

use crate::capability::{self, Scope};
use hive_application::console::{
    ConsoleCapability, ConsoleContext, ConsoleOrganization, ConsoleProject, ConsoleRepository,
    ConsoleRepositoryError as RepositoryError, DisplayPreferencesMutationResult,
    DisplayPreferencesProblem, UserDisplayPreferences,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

fn other(error: sea_orm::DbErr) -> RepositoryError {
    RepositoryError::Other(error.into())
}

pub struct PgConsoleRepository {
    db: DatabaseConnection,
}

impl PgConsoleRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

struct VisibleOrganization {
    id: Uuid,
    slug: String,
    display_name: String,
    lifecycle_status: String,
    projects: Vec<ConsoleProject>,
}

async fn principal_display_name(
    db: &DatabaseConnection,
    principal_id: Uuid,
) -> Result<Option<String>, RepositoryError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT display_name FROM principals WHERE id = $1",
        [principal_id.into()],
    );
    let row = db.query_one_raw(statement).await.map_err(other)?;
    row.map(|row| row.try_get_by::<String, _>("display_name"))
        .transpose()
        .map_err(other)
}

async fn visible_organizations(
    db: &DatabaseConnection,
    principal_id: Uuid,
) -> Result<Vec<VisibleOrganization>, RepositoryError> {
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT organization.id AS organization_id, organization.slug AS organization_slug, \
                organization.display_name AS organization_display_name, \
                organization.lifecycle_status AS organization_lifecycle_status, \
                project.id AS project_id, project.slug AS project_slug, \
                project.display_name AS project_display_name, project.lifecycle_status AS project_lifecycle_status \
         FROM organizations organization \
         LEFT JOIN organization_memberships membership ON membership.organization_id = organization.id \
         LEFT JOIN projects project ON project.organization_id = organization.id \
         WHERE (membership.principal_id = $1 \
                AND membership.started_at <= CURRENT_TIMESTAMP AND membership.ended_at IS NULL) \
            OR EXISTS (SELECT 1 FROM platform_role_assignments platform \
                       WHERE platform.principal_id = $1 AND platform.role_code = 'PLATFORM_ADMIN') \
         ORDER BY organization.display_name, organization.id, project.display_name, project.id",
        [principal_id.into()],
    );
    let rows = db.query_all_raw(statement).await.map_err(other)?;

    let mut organizations: BTreeMap<Uuid, VisibleOrganization> = BTreeMap::new();
    let mut order: Vec<Uuid> = Vec::new();
    for row in rows {
        let organization_id: Uuid = row.try_get_by("organization_id").map_err(other)?;
        if let std::collections::btree_map::Entry::Vacant(entry) =
            organizations.entry(organization_id)
        {
            entry.insert(VisibleOrganization {
                id: organization_id,
                slug: row.try_get_by("organization_slug").map_err(other)?,
                display_name: row.try_get_by("organization_display_name").map_err(other)?,
                lifecycle_status: row
                    .try_get_by("organization_lifecycle_status")
                    .map_err(other)?,
                projects: Vec::new(),
            });
            order.push(organization_id);
        }
        let project_id: Option<Uuid> = row.try_get_by("project_id").map_err(other)?;
        if let Some(project_id) = project_id {
            organizations
                .get_mut(&organization_id)
                .unwrap()
                .projects
                .push(ConsoleProject {
                    id: project_id,
                    organization_id,
                    slug: row.try_get_by("project_slug").map_err(other)?,
                    display_name: row.try_get_by("project_display_name").map_err(other)?,
                    lifecycle_status: row.try_get_by("project_lifecycle_status").map_err(other)?,
                });
        }
    }
    // BTreeMap reorders by UUID; the query's own ORDER BY (display_name, id) is the
    // contract, so rebuild output in first-seen (query) order, matching Java's
    // LinkedHashMap.
    Ok(order
        .into_iter()
        .map(|id| organizations.remove(&id).unwrap())
        .collect())
}

async fn add_capabilities_from(
    db: &DatabaseConnection,
    principal_id: Uuid,
    scope_type: &str,
    scope: Scope,
    codes: &[&str],
    result: &mut Vec<ConsoleCapability>,
    seen: &mut HashSet<(String, String, Uuid)>,
) -> Result<(), RepositoryError> {
    for &code in codes {
        if capability::has_capability(db, principal_id, code, scope, false)
            .await
            .map_err(other)?
        {
            push_capability(result, seen, code, scope_type, scope_id_of(scope));
        }
    }
    Ok(())
}

fn scope_id_of(scope: Scope) -> Uuid {
    match scope {
        Scope::Organization(id) | Scope::Project(id) | Scope::Principal(id) => id,
    }
}

fn push_capability(
    result: &mut Vec<ConsoleCapability>,
    seen: &mut HashSet<(String, String, Uuid)>,
    code: &str,
    scope_type: &str,
    scope_id: Uuid,
) {
    let key = (code.to_string(), scope_type.to_string(), scope_id);
    if seen.insert(key) {
        result.push(ConsoleCapability {
            code: code.to_string(),
            scope_type: scope_type.to_string(),
            scope_id,
        });
    }
}

fn revision(
    principal_id: Uuid,
    organizations: &[ConsoleOrganization],
    capabilities: &[ConsoleCapability],
) -> String {
    // An opaque cache-busting digest, not a value ever compared against the Java
    // system's own revision string: it only needs to change whenever the visible
    // organization/project/capability set changes for this principal, which a
    // deterministic digest over the same semantic content guarantees regardless of
    // the exact serialization Java's record toString() happens to produce.
    let mut hasher = Sha256::new();
    hasher.update(principal_id.as_bytes());
    for organization in organizations {
        hasher.update(organization.id.as_bytes());
        hasher.update(organization.lifecycle_status.as_bytes());
        for project in &organization.projects {
            hasher.update(project.id.as_bytes());
            hasher.update(project.lifecycle_status.as_bytes());
        }
    }
    for capability in capabilities {
        hasher.update(capability.code.as_bytes());
        hasher.update(capability.scope_type.as_bytes());
        hasher.update(capability.scope_id.as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[async_trait::async_trait]
impl ConsoleRepository for PgConsoleRepository {
    async fn find_context(
        &self,
        principal_id: Uuid,
    ) -> Result<Option<ConsoleContext>, RepositoryError> {
        let db = &self.db;
        // A verified local fixture principal may have no persisted profile yet. It
        // receives an empty, default-deny context so the established selector can
        // still render its accessible-empty state.
        let display_name = principal_display_name(db, principal_id)
            .await?
            .unwrap_or_else(|| "Local user".to_string());

        let organizations = visible_organizations(db, principal_id).await?;
        let project_ids: Vec<Uuid> = organizations
            .iter()
            .flat_map(|organization| organization.projects.iter().map(|project| project.id))
            .collect();
        let deployment_grants =
            capability::deployment_capabilities_many(db, principal_id, &project_ids)
                .await
                .map_err(other)?;

        let mut capabilities = Vec::new();
        let mut seen = HashSet::new();

        if capability::has_capability(
            db,
            principal_id,
            capability::PREFERENCES_UPDATE,
            Scope::Principal(principal_id),
            false,
        )
        .await
        .map_err(other)?
        {
            push_capability(
                &mut capabilities,
                &mut seen,
                capability::PREFERENCES_UPDATE,
                "PRINCIPAL",
                principal_id,
            );
        }

        let mut console_organizations = Vec::with_capacity(organizations.len());
        for organization in organizations {
            if capability::has_capability(
                db,
                principal_id,
                capability::ORGANIZATION_VIEW,
                Scope::Organization(organization.id),
                false,
            )
            .await
            .map_err(other)?
            {
                push_capability(
                    &mut capabilities,
                    &mut seen,
                    capability::ORGANIZATION_VIEW,
                    "ORGANIZATION",
                    organization.id,
                );
            }
            add_capabilities_from(
                db,
                principal_id,
                "ORGANIZATION",
                Scope::Organization(organization.id),
                capability::ADMINISTRATION_CAPABILITIES,
                &mut capabilities,
                &mut seen,
            )
            .await?;
            add_capabilities_from(
                db,
                principal_id,
                "ORGANIZATION",
                Scope::Organization(organization.id),
                &[capability::AUDIT_VIEW, capability::AUDIT_SENSITIVE_VIEW],
                &mut capabilities,
                &mut seen,
            )
            .await?;
            add_capabilities_from(
                db,
                principal_id,
                "ORGANIZATION",
                Scope::Organization(organization.id),
                capability::CONFIGURATION_CAPABILITIES,
                &mut capabilities,
                &mut seen,
            )
            .await?;

            let mut projects = organization.projects;
            for project in &projects {
                for code in [
                    capability::PROJECT_VIEW,
                    capability::AGENT_VIEW,
                    capability::AGENT_DRAFT_UPDATE,
                ] {
                    if capability::has_capability(
                        db,
                        principal_id,
                        code,
                        Scope::Project(project.id),
                        false,
                    )
                    .await
                    .map_err(other)?
                    {
                        push_capability(&mut capabilities, &mut seen, code, "PROJECT", project.id);
                    }
                }
                add_capabilities_from(
                    db,
                    principal_id,
                    "PROJECT",
                    Scope::Project(project.id),
                    capability::ADMINISTRATION_CAPABILITIES,
                    &mut capabilities,
                    &mut seen,
                )
                .await?;
                add_capabilities_from(
                    db,
                    principal_id,
                    "PROJECT",
                    Scope::Project(project.id),
                    &[capability::AUDIT_VIEW, capability::AUDIT_SENSITIVE_VIEW],
                    &mut capabilities,
                    &mut seen,
                )
                .await?;
                add_capabilities_from(
                    db,
                    principal_id,
                    "PROJECT",
                    Scope::Project(project.id),
                    capability::CONFIGURATION_CAPABILITIES,
                    &mut capabilities,
                    &mut seen,
                )
                .await?;
                if let Some(grants) = deployment_grants.get(&project.id) {
                    for &code in grants {
                        push_capability(&mut capabilities, &mut seen, code, "PROJECT", project.id);
                    }
                }
                let evaluation_grants =
                    capability::evaluation_capabilities(db, principal_id, project.id, false)
                        .await
                        .map_err(other)?;
                for code in evaluation_grants {
                    push_capability(&mut capabilities, &mut seen, code, "PROJECT", project.id);
                }
            }
            projects.sort_by(|a, b| a.display_name.cmp(&b.display_name).then(a.id.cmp(&b.id)));

            console_organizations.push(ConsoleOrganization {
                id: organization.id,
                slug: organization.slug,
                display_name: organization.display_name,
                lifecycle_status: organization.lifecycle_status,
                projects,
            });
        }

        capabilities.sort_by(|a, b| {
            a.scope_type
                .cmp(&b.scope_type)
                .then(a.scope_id.cmp(&b.scope_id))
                .then(a.code.cmp(&b.code))
        });

        let revision = revision(principal_id, &console_organizations, &capabilities);
        Ok(Some(ConsoleContext {
            principal_id,
            display_name,
            organizations: console_organizations,
            capabilities,
            revision,
        }))
    }

    async fn find_preferences(
        &self,
        principal_id: Uuid,
    ) -> Result<Option<UserDisplayPreferences>, RepositoryError> {
        let exists_statement = Statement::from_sql_and_values(
            self.db.get_database_backend(),
            "SELECT 1 FROM principals WHERE id = $1",
            [principal_id.into()],
        );
        let principal_exists = self
            .db
            .query_one_raw(exists_statement)
            .await
            .map_err(other)?
            .is_some();
        if !principal_exists {
            return Ok(None);
        }

        let preferences_statement = Statement::from_sql_and_values(
            self.db.get_database_backend(),
            "SELECT color_scheme, density, sidebar_state FROM principal_display_preferences WHERE principal_id = $1",
            [principal_id.into()],
        );
        let row = self
            .db
            .query_one_raw(preferences_statement)
            .await
            .map_err(other)?;

        Ok(Some(match row {
            Some(row) => UserDisplayPreferences {
                principal_id,
                color_scheme: row.try_get_by("color_scheme").map_err(other)?,
                density: row.try_get_by("density").map_err(other)?,
                sidebar_state: row.try_get_by("sidebar_state").map_err(other)?,
            },
            None => UserDisplayPreferences::defaults(principal_id),
        }))
    }

    async fn update_preferences(
        &self,
        principal_id: Uuid,
        color_scheme: &str,
        density: &str,
        sidebar_state: &str,
    ) -> Result<DisplayPreferencesMutationResult, RepositoryError> {
        let tx = self.db.begin().await.map_err(other)?;

        let lock_statement = Statement::from_sql_and_values(
            tx.get_database_backend(),
            "SELECT 1 FROM principals WHERE id = $1 FOR UPDATE",
            [principal_id.into()],
        );
        let principal_exists = tx
            .query_one_raw(lock_statement)
            .await
            .map_err(other)?
            .is_some();
        if !principal_exists {
            tx.rollback().await.map_err(other)?;
            return Ok(DisplayPreferencesMutationResult::refused(
                DisplayPreferencesProblem::NotFound,
            ));
        }

        let upsert_statement = Statement::from_sql_and_values(
            tx.get_database_backend(),
            "INSERT INTO principal_display_preferences (principal_id, color_scheme, density, sidebar_state) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (principal_id) DO UPDATE SET \
               color_scheme = EXCLUDED.color_scheme, density = EXCLUDED.density, sidebar_state = EXCLUDED.sidebar_state",
            [
                principal_id.into(),
                color_scheme.into(),
                density.into(),
                sidebar_state.into(),
            ],
        );
        tx.execute_raw(upsert_statement).await.map_err(other)?;

        tx.commit().await.map_err(other)?;

        Ok(DisplayPreferencesMutationResult::success(
            UserDisplayPreferences {
                principal_id,
                color_scheme: color_scheme.to_string(),
                density: density.to_string(),
                sidebar_state: sidebar_state.to_string(),
            },
        ))
    }
}
