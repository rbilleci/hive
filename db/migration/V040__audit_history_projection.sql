-- audit_event_projection below derives a read-only history from the immutable per-domain audit
-- tables. It is not a second audit authority: it stores nothing and rewrites no payload.
ALTER TABLE agent_draft_audit_events
    ADD COLUMN IF NOT EXISTS request_id UUID NULL;
ALTER TABLE agent_draft_audit_events
    ADD COLUMN IF NOT EXISTS correlation_id UUID NULL;
ALTER TABLE agent_draft_audit_events
    ADD COLUMN IF NOT EXISTS graphql_operation TEXT NULL;
ALTER TABLE agent_draft_audit_events
    ADD COLUMN IF NOT EXISTS source_ip TEXT NULL;
ALTER TABLE agent_draft_audit_events
    ADD COLUMN IF NOT EXISTS user_agent TEXT NULL;
ALTER TABLE agent_authoring_audit_events
    ADD COLUMN IF NOT EXISTS request_id UUID NULL;
ALTER TABLE agent_authoring_audit_events
    ADD COLUMN IF NOT EXISTS correlation_id UUID NULL;
ALTER TABLE agent_authoring_audit_events
    ADD COLUMN IF NOT EXISTS graphql_operation TEXT NULL;
ALTER TABLE agent_authoring_audit_events
    ADD COLUMN IF NOT EXISTS source_ip TEXT NULL;
ALTER TABLE agent_authoring_audit_events
    ADD COLUMN IF NOT EXISTS user_agent TEXT NULL;
ALTER TABLE agent_authoring_audit_events
    ADD COLUMN IF NOT EXISTS organization_id UUID NULL;
