//! Audit history is the generated read of `audit_event_projection`, a view over the per-domain
//! `*_audit_events` tables. Which events a principal reads is the tenant rule in
//! `crate::authority`. This module holds the request metadata every command writes onto its audit
//! row (`context`) and the view's computed fields:
//!
//! - `sourceIp` / `userAgent`: the recorded request metadata, or `null` unless the requesting
//!   principal holds `AUDIT_SENSITIVE.VIEW` at the event's scope (its project, or its
//!   organization for an event with no project). The two columns themselves are not generated
//!   fields, so they cannot be selected, filtered or ordered on.
//! - `sensitiveFieldsRedacted`: whether the event has such metadata and it was withheld.

#![allow(non_snake_case)] // a computed field is named after its method

pub mod context;

pub use context::{
    current as current_audit_request_metadata, scope as audit_request_metadata_scope,
};

use crate::capability::{self, Scope};
use crate::console::requester;
use crate::entity::audit_event_projection;
// `#[CustomFields]` expands to paths that start with `async_graphql::`.
use seaography::async_graphql::{self, Context};
use seaography::CustomFields;
use std::collections::HashMap;

/// The scopes already evaluated for `AUDIT_SENSITIVE.VIEW` in this request, so a page of events
/// asks the evaluator about a scope it has an answer for no more than once. The `/graphql` handler
/// puts an empty one into the request data; without it every field asks the evaluator.
#[derive(Default)]
pub struct SensitiveAuditAccess(tokio::sync::Mutex<HashMap<Scope, bool>>);

impl audit_event_projection::Model {
    fn scope(&self) -> Option<Scope> {
        self.project_id
            .map(Scope::Project)
            .or(self.organization_id.map(Scope::Organization))
    }

    fn has_sensitive_fields(&self) -> bool {
        self.source_ip.is_some() || self.user_agent.is_some()
    }

    /// Whether the requesting principal holds `AUDIT_SENSITIVE.VIEW` at this event's scope.
    async fn sensitive_visible(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        let Some(scope) = self.scope() else {
            return Ok(false);
        };
        let (principal_id, db) = requester(ctx)?;
        let evaluate = capability::has_capability(
            db,
            principal_id,
            capability::AUDIT_SENSITIVE_VIEW,
            scope,
            false,
        );
        let Some(SensitiveAuditAccess(evaluated)) = ctx.data_opt::<SensitiveAuditAccess>() else {
            return Ok(evaluate.await?);
        };
        // The guard is released before the evaluator is awaited, so a page of rows does not
        // serialise behind it. Two rows of the same scope that miss together both evaluate and
        // both write the same answer.
        let cached = evaluated.lock().await.get(&scope).copied();
        if let Some(visible) = cached {
            return Ok(visible);
        }
        let visible = evaluate.await?;
        evaluated.lock().await.insert(scope, visible);
        Ok(visible)
    }
}

#[CustomFields]
impl audit_event_projection::Model {
    /// The address the request came from, without a network suffix; `null` when none was
    /// recorded or the requesting principal may not read it.
    pub async fn sourceIp(&self, ctx: &Context<'_>) -> async_graphql::Result<Option<String>> {
        let Some(source_ip) = &self.source_ip else {
            return Ok(None);
        };
        Ok(self
            .sensitive_visible(ctx)
            .await?
            .then(|| source_ip.split('/').next().unwrap_or_default().to_string()))
    }

    /// The request's user agent; `null` when none was recorded or the requesting principal may
    /// not read it.
    pub async fn userAgent(&self, ctx: &Context<'_>) -> async_graphql::Result<Option<String>> {
        let Some(user_agent) = &self.user_agent else {
            return Ok(None);
        };
        Ok(self
            .sensitive_visible(ctx)
            .await?
            .then(|| user_agent.clone()))
    }

    /// Whether this event has a source address or a user agent that was withheld from the
    /// requesting principal.
    pub async fn sensitiveFieldsRedacted(&self, ctx: &Context<'_>) -> async_graphql::Result<bool> {
        Ok(self.has_sensitive_fields() && !self.sensitive_visible(ctx).await?)
    }
}
