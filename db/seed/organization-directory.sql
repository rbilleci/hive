INSERT INTO principals (id, subject, display_name)
VALUES ('00000000-0000-0000-0000-000000000001', 'ada.fixture', 'Ada Lovelace'),
       ('00000000-0000-0000-0000-000000000002', 'bea.fixture', 'Beatrice Hopper') ON CONFLICT (id) DO NOTHING;

UPDATE principals
SET display_name = CASE id
                       WHEN '00000000-0000-0000-0000-000000000001' THEN 'Ada Lovelace'
                       WHEN '00000000-0000-0000-0000-000000000002' THEN 'Beatrice Hopper'
                       ELSE display_name END
WHERE id IN ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002');

INSERT INTO organizations (id, slug, display_name, lifecycle_status)
VALUES ('10000000-0000-0000-0000-000000000001', 'product', 'Product', 'ACTIVE'),
       ('10000000-0000-0000-0000-000000000002', 'support', 'Support', 'ACTIVE'),
       ('10000000-0000-0000-0000-000000000003', 'quality-assurance', 'Quality Assurance', 'ARCHIVED'),
       ('10000000-0000-0000-0000-000000000004', 'sre', 'SRE',
        'ACTIVE') ON CONFLICT (id) DO NOTHING;

INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at)
VALUES ('20000000-0000-0000-0000-000000000001', '10000000-0000-0000-0000-000000000001',
        '00000000-0000-0000-0000-000000000001', CURRENT_TIMESTAMP - INTERVAL '1 day', NULL),
       ('20000000-0000-0000-0000-000000000002', '10000000-0000-0000-0000-000000000002',
        '00000000-0000-0000-0000-000000000001', CURRENT_TIMESTAMP - INTERVAL '1 day', NULL),
       ('20000000-0000-0000-0000-000000000003', '10000000-0000-0000-0000-000000000003',
        '00000000-0000-0000-0000-000000000001', CURRENT_TIMESTAMP - INTERVAL '1 day', NULL),
       ('20000000-0000-0000-0000-000000000004', '10000000-0000-0000-0000-000000000004',
        '00000000-0000-0000-0000-000000000002', CURRENT_TIMESTAMP - INTERVAL '1 day', NULL),
       ('20000000-0000-0000-0000-000000000005', '10000000-0000-0000-0000-000000000004',
        '00000000-0000-0000-0000-000000000001', CURRENT_TIMESTAMP - INTERVAL '2 days',
        CURRENT_TIMESTAMP - INTERVAL '1 day') ON CONFLICT (id) DO NOTHING;

-- organization_memberships_one_active only constrains rows whose active_marker is TRUE, so a
-- seeded active membership has to carry the marker the administration repository sets on insert.
UPDATE organization_memberships SET active_marker = TRUE WHERE ended_at IS NULL AND active_marker IS NULL;

INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status)
VALUES ('50000000-0000-0000-0000-000000000001', '10000000-0000-0000-0000-000000000001', 'customer-feedback-copilot',
        'Customer Feedback Copilot', 'ACTIVE'),
       ('50000000-0000-0000-0000-000000000002', '10000000-0000-0000-0000-000000000001', 'usage-analytics',
        'Usage Analytics', 'ARCHIVED'),
       ('50000000-0000-0000-0000-000000000003', '10000000-0000-0000-0000-000000000004', 'incident-response',
        'Incident Response', 'ACTIVE') ON CONFLICT (id) DO NOTHING;

INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status)
VALUES ('60000000-0000-0000-0000-000000000001', '50000000-0000-0000-0000-000000000001', 'feedback-triage-agent',
        'Feedback Triage Agent', 'ACTIVE'),
       ('60000000-0000-0000-0000-000000000002', '50000000-0000-0000-0000-000000000001', 'sentiment-analyst',
        'Sentiment Analyst', 'DEPRECATED'),
       ('60000000-0000-0000-0000-000000000003', '50000000-0000-0000-0000-000000000001', 'feedback-digest-scribe',
        'Feedback Digest Scribe', 'ARCHIVED'),
       ('60000000-0000-0000-0000-000000000004', '50000000-0000-0000-0000-000000000003', 'incident-triage-agent',
        'Incident Triage Agent', 'ACTIVE') ON CONFLICT (id) DO
UPDATE SET
    project_id = EXCLUDED.project_id,
    slug = EXCLUDED.slug,
    display_name = EXCLUDED.display_name,
    lifecycle_status = EXCLUDED.lifecycle_status;

INSERT INTO agent_draft_editor_roles (project_id, principal_id, role_code)
VALUES ('50000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001',
        'AGENT_DEVELOPER') ON CONFLICT DO NOTHING;

