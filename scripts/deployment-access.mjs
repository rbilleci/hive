import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { createIsolatedDatabase, postgresClient, startIsolatedLocalService, startLocalDeploymentWorker } from "./local-service.mjs";

const requester = "00000000-0000-0000-0000-000000000001";
const outsider = "d1300000-0000-0000-0000-000000000005";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const project = "50000000-0000-0000-0000-000000000003";
const membership = "d1300000-0000-0000-0000-000000000003";
const role = "d1300000-0000-0000-0000-000000000004";
const platformPrincipal = "d1300000-0000-0000-0000-000000000006";
const organizationAdminPrincipal = "d1300000-0000-0000-0000-000000000007";
const organizationMemberPrincipal = "d1300000-0000-0000-0000-000000000008";
const approverPrincipal = "d1300000-0000-0000-0000-000000000009";
const organizationAdminMembership = "d1300000-0000-0000-0000-000000000010";
const organizationMemberMembership = "d1300000-0000-0000-0000-000000000011";
const approverOrganizationMembership = "d1300000-0000-0000-0000-000000000012";
const approverProjectMembership = "d1300000-0000-0000-0000-000000000013";
const requesterProjectMembership = "d1300000-0000-0000-0000-000000000014";
const legacyConsoleRole = "d1300000-0000-0000-0000-000000000015";
let endpoint = "";
const run = Date.now().toString(36);

const deploymentFields = "id lifecycleStatus revision projectionRevision environmentDefinitionVersions { id stableDefinitionId version } plan { agentVersionId agentContentDigest environmentDefinitionVersionId targetDigest planDigest packageDigest packageReference catalogReleaseId catalogReleaseDigest } deploymentPolicySnapshots { risk requiredEvidence requiredApprovers bindingDigest evaluationRequirementExpiresAt } deploymentEvidenceSnapshots { nodes { evidenceKind evidenceDigest bindingDigest expiresAt state } } currentAttempt { id attemptNumber status failureCode failureSummary } deploymentRuntimeHealth { status summary generation }";
const problemFields = "code message resourceId expectedRevision actualRevision";

