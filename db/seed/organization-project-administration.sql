-- M09 fixtures require the declared base organization/project seed to exist first.
UPDATE principals
SET email = subject || '@local.invalid'
WHERE email IS NULL
   OR email = '';
UPDATE principals
SET email = CASE id
                WHEN '00000000-0000-0000-0000-000000000001' THEN 'ada.lovelace@local.invalid'
                WHEN '00000000-0000-0000-0000-000000000002' THEN 'beatrice.hopper@local.invalid'
                ELSE email END
WHERE id IN ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002');
UPDATE principals
SET last_seen_at = CURRENT_TIMESTAMP - INTERVAL '5 minutes'
WHERE last_seen_at IS NULL;

INSERT INTO organization_membership_roles (membership_id, role_code)
SELECT membership.id, 'ORGANIZATION_MEMBER'
FROM organization_memberships membership ON CONFLICT DO NOTHING;

INSERT INTO organization_membership_roles (membership_id, role_code)
VALUES ('20000000-0000-0000-0000-000000000001', 'ORGANIZATION_ADMIN') ON CONFLICT DO NOTHING;

INSERT INTO project_budget_policies (project_id)
SELECT id
FROM projects ON CONFLICT DO NOTHING;
INSERT INTO project_budget_policy_versions (project_id, revision, currency, monthly_limit_cents,
                                            warning_threshold_cents, change_reason)
VALUES ('50000000-0000-0000-0000-000000000001', 1, 'USD', 500000, 400000,
        'Initial local policy') ON CONFLICT DO NOTHING;
UPDATE project_budget_policies
SET current_revision = 1
WHERE project_id = '50000000-0000-0000-0000-000000000001'
  AND current_revision = 0;

INSERT INTO frozen_spend_import_batches (id, project_id, period_start, period_end, currency, state, amount_cents,
                                         includes_estimates, data_as_of, completed_at)
VALUES ('83000000-0000-0000-0000-000000000001', '50000000-0000-0000-0000-000000000001',
        date_trunc('month', CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC',
        date_trunc('month', CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' + INTERVAL '1 month',
        'USD', 'COMPLETE', 12345, TRUE, CURRENT_TIMESTAMP - INTERVAL '2 hours',
        CURRENT_TIMESTAMP - INTERVAL '1 hour') ON CONFLICT DO NOTHING;

INSERT INTO project_approval_policies (id, project_id, current_revision)
SELECT id, id, 1
FROM projects ON CONFLICT DO NOTHING;
INSERT INTO project_approval_policy_versions (policy_id, revision, digest, matrix, change_reason)
SELECT id,
       1,
       '935b9f24ed4ca64ee5c5f19835b3aaed6e8a2f4fef8fd3761e82aba0f1149ae6',
       '{"DEVELOPMENT_HIGH":{"requiredApprovers":1,"requiredEvidence":["CHANGE_SUMMARY_READY","EVALUATION_PASSED","PLAN_VALIDATED"]},"DEVELOPMENT_LOW":{"requiredApprovers":0,"requiredEvidence":["PLAN_VALIDATED"]},"DEVELOPMENT_MEDIUM":{"requiredApprovers":0,"requiredEvidence":["CHANGE_SUMMARY_READY","PLAN_VALIDATED"]},"PRODUCTION_HIGH":{"requiredApprovers":2,"requiredEvidence":["CHANGE_SUMMARY_READY","EVALUATION_PASSED","PLAN_VALIDATED"]},"PRODUCTION_LOW":{"requiredApprovers":1,"requiredEvidence":["CHANGE_SUMMARY_READY","EVALUATION_PASSED","PLAN_VALIDATED"]},"PRODUCTION_MEDIUM":{"requiredApprovers":1,"requiredEvidence":["CHANGE_SUMMARY_READY","EVALUATION_PASSED","PLAN_VALIDATED"]},"STAGING_HIGH":{"requiredApprovers":1,"requiredEvidence":["CHANGE_SUMMARY_READY","EVALUATION_PASSED","PLAN_VALIDATED"]},"STAGING_LOW":{"requiredApprovers":0,"requiredEvidence":["CHANGE_SUMMARY_READY","PLAN_VALIDATED"]},"STAGING_MEDIUM":{"requiredApprovers":1,"requiredEvidence":["CHANGE_SUMMARY_READY","EVALUATION_PASSED","PLAN_VALIDATED"]}}'::jsonb, 'Initial fixed local P-05 policy'
FROM projects ON CONFLICT DO NOTHING;
