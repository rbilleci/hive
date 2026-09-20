//! Row scoping and write refusal for the Seaography-generated API
//! (`docs/idiomatic-seaography-plan.md`, A3). The `/graphql` handler loads the principal's
//! `Authority` once per request; this hook turns it into the condition every generated query,
//! relation and dataloader applies. An entity with no rule, or a request with no authority,
//! matches no row.

use async_graphql::dynamic::ResolverContext;
use hive_persistence::authority::{deny_all, Authority};
use sea_orm::Condition;
use seaography::{GuardAction, LifecycleHooksInterface, OperationType};

/// The requesting principal's read authority, inserted into the request data by the handler.
pub struct RequestAuthority(pub Authority);

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
        let condition = ctx
            .data_opt::<RequestAuthority>()
            .and_then(|authority| authority.0.read_condition(entity))
            .unwrap_or_else(deny_all);
        Some(condition)
    }
}