async function graphql(service, principal, query, variables) {
  const response = await fetch(endpoint, {
    method: "POST", headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.errors, undefined, JSON.stringify(body.errors));
  return body.data;
}

async function publishFixture(service) {
  const created = await graphql(service, requester,
    "mutation Create($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId revision } problems { code } } }",
    { input: { projectId: project, displayName: "M13 deployment fixture " + run } });
  assert.deepEqual(created.createAgentDraft.problems, []);
  const agentId = created.createAgentDraft.agentDraft.agentId;
  const document = {
    general: { displayName: "M13 deployment fixture", description: "Deterministic local deployment fixture." },
    instructions: { source: "# local fixture", language: "markdown" }, harness: { source: "def run(value): return value", language: "python" },
    model: { reference: "model:local-safe-chat@v2" }, tools: { source: "{}", language: "json" }, skills: { source: "export const skills = [];", language: "javascript" },
    capabilities: { source: "export type Capability = string;", language: "typescript" }, subagents: { enabled: false }, memory: { strategy: "project" },
    guardrails: { source: "# guardrail checks", language: "shell" }, identity: { persona: "" }, observability: { source: "<observability/>", language: "xml" },
    limits: { maxTokens: 256 }, evaluations: { required: false }, dependencies: ["model:local-safe-chat@v2"]
  };
  const saved = await graphql(service, requester,
    "mutation Save($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: created.createAgentDraft.agentDraft.revision, document } });
  assert.deepEqual(saved.updateAgentDraft.problems, []);
  const validated = await graphql(service, requester,
    "mutation Validate($input: ValidateAgentDraftInput!) { validateAgentDraft(input: $input) { agentDraft { revision validationStatus } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: saved.updateAgentDraft.agentDraft.revision } });
  assert.equal(validated.validateAgentDraft.agentDraft.validationStatus, "VALID");
  const published = await graphql(service, requester,
    "mutation Publish($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentVersion { id } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: validated.validateAgentDraft.agentDraft.revision, warningsAcknowledged: true } });
  assert.deepEqual(published.publishAgentDraft.problems, []);
  return { agentId, versionId: published.publishAgentDraft.agentVersion.id };
}

async function environmentId(service, versionId, logicalClass) {
  const result = await graphql(service, requester, "query Environments($version: String!) { agentVersions(filters: { id: { eq: $version } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { catalogReleases { environmentDefinitionVersions(orderBy: { stableDefinitionId: ASC, version: ASC, id: ASC }, pagination: { page: { limit: 50, page: 0 } }) { nodes { id logicalEnvironmentClass stableDefinitionId version catalogReleaseDigest } } } } } }", { version: versionId });
  const environments = result.agentVersions.nodes[0].catalogReleases.environmentDefinitionVersions.nodes;
  assert(environments.every((item) => item.catalogReleaseDigest.length > 0));
  const environment = environments.find((item) => item.logicalEnvironmentClass === logicalClass);
  assert(environment, "The fixture catalog must expose " + logicalClass + ".");
  return environment.id;
}

async function request(service, versionId, environmentDefinitionVersionId, key, strategy = "ROLLING") {
  return requestFor(service, requester, versionId, environmentDefinitionVersionId, key, strategy);
}

async function requestFor(service, principal, versionId, environmentDefinitionVersionId, key, strategy = "ROLLING") {
  const result = await graphql(service, principal,
    `mutation Deploy($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { ${deploymentFields} } problems { ${problemFields} } } }`,
    { input: { agentVersionId: versionId, environmentDefinitionVersionId, strategy, idempotencyKey: key } });
  assert.deepEqual(result.deployAgentVersion.problems, []);
  return result.deployAgentVersion.deployment;
}

/// The deployment and its bounded timeline, from the generated entity query.
async function detail(service, principal, deploymentId, first = 100) {
  const result = await graphql(service, principal,
    `query Detail($id: String!) { deployments(filters: { id: { eq: $id } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { ${deploymentFields} timeline(first: ${first}) { id attemptNumber sequence stage status message source } } } }`,
    { id: deploymentId });
  return result.deployments.nodes[0] ?? null;
}

async function waitFor(service, deploymentId, status, timeout = 25_000) {
  const started = Date.now();
  while (Date.now() - started < timeout) {
    const value = await detail(service, requester, deploymentId);
    if (value?.lifecycleStatus === status) return value;
    await new Promise((resolve) => setTimeout(resolve, 140));
  }
  throw new Error(`Deployment ${deploymentId} did not reach ${status}.`);
}

async function waitForOutbox(client, deploymentId, status, minimumAttempts = 0, timeout = 25_000) {
  const started = Date.now();
  while (Date.now() - started < timeout) {
    const result = await client.query("SELECT status, attempt_count FROM deployment_outbox_events WHERE deployment_id = $1 ORDER BY created_at ASC LIMIT 1", [deploymentId]);
    if (result.rows[0]?.status === status && result.rows[0].attempt_count >= minimumAttempts) return result.rows[0];
    await new Promise((resolve) => setTimeout(resolve, 140));
  }
  throw new Error(`Deployment ${deploymentId} did not produce outbox state ${status}.`);
}

const database = await createIsolatedDatabase("hive_m13_access");
let service;
let client;
let worker;
try {
  service = await startIsolatedLocalService(database.name);
  endpoint = `http://127.0.0.1:${service.port}/graphql`;
  client = await postgresClient(database.name);
  await client.query("DELETE FROM console_role_assignments WHERE id = $1", [role]);
  await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1", [requesterProjectMembership]);
  await client.query("DELETE FROM project_memberships WHERE id = $1", [requesterProjectMembership]);
  await client.query("DELETE FROM organization_membership_roles WHERE membership_id = $1", [membership]);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [membership]);
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL) ON CONFLICT (id) DO NOTHING", [membership, privateOrganization, requester]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1)", [requesterProjectMembership, project, requester]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AGENT_DEVELOPER')", [requesterProjectMembership]);
  await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, 'm13-legacy-console', 'M13 Legacy Console', 'm13-legacy-console@local.invalid') ON CONFLICT (id) DO NOTHING", [outsider]);
  await client.query("INSERT INTO console_role_assignments (id, principal_id, project_id, role_code) VALUES ($1, $2, $3, 'AGENT_DEVELOPER') ON CONFLICT DO NOTHING", [legacyConsoleRole, outsider, project]);
  const { versionId } = await publishFixture(service);
  const development = await environmentId(service, versionId, "DEVELOPMENT");
  const staging = await environmentId(service, versionId, "STAGING");
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'PROJECT_ADMIN') ON CONFLICT DO NOTHING", [requesterProjectMembership]);
  const [concurrentDeployment, concurrentRoleChange] = await Promise.all([
    graphql(service, requester,
      "mutation ConcurrentDeploy($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }",
      { input: { agentVersionId: versionId, environmentDefinitionVersionId: development, strategy: "REPLACE", idempotencyKey: `m13-${run}-authority-lock` } }),
    graphql(service, requester,
      "mutation ConcurrentRoles($input: ReplaceAdministrationMembershipInput!) { replaceAdministrationMembershipRoles(input: $input) { problems { code } } }",
      { input: { scope: "PROJECT", scopeId: project, membershipId: requesterProjectMembership, roleCodes: ["PROJECT_ADMIN"], expectedRevision: 1 } })
  ]);
  assert.deepEqual(concurrentDeployment.deployAgentVersion.problems, []);
  assert.deepEqual(concurrentRoleChange.replaceAdministrationMembershipRoles.problems, []);
  for (const [id, subject] of [[platformPrincipal, "m13-platform"], [organizationAdminPrincipal, "m13-organization-admin"], [organizationMemberPrincipal, "m13-organization-member"], [approverPrincipal, "m13-deployment-approver"]]) {
    await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, $2, $2, $2 || '@local.invalid') ON CONFLICT (id) DO NOTHING", [id, subject]);
  }
  await client.query("INSERT INTO platform_role_assignments (principal_id, role_code) VALUES ($1, 'PLATFORM_ADMIN') ON CONFLICT DO NOTHING", [platformPrincipal]);
  await client.query("SELECT set_config('hive.m14_approval_role_assignment_actor', $1, FALSE)", [platformPrincipal]);
  for (const [id, principal, roleCode] of [[organizationAdminMembership, organizationAdminPrincipal, "ORGANIZATION_ADMIN"], [organizationMemberMembership, organizationMemberPrincipal, "ORGANIZATION_MEMBER"], [approverOrganizationMembership, approverPrincipal, "ORGANIZATION_MEMBER"]]) {
    await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL) ON CONFLICT (id) DO NOTHING", [id, privateOrganization, principal]);
    await client.query("INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, $2) ON CONFLICT DO NOTHING", [id, roleCode]);
  }
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1) ON CONFLICT (id) DO NOTHING", [approverProjectMembership, project, approverPrincipal]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'DEPLOYMENT_APPROVER') ON CONFLICT DO NOTHING", [approverProjectMembership]);

  // The frozen-inputs preview is the computed `deploymentPreview` field on the generated
  // `AgentVersions` row: the version's own tenant rule decides whether it is reachable, and the
  // field applies the same `DEPLOYMENT.VIEW`-at-the-version's-project test the deleted query did.
  const previewQuery = (selection) => `query Preview($version: String!, $environment: String!) {
    agentVersions(filters: { id: { eq: $version } }, pagination: { page: { limit: 1, page: 0 } }) {
      nodes { deploymentPreview(environmentDefinitionVersionId: $environment, strategy: "ROLLING") { ${selection} } }
    }
  }`;
  const requestPreview = async (principal, selection, environment) =>
    (await graphql(service, principal, previewQuery(selection), { version: versionId, environment }))
      .agentVersions.nodes[0]?.deploymentPreview ?? null;
  const preview = await requestPreview(requester,
    "environmentDefinitionVersion { id stableDefinitionId version catalogReleaseId catalogReleaseDigest contentDigest } risk policyDigest policyRevision requiredEvidence planDigest packageDigest agentContentDigest targetDigest bindingDigest currentTarget { deploymentId } requirementExpiresAt warnings compatibility",
    development);
  assert.equal(preview.environmentDefinitionVersion.id, development);
  assert.equal(preview.risk, "MEDIUM");
  assert.match(preview.bindingDigest, /^[a-f0-9]{64}$/);
  assert.match(preview.compatibility, /catalog release/i);
  const expiryPreview = await requestPreview(requester, "requiredEvidence requirementExpiresAt warnings", staging);
  assert(expiryPreview.requiredEvidence.includes("EVALUATION_PASSED"));
  assert.notEqual(expiryPreview.requirementExpiresAt, null);
  // An organization member with no deployment capability reads no agent version row at all, so the
  // preview is unreachable rather than empty.
  assert.equal(await requestPreview(organizationMemberPrincipal, "risk", development), null);
  const capabilityList = "query CapabilityList($project: String!) { deployments(filters: { projectId: { eq: $project } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { id } } }";
  const capabilityContext = "query CapabilityContext($project: String!) { projects(filters: { id: { eq: $project } }) { nodes { capabilities } } }";
  const deploymentCodes = async (principal) => ((await graphql(service, principal, capabilityContext, { project })).projects.nodes[0]?.capabilities ?? [])
    .filter((code) => code.startsWith("DEPLOYMENT.")).sort();
  assert.deepEqual(await deploymentCodes(platformPrincipal), ["DEPLOYMENT.CANCEL", "DEPLOYMENT.PROMOTE", "DEPLOYMENT.REQUEST", "DEPLOYMENT.RETRY", "DEPLOYMENT.ROLLBACK", "DEPLOYMENT.VIEW"]);
  assert.deepEqual(await deploymentCodes(organizationAdminPrincipal), ["DEPLOYMENT.VIEW"]);
  assert.deepEqual(await deploymentCodes(organizationMemberPrincipal), []);
  assert.deepEqual(await deploymentCodes(approverPrincipal), ["DEPLOYMENT.VIEW"]);
  assert.deepEqual(await deploymentCodes(requester), ["DEPLOYMENT.CANCEL", "DEPLOYMENT.PROMOTE", "DEPLOYMENT.REQUEST", "DEPLOYMENT.RETRY", "DEPLOYMENT.ROLLBACK", "DEPLOYMENT.VIEW"]);
  const listed = async (principal) => (await graphql(service, principal, capabilityList, { project })).deployments.nodes.length;
  assert.equal(await listed(platformPrincipal), 1);
  assert.equal(await listed(organizationAdminPrincipal), 1);
  assert.equal(await listed(organizationMemberPrincipal), 0);
  assert.equal(await listed(approverPrincipal), 1);
  assert.equal(await listed(outsider), 0);
  const forbiddenWrite = await graphql(service, organizationAdminPrincipal,
    "mutation Denied($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { problems { code } } }",
    { input: { agentVersionId: versionId, environmentDefinitionVersionId: development, strategy: "ROLLING", idempotencyKey: `m13-${run}-org-admin` } });
  assert.equal(forbiddenWrite.deployAgentVersion.problems[0].code, "FORBIDDEN");

  const waiting = await request(service, versionId, staging, `m13-${run}-cancel`);
  assert.equal(waiting.lifecycleStatus, "AWAITING_APPROVAL");
  assert.equal(waiting.deploymentEvidenceSnapshots.nodes.some((evidence) => evidence.evidenceKind === "EVALUATION_PASSED"), false);
  const evaluationRequirement = await client.query("SELECT evaluation_requirement_expires_at FROM deployment_policy_snapshots WHERE deployment_id = $1", [waiting.id]);
  assert.notEqual(evaluationRequirement.rows[0].evaluation_requirement_expires_at, null);
  const stale = await graphql(service, requester,
    `mutation Stale($input: CancelDeploymentInput!) { cancelDeployment(input: $input) { deployment { id } problems { ${problemFields} } } }`,
    { input: { deploymentId: waiting.id, expectedRevision: waiting.revision + 1 } });
  assert.equal(stale.cancelDeployment.problems[0].code, "REVISION_CONFLICT");
  const canceled = await graphql(service, requester,
    `mutation Cancel($input: CancelDeploymentInput!) { cancelDeployment(input: $input) { deployment { ${deploymentFields} } problems { ${problemFields} } } }`,
    { input: { deploymentId: waiting.id, expectedRevision: waiting.revision, reason: "credential-canary=do-not-store" } });
  assert.deepEqual(canceled.cancelDeployment.problems, []);
  assert.equal(canceled.cancelDeployment.deployment.lifecycleStatus, "CANCELED");
  const cancellationAudit = await client.query("SELECT facts::text AS facts FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'CANCELED'", [waiting.id]);
  assert.doesNotMatch(cancellationAudit.rows[0].facts, /credential-canary/i);
  // `first` bounds the timeline; the order is attempt, then timeline sequence, then key.
  const cancellationFirstEvent = await detail(service, requester, waiting.id, 1);
  assert.deepEqual(cancellationFirstEvent.timeline.map((event) => event.stage), ["REQUESTED"]);
  const cancellationTimeline = await detail(service, requester, waiting.id, 100);
  assert.deepEqual(cancellationTimeline.timeline.slice(0, 3).map((event) => event.stage),
    ["REQUESTED", "APPROVAL_INVALIDATED", "CANCELED"]);
  // This scenario used to simulate a predecessor database recording deployment approval before
  // V015/V017 (deleting those hive_schema_migrations markers, then rerunning the worker to observe
  // deployment_approval_upgrade_progress/_compatibility_progress's scheduled backfill restore
  // evaluation_requirement_expires_at) and predating V016 (deleting its marker to observe
  // deployment_legacy_request_facts's LOCAL_FAILURE-to-ROLLING backfill). Both mechanisms are removed,
  // not ported, along with their SQL/Java machinery -- see
  // PostgresDeploymentRepository.reconcileApprovalUpgrade()'s comment and V015/V016's own removal
  // comments -- so this scenario is removed too. V016's still-live, unconditional migration-replay
  // repair (environment/fingerprint/plan/policy/evidence binding, unrelated to either removed
  // mechanism) keeps its own coverage below, against a fresh deployment that never depended on
  // deployment_legacy_request_facts.

  const first = await request(service, versionId, development, `m13-${run}-idempotency`);
  const repeated = await request(service, versionId, development, `m13-${run}-idempotency`);
  assert.equal(repeated.id, first.id);
  const mismatch = await graphql(service, requester,
    `mutation Mismatch($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { problems { ${problemFields} } } }`,
    { input: { agentVersionId: versionId, environmentDefinitionVersionId: development, strategy: "CANARY", idempotencyKey: `m13-${run}-idempotency` } });
  assert.equal(mismatch.deployAgentVersion.problems[0].code, "IDEMPOTENCY_CONFLICT");

  // No DROP RULE here for deployment_evidence_snapshots_no_update either: it no longer exists under
  // Aurora DSQL compatibility (see V014's removal comment, its original declaration site), so dropping
  // it would fail rather than being a no-op.
  // No DROP RULE/DROP TRIGGER here for deployment_plan_versions_no_update, deployment_policy_snapshots_
  // no_update, deployments_frozen_request_trigger, or deployments_environment_catalog_binding_trigger:
  // none of the four exist any more under Aurora DSQL compatibility (see V014/V015's comments).
  // V016 no longer backfills request fingerprints or plan, policy, and evidence digests: the Aurora
  // DSQL alignment removed those UPDATEs with pgcrypto (see the comment that closes V016), so there is
  // no repair to observe after a ledger replay.

  worker = await startLocalDeploymentWorker(database.name, { HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "1000", HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "250" });
  const active = await waitFor(service, first.id, "ACTIVE");
  assert.equal(active.currentAttempt.status, "SUCCEEDED");
  assert.equal(active.deploymentRuntimeHealth.status, "HEALTHY");
  assert(active.timeline.length >= 5);
  const timelineStages = active.timeline.map((event) => event.stage);
  assert(timelineStages.indexOf("EXECUTION_STARTED") < timelineStages.indexOf("COMPLETED"));
  assert(timelineStages.indexOf("COMPLETED") < timelineStages.indexOf("EXECUTION_SUCCEEDED"));
  const frozen = await client.query("SELECT agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest, binding_digest FROM deployment_policy_snapshots WHERE deployment_id = $1", [first.id]);
  assert.equal(frozen.rows[0].agent_version_id, versionId);
  assert.equal(frozen.rows[0].environment_definition_version_id, development);
  assert.equal(frozen.rows[0].target_digest, active.plan.targetDigest);
  assert.equal(frozen.rows[0].plan_digest, active.plan.planDigest);
  assert.equal(frozen.rows[0].package_digest, active.plan.packageDigest);
  // No raw-SQL rewrite-rejection check here: deployment_plan_versions_no_update no longer exists under
  // Aurora DSQL compatibility (V014 stopped creating it), and PostgresDeploymentRepository never
  // UPDATEs deployment_plan_versions in the first place -- there is no application-level operation left
  // to guard.

  const attemptCount = await client.query("SELECT count(*)::int AS count FROM deployment_attempts WHERE deployment_id = $1", [first.id]);
  await client.query("UPDATE deployment_outbox_events SET status = 'PENDING', available_at = CURRENT_TIMESTAMP, delivered_at = NULL WHERE deployment_id = $1 AND event_type = 'COMPLETE_DEPLOYMENT'", [first.id]);
  await waitForOutbox(client, first.id, "DELIVERED");
  const duplicateCount = await client.query("SELECT count(*)::int AS count FROM deployment_attempts WHERE deployment_id = $1", [first.id]);
  assert.equal(duplicateCount.rows[0].count, attemptCount.rows[0].count);

  await worker.stop();
  worker = undefined;
  const retry = await request(service, versionId, development, `m13-${run}-retry`, "REPLACE");
  await client.query("UPDATE deployment_outbox_events SET payload = '{\"mode\":\"RETRY\"}'::jsonb WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [retry.id]);
  worker = await startLocalDeploymentWorker(database.name, { HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "1000", HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "250" });
  await waitForOutbox(client, retry.id, "PENDING", 1);
  await client.query("UPDATE deployment_outbox_events SET payload = '{\"mode\":\"SUCCESS\"}'::jsonb, available_at = CURRENT_TIMESTAMP WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [retry.id]);
  await waitFor(service, retry.id, "ACTIVE");

  const reclaimed = await request(service, versionId, development, `m13-${run}-lease`, "BLUE_GREEN");
  await client.query(`INSERT INTO deployment_worker_heartbeats
      (worker_id, observed_at, last_batch_deliveries, pending_events, oldest_pending_at, state, failure_code, approval_execution_compatible)
    VALUES ('abandoned', CURRENT_TIMESTAMP, 0, 0, NULL, 'READY', NULL, TRUE)
    ON CONFLICT (worker_id) DO UPDATE SET observed_at = EXCLUDED.observed_at, state = EXCLUDED.state,
      approval_execution_compatible = EXCLUDED.approval_execution_compatible`);
  await client.query("UPDATE deployment_outbox_events SET status = 'PROCESSING', claimed_at = CURRENT_TIMESTAMP - INTERVAL '1 minute', claimed_by = 'abandoned' WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [reclaimed.id]);
  await waitFor(service, reclaimed.id, "ACTIVE");
  const leaseAudit = await client.query("SELECT 1 FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'OUTBOX_LEASE_RECLAIMED'", [reclaimed.id]);
  assert.equal(leaseAudit.rowCount, 1);
  const reclaimedProjection = await detail(service, requester, reclaimed.id);
  assert(reclaimedProjection.projectionRevision > reclaimed.projectionRevision);

  await worker.stop();
  worker = undefined;
  const poison = await request(service, versionId, development, `m13-${run}-poison`, "CANARY");
  const poisonPlan = await client.query("SELECT id FROM deployment_plan_versions WHERE deployment_id = $1", [poison.id]);
  const poisonAttempt = "d1300000-0000-0000-0000-" + Date.now().toString(16).padStart(12, "0").slice(-12);
  await client.query("INSERT INTO deployment_attempts (id, deployment_id, deployment_plan_version_id, attempt_number, status, generation, started_at) VALUES ($1, $2, $3, 1, 'RUNNING', 1, CURRENT_TIMESTAMP)", [poisonAttempt, poison.id, poisonPlan.rows[0].id]);
  // Construct the retained in-progress poison state without claiming an approval handoff. No
  // DISABLE/ENABLE TRIGGER bracket needed: deployment_approval_execution_worker_gate_trigger is
  // removed (Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright -- see the Deployment/
  // approval domain step's own removal comment in V017), so this raw UPDATE already succeeds
  // directly, the same way it would need to for a real M13 regression starting after that fence.
  await client.query("UPDATE deployments SET lifecycle_status = 'IN_PROGRESS', revision = revision + 1 WHERE id = $1", [poison.id]);
  await client.query("UPDATE deployment_outbox_events SET payload = '{\"mode\":\"UNRECOGNIZED\"}'::jsonb WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [poison.id]);
  worker = await startLocalDeploymentWorker(database.name, { HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "1000", HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "250" });
  const failed = await waitFor(service, poison.id, "FAILED");
  assert.equal(failed.currentAttempt.status, "FAILED");
  assert.equal(failed.currentAttempt.failureCode, "LOCAL_OUTBOX_POISON");
  const nonterminal = await client.query("SELECT count(*)::int AS count FROM deployment_attempts WHERE deployment_id = $1 AND status IN ('QUEUED', 'RUNNING')", [poison.id]);
  assert.equal(nonterminal.rows[0].count, 0);
  await waitForOutbox(client, poison.id, "DEAD_LETTER", 3);

  // The generated connection pages by page number, newest first, with the key as the tie-break.
  const listQuery = `query Deployments($project: String!, $page: Int!) { deployments(filters: { projectId: { eq: $project }, agentVersionId: { eq: "${versionId}" } }, orderBy: { requestedAt: DESC, id: DESC }, pagination: { page: { limit: 2, page: $page } }) { nodes { id lifecycleStatus } paginationInfo { pages current } } }`;
  const list = await graphql(service, requester, listQuery, { project, page: 0 });
  assert(list.deployments.nodes.length <= 2);
  if (list.deployments.paginationInfo.pages > 1) {
    const next = await graphql(service, requester, listQuery, { project, page: 1 });
    assert(next.deployments.nodes.every((node) => !list.deployments.nodes.some((first) => first.id === node.id)));
  }
  // An unauthorized read of a visible row is an empty connection, not an error.
  const hidden = await detail(service, outsider, first.id);
  assert.equal(hidden, null);

  const quotaPrincipal = randomUUID();
  const quotaOrganizationMembership = randomUUID();
  const quotaProjectMembership = randomUUID();
  await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, $2, $2, $2 || '@local.invalid')", [quotaPrincipal, "m13-quota-" + run]);
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL)", [quotaOrganizationMembership, privateOrganization, quotaPrincipal]);
  await client.query("INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_MEMBER')", [quotaOrganizationMembership]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1)", [quotaProjectMembership, project, quotaPrincipal]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AGENT_DEVELOPER')", [quotaProjectMembership]);
  for (let index = 0; index < 19; index += 1) await requestFor(service, quotaPrincipal, versionId, staging, `m13-${run}-quota-${index}`);
  const quotaResults = await Promise.all(["a", "b"].map((suffix) => graphql(service, quotaPrincipal,
    "mutation Quota($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }",
    { input: { agentVersionId: versionId, environmentDefinitionVersionId: staging, strategy: "ROLLING", idempotencyKey: `m13-${run}-quota-final-${suffix}` } })));
  const quotaOutcomes = quotaResults.map((result) => result.deployAgentVersion);
  assert.equal(quotaOutcomes.filter((outcome) => outcome.deployment !== null).length, 1);
  assert.equal(quotaOutcomes.filter((outcome) => outcome.problems[0]?.code === "RATE_LIMITED").length, 1);
} finally {
  try {
    if (client) {
      await client.query("DELETE FROM console_role_assignments WHERE id = $1", [legacyConsoleRole]);
      await worker?.stop();
      await client.query("DELETE FROM console_role_assignments WHERE id = $1", [role]);
      await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1", [requesterProjectMembership]);
      await client.query("DELETE FROM project_memberships WHERE id = $1", [requesterProjectMembership]);
      await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1", [approverProjectMembership]);
      await client.query("DELETE FROM project_memberships WHERE id = $1", [approverProjectMembership]);
      await client.query("DELETE FROM platform_role_assignments WHERE principal_id = $1", [platformPrincipal]);
      for (const id of [organizationAdminMembership, organizationMemberMembership, approverOrganizationMembership]) await client.query("DELETE FROM organization_membership_roles WHERE membership_id = $1", [id]);
      for (const id of [organizationAdminMembership, organizationMemberMembership, approverOrganizationMembership]) await client.query("DELETE FROM organization_memberships WHERE id = $1", [id]);
      await client.query("DELETE FROM organization_membership_roles WHERE membership_id = $1", [membership]);
      await client.query("DELETE FROM organization_memberships WHERE id = $1", [membership]);
    }
  } finally {
    if (client) await client.end();
    if (worker) await worker.stop();
    if (service) await service.stop();
    await database.drop();
  }
}
