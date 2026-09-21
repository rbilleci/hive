//! Replaces Java's `PostgresAuditRequestContext` thread-local with a tokio task-local: Aurora
//! DSQL supports neither triggers nor PL/pgSQL functions, so every audit-event INSERT across the
//! agent draft, agent authoring, administration, configuration, deployment, and evaluation
//! repositories binds `request_id`/`correlation_id`/`graphql_operation`/`source_ip`/`user_agent`
//! explicitly, reading them from whichever task-local scope the current async task is running
//! inside (entered once, in `hive-api`'s `/graphql` handler, around `schema.execute(request)`).
//! A worker/maintenance task never enters this scope, so `current()` there returns `None`,
//! matching Java's `PostgresAuditRequestContext.current() == null` default on a thread the
//! `GraphqlExecutor` never touched.

use hive_application::audit::AuditRequestMetadata;
use std::future::Future;

tokio::task_local! {
    static AUDIT_REQUEST_METADATA: AuditRequestMetadata;
}

/// Runs `future` with `metadata` readable via [`current`] from anywhere inside it, including
/// through nested async calls that hold no reference to `metadata` themselves.
pub async fn scope<F: Future>(metadata: AuditRequestMetadata, future: F) -> F::Output {
    AUDIT_REQUEST_METADATA.scope(metadata, future).await
}

pub fn current() -> Option<AuditRequestMetadata> {
    AUDIT_REQUEST_METADATA.try_with(Clone::clone).ok()
}

/// The request metadata every audit row carries; all `None` outside a GraphQL request.
pub(crate) struct RequestMetadata {
    pub request_id: Option<uuid::Uuid>,
    pub correlation_id: Option<uuid::Uuid>,
    pub graphql_operation: Option<String>,
    pub source_ip: Option<String>,
    pub user_agent: Option<String>,
}

pub(crate) fn request_metadata() -> RequestMetadata {
    let metadata = current();
    RequestMetadata {
        request_id: metadata.as_ref().map(|value| value.request_id),
        correlation_id: metadata.as_ref().map(|value| value.correlation_id),
        graphql_operation: metadata
            .as_ref()
            .and_then(|value| value.graphql_operation.clone()),
        source_ip: metadata.as_ref().and_then(|value| value.source_ip.clone()),
        user_agent: metadata.and_then(|value| value.user_agent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn current_is_none_outside_any_scope() {
        assert!(current().is_none());
    }

    #[tokio::test]
    async fn current_reads_back_the_scoped_metadata() {
        let metadata = AuditRequestMetadata::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Some("query Ping"),
            Some("203.0.113.9"),
            Some("hive-tests/1.0"),
        );
        let request_id = metadata.request_id;
        scope(metadata, async {
            assert_eq!(current().unwrap().request_id, request_id);
        })
        .await;
        assert!(current().is_none());
    }
}
