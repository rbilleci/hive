//! Row-level tenant scoping for the Seaography-generated read tier (`GSR-TENANT-HOOKS`). This is
//! not authorization: the effective-capability evaluator remains the authority for every custom
//! operation (`GSR-AUTHZ-CAPABILITY`). This hook only keeps a generated read connection from
//! returning a row the requesting principal holds no active membership over, and refuses every
//! generated write as defense in depth.

use crate::schema::RequestPrincipal;
use async_graphql::dynamic::ResolverContext;
use hive_persistence::entity::tenant;
use sea_orm::{Condition, EntityTrait};
use seaography::{GuardAction, LifecycleHooksInterface, OperationType};

pub struct TenantHooks;

impl LifecycleHooksInterface for TenantHooks {
    fn entity_guard(
        &self,
        _ctx: &ResolverContext,
        _entity: &str,
        action: OperationType,
    ) -> GuardAction {
        match action {
            OperationType::Read => GuardAction::Allow,
            OperationType::Create | OperationType::Update | OperationType::Delete => {
                GuardAction::Block(Some("Generated writes are not exposed.".to_string()))
            }
        }
    }

    fn entity_filter(
        &self,
        ctx: &ResolverContext,
        entity: &str,
        _action: OperationType,
    ) -> Option<Condition> {
        let Some(principal) = ctx.data_opt::<RequestPrincipal>() else {
            // No authenticated principal: never leak a row rather than fail the schema build.
            return Some(tenant::deny_all());
        };
        // `entity` is the *overridden* type name Seaography emits (`GSR-READ-TIER`'s
        // `entity_object.type_name`, "OrganizationRead"), not the Rust module path
        // ("organization_read") — confirmed against `entity_object.type_name::<T>()`'s call site
        // in `seaography-2.0.0-rc.9/src/query/entity_query_field.rs`. A first pass here matched
        // the Rust name and silently applied no filter at all; the phase 0 spike's own printed SDL
        // caught the naming override this depends on, but only an integration test that asserts a
        // membership-less principal sees zero rows (`GSR-PHASE-2`'s risk-table item) catches this
        // specific mismatch, since a wrong `entity` string here fails open (`None` = "no filter"),
        // not closed.
        match entity {
            "OrganizationRead" => Some(tenant::organization_membership_exists(
                hive_persistence::entity::organization_read::Column::Id,
                principal.0,
            )),
            _ => None,
        }
    }
}

/// Type-checks `TenantHooks::entity_filter`'s column argument against the real entity, so a typo
/// in the match above (a table name Seaography never asks about) fails to compile rather than
/// silently returning `None` (no filter) for a real table.
#[allow(dead_code)]
fn _assert_organization_read_entity_matches() {
    let _: <hive_persistence::entity::organization_read::Entity as EntityTrait>::Column =
        hive_persistence::entity::organization_read::Column::Id;
}