INSERT INTO console_role_assignments (id, principal_id, organization_id, project_id, role_code)
VALUES ('81000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001',
        '10000000-0000-0000-0000-000000000001', NULL, 'ORGANIZATION_MEMBER'),
       ('81000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000001',
        '10000000-0000-0000-0000-000000000002', NULL, 'ORGANIZATION_MEMBER'),
       ('81000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000001',
        '10000000-0000-0000-0000-000000000003', NULL, 'ORGANIZATION_MEMBER'),
       ('81000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-000000000002',
        '10000000-0000-0000-0000-000000000004', NULL, 'ORGANIZATION_MEMBER'),
       ('81000000-0000-0000-0000-000000000005', '00000000-0000-0000-0000-000000000001', NULL,
        '50000000-0000-0000-0000-000000000001', 'AGENT_DEVELOPER') ON CONFLICT DO NOTHING;

INSERT INTO agent_operational_summaries (agent_id, draft_validation_status, draft_error_count, draft_warning_count,
                                         draft_validated_at,
                                         published_version, published_at, alias_target_count, active_alias_target_count,
                                         deployment_status,
                                         deployment_observed_at, evaluation_outcome, evaluation_completed_at,
                                         runtime_health, runtime_observed_at)
VALUES ('60000000-0000-0000-0000-000000000001', 'VALID', 0, 1, CURRENT_TIMESTAMP - INTERVAL '10 minutes',
        'v1.4.0', CURRENT_TIMESTAMP - INTERVAL '1 day', 3, 2, 'ACTIVE', CURRENT_TIMESTAMP - INTERVAL '20 seconds',
        'PASSED', CURRENT_TIMESTAMP - INTERVAL '5 minutes', 'HEALTHY', CURRENT_TIMESTAMP - INTERVAL '20 seconds'),
       ('60000000-0000-0000-0000-000000000002', 'INVALID', 2, 0, CURRENT_TIMESTAMP - INTERVAL '1 hour',
        'v1.2.0', CURRENT_TIMESTAMP - INTERVAL '7 days', 1, 1, 'DEGRADED', CURRENT_TIMESTAMP - INTERVAL '2 minutes',
        'INCONCLUSIVE', CURRENT_TIMESTAMP - INTERVAL '1 day', 'DEGRADED',
        CURRENT_TIMESTAMP - INTERVAL '7 minutes') ON CONFLICT (agent_id) DO
UPDATE SET
    draft_validation_status = EXCLUDED.draft_validation_status,
    draft_error_count = EXCLUDED.draft_error_count,
    draft_warning_count = EXCLUDED.draft_warning_count,
    draft_validated_at = EXCLUDED.draft_validated_at,
    published_version = EXCLUDED.published_version,
    published_at = EXCLUDED.published_at,
    alias_target_count = EXCLUDED.alias_target_count,
    active_alias_target_count = EXCLUDED.active_alias_target_count,
    deployment_status = EXCLUDED.deployment_status,
    deployment_observed_at = EXCLUDED.deployment_observed_at,
    evaluation_outcome = EXCLUDED.evaluation_outcome,
    evaluation_completed_at = EXCLUDED.evaluation_completed_at,
    runtime_health = EXCLUDED.runtime_health,
    runtime_observed_at = EXCLUDED.runtime_observed_at;

INSERT INTO project_dashboard_metrics (project_id, active_agents, active_deployments, failed_deployments,
                                       pending_approvals, unhealthy_resources, current_period_cost_cents,
                                       cost_availability,
                                       cost_period_start, cost_period_end, cost_currency, cost_data_as_of)
VALUES ('50000000-0000-0000-0000-000000000001', 3, 2, 1, 4, 1, 12345, 'AVAILABLE',
        date_trunc('month', CURRENT_TIMESTAMP), date_trunc('month', CURRENT_TIMESTAMP) + INTERVAL '1 month', 'USD',
        CURRENT_TIMESTAMP),
       ('50000000-0000-0000-0000-000000000002', 0, 0, 0, 0, 0, NULL, 'UNKNOWN', NULL, NULL, NULL, NULL),
       ('50000000-0000-0000-0000-000000000003', 7, 3, 2, 5, 2, 67890, 'AVAILABLE',
        date_trunc('month', CURRENT_TIMESTAMP), date_trunc('month', CURRENT_TIMESTAMP) + INTERVAL '1 month', 'USD',
        CURRENT_TIMESTAMP) ON CONFLICT (project_id) DO
UPDATE SET
    active_agents = EXCLUDED.active_agents,
    active_deployments = EXCLUDED.active_deployments,
    failed_deployments = EXCLUDED.failed_deployments,
    pending_approvals = EXCLUDED.pending_approvals,
    unhealthy_resources = EXCLUDED.unhealthy_resources,
    current_period_cost_cents = EXCLUDED.current_period_cost_cents,
    cost_availability = EXCLUDED.cost_availability,
    cost_period_start = EXCLUDED.cost_period_start,
    cost_period_end = EXCLUDED.cost_period_end,
    cost_currency = EXCLUDED.cost_currency,
    cost_data_as_of = EXCLUDED.cost_data_as_of;
