//! Row scoping and write refusal for the Seaography-generated API
//! (`docs/idiomatic-seaography-plan.md`, A3). The `/graphql` handler loads the principal's
//! `Authority` once per request; this hook turns it into the condition every generated query,
//! relation and dataloader applies. An entity with no rule, or a request with no authority,
//! matches no row.

use async_graphql::dynamic::ResolverContext;
use hive_persistence::authority::deny_all;
pub use hive_persistence::authority::RequestAuthority;
use sea_orm::Condition;
use seaography::{GuardAction, LifecycleHooksInterface, OperationType};

const AUTHORITY_UNAVAILABLE: &str = "Access could not be determined; try again.";

pub struct TenantHooks;

impl LifecycleHooksInterface for TenantHooks {
    fn entity_guard(
        &self,
        ctx: &ResolverContext,
        _entity: &str,
        action: OperationType,
    ) -> GuardAction {
        match action {
            OperationType::Read => match ctx.data_opt::<RequestAuthority>() {
                Some(RequestAuthority(Some(_))) => GuardAction::Allow,
                _ => GuardAction::Block(Some(AUTHORITY_UNAVAILABLE.to_string())),
            },
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
            .and_then(|authority| authority.0.as_ref())
            .and_then(|authority| authority.read_condition(entity))
            .unwrap_or_else(deny_all);
        Some(condition)
    }
}