UPDATE agent_authoring_audit_events event
SET organization_id = project.organization_id FROM projects project
WHERE project.id = event.project_id AND event.organization_id IS NULL;
-- Aurora DSQL has no ALTER COLUMN ... SET NOT NULL at all ("unsupported ALTER TABLE ALTER COLUMN ...
-- SET NOT NULL statement"), so the requirement is a CHECK. The migrator rewrites an ADD CONSTRAINT
-- ... CHECK into Aurora DSQL's required NOT VALID + VALIDATE CONSTRAINT pair itself; see
-- `run_add_check_constraint`.
ALTER TABLE agent_authoring_audit_events
    ADD CONSTRAINT agent_authoring_audit_events_organization_id_nn CHECK (organization_id IS NOT NULL);
ALTER TABLE administration_audit_events
    ADD COLUMN IF NOT EXISTS request_id UUID NULL;
ALTER TABLE administration_audit_events
    ADD COLUMN IF NOT EXISTS correlation_id UUID NULL;
ALTER TABLE administration_audit_events
    ADD COLUMN IF NOT EXISTS graphql_operation TEXT NULL;
ALTER TABLE administration_audit_events
    ADD COLUMN IF NOT EXISTS source_ip TEXT NULL;
ALTER TABLE administration_audit_events
    ADD COLUMN IF NOT EXISTS user_agent TEXT NULL;
ALTER TABLE configuration_audit_events
    ADD COLUMN IF NOT EXISTS request_id UUID NULL;
ALTER TABLE configuration_audit_events
    ADD COLUMN IF NOT EXISTS correlation_id UUID NULL;
ALTER TABLE configuration_audit_events
    ADD COLUMN IF NOT EXISTS graphql_operation TEXT NULL;
ALTER TABLE configuration_audit_events
    ADD COLUMN IF NOT EXISTS source_ip TEXT NULL;
ALTER TABLE configuration_audit_events
    ADD COLUMN IF NOT EXISTS user_agent TEXT NULL;
ALTER TABLE deployment_audit_events
    ADD COLUMN IF NOT EXISTS request_id UUID NULL;
ALTER TABLE deployment_audit_events
    ADD COLUMN IF NOT EXISTS correlation_id UUID NULL;
ALTER TABLE deployment_audit_events
    ADD COLUMN IF NOT EXISTS graphql_operation TEXT NULL;
ALTER TABLE deployment_audit_events
    ADD COLUMN IF NOT EXISTS source_ip TEXT NULL;
ALTER TABLE deployment_audit_events
    ADD COLUMN IF NOT EXISTS user_agent TEXT NULL;
ALTER TABLE evaluation_audit_events
    ADD COLUMN IF NOT EXISTS request_id UUID NULL;
ALTER TABLE evaluation_audit_events
    ADD COLUMN IF NOT EXISTS correlation_id UUID NULL;
ALTER TABLE evaluation_audit_events
    ADD COLUMN IF NOT EXISTS graphql_operation TEXT NULL;
ALTER TABLE evaluation_audit_events
    ADD COLUMN IF NOT EXISTS source_ip TEXT NULL;
ALTER TABLE evaluation_audit_events
    ADD COLUMN IF NOT EXISTS user_agent TEXT NULL;

CREATE INDEX IF NOT EXISTS agent_draft_audit_events_audit_time ON agent_draft_audit_events (agent_id, occurred_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS agent_authoring_audit_events_audit_project_time ON agent_authoring_audit_events (project_id, occurred_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS agent_authoring_audit_events_audit_organization_time ON agent_authoring_audit_events (organization_id, occurred_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS administration_audit_events_audit_scope_time ON administration_audit_events (scope_type, scope_id, occurred_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS configuration_audit_events_audit_project_time ON configuration_audit_events (project_id, occurred_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_audit_events_audit_time ON deployment_audit_events (deployment_id, occurred_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS evaluation_audit_events_audit_time ON evaluation_audit_events (run_id, occurred_at DESC, id DESC);
-- No WHERE clause: Aurora DSQL does not support partial indexes (verified against the real cluster --
-- CREATE INDEX ... WHERE fails with 0A000). A full index over a mostly-non-null column is a fine
-- substitute at this table's scale; there is no query-planner selectivity loss worth the incompatibility.
CREATE INDEX IF NOT EXISTS evaluation_audit_events_definition_audit_time ON evaluation_audit_events (definition_id, occurred_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS agent_draft_audit_events_audit_actor_time ON agent_draft_audit_events (principal_id, occurred_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS agent_authoring_audit_events_audit_actor_time ON agent_authoring_audit_events (project_id, actor_principal_id, occurred_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS administration_audit_events_audit_actor_time ON administration_audit_events (scope_type, scope_id, actor_principal_id, occurred_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS configuration_audit_events_audit_actor_time ON configuration_audit_events (project_id, actor_principal_id, occurred_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS deployment_audit_events_audit_actor_time ON deployment_audit_events (deployment_id, actor_principal_id, occurred_at DESC, id DESC);
-- No WHERE clauses, same reason as evaluation_audit_events_definition_audit_time above.
CREATE INDEX IF NOT EXISTS agent_authoring_audit_events_correlation ON agent_authoring_audit_events (correlation_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS agent_draft_audit_events_correlation ON agent_draft_audit_events (correlation_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS administration_audit_events_correlation ON administration_audit_events (correlation_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS configuration_audit_events_correlation ON configuration_audit_events (correlation_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS deployment_audit_events_correlation ON deployment_audit_events (correlation_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS evaluation_audit_events_correlation ON evaluation_audit_events (correlation_id, occurred_at DESC);

-- Aurora DSQL supports neither triggers nor PL/pgSQL functions, so nothing in the database populates
-- the five request-metadata columns added above or resolves
-- agent_authoring_audit_events.organization_id. Each writer supplies them: every audit INSERT passes
-- request_id, correlation_id, graphql_operation, source_ip, and user_agent from the ambient request
-- context (`hive_persistence::audit::context::current`), and the agent-draft writer resolves
-- organization_id before insert. An audit INSERT that skips them silently loses the correlation, and
-- the organization_id CHECK above fails the write outright.
--
-- Nothing rejects an UPDATE or DELETE against these audit tables either. Every writer only INSERTs,
-- so no code path exercises the gap; it existed as defense in depth against direct SQL bypassing the
-- application, which Aurora DSQL's IAM-gated connections address instead.
--
-- agent_draft_audit_events.agent_id has no FOREIGN KEY: Aurora DSQL does not support them.
-- `legacy_audit` confirms the agent exists before every INSERT into this table.

CREATE
OR REPLACE VIEW audit_event_projection AS
SELECT 'agent_draft:' || event.id::text AS projection_id, 'AGENT_DRAFT'::text AS source_kind, event.id AS source_event_id,
       project.organization_id,
       project.id         AS project_id,
       event.principal_id AS actor_principal_id,
       CASE event.action WHEN 'UPDATED' THEN 'AGENT_SAVED' WHEN 'VALIDATED' THEN 'AGENT_VALIDATED' END::text AS action,
  'AGENT'::text AS resource_type, event.agent_id AS resource_id, 'SUCCEEDED'::text AS outcome,
  NULL::text AS before_digest, event.content_digest::text AS after_digest, '[]'::jsonb AS safe_changed_fields,
  jsonb_build_array(jsonb_build_object('type','AGENT','id',event.agent_id::text)) AS resource_references,
  event.request_id, event.correlation_id, event.graphql_operation, event.source_ip::text AS source_ip, event.user_agent, event.occurred_at, 'AGENT.VIEW'::text AS required_capability
FROM agent_draft_audit_events event JOIN agents agent
ON agent.id = event.agent_id JOIN projects project ON project.id = agent.project_id
WHERE event.action IN ('UPDATED'
    , 'VALIDATED')
  AND NOT EXISTS (SELECT 1 FROM agent_authoring_audit_events mirror WHERE mirror.agent_id = event.agent_id
  AND mirror.actor_principal_id = event.principal_id
  AND mirror.revision = event.revision
  AND mirror.content_digest::text = event.content_digest
  AND ((event.action = 'UPDATED'
  AND mirror.action = 'SAVED')
   OR (event.action = 'VALIDATED'
  AND mirror.action = 'VALIDATED')))
UNION ALL
SELECT 'agent_authoring:' || event.id::text, 'AGENT_AUTHORING',
       event.id,
       event.organization_id,
       event.project_id,
       event.actor_principal_id,
       CASE event.action
           WHEN 'CREATED' THEN 'AGENT_CREATED'
           WHEN 'SAVED' THEN 'AGENT_SAVED'
           WHEN 'VALIDATED' THEN 'AGENT_VALIDATED'
           WHEN 'PUBLISHED' THEN 'AGENT_VERSION_PUBLISHED' END,
       CASE WHEN event.action = 'PUBLISHED' THEN 'AGENT_VERSION' ELSE 'AGENT' END,
       CASE WHEN event.action = 'PUBLISHED' THEN event.version_id ELSE event.agent_id END,
       'SUCCEEDED',
       NULL::text, event.content_digest::text, '[]'::jsonb, jsonb_build_array(
        jsonb_build_object('type', 'AGENT', 'id', event.agent_id::text),
        jsonb_build_object('type', 'AGENT_VERSION', 'id', event.version_id::text)),
       event.request_id,
       event.correlation_id,
       event.graphql_operation,
       event.source_ip::text, event.user_agent,
       event.occurred_at,
       'AGENT.VIEW'
FROM agent_authoring_audit_events event
WHERE event.action IN ('CREATED', 'SAVED', 'VALIDATED', 'PUBLISHED')
UNION ALL
SELECT 'administration:' || event.id::text, 'ADMINISTRATION',
       event.id,
       event.scope_id,
       NULL::uuid, event.actor_principal_id,
       'ADMINISTRATION_CHANGED',
       'ORGANIZATION',
       event.scope_id,
       'SUCCEEDED',
       event.before_digest::text, event.after_digest::text, '[]'::jsonb, jsonb_build_array(jsonb_build_object('type', 'ORGANIZATION', 'id', event.scope_id::text)),
       event.request_id,
       event.correlation_id,
       event.graphql_operation,
       event.source_ip::text, event.user_agent,
       event.occurred_at,
       'ORGANIZATION.VIEW'
FROM administration_audit_events event
WHERE event.scope_type = 'ORGANIZATION'
  AND event.action IN (
                       'PROJECT_APPROVAL_POLICY_CREATED', 'PROJECT_CREATED', 'ORGANIZATION_MEMBERSHIP_ADDED',
                       'PROJECT_MEMBERSHIP_ADDED',
                       'ORGANIZATION_MEMBERSHIP_ROLES_REPLACED', 'PROJECT_MEMBERSHIP_ROLES_REPLACED',
                       'ORGANIZATION_MEMBERSHIP_ENDED',
                       'PROJECT_MEMBERSHIP_ENDED', 'ORGANIZATION_ARCHIVED', 'PROJECT_ARCHIVED', 'ORGANIZATION_RESTORED',
                       'PROJECT_RESTORED',
                       'PROJECT_BUDGET_POLICY_UPDATED', 'PROJECT_APPROVAL_POLICY_UPDATED', 'PROJECT_GENERAL_UPDATED',
                       'PROJECT_CONNECTION_CREATED',
                       'PROJECT_CONNECTION_UPDATED')
UNION ALL
SELECT 'administration:' || event.id::text, 'ADMINISTRATION',
       event.id,
       project.organization_id,
       project.id,
       event.actor_principal_id,
       'ADMINISTRATION_CHANGED',
       'PROJECT',
       event.scope_id,
       'SUCCEEDED',
       event.before_digest::text, event.after_digest::text, '[]'::jsonb, jsonb_build_array(jsonb_build_object('type', 'PROJECT', 'id', event.scope_id::text)),
       event.request_id,
       event.correlation_id,
       event.graphql_operation,
       event.source_ip::text, event.user_agent,
       event.occurred_at,
       'PROJECT.VIEW'
FROM administration_audit_events event
         JOIN projects project ON project.id = event.scope_id
WHERE event.scope_type = 'PROJECT'
  AND event.action IN (
                       'PROJECT_APPROVAL_POLICY_CREATED', 'PROJECT_CREATED', 'ORGANIZATION_MEMBERSHIP_ADDED',
                       'PROJECT_MEMBERSHIP_ADDED',
                       'ORGANIZATION_MEMBERSHIP_ROLES_REPLACED', 'PROJECT_MEMBERSHIP_ROLES_REPLACED',
                       'ORGANIZATION_MEMBERSHIP_ENDED',
                       'PROJECT_MEMBERSHIP_ENDED', 'ORGANIZATION_ARCHIVED', 'PROJECT_ARCHIVED', 'ORGANIZATION_RESTORED',
                       'PROJECT_RESTORED',
                       'PROJECT_BUDGET_POLICY_UPDATED', 'PROJECT_APPROVAL_POLICY_UPDATED', 'PROJECT_GENERAL_UPDATED',
                       'PROJECT_CONNECTION_CREATED',
                       'PROJECT_CONNECTION_UPDATED')
UNION ALL
SELECT 'configuration:' || event.id::text, 'CONFIGURATION',
       event.id,
       project.organization_id,
       event.project_id,
       event.actor_principal_id,
       'CONFIGURATION_CHANGED',
       'CONFIGURATION',
       event.subject_id,
       CASE
           WHEN event.action = 'REUSABLE_RESOURCE_VALIDATED' AND event.detail = 'INVALID' THEN 'FAILED'
           ELSE 'SUCCEEDED' END,
       NULL::text, event.content_digest::text, '[]'::jsonb, jsonb_build_array(jsonb_build_object('type', 'CONFIGURATION', 'id', event.subject_id::text)),
       event.request_id,
       event.correlation_id,
       event.graphql_operation,
       event.source_ip::text, event.user_agent,
       event.occurred_at,
       'CONFIGURATION.VIEW'
FROM configuration_audit_events event
         JOIN projects project ON project.id = event.project_id
WHERE event.action IN ('REUSABLE_RESOURCE_CREATED', 'REUSABLE_RESOURCE_DRAFT_UPDATED', 'REUSABLE_RESOURCE_VALIDATED',
                       'REUSABLE_RESOURCE_PUBLISHED', 'MCP_SERVER_CREATED', 'MCP_SERVER_UPDATED',
                       'LEGACY_TOOL_METADATA_SAVED')
UNION ALL
SELECT 'deployment:' || event.id::text, 'DEPLOYMENT',
       event.id,
       deployment.organization_id,
       deployment.project_id,
       event.actor_principal_id,
       CASE event.action
           WHEN 'REQUESTED' THEN 'DEPLOYMENT_REQUESTED'
           WHEN 'CANCELED' THEN 'DEPLOYMENT_CANCELED'
           WHEN 'APPROVAL_RECORDED' THEN 'DEPLOYMENT_APPROVAL_RECORDED'
           WHEN 'APPROVAL_REJECTED' THEN 'DEPLOYMENT_APPROVAL_REJECTED'
           WHEN 'APPROVAL_SATISFIED' THEN 'DEPLOYMENT_APPROVED'
           WHEN 'APPROVAL_INVALIDATED' THEN 'DEPLOYMENT_APPROVAL_INVALIDATED'
           WHEN 'APPROVAL_EXPIRED' THEN 'DEPLOYMENT_APPROVAL_EXPIRED'
           WHEN 'APPROVAL_REPLAYED' THEN 'DEPLOYMENT_APPROVAL_REPLAYED'
           WHEN 'APPROVAL_EXECUTION_BLOCKED' THEN 'DEPLOYMENT_EXECUTION_FAILED'
           WHEN 'EXECUTION_STARTED' THEN 'DEPLOYMENT_EXECUTION_STARTED'
           WHEN 'EXECUTION_SUCCEEDED' THEN 'DEPLOYMENT_EXECUTION_SUCCEEDED'
           WHEN 'EXECUTION_FAILED' THEN 'DEPLOYMENT_EXECUTION_FAILED'
           WHEN 'PROMOTION_RECORDED' THEN 'DEPLOYMENT_PROMOTED'
           WHEN 'ROLLBACK_RECORDED' THEN 'DEPLOYMENT_ROLLED_BACK'
           WHEN 'RETRY_RECORDED' THEN 'DEPLOYMENT_RETRIED'
           WHEN 'OUTBOX_DELIVERY_RETRIED' THEN 'DEPLOYMENT_DELIVERY_RETRIED'
           WHEN 'OUTBOX_DEAD_LETTERED' THEN 'DEPLOYMENT_DELIVERY_DEAD_LETTERED'
           WHEN 'OUTBOX_LEASE_RECLAIMED' THEN 'DEPLOYMENT_LEASE_RECLAIMED' END,
       'DEPLOYMENT',
       event.deployment_id,
       CASE
           WHEN event.action IN ('CANCELED') THEN 'CANCELED'
           WHEN event.action IN
                ('APPROVAL_REJECTED', 'APPROVAL_INVALIDATED', 'APPROVAL_EXPIRED', 'APPROVAL_EXECUTION_BLOCKED',
                 'EXECUTION_FAILED', 'OUTBOX_DEAD_LETTERED') THEN 'FAILED'
           ELSE 'SUCCEEDED' END,
       NULL::text, NULL::text, '[]'::jsonb, jsonb_build_array(
        jsonb_build_object('type', 'DEPLOYMENT', 'id', event.deployment_id::text),
        jsonb_build_object('type', 'AGENT_VERSION', 'id', deployment.agent_version_id::text),
        jsonb_build_object('type', 'APPROVAL_REQUIREMENT', 'id', event.facts ->>'requirementId'),
        jsonb_build_object('type', 'APPROVAL_DECISION', 'id', event.facts ->>'decisionId'),
        jsonb_build_object('type', 'DEPLOYMENT', 'id', event.facts ->>'resultDeploymentId')),
       event.request_id,
       event.correlation_id,
       event.graphql_operation,
       event.source_ip::text, event.user_agent,
       event.occurred_at,
       'DEPLOYMENT.VIEW'
FROM deployment_audit_events event
         JOIN deployments deployment ON deployment.id = event.deployment_id
WHERE event.action IN
      ('REQUESTED', 'CANCELED', 'APPROVAL_RECORDED', 'APPROVAL_REJECTED', 'APPROVAL_SATISFIED', 'APPROVAL_INVALIDATED',
       'APPROVAL_EXPIRED', 'APPROVAL_REPLAYED', 'APPROVAL_EXECUTION_BLOCKED', 'EXECUTION_STARTED',
       'EXECUTION_SUCCEEDED', 'EXECUTION_FAILED', 'PROMOTION_RECORDED', 'ROLLBACK_RECORDED', 'RETRY_RECORDED',
       'OUTBOX_DELIVERY_RETRIED', 'OUTBOX_DEAD_LETTERED', 'OUTBOX_LEASE_RECLAIMED')
UNION ALL
SELECT 'evaluation:' || event.id::text, 'EVALUATION',
       event.id,
       project.organization_id,
       project.id,
       event.actor_principal_id,
       CASE event.action
           WHEN 'CREATED' THEN 'EVALUATION_CREATED'
           WHEN 'UPDATED' THEN 'EVALUATION_UPDATED'
           WHEN 'VALIDATED' THEN 'EVALUATION_VALIDATED'
           WHEN 'PUBLISHED' THEN 'EVALUATION_PUBLISHED'
           WHEN 'QUEUED' THEN 'EVALUATION_RUN_STARTED'
           WHEN 'RERUN_QUEUED' THEN 'EVALUATION_RUN_STARTED'
           WHEN 'STARTED' THEN 'EVALUATION_RUN_STARTED'
           WHEN 'COMPLETED' THEN 'EVALUATION_RUN_PASSED'
           WHEN 'FAILED' THEN 'EVALUATION_RUN_FAILED'
           WHEN 'CANCELED' THEN 'EVALUATION_RUN_CANCELED' END,
       CASE WHEN event.run_id IS NULL THEN 'EVALUATION_DEFINITION' ELSE 'EVALUATION_RUN' END,
       COALESCE(event.run_id, event.definition_id),
       CASE
           WHEN event.action IN ('FAILED') THEN 'FAILED'
           WHEN event.action = 'CANCELED' THEN 'CANCELED'
           ELSE 'SUCCEEDED' END,
       NULL::text, NULL::text, '[]'::jsonb, jsonb_build_array(
        jsonb_build_object('type', 'EVALUATION_RUN', 'id', event.run_id::text),
        jsonb_build_object('type', 'EVALUATION_DEFINITION', 'id', definition.id::text),
        jsonb_build_object('type', 'AGENT_VERSION', 'id', target.agent_version_id::text),
        jsonb_build_object('type', 'DEPLOYMENT', 'id', target.deployment_id::text)),
       event.request_id,
       event.correlation_id,
       event.graphql_operation,
       event.source_ip::text, event.user_agent,
       event.occurred_at,
       CASE WHEN event.run_id IS NULL THEN 'EVALUATION_DEFINITION.VIEW' ELSE 'EVALUATION_RUN.VIEW' END
FROM evaluation_audit_events event
         LEFT JOIN evaluation_runs run ON run.id = event.run_id
         LEFT JOIN evaluation_definition_versions version ON version.id = run.definition_version_id
         LEFT JOIN evaluation_definitions definition
                   ON definition.id = COALESCE(event.definition_id, version.definition_id)
         LEFT JOIN evaluation_target_snapshots target ON target.run_id = event.run_id
         JOIN projects project ON project.id = definition.project_id
WHERE event.action IN
      ('CREATED', 'UPDATED', 'VALIDATED', 'PUBLISHED', 'QUEUED', 'RERUN_QUEUED', 'STARTED', 'COMPLETED', 'FAILED',
       'CANCELED');
