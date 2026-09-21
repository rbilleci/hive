import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { createIsolatedDatabase, postgresClient, startIsolatedLocalService, startLocalDeploymentWorker } from "./local-service.mjs";

const requester = "d1400000-0000-0000-0000-000000000001";
const approverOne = "d1400000-0000-0000-0000-000000000002";
const approverTwo = "d1400000-0000-0000-0000-000000000003";
const viewer = "d1400000-0000-0000-0000-000000000004";
const outsider = "d1400000-0000-0000-0000-000000000005";
const lateJoiner = "d1400000-0000-0000-0000-000000000006";
const developerOnly = "d1400000-0000-0000-0000-000000000007";
const platformAdministrator = "d1400000-0000-0000-0000-000000000008";
const workerSafetyRequester = "d1400000-0000-0000-0000-000000000009";
const organization = "10000000-0000-0000-0000-000000000004";
const project = "50000000-0000-0000-0000-000000000003";
const run = Date.now().toString(36);
let endpoint = "";
let lastGraphqlRequestId = "";

const deploymentFields = "id projectId lifecycleStatus revision deploymentPolicySnapshots { policyDigest policyRevision risk requiredEvidence requiredApprovers }";
// The approval surface is the generated `deploymentApprovalRequirements` entity query: the
// requirement row itself is the inbox item, its deployment is the `deployments` relation, its
// decision history is the `deploymentApprovalDecisions` relation, and `status`, `requester`,
// `satisfiedParticipants`, `qualifyingApprovalCount`, `eligible`, `decisionAvailable` and
// `approvalSnapshot` are computed fields on that row.
const decisionFields = "deploymentApprovalDecisions(orderBy: { decidedAt: ASC, id: ASC }, pagination: { page: { limit: 20, page: 0 } }) { nodes { id actorPrincipalId decision comment rejectionReason eligibilityCheckedAt decidedAt } pageInfo { hasNextPage } }";
const requirementFields = `id deploymentId projectId revision status expiresAt requester { id subject } requiredApprovers qualifyingApprovalCount satisfiedParticipants { id subject } approvalSnapshot { policyDigest policyRevision environmentClass risk riskLevel rule { requiredEvidence requiredDistinctApproverCount } target { agentVersionId agentVersionDigest environmentDefinitionVersionId environmentDefinitionDigest targetDigest deploymentPlanDigest artifactDigest } evidence { kind digest bindingDigest expiresAt state } expiresAt } ${decisionFields}`;
const approvalItemFields = `decisionAvailable eligible ${requirementFields} deployments { ${deploymentFields} }`;
const newestFirst = "orderBy: { requestedAt: DESC, id: DESC }";

// The generated row carries the wrapper fields the deleted `ApprovalInboxItem` type added, so the
// scenarios below keep reading one shape.
function approvalItem(node) {
  if (!node) return null;
  return { decisionAvailable: node.decisionAvailable, eligible: node.eligible, requirement: node, deployment: node.deployments };
}

// One page of the inbox, scoped the way `approvalInbox(organizationId:/projectId:)` was. `scope`
// is a generated filter fragment, written with the identifier inline.
function inboxQuery(name, fields, scope = "") {
  const filters = scope ? `filters: { ${scope} }, ` : "";
  return `query ${name}($limit: Int!, $page: Int!) { deploymentApprovalRequirements(${filters}${newestFirst}, pagination: { page: { limit: $limit, page: $page } }) { nodes { ${fields} } pageInfo { hasNextPage } } }`;
}

async function graphql(service, principal, query, variables) {
  const response = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  lastGraphqlRequestId = response.headers.get("X-Request-Id") ?? "";
  const body = await response.json();
  assert.equal(body.errors, undefined, JSON.stringify(body.errors));
  return body.data;
}

async function publishFixture(service) {
  const created = await graphql(service, requester,
    "mutation Create($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId revision } problems { code } } }",
    { input: { projectId: project, displayName: "M14 approval fixture " + run } });
  assert.deepEqual(created.createAgentDraft.problems, []);
  const agentId = created.createAgentDraft.agentDraft.agentId;
  const document = {
    general: { displayName: "M14 approval fixture", description: "Local approval integration fixture." },
    instructions: { source: "# local fixture", language: "markdown" }, harness: { source: "def run(value): return value", language: "python" },
    model: { reference: "model:local-safe-chat@v2" }, tools: { source: "{}", language: "json" }, skills: { source: "export const skills = [];", language: "javascript" },
    capabilities: { source: "export type Capability = string;", language: "typescript" }, subagents: { enabled: false }, memory: { strategy: "project" },
    guardrails: { source: "# local guardrail checks", language: "shell" }, identity: { persona: "" }, observability: { source: "<observability/>", language: "xml" },
    limits: { maxTokens: 256 }, evaluations: { required: false }, dependencies: ["model:local-safe-chat@v2"]
  };
  const saved = await graphql(service, requester,
    "mutation Save($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: created.createAgentDraft.agentDraft.revision, document } });
  assert.deepEqual(saved.updateAgentDraft.problems, []);
  const validated = await graphql(service, requester,
    "mutation Validate($input: ValidateAgentDraftInput!) { validateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: saved.updateAgentDraft.agentDraft.revision } });
  assert.deepEqual(validated.validateAgentDraft.problems, []);
  const published = await graphql(service, requester,
    "mutation Publish($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentVersion { id } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: validated.validateAgentDraft.agentDraft.revision, warningsAcknowledged: true } });
  assert.deepEqual(published.publishAgentDraft.problems, []);
  return { agentId, versionId: published.publishAgentDraft.agentVersion.id, document };
}

async function publishHighRiskVersion(service, agentId, document, marker = "") {
  const draft = await graphql(service, requester,
    "query Draft($agentId: String!) { agentDrafts(filters: { agentId: { eq: $agentId } }) { nodes { revision } } }",
    { agentId });
  const changed = { ...document, guardrails: { ...document.guardrails, source: document.guardrails.source + "\n# require an additional review" + marker } };
  const saved = await graphql(service, requester,
    "mutation Save($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: draft.agentDrafts.nodes[0].revision, document: changed } });
  assert.deepEqual(saved.updateAgentDraft.problems, []);
  const validated = await graphql(service, requester,
    "mutation Validate($input: ValidateAgentDraftInput!) { validateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: saved.updateAgentDraft.agentDraft.revision } });
  assert.deepEqual(validated.validateAgentDraft.problems, []);
  const published = await graphql(service, requester,
    "mutation Publish($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentVersion { id } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: validated.validateAgentDraft.agentDraft.revision, warningsAcknowledged: true } });
  assert.deepEqual(published.publishAgentDraft.problems, []);
  return published.publishAgentDraft.agentVersion.id;
}

async function environment(service, versionId, logicalClass) {
  const result = await graphql(service, requester,
    "query Environments($version: String!) { agentVersions(filters: { id: { eq: $version } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { catalogReleases { environmentDefinitionVersions(orderBy: { stableDefinitionId: ASC, version: ASC, id: ASC }, pagination: { page: { limit: 50, page: 0 } }) { nodes { id logicalEnvironmentClass stableDefinitionId version catalogReleaseDigest } } } } } }",
    { version: versionId });
  const value = result.agentVersions.nodes[0].catalogReleases.environmentDefinitionVersions.nodes.find((entry) => entry.logicalEnvironmentClass === logicalClass);
  assert(value, "The local catalog must expose " + logicalClass + ".");
  return value.id;
}

async function request(service, versionId, environmentId, suffix, evaluationPassed = true, principal = requester) {
  let result;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    result = await graphql(service, principal,
      `mutation Deploy($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { ${deploymentFields} } problems { code } } }`,
      { input: { agentVersionId: versionId, environmentDefinitionVersionId: environmentId, strategy: "ROLLING", idempotencyKey: `m14-${run}-${suffix}` } });
    if (result.deployAgentVersion.problems[0]?.code !== "REVISION_CONFLICT") break;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  assert.deepEqual(result.deployAgentVersion.problems, []);
  if (evaluationPassed && result.deployAgentVersion.deployment.deploymentPolicySnapshots.requiredEvidence.includes("EVALUATION_PASSED")) {
    await recordEvaluationPassed(result.deployAgentVersion.deployment.id);
  }
  return result.deployAgentVersion.deployment;
}

// M16 owns this fact in product code. The isolated M14 fixture appends an exact-bound test fact
// directly so it can exercise M14 review and execution after an evaluation exists.
async function recordEvaluationPassed(deploymentId, targetDigest = null) {
  const policy = await client.query("SELECT binding_digest, agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest, evaluation_requirement_expires_at FROM deployment_policy_snapshots WHERE deployment_id = $1", [deploymentId]);
  assert.equal(policy.rowCount, 1);
  const fact = policy.rows[0];
  await client.query(`INSERT INTO deployment_evidence_snapshots (id, deployment_id, evidence_kind, evidence_digest, expires_at, agent_version_id,
      environment_definition_version_id, target_digest, plan_digest, package_digest, binding_digest)
    VALUES ($1, $2, 'EVALUATION_PASSED', encode(sha256(convert_to($3 || '|EVALUATION_PASSED', 'UTF8')), 'hex'), $4, $5, $6, $7, $8, $9, $3)`,
    [randomUUID(), deploymentId, fact.binding_digest, fact.evaluation_requirement_expires_at, fact.agent_version_id,
      fact.environment_definition_version_id, targetDigest ?? fact.target_digest, fact.plan_digest, fact.package_digest]);
  // Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, so no trigger fires on this
  // INSERT. The evaluation write path calls touch_projection() and automatic_approval_handoff()
  // itself, but this fixture bypasses that path with a raw INSERT, and both are private to the
  // persistence crate and unreachable from here. So this replicates only the one branch this
  // fixture's own deployments (always zero-approver, always still PENDING/non-terminal/unexpired at
  // this point) can reach: the still-PENDING requirement becomes SATISFIED now that every required
  // evidence kind -- including the EVALUATION_PASSED row just inserted above -- is present, and a
  // deployment_approval_handoff_releases row queues the resulting execution for a compatible worker's
  // own maintenance pass, exactly as automatic_approval_handoff()'s zero-approver branch does.
  await client.query(`UPDATE deployment_approval_requirements
    SET status = 'SATISFIED', revision = revision + 1, satisfied_at = CURRENT_TIMESTAMP, satisfied_participants = '[]'::jsonb
    WHERE deployment_id = $1 AND status = 'PENDING' AND required_approvers = 0 AND expires_at > CURRENT_TIMESTAMP`, [deploymentId]);
  await client.query(`INSERT INTO deployment_approval_handoff_releases (deployment_id)
    SELECT $1 FROM deployment_approval_requirements WHERE deployment_id = $1 AND status = 'SATISFIED'
    ON CONFLICT (deployment_id) DO NOTHING`, [deploymentId]);
  await client.query("UPDATE deployments SET projection_revision = projection_revision + 1 WHERE id = $1", [deploymentId]);
}

// This writes the retained M13 column lists against a migrated database. V017 must create the
// frozen requirement and preserve a delayed handoff without relying on an M14 API writer.
async function legacyDeploymentFrom(sourceDeploymentId, suffix, deploymentId = randomUUID()) {
  const planId = randomUUID();
  await client.query(`INSERT INTO deployments (id, organization_id, project_id, agent_id, agent_version_id, catalog_release_id,
      catalog_release_digest, environment, target_digest, strategy, lifecycle_status, revision, idempotency_key, requested_by,
      requested_at, updated_at, environment_definition_version_id, request_fingerprint, projection_revision)
    SELECT $1, organization_id, project_id, agent_id, agent_version_id, catalog_release_id, catalog_release_digest, environment,
      target_digest, strategy, 'REQUESTED', 1, $2, requested_by, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP,
      environment_definition_version_id, request_fingerprint, projection_revision
    FROM deployments WHERE id = $3`, [deploymentId, `m13-${suffix}-${run}`, sourceDeploymentId]);
  await client.query(`INSERT INTO deployment_plan_versions (id, deployment_id, version_number, agent_version_id, catalog_release_id,
      environment, environment_definition_version_id, agent_content_digest, catalog_release_digest, target_digest, compiler_version,
      canonical_plan, plan_digest, package_digest, package_reference, created_by)
    SELECT $1, $2, version_number, agent_version_id, catalog_release_id, environment, environment_definition_version_id,
      agent_content_digest, catalog_release_digest, target_digest, compiler_version, canonical_plan, plan_digest, package_digest,
      package_reference, created_by FROM deployment_plan_versions WHERE deployment_id = $3 AND version_number = 1`,
    [planId, deploymentId, sourceDeploymentId]);
  await client.query(`INSERT INTO deployment_plan_review_facts (plan_id, active_agent_version_number, change_summary,
      requested_dependency_versions, added_dependency_versions, removed_dependency_versions)
    SELECT $1, active_agent_version_number, change_summary, requested_dependency_versions, added_dependency_versions,
      removed_dependency_versions
    FROM deployment_plan_review_facts review JOIN deployment_plan_versions plan ON plan.id = review.plan_id
    WHERE plan.deployment_id = $2 AND plan.version_number = 1
      AND NOT EXISTS (SELECT 1 FROM deployment_plan_review_facts WHERE plan_id = $1)`, [planId, sourceDeploymentId]);
  await client.query(`INSERT INTO deployment_policy_snapshots (deployment_id, policy_id, policy_revision, policy_digest, policy_matrix,
      logical_environment_class, risk, required_evidence, required_approvers, agent_version_id, environment_definition_version_id,
      target_digest, plan_digest, package_digest, binding_digest, evaluation_requirement_expires_at)
    SELECT $1, policy_id, policy_revision, policy_digest, policy_matrix, logical_environment_class, risk, required_evidence,
      required_approvers, agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest,
      binding_digest, evaluation_requirement_expires_at FROM deployment_policy_snapshots WHERE deployment_id = $2`,
    [deploymentId, sourceDeploymentId]);
  // Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, so nothing creates this row
  // automatically on the INSERT above, and ensure_requirement() is private to the persistence crate
  // and unreachable from this script. So this fixture replicates its one observable effect for a
  // fresh, non-archived, REQUESTED-lifecycle row instead: clone the source deployment's own
  // already-resolved requirement shape (its required_approvers and whatever terminal/pending outcome
  // a real deploy() already gave it), since a real ensure_requirement() call for this brand-new row
  // finds the identical deployment/policy facts (copied from the same source immediately above) and
  // reaches the same result.
  await client.query(`INSERT INTO deployment_approval_requirements
      (id, deployment_id, revision, organization_id, project_id, requested_at, required_approvers, status,
       expires_at, satisfied_at, rejected_at, invalidated_at, invalidation_code, satisfied_participants)
    SELECT gen_random_uuid(), $1, 1, deployment.organization_id, deployment.project_id, deployment.requested_at,
      source.required_approvers, source.status, deployment.requested_at + INTERVAL '24 hours',
      source.satisfied_at, source.rejected_at, source.invalidated_at, source.invalidation_code, source.satisfied_participants
    FROM deployments deployment JOIN deployment_approval_requirements source ON source.deployment_id = $2
    WHERE deployment.id = $1
    ON CONFLICT (deployment_id) DO NOTHING`, [deploymentId, sourceDeploymentId]);
  await client.query(`INSERT INTO deployment_evidence_snapshots (id, deployment_id, evidence_kind, evidence_digest, expires_at,
      agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest, binding_digest)
    SELECT gen_random_uuid(), $1, evidence_kind, evidence_digest, expires_at, agent_version_id, environment_definition_version_id,
      target_digest, plan_digest, package_digest, binding_digest FROM deployment_evidence_snapshots WHERE deployment_id = $2`,
    [deploymentId, sourceDeploymentId]);
  // No trigger fires on this INSERT either when a copied row carries evidence_kind =
  // 'EVALUATION_PASSED', and automatic_approval_handoff() is private to the persistence crate and
  // unreachable from this script. Its effect beyond the requirement shape already cloned above is:
  // for a SATISFIED requirement on a REQUESTED/APPROVED deployment, queue a
  // deployment_approval_handoff_releases row so a compatible worker's own maintenance pass
  // (release_compatible_approval_handoffs()) picks it up and enqueues EXECUTE_DEPLOYMENT -- the same
  // durable queue a real automatic_approval_handoff() call leaves behind when it cannot enqueue
  // synchronously itself. Every caller of this fixture already runs against a started local worker, so
  // that maintenance pass reaches this row the same way it would a real one.
  await client.query(`INSERT INTO deployment_approval_handoff_releases (deployment_id)
    SELECT $1 FROM deployment_approval_requirements WHERE deployment_id = $1 AND status = 'SATISFIED'
    ON CONFLICT (deployment_id) DO NOTHING`, [deploymentId]);
  await client.query("INSERT INTO deployment_runtime_health (deployment_id, status, summary, generation) VALUES ($1, 'NOT_OBSERVED', 'No local runtime observation is available yet.', 1)", [deploymentId]);
  return deploymentId;
}

// This retained M13 shape deliberately pauses before the policy snapshot. It covers the
// rolling-upgrade interval where an archive/restore boundary can occur before M14 observes the
// delayed policy row and creates its frozen requirement.
async function legacyDeploymentBeforePolicy(sourceDeploymentId, projectId, suffix, deploymentId = randomUUID()) {
  const planId = randomUUID();
  await client.query(`INSERT INTO deployments (id, organization_id, project_id, agent_id, agent_version_id, catalog_release_id,
      catalog_release_digest, environment, target_digest, strategy, lifecycle_status, revision, idempotency_key, requested_by,
      requested_at, updated_at, environment_definition_version_id, request_fingerprint, projection_revision)
    SELECT $1, organization_id, $2, agent_id, agent_version_id, catalog_release_id, catalog_release_digest, environment,
      target_digest, strategy, 'REQUESTED', 1, $3, requested_by, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP,
      environment_definition_version_id, request_fingerprint, projection_revision
    FROM deployments WHERE id = $4`, [deploymentId, projectId, `m13-delayed-policy-${suffix}-${run}`, sourceDeploymentId]);
  await client.query(`INSERT INTO deployment_plan_versions (id, deployment_id, version_number, agent_version_id, catalog_release_id,
      environment, environment_definition_version_id, agent_content_digest, catalog_release_digest, target_digest, compiler_version,
      canonical_plan, plan_digest, package_digest, package_reference, created_by)
    SELECT $1, $2, version_number, agent_version_id, catalog_release_id, environment, environment_definition_version_id,
      agent_content_digest, catalog_release_digest, target_digest, compiler_version, canonical_plan, plan_digest, package_digest,
      package_reference, created_by FROM deployment_plan_versions WHERE deployment_id = $3 AND version_number = 1`,
    [planId, deploymentId, sourceDeploymentId]);
  await client.query(`INSERT INTO deployment_plan_review_facts (plan_id, active_agent_version_number, change_summary,
      requested_dependency_versions, added_dependency_versions, removed_dependency_versions)
    SELECT $1, active_agent_version_number, change_summary, requested_dependency_versions, added_dependency_versions,
      removed_dependency_versions
    FROM deployment_plan_review_facts review JOIN deployment_plan_versions plan ON plan.id = review.plan_id
    WHERE plan.deployment_id = $2 AND plan.version_number = 1
      AND NOT EXISTS (SELECT 1 FROM deployment_plan_review_facts WHERE plan_id = $1)`, [planId, sourceDeploymentId]);
  await client.query("INSERT INTO deployment_runtime_health (deployment_id, status, summary, generation) VALUES ($1, 'NOT_OBSERVED', 'No local runtime observation is available yet.', 1)", [deploymentId]);
  return deploymentId;
}

async function approval(service, principal, requirementId) {
  const result = await graphql(service, principal,
    `query Requirement($id: String!) { deploymentApprovalRequirements(filters: { id: { eq: $id } }, ${newestFirst}, pagination: { page: { limit: 1, page: 0 } }) { nodes { ${approvalItemFields} } } }`,
    { id: requirementId });
  return approvalItem(result.deploymentApprovalRequirements.nodes[0]);
}

async function requirementForDeployment(client, deploymentId) {
  const result = await client.query("SELECT id FROM deployment_approval_requirements WHERE deployment_id = $1", [deploymentId]);
  assert.equal(result.rowCount, 1);
  return result.rows[0].id;
}

async function decide(service, principal, requirementId, revision, decision, comment = null, rejectionReason = null, idempotencyKey = randomUUID()) {
  return graphql(service, principal,
    `mutation Decide($input: DecideDeploymentApprovalInput!) { decideDeploymentApproval(input: $input) { decision { id decision comment rejectionReason } requirement { ${requirementFields} } deployment { ${deploymentFields} } problems { __typename code message resourceId expectedRevision actualRevision } } }`,
    { input: { approvalRequirementId: requirementId, expectedRevision: revision, decision, comment, rejectionReason, idempotencyKey } });
}

async function waitForLifecycle(service, deploymentId, lifecycle) {
  const started = Date.now();
  while (Date.now() - started < 25_000) {
    const result = await graphql(service, requester,
      "query Deployment($id: String!) { deployments(filters: { id: { eq: $id } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { id lifecycleStatus } } }", { id: deploymentId });
    if (result.deployments.nodes[0]?.lifecycleStatus === lifecycle) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`Deployment ${deploymentId} did not reach ${lifecycle}.`);
}

async function waitForLifecycleViaDatabase(deploymentId, lifecycle) {
  const started = Date.now();
  while (Date.now() - started < 25_000) {
    const result = await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [deploymentId]);
    if (result.rows[0]?.lifecycle_status === lifecycle) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`Deployment ${deploymentId} did not reach ${lifecycle}.`);
}

async function waitForRequirementStatus(requirementId, status) {
  const started = Date.now();
  while (Date.now() - started < 25_000) {
    const result = await client.query("SELECT status FROM deployment_approval_requirements WHERE id = $1", [requirementId]);
    if (result.rows[0]?.status === status) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`Approval requirement ${requirementId} did not reach ${status}.`);
}

async function waitForApprovalMaintenance(service, timeoutMillis = 25_000) {
  const started = Date.now();
  let last;
  while (Date.now() - started < timeoutMillis) {
    const response = await fetch(`http://127.0.0.1:${service.port}/health`);
    const body = await response.json();
    last = { status: response.status, body };
    if (response.status === 200) {
      if (body.status === "ok" && body.approvalMaintenance === "ok") return;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`Approval maintenance did not complete its read-model upgrade: ${JSON.stringify(last)}.`);
}

// reconcile_project_archives() is private to the persistence crate and runs only from
// reconcile_approval_upgrade(), itself reached only from the API process's own 1-second maintenance
// tick (the process this whole file drives through `service`); no raw-SQL surface invokes it. Polling
// alone is correct: that tick runs for the lifetime of `service`, with or without a local deployment
// worker started.
async function waitForArchiveReconciliation(eventId) {
  const started = Date.now();
  while (Date.now() - started < 25_000) {
    const event = await client.query("SELECT processed_at IS NOT NULL AS processed FROM deployment_approval_project_archive_events WHERE id = $1", [eventId]);
    if (event.rows[0]?.processed) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`Archive event ${eventId} did not reconcile.`);
}

// A raw "UPDATE projects SET lifecycle_status = ..." records no archive event: only
// record_project_archive_event() does, and it is reached only through lifecycle(). So these two
// helpers route archive and restore through the archiveAdministrationScope and
// restoreAdministrationScope mutations, which is the only way the archive event that this domain's
// revision-boundary scenarios depend on gets recorded.
// platformAdministrator (PLATFORM_ADMIN, granted well before any caller of these two helpers runs)
// acts here rather than requester: these helpers archive/restore ad-hoc scenario-local projects this
// file creates via a raw INSERT INTO projects, which requester has no PROJECT_ADMIN/organization
// membership on, unlike the shared `project` its own archiveProject/restoreProject calls above target.
async function archiveProjectViaApi(service, projectId, reason = "Approval archive boundary") {
  const revision = Number((await client.query("SELECT revision FROM projects WHERE id = $1", [projectId])).rows[0].revision);
  const result = await graphql(service, platformAdministrator,
    "mutation Archive($input: LifecycleAdministrationInput!) { archiveAdministrationScope(input: $input) { project { revision } problems { code } } }",
    { input: { scope: "PROJECT", scopeId: projectId, expectedRevision: revision, reason } });
  assert.deepEqual(result.archiveAdministrationScope.problems, []);
}

async function restoreProjectViaApi(service, projectId) {
  const revision = Number((await client.query("SELECT revision FROM projects WHERE id = $1", [projectId])).rows[0].revision);
  const result = await graphql(service, platformAdministrator,
    "mutation Restore($input: LifecycleAdministrationInput!) { restoreAdministrationScope(input: $input) { project { revision } problems { code } } }",
    { input: { scope: "PROJECT", scopeId: projectId, expectedRevision: revision } });
  assert.deepEqual(result.restoreAdministrationScope.problems, []);
}

// ensure_requirement(), invalidate_archived_approval_requirement() and
// record_terminal_invalidation() are private to the persistence crate and unreachable from this
// script, so this replicates them in SQL for fixtures that construct a deployment row directly
// (bypassing insertApprovalRequirement()) and need the same dynamic archived-or-terminal-lifecycle
// determination a real call makes, rather than a clone of an already-resolved source's requirement
// shape (legacyDeploymentFrom()'s simpler replacement, sufficient only when the source's own outcome
// can be copied verbatim). Aurora DSQL rejects CREATE FUNCTION outright, so there is no
// archive-boundary function to call: the archived check below inlines the same revision-aware
// predicate deployment_archive_boundary() applies.
async function ensureRequirementViaSql(deploymentId) {
  const archived = (await client.query(`SELECT EXISTS (
      SELECT 1 FROM deployments deployment
        JOIN deployment_approval_project_archive_events event ON event.project_id = deployment.project_id
      WHERE deployment.id = $1
        AND ((deployment.project_lifecycle_revision IS NOT NULL AND event.archived_project_revision IS NOT NULL
                AND deployment.project_lifecycle_revision <= event.archived_project_revision)
            OR ((deployment.project_lifecycle_revision IS NULL OR event.archived_project_revision IS NULL)
                AND deployment.requested_at <= event.archived_at))
    ) AS archived`, [deploymentId])).rows[0].archived;
  const inserted = await client.query(`INSERT INTO deployment_approval_requirements
      (id, deployment_id, revision, organization_id, project_id, requested_at, required_approvers, status,
       expires_at, invalidated_at, invalidation_code)
    SELECT $1, deployment.id, 1, deployment.organization_id, deployment.project_id, deployment.requested_at,
      policy.required_approvers,
      CASE WHEN $2 OR deployment.lifecycle_status IN ('IN_PROGRESS', 'ACTIVE', 'FAILED', 'CANCELED', 'ROLLED_BACK')
        THEN 'INVALIDATED' ELSE 'PENDING' END,
      deployment.requested_at + INTERVAL '24 hours',
      CASE WHEN $2 OR deployment.lifecycle_status IN ('IN_PROGRESS', 'ACTIVE', 'FAILED', 'CANCELED', 'ROLLED_BACK')
        THEN CURRENT_TIMESTAMP ELSE NULL END,
      CASE WHEN $2 THEN 'PROJECT_ARCHIVED'
        WHEN deployment.lifecycle_status IN ('IN_PROGRESS', 'ACTIVE', 'FAILED', 'CANCELED', 'ROLLED_BACK') THEN 'TERMINAL_LIFECYCLE'
        ELSE NULL END
    FROM deployments deployment JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id
    WHERE deployment.id = $3
    ON CONFLICT (deployment_id) DO NOTHING
    RETURNING id, status`, [randomUUID(), archived, deploymentId]);
  if (inserted.rowCount === 0) return;
  const requirementId = inserted.rows[0].id;
  if (inserted.rows[0].status !== "INVALIDATED") return;
  let archiveActor = null;
  if (archived) {
    const actorRow = await client.query(`SELECT event.actor_principal_id
      FROM deployment_approval_project_archive_events event JOIN deployments deployment ON deployment.project_id = event.project_id
      WHERE deployment.id = $1
        AND ((deployment.project_lifecycle_revision IS NOT NULL AND event.archived_project_revision IS NOT NULL
              AND deployment.project_lifecycle_revision <= event.archived_project_revision)
          OR ((deployment.project_lifecycle_revision IS NULL OR event.archived_project_revision IS NULL)
              AND deployment.requested_at <= event.archived_at))
      ORDER BY event.archived_project_revision DESC NULLS LAST, event.archived_at DESC, event.id DESC LIMIT 1`, [deploymentId]);
    archiveActor = actorRow.rows[0]?.actor_principal_id ?? null;
  }
  const sequence = (await client.query(`INSERT INTO deployment_timeline_counters (deployment_id, attempt_number, next_sequence) VALUES ($1, 0, 2)
    ON CONFLICT (deployment_id, attempt_number) DO UPDATE SET next_sequence = deployment_timeline_counters.next_sequence + 1
    RETURNING next_sequence - 1 AS sequence`, [deploymentId])).rows[0].sequence;
  await client.query(`INSERT INTO deployment_audit_events (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence)
    VALUES ($1, $2, $3, 'APPROVAL_INVALIDATED', jsonb_build_object('requirementId', $4::text, 'code', $5::text), NULL, 0, $6)`,
    [randomUUID(), deploymentId, archiveActor, requirementId, archived ? "PROJECT_ARCHIVED" : "TERMINAL_LIFECYCLE", sequence]);
  if (archived) {
    await client.query(`UPDATE deployment_runtime_health SET status = 'CANCELED', summary = 'Project archive terminalized this delayed approval cycle.',
      observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1`, [deploymentId]);
    await client.query(`UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1, projection_revision = projection_revision + 1,
      updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED')`, [deploymentId]);
    await client.query("DELETE FROM deployment_approval_handoff_releases WHERE deployment_id = $1", [deploymentId]);
  }
  await client.query("UPDATE deployments SET projection_revision = projection_revision + 1 WHERE id = $1", [deploymentId]);
}

// automatic_approval_handoff()'s zero-approver-satisfy branch, replicated in SQL for a
// fixture-constructed deployment (see ensureRequirementViaSql() above) whose requirement is still
// PENDING with zero required approvers and evidence already ready by construction.
async function satisfyZeroApproverViaSql(deploymentId) {
  await client.query(`UPDATE deployment_approval_requirements
    SET status = 'SATISFIED', revision = revision + 1, satisfied_at = CURRENT_TIMESTAMP, satisfied_participants = '[]'::jsonb
    WHERE deployment_id = $1 AND status = 'PENDING' AND required_approvers = 0 AND expires_at > CURRENT_TIMESTAMP`, [deploymentId]);
  await client.query(`INSERT INTO deployment_approval_handoff_releases (deployment_id)
    SELECT $1 FROM deployment_approval_requirements WHERE deployment_id = $1 AND status = 'SATISFIED'
    ON CONFLICT (deployment_id) DO NOTHING`, [deploymentId]);
  await client.query("UPDATE deployments SET projection_revision = projection_revision + 1 WHERE id = $1", [deploymentId]);
}

async function approveAndActivate(service, client, deployment) {
  const id = await requirementForDeployment(client, deployment.id);
  const current = await approval(service, approverOne, id);
  assert.equal(current.requirement.status, "PENDING");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_outbox_events WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [deployment.id])).rows[0].count, 0);
  const result = await decide(service, approverOne, id, current.requirement.revision, "APPROVE", "REVIEWED_CHANGE_SCOPE");
  assert.deepEqual(result.decideDeploymentApproval.problems, []);
  assert.equal(result.decideDeploymentApproval.requirement.status, "SATISFIED");
  await waitForLifecycle(service, deployment.id, "ACTIVE");
  return id;
}

async function invalidateEvidence(client, deploymentId, mutation) {
  await mutation();
  const requirement = await requirementForDeployment(client, deploymentId);
  return requirement;
}

const database = await createIsolatedDatabase("hive_m14_access");
let service;
let client;
let worker;
try {
  service = await startIsolatedLocalService(database.name);
  endpoint = `http://127.0.0.1:${service.port}/graphql`;
  client = await postgresClient(database.name);
  worker = await startLocalDeploymentWorker(database.name, {
    HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "200",
    HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "100"
  });
  const principals = [[requester, "m14-requester"], [approverOne, "m14-approver-one"], [approverTwo, "m14-approver-two"], [viewer, "m14-viewer"], [outsider, "m14-outsider"], [developerOnly, "m14-developer-only"], [platformAdministrator, "m14-platform-administrator"], [workerSafetyRequester, "m14-worker-safety-requester"]];
  for (const [id, subject] of principals) await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, $2, $2, $2 || '@local.invalid') ON CONFLICT (id) DO NOTHING", [id, subject]);
  await client.query("INSERT INTO platform_role_assignments (principal_id, role_code) VALUES ($1, 'PLATFORM_ADMIN') ON CONFLICT DO NOTHING", [platformAdministrator]);
  await client.query("SELECT set_config('hive.m14_approval_role_assignment_actor', $1, FALSE)", [platformAdministrator]);
  const memberships = new Map([
    [requester, ["AGENT_DEVELOPER", "PROJECT_ADMIN"]],
    [approverOne, ["DEPLOYMENT_APPROVER"]],
    [approverTwo, ["DEPLOYMENT_APPROVER"]],
    [viewer, ["PROJECT_ADMIN"]],
    [developerOnly, ["AGENT_DEVELOPER"]],
    [workerSafetyRequester, ["AGENT_DEVELOPER"]]
  ]);
  for (const [principal, roleCodes] of memberships) {
    const membershipId = randomUUID();
    await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL)", [membershipId, organization, principal]);
    await client.query("INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_MEMBER')", [membershipId]);
    const projectMembershipId = randomUUID();
    await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1)", [projectMembershipId, project, principal]);
    for (const roleCode of roleCodes) await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, $2)", [projectMembershipId, roleCode]);
  }
  const requesterRoleMembership = (await client.query(
    "SELECT id, revision FROM project_memberships WHERE principal_id = $1 AND project_id = $2", [requester, project])).rows[0];
  // No raw-SQL "direct writer gets rejected" assertion here: Aurora DSQL rejects CREATE TRIGGER and
  // CREATE FUNCTION outright, so there is no database-level role-assignment guard.
  // approval_role_transition_allowed() is the sole enforcement point, and it guards only writes that
  // go through the persistence layer, not a raw SQL INSERT. The requesterSelfGrant assertion below
  // exercises that same invariant through the real GraphQL path.
  await client.query("SELECT set_config('hive.m14_approval_role_assignment_actor', $1, FALSE)", [platformAdministrator]);
  const requesterAdministration = await graphql(service, requester,
    "query ProjectRoles($projectId: String!) { projects(filters: { id: { eq: $projectId } }) { nodes { assignableRoles } } }", { projectId: project });
  assert.equal(requesterAdministration.projects.nodes[0].assignableRoles.includes("DEPLOYMENT_APPROVER"), false);
  const requesterSelfGrant = await graphql(service, requester,
    "mutation SelfGrant($input: ReplaceAdministrationMembershipInput!) { replaceAdministrationMembershipRoles(input: $input) { problems { code } } }",
    { input: { scope: "PROJECT", scopeId: project, membershipId: requesterRoleMembership.id,
      roleCodes: ["AGENT_DEVELOPER", "DEPLOYMENT_APPROVER", "PROJECT_ADMIN"], expectedRevision: Number(requesterRoleMembership.revision) } });
  assert.equal(requesterSelfGrant.replaceAdministrationMembershipRoles.problems[0].code, "FORBIDDEN");
  const platformGrant = await graphql(service, platformAdministrator,
    "mutation PlatformGrant($input: ReplaceAdministrationMembershipInput!) { replaceAdministrationMembershipRoles(input: $input) { project { projectMemberships { nodes { id roleCodes revision } } } problems { code } } }",
    { input: { scope: "PROJECT", scopeId: project, membershipId: requesterRoleMembership.id,
      roleCodes: ["AGENT_DEVELOPER", "DEPLOYMENT_APPROVER", "PROJECT_ADMIN"], expectedRevision: Number(requesterRoleMembership.revision) } });
  assert.deepEqual(platformGrant.replaceAdministrationMembershipRoles.problems, []);
  const grantedMembership = platformGrant.replaceAdministrationMembershipRoles.project.projectMemberships.nodes
    .find((membership) => membership.id === requesterRoleMembership.id);
  assert(grantedMembership.roleCodes.includes("DEPLOYMENT_APPROVER"));
  const requesterEndApprover = await graphql(service, requester,
    "mutation EndApprover($input: EndAdministrationMembershipInput!) { endAdministrationMembership(input: $input) { problems { code } } }",
    { input: { scope: "PROJECT", scopeId: project, membershipId: requesterRoleMembership.id,
      expectedRevision: grantedMembership.revision, reason: "Requester cannot remove an approval authority." } });
  assert.equal(requesterEndApprover.endAdministrationMembership.problems[0].code, "FORBIDDEN");
  const platformRemove = await graphql(service, platformAdministrator,
    "mutation PlatformRemove($input: ReplaceAdministrationMembershipInput!) { replaceAdministrationMembershipRoles(input: $input) { problems { code } } }",
    { input: { scope: "PROJECT", scopeId: project, membershipId: requesterRoleMembership.id,
      roleCodes: ["AGENT_DEVELOPER", "PROJECT_ADMIN"], expectedRevision: grantedMembership.revision } });
  assert.deepEqual(platformRemove.replaceAdministrationMembershipRoles.problems, []);
  await waitForApprovalMaintenance(service);

  const base = await publishFixture(service);
  const development = await environment(service, base.versionId, "DEVELOPMENT");
  const staging = await environment(service, base.versionId, "STAGING");
  const production = await environment(service, base.versionId, "PRODUCTION");

  const developmentMedium = await request(service, base.versionId, development, "development-medium");
  assert.deepEqual([developmentMedium.deploymentPolicySnapshots.risk, developmentMedium.deploymentPolicySnapshots.requiredEvidence, developmentMedium.deploymentPolicySnapshots.requiredApprovers], ["MEDIUM", ["CHANGE_SUMMARY_READY", "PLAN_VALIDATED"], 0]);
  const developmentMediumRequirement = await requirementForDeployment(client, developmentMedium.id);
  assert.equal((await approval(service, requester, developmentMediumRequirement)).requirement.status, "SATISFIED");
  await waitForLifecycle(service, developmentMedium.id, "ACTIVE");
  // No raw-SQL "invalidation bumps the projection" assertion here: Aurora DSQL rejects CREATE TRIGGER
  // and CREATE FUNCTION outright, so no trigger reacts to a deployment_evidence_invalidations write,
  // and nothing in this codebase writes that table (no GraphQL mutation, no persistence-layer
  // INSERT). Only a hypothetical direct writer could reach this state, and the effect would be a
  // projection touch that automatic_approval_handoff() no-ops on for an ACTIVE (terminal) deployment.
  const developmentLow = await request(service, base.versionId, development, "development-low");
  assert.deepEqual([developmentLow.deploymentPolicySnapshots.risk, developmentLow.deploymentPolicySnapshots.requiredEvidence, developmentLow.deploymentPolicySnapshots.requiredApprovers], ["LOW", ["PLAN_VALIDATED"], 0]);
  await waitForLifecycle(service, developmentLow.id, "ACTIVE");

  const stagingMedium = await request(service, base.versionId, staging, "staging-medium");
  assert.deepEqual([stagingMedium.deploymentPolicySnapshots.risk, stagingMedium.deploymentPolicySnapshots.requiredEvidence, stagingMedium.deploymentPolicySnapshots.requiredApprovers], ["MEDIUM", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 1]);
  await approveAndActivate(service, client, stagingMedium);
  const stagingLow = await request(service, base.versionId, staging, "staging-low");
  assert.deepEqual([stagingLow.deploymentPolicySnapshots.risk, stagingLow.deploymentPolicySnapshots.requiredEvidence, stagingLow.deploymentPolicySnapshots.requiredApprovers], ["LOW", ["CHANGE_SUMMARY_READY", "PLAN_VALIDATED"], 0]);
  await waitForLifecycle(service, stagingLow.id, "ACTIVE");

  const productionMedium = await request(service, base.versionId, production, "production-medium");
  assert.deepEqual([productionMedium.deploymentPolicySnapshots.risk, productionMedium.deploymentPolicySnapshots.requiredEvidence, productionMedium.deploymentPolicySnapshots.requiredApprovers], ["MEDIUM", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 1]);
  await approveAndActivate(service, client, productionMedium);
  const productionLow = await request(service, base.versionId, production, "production-low");
  assert.deepEqual([productionLow.deploymentPolicySnapshots.risk, productionLow.deploymentPolicySnapshots.requiredEvidence, productionLow.deploymentPolicySnapshots.requiredApprovers], ["LOW", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 1]);
  await approveAndActivate(service, client, productionLow);

  const highVersionId = await publishHighRiskVersion(service, base.agentId, base.document);
  const developmentHigh = await request(service, highVersionId, development, "development-high");
  assert.deepEqual([developmentHigh.deploymentPolicySnapshots.risk, developmentHigh.deploymentPolicySnapshots.requiredEvidence, developmentHigh.deploymentPolicySnapshots.requiredApprovers], ["HIGH", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 1]);
  await approveAndActivate(service, client, developmentHigh);
  const stagingHigh = await request(service, highVersionId, staging, "staging-high");
  assert.deepEqual([stagingHigh.deploymentPolicySnapshots.risk, stagingHigh.deploymentPolicySnapshots.requiredEvidence, stagingHigh.deploymentPolicySnapshots.requiredApprovers], ["HIGH", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 1]);
  await approveAndActivate(service, client, stagingHigh);
  const archivedPending = await request(service, highVersionId, production, "project-archive-terminalizes-pending");
  const archivedPendingRequirement = await requirementForDeployment(client, archivedPending.id);
  const archivedPendingRevision = (await approval(service, approverOne, archivedPendingRequirement)).requirement.revision;
  const projectRevision = Number((await client.query("SELECT revision FROM projects WHERE id = $1", [project])).rows[0].revision);
  const archiveProject = await graphql(service, requester,
    "mutation Archive($input: LifecycleAdministrationInput!) { archiveAdministrationScope(input: $input) { project { lifecycleStatus revision } problems { code } } }",
    { input: { scope: "PROJECT", scopeId: project, expectedRevision: projectRevision, reason: "Approval archive boundary" } });
  assert.deepEqual(archiveProject.archiveAdministrationScope.problems, []);
  assert.equal(archiveProject.archiveAdministrationScope.project.lifecycleStatus, "ARCHIVED");
  await waitForRequirementStatus(archivedPendingRequirement, "INVALIDATED");
  const archived = await client.query("SELECT status, revision, invalidation_code FROM deployment_approval_requirements WHERE id = $1", [archivedPendingRequirement]);
  assert.deepEqual(archived.rows[0], { status: "INVALIDATED", revision: String(archivedPendingRevision + 1), invalidation_code: "PROJECT_ARCHIVED" });
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [archivedPending.id])).rows[0].lifecycle_status, "CANCELED");
  const archiveAudit = await client.query("SELECT facts->>'code' AS code, actor_principal_id FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_INVALIDATED'", [archivedPending.id]);
  assert.deepEqual(archiveAudit.rows[0], { code: "PROJECT_ARCHIVED", actor_principal_id: requester });
  const archivedProjectDecision = await decide(service, approverTwo, archivedPendingRequirement, Number(archived.rows[0].revision), "APPROVE");
  assert.equal(archivedProjectDecision.decideDeploymentApproval.problems[0].code, "PROJECT_ARCHIVED");
  const archivedCrossTenantDecision = await decide(service, outsider, archivedPendingRequirement, Number(archived.rows[0].revision), "APPROVE");
  assert.equal(archivedCrossTenantDecision.decideDeploymentApproval.problems[0].code, "NOT_FOUND");
  const restoreProject = await graphql(service, requester,
    "mutation Restore($input: LifecycleAdministrationInput!) { restoreAdministrationScope(input: $input) { project { lifecycleStatus } problems { code } } }",
    { input: { scope: "PROJECT", scopeId: project, expectedRevision: archiveProject.archiveAdministrationScope.project.revision } });
  assert.deepEqual(restoreProject.restoreAdministrationScope.problems, []);
  const restoredArchiveDecision = await decide(service, approverTwo, archivedPendingRequirement, Number(archived.rows[0].revision), "APPROVE");
  assert.equal(restoredArchiveDecision.decideDeploymentApproval.problems[0].code, "PROJECT_ARCHIVED");
  // No actorless-archive-event scenario here: it is structurally unreachable, not just untested.
  // record_project_archive_event() is the only writer of archive events, it is reached only through
  // lifecycle()'s validated PROJECT-archive path, and it always supplies the caller's real actor as a
  // non-null parameter -- no path creates an archive event without an actor already in hand.
  const productionHigh = await request(service, highVersionId, production, "production-high");
  assert.deepEqual([productionHigh.deploymentPolicySnapshots.risk, productionHigh.deploymentPolicySnapshots.requiredEvidence, productionHigh.deploymentPolicySnapshots.requiredApprovers], ["HIGH", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 2]);
  const productionHighRequirement = await requirementForDeployment(client, productionHigh.id);
  const productionHighFirst = await approval(service, approverOne, productionHighRequirement);
  assert.equal(productionHighFirst.decisionAvailable, true);
  assert.equal(productionHighFirst.requirement.requester.subject, "m14-requester");
  assert.equal((await approval(service, viewer, productionHighRequirement)).decisionAvailable, false);
  const firstDecisionKey = randomUUID();
  const firstDecision = await decide(service, approverOne, productionHighRequirement, productionHighFirst.requirement.revision, "APPROVE", "REVIEWED_CHANGE_SCOPE", null, firstDecisionKey);
  assert.deepEqual(firstDecision.decideDeploymentApproval.problems, []);
  assert.equal(firstDecision.decideDeploymentApproval.requirement.status, "PENDING");
  assert.equal(firstDecision.decideDeploymentApproval.requirement.qualifyingApprovalCount, 1);
  const recordedEligibility = await client.query(`SELECT facts->>'eligibilityCapability' AS capability,
      (facts->>'eligibilityGranted')::boolean AS granted, facts->>'requestId' AS request_id
    FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_RECORDED'
    ORDER BY occurred_at DESC, id DESC LIMIT 1`, [productionHigh.id]);
  assert.deepEqual(recordedEligibility.rows[0], { capability: "DEPLOYMENT_APPROVAL.DECIDE", granted: true, request_id: firstDecisionKey });
  const retryFactsBeforeConflict = await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1", [productionHigh.id]);
  const changedCommentRetry = await decide(service, approverOne, productionHighRequirement, productionHighFirst.requirement.revision,
    "APPROVE", "AUTHORIZATION_GRANTED", null, firstDecisionKey);
  assert.deepEqual(changedCommentRetry.decideDeploymentApproval.problems[0], { __typename: "Problem", code: "IDEMPOTENCY_CONFLICT",
    message: "This idempotency key belongs to a different approval decision.", resourceId: null, expectedRevision: null, actualRevision: null });
  const changedDecisionRetry = await decide(service, approverOne, productionHighRequirement, productionHighFirst.requirement.revision,
    "REJECT", null, "UNACCEPTABLE_CHANGE_SCOPE", firstDecisionKey);
  assert.equal(changedDecisionRetry.decideDeploymentApproval.problems[0].code, "IDEMPOTENCY_CONFLICT");
  const changedRevisionRetry = await decide(service, approverOne, productionHighRequirement, productionHighFirst.requirement.revision + 1,
    "APPROVE", "REVIEWED_CHANGE_SCOPE", null, firstDecisionKey);
  assert.equal(changedRevisionRetry.decideDeploymentApproval.problems[0].code, "IDEMPOTENCY_CONFLICT");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1", [productionHigh.id])).rows[0].count,
    retryFactsBeforeConflict.rows[0].count);
  const duplicate = await decide(service, approverOne, productionHighRequirement, productionHighFirst.requirement.revision, "APPROVE");
  assert.equal(duplicate.decideDeploymentApproval.problems[0].code, "DUPLICATE_APPROVER");
  const ineligible = await decide(service, viewer, productionHighRequirement, productionHighFirst.requirement.revision, "APPROVE");
  assert.equal(ineligible.decideDeploymentApproval.problems[0].code, "APPROVER_INELIGIBLE");
  const roleMembership = await client.query("SELECT membership_id FROM project_membership_roles role JOIN project_memberships membership ON membership.id = role.membership_id WHERE membership.principal_id = $1 AND membership.project_id = $2 AND role.role_code = 'DEPLOYMENT_APPROVER'", [approverTwo, project]);
  await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1 AND role_code = 'DEPLOYMENT_APPROVER'", [roleMembership.rows[0].membership_id]);
  // No scope-cache-refreshed-to-zero assertion here: Aurora DSQL rejects CREATE TRIGGER and CREATE
  // FUNCTION outright, so there are no scope-cache triggers, and refresh_membership_scope() and
  // refresh_role_scope() fire only for writes through the persistence layer, not this raw SQL
  // DELETE. The explicit DELETE below clears the
  // cache row before capabilityLoss needs it gone; decide()'s own NOT_FOUND check on a real
  // capability re-derivation, not the discovery cache, is what actually matters there.
  // A stale compact discovery row cannot supply the required P-10 inbox authority. The global
  // request remains non-disclosing before the keyset predicate evaluates the stale row.
  await client.query("INSERT INTO deployment_approval_principal_project_scopes (principal_id, project_id, valid_after) VALUES ($1, $2, CURRENT_TIMESTAMP) ON CONFLICT (principal_id, project_id) DO NOTHING", [approverTwo, project]);
  // A generated list a principal has no view capability for is an empty connection, not a refusal:
  // the stale discovery row supplies no authority, so the requirement is simply not there.
  const staleScopeInbox = await graphql(service, approverTwo,
    inboxQuery("StaleScope", "id"), { limit: 50, page: 0 });
  assert.deepEqual(staleScopeInbox.deploymentApprovalRequirements.nodes.map((node) => node.id), []);
  await client.query("DELETE FROM deployment_approval_principal_project_scopes WHERE principal_id = $1 AND project_id = $2", [approverTwo, project]);
  const capabilityLoss = await decide(service, approverTwo, productionHighRequirement, productionHighFirst.requirement.revision, "APPROVE");
  assert.equal(capabilityLoss.decideDeploymentApproval.problems[0].code, "NOT_FOUND");
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'DEPLOYMENT_APPROVER')", [roleMembership.rows[0].membership_id]);
  // No scope-cache-refreshed-to-one assertion here, for the same reason as the DELETE above -- this
  // raw SQL INSERT bypasses the repository too. secondDecision below only needs decide()'s own
  // real-time capability check to see the restored role, not the discovery cache.
  // No raw-SQL direct-writer rejection check here: Aurora DSQL rejects CREATE TRIGGER and CREATE
  // FUNCTION outright, so there is no requirement-transition trigger, and the guarantee such a trigger
  // would enforce holds by construction instead -- satisfied_participants is derived from
  // deployment_approval_decisions, whose UNIQUE constraint makes a duplicate participant structurally
  // impossible. A raw SQL statement bypassing the application has no guard at all, so there is no
  // application-level operation to test.
  // Hold the local executor after the second immutable decision so this archive-boundary test
  // exercises the satisfied handoff before any compatible worker may begin execution.
  await worker.stop();
  worker = undefined;
  const secondDecision = await decide(service, approverTwo, productionHighRequirement, productionHighFirst.requirement.revision, "APPROVE", "REVIEWED_CHANGE_SCOPE");
  assert.deepEqual(secondDecision.decideDeploymentApproval.problems, []);
  assert.equal(secondDecision.decideDeploymentApproval.requirement.status, "SATISFIED");
  assert.deepEqual(secondDecision.decideDeploymentApproval.requirement.satisfiedParticipants.map((principal) => principal.id).sort(), [approverOne, approverTwo].sort());
  // No backfill scenario here: the decision write path writes deployment_approval_replay_receipts
  // synchronously for every decision and every APPROVAL_REPLAYED audit fact, so no receipt is ever
  // missing. secondDecisionRequest's own receipt (written synchronously when secondDecision was
  // created above) is exactly what recoveredReplay below exercises.
  const secondDecisionRequest = await client.query("SELECT request_key FROM deployment_approval_decisions WHERE id = $1", [secondDecision.decideDeploymentApproval.decision.id]);
  const historicalReplayCount = await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_REPLAYED'", [productionHigh.id]);
  const recoveredReplay = await decide(service, approverTwo, productionHighRequirement, productionHighFirst.requirement.revision,
    "APPROVE", "REVIEWED_CHANGE_SCOPE", null, secondDecisionRequest.rows[0].request_key);
  assert.deepEqual(recoveredReplay.decideDeploymentApproval.problems, []);
  assert.equal(recoveredReplay.decideDeploymentApproval.decision.id, secondDecision.decideDeploymentApproval.decision.id);
  // This first replay of secondDecisionRequest's own key appends exactly one APPROVAL_REPLAYED fact:
  // no replayed fact exists for this key yet.
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_REPLAYED'", [productionHigh.id])).rows[0].count,
    historicalReplayCount.rows[0].count + 1);
  // A response lost after the first commit retries the same immutable request identity even after
  // the second decision terminalizes the requirement.
  const replayedDecision = await decide(service, approverOne, productionHighRequirement, productionHighFirst.requirement.revision,
    "APPROVE", "REVIEWED_CHANGE_SCOPE", null, firstDecisionKey);
  const replayedDecisionCorrelation = lastGraphqlRequestId;
  assert.deepEqual(replayedDecision.decideDeploymentApproval.problems, []);
  assert.equal(replayedDecision.decideDeploymentApproval.decision.id, firstDecision.decideDeploymentApproval.decision.id);
  // The decision history pages by page number, over the generated `deploymentApprovalDecisions`
  // relation.
  const decisionHistory = "query DecisionHistory($id: String!, $page: Int!) { deploymentApprovalRequirements(filters: { id: { eq: $id } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { deploymentApprovalDecisions(orderBy: { decidedAt: ASC, id: ASC }, pagination: { page: { limit: 1, page: $page } }) { nodes { id actorPrincipalId approvalRequirementId } pageInfo { hasNextPage } } } } }";
  const decisionHistoryFirst = await graphql(service, approverOne, decisionHistory,
    { id: productionHighRequirement, page: 0 });
  const firstHistoryPage = decisionHistoryFirst.deploymentApprovalRequirements.nodes[0].deploymentApprovalDecisions;
  assert.equal(firstHistoryPage.nodes.length, 1);
  assert.equal(firstHistoryPage.pageInfo.hasNextPage, true);
  const decisionHistorySecond = await graphql(service, approverOne, decisionHistory,
    { id: productionHighRequirement, page: 1 });
  const secondHistoryPage = decisionHistorySecond.deploymentApprovalRequirements.nodes[0].deploymentApprovalDecisions;
  assert.equal(secondHistoryPage.nodes.length, 1);
  assert.equal(secondHistoryPage.pageInfo.hasNextPage, false);
  assert.notEqual(secondHistoryPage.nodes[0].id, firstHistoryPage.nodes[0].id);
  assert.deepEqual(secondDecision.decideDeploymentApproval.requirement.satisfiedParticipants.map((principal) => principal.subject).sort(),
    ["m14-approver-one", "m14-approver-two"]);
  // A lost decision response remains replayable after a later archive boundary. The retry only
  // reads the actor's immutable decision and must not become a new archive-era decision attempt.
  const replayArchiveRevision = Number((await client.query("SELECT revision FROM projects WHERE id = $1", [project])).rows[0].revision);
  const replayArchive = await graphql(service, requester,
    "mutation ArchiveReplay($input: LifecycleAdministrationInput!) { archiveAdministrationScope(input: $input) { project { revision } problems { code } } }",
    { input: { scope: "PROJECT", scopeId: project, expectedRevision: replayArchiveRevision, reason: "Replay archive boundary" } });
  assert.deepEqual(replayArchive.archiveAdministrationScope.problems, []);
  await waitForLifecycle(service, productionHigh.id, "CANCELED");
  const replayRestore = await graphql(service, requester,
    "mutation RestoreReplay($input: LifecycleAdministrationInput!) { restoreAdministrationScope(input: $input) { project { lifecycleStatus } problems { code } } }",
    { input: { scope: "PROJECT", scopeId: project, expectedRevision: replayArchive.archiveAdministrationScope.project.revision } });
  assert.deepEqual(replayRestore.restoreAdministrationScope.problems, []);
  const replayActorMembership = await client.query("SELECT membership_id FROM project_membership_roles role JOIN project_memberships membership ON membership.id = role.membership_id WHERE membership.principal_id = $1 AND membership.project_id = $2 AND role.role_code = 'DEPLOYMENT_APPROVER'", [approverOne, project]);
  await client.query("UPDATE project_membership_roles SET role_code = 'AUDITOR' WHERE membership_id = $1 AND role_code = 'DEPLOYMENT_APPROVER'", [replayActorMembership.rows[0].membership_id]);
  const replayWithDecideRevoked = await decide(service, approverOne, productionHighRequirement, productionHighFirst.requirement.revision,
    "APPROVE", "REVIEWED_CHANGE_SCOPE", null, firstDecisionKey);
  assert.deepEqual(replayWithDecideRevoked.decideDeploymentApproval.problems, []);
  assert.equal(replayWithDecideRevoked.decideDeploymentApproval.decision.id, firstDecision.decideDeploymentApproval.decision.id);
  await client.query("UPDATE project_membership_roles SET role_code = 'DEPLOYMENT_APPROVER' WHERE membership_id = $1 AND role_code = 'AUDITOR'", [replayActorMembership.rows[0].membership_id]);
  const replayedAfterArchive = await decide(service, approverOne, productionHighRequirement, productionHighFirst.requirement.revision,
    "APPROVE", "REVIEWED_CHANGE_SCOPE", null, firstDecisionKey);
  assert.deepEqual(replayedAfterArchive.decideDeploymentApproval.problems, []);
  assert.equal(replayedAfterArchive.decideDeploymentApproval.decision.id, firstDecision.decideDeploymentApproval.decision.id);
  worker = await startLocalDeploymentWorker(database.name, {
    HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "200",
    HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "100"
  });
  assert.match(lastGraphqlRequestId, /^[0-9a-f-]{36}$/);
  const replayAudit = await client.query(`SELECT facts->>'decisionId' AS decision_id, facts->>'requirementId' AS requirement_id,
      facts->>'requestId' AS request_id, facts->>'correlationId' AS correlation_id, facts->>'outcome' AS outcome
    FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_REPLAYED'
    ORDER BY occurred_at DESC, id DESC LIMIT 1`, [productionHigh.id]);
  assert.deepEqual(replayAudit.rows[0], { decision_id: firstDecision.decideDeploymentApproval.decision.id,
    requirement_id: productionHighRequirement, request_id: firstDecisionKey, correlation_id: replayedDecisionCorrelation,
    outcome: "IMMUTABLE_DECISION_RETURNED" });
  const replayTimeline = await graphql(service, approverOne,
    "query ReplayTimeline($id: String!) { deployments(filters: { id: { eq: $id } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { timeline(first: 100) { stage status message source } } } }",
    { id: productionHigh.id });
  const replayTimelineEvent = replayTimeline.deployments.nodes[0].timeline
    .find((event) => event.stage === "APPROVAL_REPLAYED");
  assert.deepEqual(replayTimelineEvent, { stage: "APPROVAL_REPLAYED", status: "RECORDED",
    message: "The service returned the actor's immutable decision for this request.", source: "SERVICE" });
  const replayStormBefore = (await client.query(`SELECT projection_revision,
      (SELECT count(*)::int FROM deployment_approval_decisions WHERE approval_requirement_id = $1) AS decision_count,
      (SELECT count(*)::int FROM deployment_outbox_events WHERE deployment_id = $2) AS outbox_count
    FROM deployments WHERE id = $2`, [productionHighRequirement, productionHigh.id])).rows[0];
  const replayStorm = [];
  for (let batch = 0; batch < 10; batch += 1) {
    replayStorm.push(...await Promise.all(Array.from({ length: 10 }, () => decide(service, approverOne,
      productionHighRequirement, productionHighFirst.requirement.revision, "APPROVE", "REVIEWED_CHANGE_SCOPE", null, firstDecisionKey))));
  }
  for (const replay of replayStorm) {
    assert.deepEqual(replay.decideDeploymentApproval.problems, []);
    assert.equal(replay.decideDeploymentApproval.decision.id, firstDecision.decideDeploymentApproval.decision.id);
  }
  const replayStormAfter = (await client.query(`SELECT projection_revision,
      (SELECT count(*)::int FROM deployment_approval_decisions WHERE approval_requirement_id = $1) AS decision_count,
      (SELECT count(*)::int FROM deployment_outbox_events WHERE deployment_id = $2) AS outbox_count
    FROM deployments WHERE id = $2`, [productionHighRequirement, productionHigh.id])).rows[0];
  assert.deepEqual(replayStormAfter, replayStormBefore);
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_REPLAYED' AND facts->>'decisionId' = $2", [productionHigh.id, firstDecision.decideDeploymentApproval.decision.id])).rows[0].count, 1);
  // No raw-SQL immutability checks here: deployment_approval_replay_receipts_no_update/_no_delete are
  // removed (Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright -- see V030's removal
  // comment), and auditApprovalReplay() is this table's only writer -- a single, unconditional INSERT,
  // never an UPDATE or DELETE -- so there is no application-level operation left to test.
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_decisions WHERE approval_requirement_id = $1 AND actor_principal_id = $2 AND request_key = $3", [productionHighRequirement, approverOne, firstDecisionKey])).rows[0].count, 1);
  // A transaction can observe a cancellation after it selected the inbox requirement. The item
  // remains visible for its immutable history, but it cannot advertise an actionable decision.
  const canceledInboxDeployment = await request(service, highVersionId, production, "canceled-inbox-projection");
  const canceledInboxRequirement = await requirementForDeployment(client, canceledInboxDeployment.id);
  // No DISABLE/ENABLE TRIGGER bracket needed here: deployment_approval_lifecycle_handoff_trigger is
  // removed (Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright -- see V017's removal
  // comment), so this raw UPDATE already lands on PENDING + CANCELED with nothing left to disable.
  await client.query("UPDATE deployments SET lifecycle_status = 'CANCELED', revision = revision + 1 WHERE id = $1", [canceledInboxDeployment.id]);
  const canceledInbox = await graphql(service, approverOne,
    inboxQuery("CanceledInbox", "decisionAvailable id status deployments { lifecycleStatus }", "projectId: { eq: \"50000000-0000-0000-0000-000000000003\" }"),
    { limit: 50, page: 0 });
  const canceledInboxItem = canceledInbox.deploymentApprovalRequirements.nodes
    .find((item) => item.id === canceledInboxRequirement);
  assert.deepEqual(canceledInboxItem, { decisionAvailable: false, id: canceledInboxRequirement,
    status: "PENDING", deployments: { lifecycleStatus: "CANCELED" } });
  // A requirement's decision relation carries only its own decisions: no page of it ever reaches
  // another requirement's decisions, where the deleted cursor field refused a foreign cursor.
  const foreignRequirement = await requirementForDeployment(client, stagingMedium.id);
  const foreignDecisionHistory = await graphql(service, approverOne, decisionHistory,
    { id: foreignRequirement, page: 0 });
  const foreignDecisions = foreignDecisionHistory.deploymentApprovalRequirements.nodes[0].deploymentApprovalDecisions.nodes;
  assert.deepEqual(foreignDecisions.map((decision) => decision.approvalRequirementId), foreignDecisions.map(() => foreignRequirement));
  assert.equal(foreignDecisions.some((decision) => decision.id === firstHistoryPage.nodes[0].id), false);
  // No raw-SQL immutability checks here: Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION
  // outright, so no deployment_approval_decisions_no_update or _no_delete guard exists, and
  // insert_decision() is this table's only writer -- a single, unconditional INSERT, never an UPDATE
  // or DELETE -- so there is no application-level operation to test.

  const raceDeployment = await request(service, highVersionId, production, "approval-cancel-race");
  const raceRequirement = await requirementForDeployment(client, raceDeployment.id);
  const raceInitial = await approval(service, approverOne, raceRequirement);
  const [racedDecision, racedCancel] = await Promise.all([
    decide(service, approverOne, raceRequirement, raceInitial.requirement.revision, "APPROVE", "REVIEWED_CHANGE_SCOPE"),
    graphql(service, requester,
      `mutation CancelRace($input: CancelDeploymentInput!) { cancelDeployment(input: $input) { deployment { id lifecycleStatus } problems { code } } }`,
      { input: { deploymentId: raceDeployment.id, expectedRevision: raceDeployment.revision, reason: "Concurrent cancellation" } })
  ]);
  assert.equal(racedDecision.decideDeploymentApproval.problems.length === 0 || ["APPROVAL_REQUIREMENT_NOT_PENDING", "REVISION_CONFLICT"].includes(racedDecision.decideDeploymentApproval.problems[0].code), true, JSON.stringify({ racedDecision, racedCancel }));
  assert.equal(racedCancel.cancelDeployment.problems.length === 0 || racedCancel.cancelDeployment.problems[0].code === "REVISION_CONFLICT", true);

  const crossVersionId = await publishHighRiskVersion(service, base.agentId, base.document, "\n# cross authority fixture");
  const crossOne = await request(service, crossVersionId, production, "cross-authority-one");
  const crossTwo = await request(service, crossVersionId, production, "cross-authority-two");
  const crossOneRequirement = await requirementForDeployment(client, crossOne.id);
  const crossTwoRequirement = await requirementForDeployment(client, crossTwo.id);
  const crossOneFirst = await approval(service, approverOne, crossOneRequirement);
  const crossTwoFirst = await approval(service, approverTwo, crossTwoRequirement);
  assert.deepEqual((await decide(service, approverOne, crossOneRequirement, crossOneFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  assert.deepEqual((await decide(service, approverTwo, crossTwoRequirement, crossTwoFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const [crossOneSecond, crossTwoSecond] = await Promise.all([
    decide(service, approverTwo, crossOneRequirement, (await approval(service, approverTwo, crossOneRequirement)).requirement.revision, "APPROVE"),
    decide(service, approverOne, crossTwoRequirement, (await approval(service, approverOne, crossTwoRequirement)).requirement.revision, "APPROVE")
  ]);
  assert.deepEqual(crossOneSecond.decideDeploymentApproval.problems, []);
  assert.deepEqual(crossTwoSecond.decideDeploymentApproval.problems, []);

  const selfDeployment = await request(service, highVersionId, production, "self");
  const selfRequirement = await requirementForDeployment(client, selfDeployment.id);
  const requesterMembership = await client.query("SELECT id FROM project_memberships WHERE principal_id = $1 AND project_id = $2", [requester, project]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'DEPLOYMENT_APPROVER')", [requesterMembership.rows[0].id]);
  const self = await decide(service, requester, selfRequirement, (await approval(service, requester, selfRequirement)).requirement.revision, "APPROVE");
  assert.equal(self.decideDeploymentApproval.problems[0].code, "SELF_APPROVAL_FORBIDDEN");
  const selfReject = await decide(service, requester, selfRequirement, (await approval(service, requester, selfRequirement)).requirement.revision, "REJECT", null, "UNACCEPTABLE_CHANGE_SCOPE");
  assert.equal(selfReject.decideDeploymentApproval.problems[0].code, "SELF_APPROVAL_FORBIDDEN");
  const selfProjection = await approval(service, requester, selfRequirement);
  assert.equal(selfProjection.eligible, true);
  assert.equal(selfProjection.decisionAvailable, false);
  await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1 AND role_code = 'DEPLOYMENT_APPROVER'", [requesterMembership.rows[0].id]);

  const rejectionDeployment = await request(service, highVersionId, production, "rejection");
  const rejectionRequirement = await requirementForDeployment(client, rejectionDeployment.id);
  const rejectionInitial = await approval(service, approverOne, rejectionRequirement);
  const missingReason = await decide(service, approverOne, rejectionRequirement, rejectionInitial.requirement.revision, "REJECT");
  assert.equal(missingReason.decideDeploymentApproval.problems[0].code, "REJECTION_REASON_REQUIRED");
  const rejected = await decide(service, approverOne, rejectionRequirement, rejectionInitial.requirement.revision, "REJECT", null, "CHANGE_SCOPE_NOT_APPROVED");
  assert.deepEqual(rejected.decideDeploymentApproval.problems, []);
  assert.equal(rejected.decideDeploymentApproval.requirement.status, "REJECTED");
  assert.equal(rejected.decideDeploymentApproval.deployment.lifecycleStatus, "CANCELED");
  assert.equal(rejected.decideDeploymentApproval.decision.comment, null);
  const rejectionAudit = await client.query("SELECT facts->>'rejectionReason' AS rejection_reason FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_REJECTED'", [rejectionDeployment.id]);
  assert.deepEqual(rejectionAudit.rows, [{ rejection_reason: "CHANGE_SCOPE_NOT_APPROVED" }]);
  assert.equal((await decide(service, approverTwo, rejectionRequirement, rejected.decideDeploymentApproval.requirement.revision, "APPROVE")).decideDeploymentApproval.problems[0].code, "APPROVAL_REQUIREMENT_NOT_PENDING");

  const missingDeployment = await request(service, highVersionId, production, "evidence-missing");
  const missingRequirement = await invalidateEvidence(client, missingDeployment.id, async () => {
    await client.query("DROP RULE IF EXISTS deployment_evidence_snapshots_no_delete ON deployment_evidence_snapshots");
    await client.query("DELETE FROM deployment_evidence_snapshots WHERE deployment_id = $1 AND evidence_kind = 'PLAN_VALIDATED'", [missingDeployment.id]);
  });
  const missingResult = await decide(service, approverOne, missingRequirement, (await approval(service, approverOne, missingRequirement)).requirement.revision, "APPROVE");
  assert.equal(missingResult.decideDeploymentApproval.problems[0].code, "APPROVAL_EVIDENCE_MISSING");
  const missingProjection = await approval(service, approverOne, missingRequirement);
  assert.equal(missingProjection.requirement.status, "INVALIDATED");
  assert.equal(missingProjection.requirement.approvalSnapshot.evidence.find((evidence) => evidence.kind === "PLAN_VALIDATED").state, "MISSING");

  const columnMismatchDeployment = await request(service, highVersionId, production, "evidence-column-mismatch", false);
  await recordEvaluationPassed(columnMismatchDeployment.id, "0".repeat(64));
  const columnMismatchRequirement = await requirementForDeployment(client, columnMismatchDeployment.id);
  const columnMismatch = await decide(service, approverOne, columnMismatchRequirement, (await approval(service, approverOne, columnMismatchRequirement)).requirement.revision, "APPROVE");
  assert.equal(columnMismatch.decideDeploymentApproval.problems[0].code, "APPROVAL_EVIDENCE_MISMATCH");

  const revokedDeployment = await request(service, highVersionId, production, "evidence-revoked");
  const revokedRequirement = await invalidateEvidence(client, revokedDeployment.id, async () => {
    const evidence = await client.query("SELECT id FROM deployment_evidence_snapshots WHERE deployment_id = $1 AND evidence_kind = 'EVALUATION_PASSED'", [revokedDeployment.id]);
    await client.query("INSERT INTO deployment_evidence_invalidations (id, evidence_snapshot_id, kind) VALUES ($1, $2, 'REVOKED')", [randomUUID(), evidence.rows[0].id]);
  });
  // No pre-decide() status/audit-count assertions here: no trigger reacts to a
  // deployment_evidence_invalidations write and no application path writes that table, so the raw-SQL
  // insert above does not reconcile the still-PENDING requirement. decide()'s own real-time evidence
  // check -- which reconciles as a side effect of evaluating the attempt, the same pattern
  // missingRequirement above relies on -- is what catches this.
  const revoked = await decide(service, approverOne, revokedRequirement, (await approval(service, approverOne, revokedRequirement)).requirement.revision, "APPROVE");
  assert.equal(revoked.decideDeploymentApproval.problems[0].code, "APPROVAL_EVIDENCE_MISMATCH");
  assert.equal((await client.query("SELECT status FROM deployment_approval_requirements WHERE id = $1", [revokedRequirement])).rows[0].status, "INVALIDATED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_INVALIDATED'", [revokedDeployment.id])).rows[0].count, 1);
  assert.equal((await approval(service, approverOne, revokedRequirement)).requirement.approvalSnapshot.evidence.find((evidence) => evidence.kind === "EVALUATION_PASSED").state, "REVOKED");

  const failedEvidenceDeployment = await request(service, highVersionId, production, "evidence-failed");
  const failedEvidenceRequirement = await invalidateEvidence(client, failedEvidenceDeployment.id, async () => {
    const evidence = await client.query("SELECT id FROM deployment_evidence_snapshots WHERE deployment_id = $1 AND evidence_kind = 'EVALUATION_PASSED'", [failedEvidenceDeployment.id]);
    await client.query("INSERT INTO deployment_evidence_invalidations (id, evidence_snapshot_id, kind) VALUES ($1, $2, 'FAILED')", [randomUUID(), evidence.rows[0].id]);
  });
  const failedEvidence = await decide(service, approverOne, failedEvidenceRequirement, (await approval(service, approverOne, failedEvidenceRequirement)).requirement.revision, "APPROVE");
  assert.equal(failedEvidence.decideDeploymentApproval.problems[0].code, "APPROVAL_EVIDENCE_MISMATCH");
  assert.equal((await approval(service, approverOne, failedEvidenceRequirement)).requirement.approvalSnapshot.evidence.find((evidence) => evidence.kind === "EVALUATION_PASSED").state, "FAILED");

  const expiredEvidenceDeployment = await request(service, highVersionId, production, "evidence-expired");
  const expiredEvidenceRequirement = await invalidateEvidence(client, expiredEvidenceDeployment.id, async () => {
    await client.query("DROP RULE IF EXISTS deployment_evidence_snapshots_no_update ON deployment_evidence_snapshots");
    await client.query("UPDATE deployment_evidence_snapshots SET expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second' WHERE deployment_id = $1 AND evidence_kind = 'EVALUATION_PASSED'", [expiredEvidenceDeployment.id]);
  });
  const expiredEvidence = await decide(service, approverOne, expiredEvidenceRequirement, (await approval(service, approverOne, expiredEvidenceRequirement)).requirement.revision, "APPROVE");
  assert.equal(expiredEvidence.decideDeploymentApproval.problems[0].code, "APPROVAL_EVIDENCE_EXPIRED");
  assert.equal((await approval(service, approverOne, expiredEvidenceRequirement)).requirement.approvalSnapshot.evidence.find((evidence) => evidence.kind === "EVALUATION_PASSED").state, "EXPIRED");

  const inboxExpiredDeployment = await request(service, highVersionId, production, "inbox-expired");
  const inboxExpiredRequirement = await requirementForDeployment(client, inboxExpiredDeployment.id);
  // No DISABLE/ENABLE TRIGGER bracket needed here: Aurora DSQL supports no triggers, so nothing
  // rejects this raw expires_at rewrite.
  await client.query("UPDATE deployment_approval_requirements SET expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second' WHERE id = $1", [inboxExpiredRequirement]);
  await waitForRequirementStatus(inboxExpiredRequirement, "EXPIRED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_EXPIRED'", [inboxExpiredDeployment.id])).rows[0].count, 1);

  const mismatchDeployment = await request(service, highVersionId, production, "evidence-mismatch");
  const mismatchRequirement = await invalidateEvidence(client, mismatchDeployment.id, async () => {
    await client.query("DROP RULE IF EXISTS deployment_evidence_snapshots_no_update ON deployment_evidence_snapshots");
    // The mismatch check binds evidence to the frozen policy facts (binding, target, plan, package).
    // It does not recompute evidence_digest: that needs pgcrypto, which Aurora DSQL rejects.
    await client.query("UPDATE deployment_evidence_snapshots SET target_digest = repeat('0', 64) WHERE deployment_id = $1 AND evidence_kind = 'EVALUATION_PASSED'", [mismatchDeployment.id]);
  });
  const mismatch = await decide(service, approverOne, mismatchRequirement, (await approval(service, approverOne, mismatchRequirement)).requirement.revision, "APPROVE");
  assert.equal(mismatch.decideDeploymentApproval.problems[0].code, "APPROVAL_EVIDENCE_MISMATCH");
  assert.equal((await approval(service, approverOne, mismatchRequirement)).requirement.approvalSnapshot.evidence.find((evidence) => evidence.kind === "EVALUATION_PASSED").state, "MISMATCH");

  for (const [suffix, rewrite] of [
    ["target-mismatch", async (deploymentId) => { await client.query("DROP RULE IF EXISTS deployment_policy_snapshots_no_update ON deployment_policy_snapshots"); await client.query("UPDATE deployment_policy_snapshots SET target_digest = repeat('0', 64) WHERE deployment_id = $1", [deploymentId]); }],
    ["version-mismatch", async (deploymentId) => { await client.query("UPDATE deployment_policy_snapshots SET agent_version_id = $1 WHERE deployment_id = $2", [base.versionId, deploymentId]); }],
    ["plan-mismatch", async (deploymentId) => { await client.query("DROP RULE IF EXISTS deployment_plan_versions_no_update ON deployment_plan_versions"); await client.query("UPDATE deployment_plan_versions SET package_digest = repeat('1', 64) WHERE deployment_id = $1", [deploymentId]); }],
    ["artifact-mismatch", async (deploymentId) => { await client.query("UPDATE deployment_policy_snapshots SET package_digest = repeat('2', 64) WHERE deployment_id = $1", [deploymentId]); }],
    ["risk-mismatch", async (deploymentId) => { await client.query("UPDATE deployment_policy_snapshots SET risk = CASE WHEN risk = 'HIGH' THEN 'LOW' ELSE 'HIGH' END WHERE deployment_id = $1", [deploymentId]); }],
    ["rule-mismatch", async (deploymentId) => { await client.query("UPDATE deployment_policy_snapshots SET required_approvers = CASE WHEN required_approvers = 2 THEN 1 ELSE 2 END WHERE deployment_id = $1", [deploymentId]); }]
  ]) {
    const deployment = await request(service, highVersionId, production, suffix);
    const requirement = await invalidateEvidence(client, deployment.id, () => rewrite(deployment.id));
    const result = await decide(service, approverOne, requirement, (await approval(service, approverOne, requirement)).requirement.revision, "APPROVE");
    assert.equal(result.decideDeploymentApproval.problems[0]?.code, "APPROVAL_EVIDENCE_MISMATCH", `${suffix}: ${JSON.stringify(result.decideDeploymentApproval)}`);
  }

  const expiredDeployment = await request(service, highVersionId, production, "requirement-expired");
  const expiredRequirement = await requirementForDeployment(client, expiredDeployment.id);
  // No DISABLE/ENABLE TRIGGER bracket needed here either -- same reasoning as the inbox-expired case above.
  await client.query("UPDATE deployment_approval_requirements SET expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second' WHERE id = $1", [expiredRequirement]);
  // The maintenance tick is the only writer of the expiry transition now: the deleted
  // approvalRequirement read performed it as a side effect of answering, and a generated read
  // cannot write. The surface already reports EXPIRED before the tick runs (see the StaleInbox
  // check below); this scenario is about the decision, so it waits for the stored transition and
  // then decides on the requirement's own current revision.
  await waitForRequirementStatus(expiredRequirement, "EXPIRED");
  const expiredCurrent = await approval(service, approverOne, expiredRequirement);
  const expired = await decide(service, approverOne, expiredRequirement, expiredCurrent.requirement.revision, "APPROVE");
  assert.equal(expired.decideDeploymentApproval.problems[0].code, "APPROVAL_REQUIREMENT_EXPIRED",
    JSON.stringify(expired.decideDeploymentApproval.problems));
  assert.equal((await approval(service, approverOne, expiredRequirement)).requirement.status, "EXPIRED");

  const canceledDeployment = await request(service, highVersionId, production, "canceled");
  const canceledRequirement = await requirementForDeployment(client, canceledDeployment.id);
  const canceled = await graphql(service, requester,
    `mutation Cancel($input: CancelDeploymentInput!) { cancelDeployment(input: $input) { deployment { ${deploymentFields} } problems { code } } }`,
    { input: { deploymentId: canceledDeployment.id, expectedRevision: canceledDeployment.revision, reason: "Stop the local approval test" } });
  assert.deepEqual(canceled.cancelDeployment.problems, []);
  assert.equal((await approval(service, approverOne, canceledRequirement)).requirement.status, "INVALIDATED");

  const retrySource = await request(service, highVersionId, production, "retry-source");
  const retryRequirement = await requirementForDeployment(client, retrySource.id);
  const retrySourceSnapshot = (await approval(service, approverOne, retryRequirement)).requirement.approvalSnapshot;
  const policy = await graphql(service, requester,
    "query Policy($id: String!) { projectApprovalPolicies(filters: { projectId: { eq: $id } }) { nodes { currentVersion { revision rules { cell requiredEvidence requiredApprovers } } } } }", { id: project });
  const baseline = Object.fromEntries(policy.projectApprovalPolicies.nodes[0].currentVersion.rules.map((item) => [item.cell, { requiredEvidence: item.requiredEvidence, requiredApprovers: item.requiredApprovers }]));
  const strengthened = { ...baseline,
    DEVELOPMENT_LOW: { requiredEvidence: ["PLAN_VALIDATED", "CHANGE_SUMMARY_READY", "EVALUATION_PASSED"], requiredApprovers: 0 },
    STAGING_LOW: { requiredEvidence: ["PLAN_VALIDATED", "CHANGE_SUMMARY_READY", "EVALUATION_PASSED"], requiredApprovers: 0 }
  };
  const update = await graphql(service, requester,
    "mutation Policy($input: UpdateProjectApprovalPolicyInput!) { updateProjectApprovalPolicy(input: $input) { project { projectApprovalPolicies { currentVersion { revision digest } } } problems { code } } }",
    { input: { projectId: project, expectedRevision: policy.projectApprovalPolicies.nodes[0].currentVersion.revision, matrix: Object.entries(strengthened).map(([cell, rule]) => ({ cell, ...rule })), reason: "Freeze retry policy fixture" } });
  assert.deepEqual(update.updateProjectApprovalPolicy.problems, []);
  await client.query("DROP RULE IF EXISTS deployment_evidence_snapshots_no_delete ON deployment_evidence_snapshots");
  await client.query("DELETE FROM deployment_evidence_snapshots WHERE deployment_id = $1 AND evidence_kind IN ('PLAN_VALIDATED', 'EVALUATION_PASSED')", [retrySource.id]);
  const retryInvalidated = await decide(service, approverOne, retryRequirement, (await approval(service, approverOne, retryRequirement)).requirement.revision, "APPROVE");
  assert.equal(retryInvalidated.decideDeploymentApproval.problems[0].code, "APPROVAL_EVIDENCE_MISSING");
  const sourceAfterPolicyChange = await approval(service, approverOne, retryRequirement);
  assert.equal(sourceAfterPolicyChange.requirement.approvalSnapshot.policyDigest, retrySourceSnapshot.policyDigest);
  assert.equal(sourceAfterPolicyChange.requirement.approvalSnapshot.policyRevision, retrySourceSnapshot.policyRevision);
  const sameCycle = await request(service, highVersionId, production, "retry-source");
  assert.equal(sameCycle.id, retrySource.id);
  const retried = await request(service, highVersionId, production, "retry-new-cycle");
  const retriedRequirement = await approval(service, approverOne, await requirementForDeployment(client, retried.id));
  assert.equal(retriedRequirement.requirement.approvalSnapshot.policyRevision, update.updateProjectApprovalPolicy.project.projectApprovalPolicies.currentVersion.revision);
  assert.notEqual(retriedRequirement.requirement.id, retryRequirement);

  const zeroEvaluation = await request(service, highVersionId, development, "zero-approver-evaluation", false);
  assert.deepEqual([zeroEvaluation.deploymentPolicySnapshots.risk, zeroEvaluation.deploymentPolicySnapshots.requiredApprovers], ["LOW", 0]);
  assert.notEqual((await client.query("SELECT evaluation_requirement_expires_at FROM deployment_policy_snapshots WHERE deployment_id = $1", [zeroEvaluation.id])).rows[0].evaluation_requirement_expires_at, null);
  const zeroEvaluationRequirement = await requirementForDeployment(client, zeroEvaluation.id);
  assert.equal((await client.query("SELECT status FROM deployment_approval_requirements WHERE id = $1", [zeroEvaluationRequirement])).rows[0].status, "PENDING");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_outbox_events WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [zeroEvaluation.id])).rows[0].count, 0);
  const zeroEvaluationDetail = await approval(service, approverOne, zeroEvaluationRequirement);
  assert.equal(zeroEvaluationDetail.requirement.status, "PENDING");
  assert.equal(zeroEvaluationDetail.decisionAvailable, false);
  const zeroDecision = await decide(service, approverOne, zeroEvaluationRequirement, zeroEvaluationDetail.requirement.revision, "REJECT", null, "UNACCEPTABLE_CHANGE_SCOPE");
  assert.equal(zeroDecision.decideDeploymentApproval.problems[0].code, "APPROVAL_REQUIREMENT_NOT_PENDING");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_decisions WHERE approval_requirement_id = $1", [zeroEvaluationRequirement])).rows[0].count, 0);
  await recordEvaluationPassed(zeroEvaluation.id);
  await waitForLifecycle(service, zeroEvaluation.id, "ACTIVE");
  // No append-only DELETE check here: the migrations define no
  // deployment_approval_requirements_no_delete guard, and no code path DELETEs from this table --
  // there is no application-level operation for such a guard to protect.

  // This insert keeps the M13 policy-snapshot column list. V017 derives its additive digest before
  // enforcing the new non-null invariant, so an older API can write while a compatible worker rolls out.
  const m13CompatibleDeployment = randomUUID();
  await client.query(`INSERT INTO deployments (id, organization_id, project_id, agent_id, agent_version_id, catalog_release_id,
      catalog_release_digest, environment, target_digest, strategy, lifecycle_status, revision, idempotency_key, requested_by,
      requested_at, updated_at, environment_definition_version_id, request_fingerprint, projection_revision)
    SELECT $1, organization_id, project_id, agent_id, agent_version_id, catalog_release_id, catalog_release_digest, environment,
      target_digest, strategy, 'CANCELED', 1, $2, requested_by, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP,
      environment_definition_version_id, request_fingerprint, projection_revision
    FROM deployments WHERE id = $3`, [m13CompatibleDeployment, `m13-compatible-${run}`, zeroEvaluation.id]);
  // risk_verification_digest is computed explicitly here (encode(digest(risk || '|' || binding_digest,
  // 'sha256'), 'hex')), not left for a trigger to fill in: deployment_policy_snapshot_risk_verification_
  // digest_trigger does not exist, because Aurora DSQL supports no triggers, and this raw insert has
  // no other source for the value the assertion below expects.
  await client.query(`INSERT INTO deployment_policy_snapshots (deployment_id, policy_id, policy_revision, policy_digest, policy_matrix,
      logical_environment_class, risk, required_evidence, required_approvers, agent_version_id, environment_definition_version_id,
      target_digest, plan_digest, package_digest, binding_digest, evaluation_requirement_expires_at, risk_verification_digest)
    SELECT $1, policy_id, policy_revision, policy_digest, policy_matrix, logical_environment_class, risk, required_evidence,
      required_approvers, agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest,
      binding_digest, evaluation_requirement_expires_at, encode(sha256(convert_to(risk || '|' || binding_digest, 'UTF8')), 'hex')
    FROM deployment_policy_snapshots WHERE deployment_id = $2`, [m13CompatibleDeployment, zeroEvaluation.id]);
  // ensure_requirement() is private to the persistence crate and unreachable from this script. Unlike
  // legacyDeploymentFrom() above, this row is not a clone of an already-resolved source: it is a
  // brand-new requirement for a deployment inserted directly into a terminal (CANCELED)
  // lifecycle_status, so this replicates ensure_requirement()'s terminal-lifecycle INVALIDATED branch
  // and record_terminal_invalidation()'s timeline-sequence claim plus APPROVAL_INVALIDATED audit fact.
  await client.query(`INSERT INTO deployment_approval_requirements
      (id, deployment_id, revision, organization_id, project_id, requested_at, required_approvers, status,
       expires_at, invalidated_at, invalidation_code)
    SELECT gen_random_uuid(), deployment.id, 1, deployment.organization_id, deployment.project_id, deployment.requested_at,
      policy.required_approvers, 'INVALIDATED', deployment.requested_at + INTERVAL '24 hours', CURRENT_TIMESTAMP, 'TERMINAL_LIFECYCLE'
    FROM deployments deployment JOIN deployment_policy_snapshots policy ON policy.deployment_id = deployment.id
    WHERE deployment.id = $1
    ON CONFLICT (deployment_id) DO NOTHING
    RETURNING id`, [m13CompatibleDeployment]).then(async (inserted) => {
    if (inserted.rowCount === 0) return;
    const requirementId = inserted.rows[0].id;
    const sequence = (await client.query(`INSERT INTO deployment_timeline_counters (deployment_id, attempt_number, next_sequence)
      VALUES ($1, 0, 2) ON CONFLICT (deployment_id, attempt_number) DO UPDATE SET next_sequence = deployment_timeline_counters.next_sequence + 1
      RETURNING next_sequence - 1 AS sequence`, [m13CompatibleDeployment])).rows[0].sequence;
    await client.query(`INSERT INTO deployment_audit_events (id, deployment_id, actor_principal_id, action, facts, deployment_attempt_id, attempt_number, timeline_sequence)
      VALUES ($1, $2, NULL, 'APPROVAL_INVALIDATED', jsonb_build_object('requirementId', $3::text, 'code', 'TERMINAL_LIFECYCLE'), NULL, 0, $4)`,
      [randomUUID(), m13CompatibleDeployment, requirementId, sequence]);
  });
  assert.equal((await client.query("SELECT risk_verification_digest = encode(sha256(convert_to(risk || '|' || binding_digest, 'UTF8')), 'hex') AS valid FROM deployment_policy_snapshots WHERE deployment_id = $1", [m13CompatibleDeployment])).rows[0].valid, true);
  assert.equal((await client.query("SELECT status, invalidation_code FROM deployment_approval_requirements WHERE deployment_id = $1", [m13CompatibleDeployment])).rows[0].status, "INVALIDATED");
  assert.equal((await client.query("SELECT status, invalidation_code FROM deployment_approval_requirements WHERE deployment_id = $1", [m13CompatibleDeployment])).rows[0].invalidation_code, "TERMINAL_LIFECYCLE");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_INVALIDATED'", [m13CompatibleDeployment])).rows[0].count, 1);

  // No "deployment missing its requirement row" scenario here: it is structurally unreachable, not
  // just untested. insertApprovalRequirement() creates that row synchronously for every deployment
  // the single write path produces, and legacyDeploymentFrom() does too.

  // No "a claimant already holds PROCESSING before the requirement exists" scenario here either:
  // Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, so there is no outbox gate or
  // execution-worker gate in the database to fail closed. execute()'s own active_project(),
  // approval_execution_eligible() and compatible_approval_worker() pre-checks cover the write path.

  const delayedEvaluationVersion = await publishHighRiskVersion(service, base.agentId, base.document, "\n# delayed evaluation handoff fixture");
  const delayedEvaluation = await request(service, delayedEvaluationVersion, production, "delayed-evaluation", false);
  const delayedEvaluationRequirement = await requirementForDeployment(client, delayedEvaluation.id);
  const delayedEvaluationFirst = await approval(service, approverOne, delayedEvaluationRequirement);
  assert.deepEqual((await decide(service, approverOne, delayedEvaluationRequirement, delayedEvaluationFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const delayedEvaluationSecond = await approval(service, approverTwo, delayedEvaluationRequirement);
  assert.deepEqual((await decide(service, approverTwo, delayedEvaluationRequirement, delayedEvaluationSecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  assert.equal((await client.query("SELECT status FROM deployment_approval_requirements WHERE id = $1", [delayedEvaluationRequirement])).rows[0].status, "SATISFIED");
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [delayedEvaluation.id])).rows[0].lifecycle_status, "APPROVED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_outbox_events WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [delayedEvaluation.id])).rows[0].count, 0);
  await recordEvaluationPassed(delayedEvaluation.id);
  await waitForLifecycle(service, delayedEvaluation.id, "ACTIVE");

  const expiredDelayedEvaluationVersion = await publishHighRiskVersion(service, base.agentId, base.document, "\n# delayed evaluation expiry fixture");
  const expiredDelayedEvaluation = await request(service, expiredDelayedEvaluationVersion, production, "delayed-evaluation-expired", false);
  const expiredDelayedRequirement = await requirementForDeployment(client, expiredDelayedEvaluation.id);
  const expiredDelayedFirst = await approval(service, approverOne, expiredDelayedRequirement);
  assert.deepEqual((await decide(service, approverOne, expiredDelayedRequirement, expiredDelayedFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const expiredDelayedSecond = await approval(service, approverTwo, expiredDelayedRequirement);
  assert.deepEqual((await decide(service, approverTwo, expiredDelayedRequirement, expiredDelayedSecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  await client.query("UPDATE deployment_approval_requirements SET expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second' WHERE id = $1", [expiredDelayedRequirement]);
  await waitForLifecycle(service, expiredDelayedEvaluation.id, "CANCELED");
  assert.equal((await client.query("SELECT status FROM deployment_approval_requirements WHERE id = $1", [expiredDelayedRequirement])).rows[0].status, "SATISFIED");
  await recordEvaluationPassed(expiredDelayedEvaluation.id);
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_outbox_events WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [expiredDelayedEvaluation.id])).rows[0].count, 0);

  // This reproduces the column order and event sequence of the retained M13 writer. V017 derives
  // its new digest, creates the requirement, suppresses the legacy event, and lets M14 approval
  // decisions resume the cycle with a compatible worker.
  const legacyDeployment = randomUUID();
  const legacyPlan = randomUUID();
  await client.query(`INSERT INTO deployments (id, organization_id, project_id, agent_id, agent_version_id, catalog_release_id,
      catalog_release_digest, environment, target_digest, strategy, lifecycle_status, revision, idempotency_key, requested_by,
      requested_at, updated_at, environment_definition_version_id, request_fingerprint, projection_revision)
    SELECT $1, organization_id, project_id, agent_id, agent_version_id, catalog_release_id, catalog_release_digest, environment,
      target_digest, strategy, 'REQUESTED', 1, $2, requested_by, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP,
      environment_definition_version_id, request_fingerprint, projection_revision
    FROM deployments WHERE id = $3`, [legacyDeployment, `m13-handoff-${run}`, productionHigh.id]);
  await client.query(`INSERT INTO deployment_plan_versions (id, deployment_id, version_number, agent_version_id, catalog_release_id,
      environment, environment_definition_version_id, agent_content_digest, catalog_release_digest, target_digest, compiler_version,
      canonical_plan, plan_digest, package_digest, package_reference, created_by)
    SELECT $1, $2, version_number, agent_version_id, catalog_release_id, environment, environment_definition_version_id,
      agent_content_digest, catalog_release_digest, target_digest, compiler_version, canonical_plan, plan_digest, package_digest,
      package_reference, created_by
    FROM deployment_plan_versions WHERE deployment_id = $3 AND version_number = 1`, [legacyPlan, legacyDeployment, productionHigh.id]);
  await client.query(`INSERT INTO deployment_plan_review_facts (plan_id, active_agent_version_number, change_summary,
      requested_dependency_versions, added_dependency_versions, removed_dependency_versions)
    SELECT $1, active_agent_version_number, change_summary, requested_dependency_versions, added_dependency_versions,
      removed_dependency_versions
    FROM deployment_plan_review_facts review JOIN deployment_plan_versions plan ON plan.id = review.plan_id
    WHERE plan.deployment_id = $2 AND plan.version_number = 1
      AND NOT EXISTS (SELECT 1 FROM deployment_plan_review_facts WHERE plan_id = $1)`, [legacyPlan, productionHigh.id]);
  await client.query(`INSERT INTO deployment_policy_snapshots (deployment_id, policy_id, policy_revision, policy_digest, policy_matrix,
      logical_environment_class, risk, required_evidence, required_approvers, agent_version_id, environment_definition_version_id,
      target_digest, plan_digest, package_digest, binding_digest, evaluation_requirement_expires_at)
    SELECT $1, policy_id, policy_revision, policy_digest, policy_matrix, logical_environment_class, risk, required_evidence,
      required_approvers, agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest,
      binding_digest, evaluation_requirement_expires_at
    FROM deployment_policy_snapshots WHERE deployment_id = $2`, [legacyDeployment, productionHigh.id]);
  // Nothing creates this row automatically on the policy-snapshot INSERT above, so
  // ensureRequirementViaSql() derives it (see its own comment). This deployment is inserted REQUESTED,
  // a fresh cycle rather than a clone of productionHigh's own current state -- by this point in the
  // run productionHigh's own requirement is already past PENDING (decided twice above), so its
  // requirement must be freshly derived here too, not copied from that already-resolved source.
  await ensureRequirementViaSql(legacyDeployment);
  await client.query(`INSERT INTO deployment_evidence_snapshots (id, deployment_id, evidence_kind, evidence_digest, expires_at,
      agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest, binding_digest)
    SELECT gen_random_uuid(), $1, evidence_kind, evidence_digest, expires_at, agent_version_id, environment_definition_version_id,
      target_digest, plan_digest, package_digest, binding_digest
    FROM deployment_evidence_snapshots WHERE deployment_id = $2`, [legacyDeployment, productionHigh.id]);
  await client.query("INSERT INTO deployment_runtime_health (deployment_id, status, summary, generation) VALUES ($1, 'NOT_OBSERVED', 'No local runtime observation is available yet.', 1)", [legacyDeployment]);
  // No premature-EXECUTE_DEPLOYMENT-event-suppressed assertion here: there is no outbox gate trigger
  // and nothing inserts that event prematurely -- automatic_approval_handoff() (called from decide()
  // below) is the only path that enqueues an EXECUTE_DEPLOYMENT event, and only once the requirement
  // is SATISFIED.
  const legacyRequirement = await requirementForDeployment(client, legacyDeployment);
  const legacyFirst = await approval(service, approverOne, legacyRequirement);
  assert.deepEqual((await decide(service, approverOne, legacyRequirement, legacyFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const legacySecond = await approval(service, approverTwo, legacyRequirement);
  assert.deepEqual((await decide(service, approverTwo, legacyRequirement, legacySecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  await waitForLifecycle(service, legacyDeployment, "ACTIVE");

  const oversized = await decide(service, approverOne, retriedRequirement.requirement.id, retriedRequirement.requirement.revision, "APPROVE", "x".repeat(2001));
  assert.equal(oversized.decideDeploymentApproval.problems[0].code, "INVALID_APPROVAL_DECISION");
  const persistedDecisionsBeforeReviewText = (await client.query("SELECT count(*)::int AS count FROM deployment_approval_decisions WHERE approval_requirement_id = $1", [retriedRequirement.requirement.id])).rows[0].count;
  for (const sensitiveText of [
    "api key=not-a-secret-value", "token=not-a-secret-value", "client_secret=not-a-secret-value",
    "refresh_token=not-a-secret-value", "AWS_SESSION_TOKEN=not-a-secret-value", "AWS_SECRET_ACCESS_KEY=value",
    "password is hunter2", "pаssword hunter2", "hunter2", "api key is abc123", "client_secret is supersecret", "AWS_SESSION_TOKEN is value", "token is rotated hunter2", "password is valid oldsecret", "token ABC123", "password hunter2", "token\u200BABC123", "password\u200B hunter2",
    "token is rotated before release, hunter2", "token is rotated before release. hunter2", "token is rotated before release! hunter2", "token is rotated before release? hunter2", "password is valid for huntersecret",
    "AWS_SECRET_ACCESS_KEY_hunter2 is valid", "api-key-hunter2 is valid", "password-hunter2 is valid",
    "sk_live_value", "ghp_abcdefghijklmno", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signaturevalue",
    "person@example.invalid", "+12025550123", "1 Main Street", "AKIA0123456789ABCDEF"
  ]) {
    const sensitive = await decide(service, approverOne, retriedRequirement.requirement.id, retriedRequirement.requirement.revision, "APPROVE", sensitiveText);
    assert.equal(sensitive.decideDeploymentApproval.problems[0].code, "INVALID_APPROVAL_DECISION");
    const sensitiveReject = await decide(service, approverOne, retriedRequirement.requirement.id, retriedRequirement.requirement.revision, "REJECT", null, sensitiveText);
    assert.equal(sensitiveReject.decideDeploymentApproval.problems[0].code, "INVALID_APPROVAL_DECISION");
  }
  const contradictoryApprove = await decide(service, approverOne, retriedRequirement.requirement.id, retriedRequirement.requirement.revision,
    "APPROVE", "Reviewed the deployment change scope.", "The requested change scope is not approved.");
  assert.equal(contradictoryApprove.decideDeploymentApproval.problems[0].code, "INVALID_APPROVAL_DECISION");
  const contradictoryReject = await decide(service, approverOne, retriedRequirement.requirement.id, retriedRequirement.requirement.revision,
    "REJECT", "Reviewed the deployment change scope.", "The requested change scope is not approved.");
  assert.equal(contradictoryReject.decideDeploymentApproval.problems[0].code, "INVALID_APPROVAL_DECISION");
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "x".repeat(2001)]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'REJECT', NULL, $4, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "x".repeat(2001)]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "token=not-a-secret-value"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'REJECT', NULL, $4, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "person@example.invalid"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "client_secret=not-a-secret-value"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'REJECT', NULL, $4, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "AWS_SESSION_TOKEN=not-a-secret-value"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "password is hunter2"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "token ABC123"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'REJECT', NULL, $4, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "password hunter2"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "token\u200BABC123"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'REJECT', NULL, $4, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "password\u200B hunter2"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "pаssword hunter2"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'REJECT', NULL, $4, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "AWS_SESSION_TOKEN is value"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "token is rotated before release, hunter2"]), /check constraint/);
  await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'REJECT', NULL, $4, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverOne, "password is valid for huntersecret"]), /check constraint/);
  for (const trailingSecret of ["token is rotated before release. hunter2", "token is rotated before release! hunter2", "token is rotated before release? hunter2"]) {
    await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
        (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
      VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
      [randomUUID(), retriedRequirement.requirement.id, approverOne, trailingSecret]), /check constraint/);
  }
  for (const suffixedLabel of ["AWS_SECRET_ACCESS_KEY_hunter2 is valid", "api-key-hunter2 is valid", "password-hunter2 is valid"]) {
    await assert.rejects(client.query(`INSERT INTO deployment_approval_decisions
        (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
      VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
      [randomUUID(), retriedRequirement.requirement.id, approverOne, suffixedLabel]), /check constraint/);
  }
  const directOrdinaryComment = "REVIEWED_CHANGE_SCOPE";
  await client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, approverTwo, directOrdinaryComment]);
  const directTerminalStatusComment = "AUTHORIZATION_GRANTED";
  await client.query(`INSERT INTO deployment_approval_decisions
      (id, approval_requirement_id, actor_principal_id, decision, comment, rejection_reason, eligibility_checked_at, request_expected_revision, request_fingerprint)
    VALUES ($1, $2, $3, 'APPROVE', $4, NULL, CURRENT_TIMESTAMP, 1, repeat('a', 64))`,
    [randomUUID(), retriedRequirement.requirement.id, viewer, directTerminalStatusComment]);
  const ordinaryComment = "AUTHORIZATION_GRANTED";
  const ordinary = await decide(service, approverOne, retriedRequirement.requirement.id, retriedRequirement.requirement.revision, "APPROVE", ordinaryComment);
  assert.deepEqual(ordinary.decideDeploymentApproval.problems, []);
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_decisions WHERE approval_requirement_id = $1", [retriedRequirement.requirement.id])).rows[0].count, persistedDecisionsBeforeReviewText + 3);
  assert.equal(ordinary.decideDeploymentApproval.decision.comment, ordinaryComment);
  assert.equal(ordinary.decideDeploymentApproval.requirement.approvalSnapshot.riskLevel,
    ordinary.decideDeploymentApproval.requirement.approvalSnapshot.risk);
  // No raw-SQL frozen-fact rejection check here: Aurora DSQL supports no triggers, so there is no
  // requirement-transition guard in the database. transition_requirement() never includes
  // required_approvers in its UPDATE's SET clause, so the application write path cannot change it
  // either way, and a raw SQL statement bypassing the application has no guard at all.

  // A project role can predate its organization membership. V018 must avoid an organization-wide
  // scope rewrite while still discovering the principal after the membership becomes active.
  await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, 'm14-late-joiner', 'm14-late-joiner', 'm14-late-joiner@local.invalid')", [lateJoiner]);
  const lateProjectMembership = randomUUID();
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1)", [lateProjectMembership, project, lateJoiner]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'DEPLOYMENT_APPROVER')", [lateProjectMembership]);
  const lateBeforeOrganization = await graphql(service, lateJoiner,
    inboxQuery("LateBefore", "id", `projectId: { eq: "${project}" }`), { limit: 1, page: 0 });
  assert.deepEqual(lateBeforeOrganization.deploymentApprovalRequirements.nodes, []);
  const lateOrganizationMembership = randomUUID();
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL)", [lateOrganizationMembership, organization, lateJoiner]);
  await client.query("INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_MEMBER')", [lateOrganizationMembership]);
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_principal_project_scopes WHERE principal_id = $1 AND project_id = $2", [lateJoiner, project])).rows[0].count, 0);
  const lateAfterOrganization = await graphql(service, lateJoiner,
    inboxQuery("LateAfter", "id", `projectId: { eq: "${project}" }`), { limit: 50, page: 0 });
  assert(lateAfterOrganization.deploymentApprovalRequirements.nodes.some((node) => node.id === retriedRequirement.requirement.id));

  const page50 = `${newestFirst}, pagination: { page: { limit: 50, page: 0 } }`;
  const inbox = await graphql(service, approverOne,
    `query Inbox($organization: String!, $project: String!) { global: deploymentApprovalRequirements(${page50}) { nodes { ${approvalItemFields} } } organization: deploymentApprovalRequirements(filters: { organizationId: { eq: $organization } }, ${page50}) { nodes { id } } project: deploymentApprovalRequirements(filters: { projectId: { eq: $project } }, ${page50}) { nodes { id } } }`,
    { organization, project });
  assert(inbox.global.nodes.some((node) => node.id === retriedRequirement.requirement.id));
  assert(inbox.organization.nodes.some((node) => node.id === retriedRequirement.requirement.id));
  assert(inbox.project.nodes.some((node) => node.id === retriedRequirement.requirement.id));
  // The approval item's deployment is the generated entity, so its frozen plan is the stored
  // `deployment_plan_versions` row. An approver holds `DEPLOYMENT.VIEW`, which already read the
  // same canonical plan through the deleted `deploymentProjection` query.
  const approvalPlan = await graphql(service, approverOne,
    "query ApprovalPlan($id: String!) { deploymentApprovalRequirements(filters: { id: { eq: $id } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { deployments { plan { canonicalPlan } } } } }",
    { id: retriedRequirement.requirement.id });
  assert.equal(typeof approvalPlan.deploymentApprovalRequirements.nodes[0].deployments.plan.canonicalPlan, "object");
  // A principal with no `DEPLOYMENT_APPROVAL.VIEW` reads an empty connection, by row and by scope.
  const hidden = await graphql(service, outsider,
    `query Hidden($id: String!, $project: String!) { detail: deploymentApprovalRequirements(filters: { id: { eq: $id } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { id } } scoped: deploymentApprovalRequirements(filters: { projectId: { eq: $project } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { id } } }`,
    { id: retriedRequirement.requirement.id, project });
  assert.deepEqual(hidden.detail.nodes, []);
  assert.deepEqual(hidden.scoped.nodes, []);
  // An `AGENT_DEVELOPER` holds `DEPLOYMENT.VIEW` but not `DEPLOYMENT_APPROVAL.VIEW`.
  const developerInbox = await graphql(service, developerOnly,
    "query DeveloperInbox($organization: String!) { global: deploymentApprovalRequirements(pagination: { page: { limit: 1, page: 0 } }) { nodes { id } } organization: deploymentApprovalRequirements(filters: { organizationId: { eq: $organization } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { id } } }", { organization });
  assert.deepEqual(developerInbox.global.nodes, []);
  assert.deepEqual(developerInbox.organization.nodes, []);
  // A malformed identifier is a type-conversion error on the generated filter, not "no row". Either
  // way nothing is disclosed.
  const malformedScope = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(approverOne)}` },
    body: JSON.stringify({ query: "query MalformedScope { deploymentApprovalRequirements(filters: { projectId: { eq: \"invalid\" } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { id } } }" })
  });
  assert.equal(malformedScope.status, 200);
  const malformedScopeBody = await malformedScope.json();
  assert.equal(malformedScopeBody.errors?.length > 0, true);
  assert.equal(malformedScopeBody.data?.deploymentApprovalRequirements ?? null, null);
  // The requirement/decision relation is navigable in both directions, so the graph is bounded by
  // the schema's own depth limit rather than by a missing field. A request that walks past it is
  // refused before a single row is read.
  let recursiveDecisionSelection = "id";
  for (let depth = 0; depth < 8; depth += 1) {
    recursiveDecisionSelection = `deploymentApprovalDecisions { nodes { deploymentApprovalRequirements { ${recursiveDecisionSelection} } } }`;
  }
  const recursiveDecision = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(approverOne)}` },
    body: JSON.stringify({ query: `query RecursiveDecision($id: String!) { deploymentApprovalRequirements(filters: { id: { eq: $id } }) { nodes { ${recursiveDecisionSelection} } } }`, variables: { id: retriedRequirement.requirement.id } })
  });
  assert.equal(recursiveDecision.status, 200);
  assert.equal((await recursiveDecision.json()).errors?.length > 0, true);

  // M14 holds an approved handoff while only an M13 worker remains. A compatible worker later
  // releases the retained event. Product evaluation insertion remains outside this M14 fixture.
  await worker.stop();
  worker = undefined;
  await client.query("UPDATE deployment_worker_heartbeats SET approval_execution_compatible = FALSE, observed_at = CURRENT_TIMESTAMP - INTERVAL '1 minute'");
  await client.query(`INSERT INTO deployment_worker_heartbeats
      (worker_id, observed_at, last_batch_deliveries, pending_events, oldest_pending_at, state, failure_code, approval_execution_compatible)
    VALUES ('fixture-m13-legacy', CURRENT_TIMESTAMP, 0, 0, NULL, 'READY', NULL, FALSE)`);
  const fairnessWaitingSource = await request(service, highVersionId, development, "handoff-fairness-source", false);
  const fairnessWaitingRequirement = await requirementForDeployment(client, fairnessWaitingSource.id);
  const siblingProject = randomUUID();
  await client.query("INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status, revision) VALUES ($1, $2, $3, 'M14 sibling project', 'ACTIVE', 1)", [siblingProject, organization, `m14-sibling-${run}`]);
  // V027 records the project revision observed by each deployment insert. The retained archive
  // history must terminalize the pre-archive request without falsely terminalizing the request
  // inserted after an archive and restore in the same timestamp resolution.
  const revisionBoundaryProject = randomUUID();
  await client.query("INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status, revision) VALUES ($1, $2, $3, 'M14 archive revision boundary', 'ACTIVE', 1)", [revisionBoundaryProject, organization, `m14-revision-boundary-${run}`]);
  const revisionBoundaryBefore = randomUUID();
  const revisionBoundaryAfter = randomUUID();
  const insertRevisionBoundaryDeployment = async (deploymentId, suffix) => client.query(`INSERT INTO deployments (id, organization_id, project_id, agent_id, agent_version_id, catalog_release_id,
      catalog_release_digest, environment, target_digest, strategy, lifecycle_status, revision, idempotency_key, requested_by,
      requested_at, updated_at, environment_definition_version_id, request_fingerprint, projection_revision)
    SELECT $1, organization_id, $2, agent_id, agent_version_id, catalog_release_id, catalog_release_digest, environment,
      target_digest, strategy, lifecycle_status, revision, $3, requested_by, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP,
      environment_definition_version_id, request_fingerprint, projection_revision
    FROM deployments WHERE id = $4`, [deploymentId, revisionBoundaryProject, `m14-revision-boundary-${suffix}-${run}`, fairnessWaitingSource.id]);
  await insertRevisionBoundaryDeployment(revisionBoundaryBefore, "before");
  // No project_lifecycle_revision-stamped assertion here: nothing stamps that column on this raw
  // insert.
  // The boundary checks below still hold on requested_at's own timestamp ordering (these two raw
  // inserts are sequential, not concurrent, around a real archive/restore in between).
  await archiveProjectViaApi(service, revisionBoundaryProject);
  await restoreProjectViaApi(service, revisionBoundaryProject);
  await insertRevisionBoundaryDeployment(revisionBoundaryAfter, "after");
  // boundary below inlines the same revision-aware predicate deployment_archive_boundary() applies,
  // as a correlated EXISTS: Aurora DSQL rejects CREATE FUNCTION outright, so there is no SQL function
  // to call (see ensureRequirementViaSql()'s own comment).
  const revisionBoundary = await client.query(`SELECT deployment.id, deployment.lifecycle_status,
      EXISTS (
        SELECT 1 FROM deployment_approval_project_archive_events event
        WHERE event.project_id = deployment.project_id
          AND ((deployment.project_lifecycle_revision IS NOT NULL AND event.archived_project_revision IS NOT NULL
                  AND deployment.project_lifecycle_revision <= event.archived_project_revision)
              OR ((deployment.project_lifecycle_revision IS NULL OR event.archived_project_revision IS NULL)
                  AND deployment.requested_at <= event.archived_at))
      ) AS boundary
    FROM deployments deployment WHERE deployment.id = ANY($1::uuid[]) ORDER BY deployment.id`, [[revisionBoundaryBefore, revisionBoundaryAfter]]);
  const boundaryByDeployment = new Map(revisionBoundary.rows.map((row) => [row.id, row]));
  assert.equal(boundaryByDeployment.get(revisionBoundaryBefore).boundary, true);
  assert.equal(boundaryByDeployment.get(revisionBoundaryAfter).boundary, false);
  assert.notEqual(boundaryByDeployment.get(revisionBoundaryAfter).lifecycle_status, "CANCELED");
  // No project_lifecycle_revision immutability check here: nothing writes the column, so there is no
  // application-level operation for a guard to protect.
  // No archive-event-immutability assert.rejects() checks here either (identity/boundary facts staying
  // unchanged, delivery only transitioning outside reconciliation, no DELETE ever taking effect):
  // Aurora DSQL supports no triggers or rules, and all three guarantees hold by construction --
  // record_project_archive_event() only INSERTs, reconcile_project_archives() is the only UPDATE in
  // this codebase and only ever sets processed_at once, and nothing DELETEs.
  // An M13 deployment writer can persist its deployment before the policy snapshot. If an
  // archive/restore occurs during that interval, the delayed snapshot must create a terminal
  // archived request and cancel the retained deployment rather than leave it REQUESTED.
  const delayedPolicyProject = randomUUID();
  await client.query("INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status, revision) VALUES ($1, $2, $3, 'M14 delayed policy archive', 'ACTIVE', 1)", [delayedPolicyProject, organization, `m14-delayed-policy-${run}`]);
  const delayedPolicyDeployment = await legacyDeploymentBeforePolicy(fairnessWaitingSource.id, delayedPolicyProject, "archive");
  await archiveProjectViaApi(service, delayedPolicyProject);
  await restoreProjectViaApi(service, delayedPolicyProject);
  await client.query(`INSERT INTO deployment_policy_snapshots (deployment_id, policy_id, policy_revision, policy_digest, policy_matrix,
      logical_environment_class, risk, required_evidence, required_approvers, agent_version_id, environment_definition_version_id,
      target_digest, plan_digest, package_digest, binding_digest, evaluation_requirement_expires_at)
    SELECT $1, policy_id, policy_revision, policy_digest, policy_matrix, logical_environment_class, risk, required_evidence,
      required_approvers, agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest,
      binding_digest, evaluation_requirement_expires_at FROM deployment_policy_snapshots WHERE deployment_id = $2`,
    [delayedPolicyDeployment, fairnessWaitingSource.id]);
  // Nothing creates the requirement on the policy-snapshot INSERT above, so ensureRequirementViaSql()
  // derives it (see its own comment).
  await ensureRequirementViaSql(delayedPolicyDeployment);
  const delayedPolicyRequirement = await client.query("SELECT status, invalidation_code FROM deployment_approval_requirements WHERE deployment_id = $1", [delayedPolicyDeployment]);
  assert.deepEqual(delayedPolicyRequirement.rows[0], { status: "INVALIDATED", invalidation_code: "PROJECT_ARCHIVED" });
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [delayedPolicyDeployment])).rows[0].lifecycle_status, "CANCELED");
  assert.equal((await client.query("SELECT status FROM deployment_runtime_health WHERE deployment_id = $1", [delayedPolicyDeployment])).rows[0].status, "CANCELED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_handoff_releases WHERE deployment_id = $1", [delayedPolicyDeployment])).rows[0].count, 0);
  // Archive maintenance can pause between two archive/restore cycles. Each archive must append
  // its own revision boundary so the second archive still terminalizes the post-first-restore row.
  const repeatedArchiveProject = randomUUID();
  await client.query("INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status, revision) VALUES ($1, $2, $3, 'M14 repeated archive boundary', 'ACTIVE', 1)", [repeatedArchiveProject, organization, `m14-repeated-archive-${run}`]);
  await archiveProjectViaApi(service, repeatedArchiveProject);
  await restoreProjectViaApi(service, repeatedArchiveProject);
  // A zero-approver requirement can already be satisfied before the archive page runs. The
  // boundary must cancel its release without rewriting the immutable satisfied requirement.
  const zeroArchiveProject = randomUUID();
  await client.query("INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status, revision) VALUES ($1, $2, $3, 'M14 zero approval archive', 'ACTIVE', 1)", [zeroArchiveProject, organization, `m14-zero-archive-${run}`]);
  const zeroArchiveDeployment = await legacyDeploymentBeforePolicy(zeroEvaluation.id, zeroArchiveProject, "zero-archive");
  await client.query(`INSERT INTO deployment_policy_snapshots (deployment_id, policy_id, policy_revision, policy_digest, policy_matrix,
      logical_environment_class, risk, required_evidence, required_approvers, agent_version_id, environment_definition_version_id,
      target_digest, plan_digest, package_digest, binding_digest, evaluation_requirement_expires_at)
    SELECT $1, policy_id, policy_revision, policy_digest, policy_matrix, logical_environment_class, risk, required_evidence,
      required_approvers, agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest,
      binding_digest, evaluation_requirement_expires_at FROM deployment_policy_snapshots WHERE deployment_id = $2`,
    [zeroArchiveDeployment, zeroEvaluation.id]);
  // Same reasoning as delayedPolicyDeployment above.
  await ensureRequirementViaSql(zeroArchiveDeployment);
  await client.query(`INSERT INTO deployment_evidence_snapshots (id, deployment_id, evidence_kind, evidence_digest, expires_at,
      agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest, binding_digest)
    SELECT gen_random_uuid(), $1, evidence_kind, evidence_digest, expires_at, agent_version_id, environment_definition_version_id,
      target_digest, plan_digest, package_digest, binding_digest FROM deployment_evidence_snapshots WHERE deployment_id = $2`,
    [zeroArchiveDeployment, zeroEvaluation.id]);
  // Nothing reacts to the evidence copy above either, and this zero-approver requirement reaches
  // SATISFIED only once the handoff (not just ensureRequirementViaSql()) runs.
  await satisfyZeroApproverViaSql(zeroArchiveDeployment);
  const zeroArchiveRequirement = await requirementForDeployment(client, zeroArchiveDeployment);
  await waitForRequirementStatus(zeroArchiveRequirement, "SATISFIED");
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [zeroArchiveDeployment])).rows[0].lifecycle_status, "REQUESTED");
  await archiveProjectViaApi(service, zeroArchiveProject);
  const zeroArchiveEvent = (await client.query("SELECT id FROM deployment_approval_project_archive_events WHERE project_id = $1 ORDER BY archived_at DESC LIMIT 1", [zeroArchiveProject])).rows[0].id;
  await waitForArchiveReconciliation(zeroArchiveEvent);
  assert.equal((await client.query("SELECT status FROM deployment_approval_requirements WHERE id = $1", [zeroArchiveRequirement])).rows[0].status, "SATISFIED");
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [zeroArchiveDeployment])).rows[0].lifecycle_status, "CANCELED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_handoff_releases WHERE deployment_id = $1", [zeroArchiveDeployment])).rows[0].count, 0);
  const repeatedArchiveDeployment = randomUUID();
  await client.query(`INSERT INTO deployments (id, organization_id, project_id, agent_id, agent_version_id, catalog_release_id,
      catalog_release_digest, environment, target_digest, strategy, lifecycle_status, revision, idempotency_key, requested_by,
      requested_at, updated_at, environment_definition_version_id, request_fingerprint, projection_revision)
    SELECT $1, organization_id, $2, agent_id, agent_version_id, catalog_release_id, catalog_release_digest, environment,
      target_digest, strategy, lifecycle_status, revision, $3, requested_by, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP,
      environment_definition_version_id, request_fingerprint, projection_revision
    FROM deployments WHERE id = $4`, [repeatedArchiveDeployment, repeatedArchiveProject, `m14-repeated-archive-deployment-${run}`, fairnessWaitingSource.id]);
  const repeatedArchiveRequirement = randomUUID();
  await client.query(`INSERT INTO deployment_approval_requirements (id, deployment_id, revision, organization_id, project_id, requested_at,
      required_approvers, status, expires_at, satisfied_at, rejected_at, invalidated_at, invalidation_code, satisfied_participants)
    SELECT $1, $2, revision, organization_id, $3, requested_at, required_approvers, status, expires_at, satisfied_at,
      rejected_at, invalidated_at, invalidation_code, satisfied_participants
    FROM deployment_approval_requirements WHERE id = $4`, [repeatedArchiveRequirement, repeatedArchiveDeployment, repeatedArchiveProject, fairnessWaitingRequirement]);
  await archiveProjectViaApi(service, repeatedArchiveProject);
  const repeatedArchiveEvents = await client.query("SELECT id, archived_project_revision FROM deployment_approval_project_archive_events WHERE project_id = $1 ORDER BY archived_project_revision", [repeatedArchiveProject]);
  assert.deepEqual(repeatedArchiveEvents.rows.map((row) => row.archived_project_revision), ["2", "4"]);
  await waitForArchiveReconciliation(repeatedArchiveEvents.rows.at(-1).id);
  assert.deepEqual((await client.query("SELECT status, invalidation_code FROM deployment_approval_requirements WHERE id = $1", [repeatedArchiveRequirement])).rows[0], { status: "INVALIDATED", invalidation_code: "PROJECT_ARCHIVED" });
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [repeatedArchiveDeployment])).rows[0].lifecycle_status, "CANCELED");
  await restoreProjectViaApi(service, repeatedArchiveProject);
  const siblingDeployment = randomUUID();
  await client.query(`INSERT INTO deployments (id, organization_id, project_id, agent_id, agent_version_id, catalog_release_id,
      catalog_release_digest, environment, target_digest, strategy, lifecycle_status, revision, idempotency_key, requested_by,
      requested_at, updated_at, environment_definition_version_id, request_fingerprint, projection_revision)
    SELECT $1, organization_id, $2, agent_id, agent_version_id, catalog_release_id, catalog_release_digest, environment,
      target_digest, strategy, lifecycle_status, revision, $3, requested_by, requested_at, updated_at,
      environment_definition_version_id, request_fingerprint, projection_revision
    FROM deployments WHERE id = $4`, [siblingDeployment, siblingProject, `m14-sibling-${run}`, fairnessWaitingSource.id]);
  // The deployment and requirement read queries inner-join deployment_plan_versions,
  // deployment_plan_review_facts, deployment_policy_snapshots, and deployment_runtime_health -- a
  // requirement whose deployment lacks any one of them is silently dropped from any inbox page that
  // reaches it. Clone all four from fairnessWaitingSource, matching legacyDeploymentFrom()'s own
  // established pattern, so siblingRequirement resolves through the real approvalInbox API, not only
  // through a direct visibility-function call.
  const siblingPlanId = randomUUID();
  await client.query(`INSERT INTO deployment_plan_versions (id, deployment_id, version_number, agent_version_id, catalog_release_id,
      environment, environment_definition_version_id, agent_content_digest, catalog_release_digest, target_digest, compiler_version,
      canonical_plan, plan_digest, package_digest, package_reference, created_by)
    SELECT $1, $2, version_number, agent_version_id, catalog_release_id, environment, environment_definition_version_id,
      agent_content_digest, catalog_release_digest, target_digest, compiler_version, canonical_plan, plan_digest, package_digest,
      package_reference, created_by FROM deployment_plan_versions WHERE deployment_id = $3 AND version_number = 1`,
    [siblingPlanId, siblingDeployment, fairnessWaitingSource.id]);
  await client.query(`INSERT INTO deployment_plan_review_facts (plan_id, active_agent_version_number, change_summary,
      requested_dependency_versions, added_dependency_versions, removed_dependency_versions)
    SELECT $1, active_agent_version_number, change_summary, requested_dependency_versions, added_dependency_versions,
      removed_dependency_versions
    FROM deployment_plan_review_facts review JOIN deployment_plan_versions plan ON plan.id = review.plan_id
    WHERE plan.deployment_id = $2 AND plan.version_number = 1
      AND NOT EXISTS (SELECT 1 FROM deployment_plan_review_facts WHERE plan_id = $1)`, [siblingPlanId, fairnessWaitingSource.id]);
  await client.query(`INSERT INTO deployment_policy_snapshots (deployment_id, policy_id, policy_revision, policy_digest, policy_matrix,
      logical_environment_class, risk, required_evidence, required_approvers, agent_version_id, environment_definition_version_id,
      target_digest, plan_digest, package_digest, binding_digest, evaluation_requirement_expires_at)
    SELECT $1, policy_id, policy_revision, policy_digest, policy_matrix, logical_environment_class, risk, required_evidence,
      required_approvers, agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest,
      binding_digest, evaluation_requirement_expires_at FROM deployment_policy_snapshots WHERE deployment_id = $2`,
    [siblingDeployment, fairnessWaitingSource.id]);
  await client.query("INSERT INTO deployment_runtime_health (deployment_id, status, summary, generation) VALUES ($1, 'NOT_OBSERVED', 'No local runtime observation is available yet.', 1)", [siblingDeployment]);
  const siblingRequirement = randomUUID();
  await client.query(`INSERT INTO deployment_approval_requirements (id, deployment_id, revision, organization_id, project_id, requested_at,
      required_approvers, status, expires_at, satisfied_at, rejected_at, invalidated_at, invalidation_code, satisfied_participants)
    SELECT $1, $2, revision, organization_id, $3, requested_at, required_approvers, status, expires_at, satisfied_at,
      rejected_at, invalidated_at, invalidation_code, satisfied_participants
    FROM deployment_approval_requirements WHERE id = $4`, [siblingRequirement, siblingDeployment, siblingProject, fairnessWaitingRequirement]);
  const viewerOrganizationMembership = await client.query("SELECT id FROM organization_memberships WHERE principal_id = $1 AND organization_id = $2", [viewer, organization]);
  await client.query("INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_ADMIN')", [viewerOrganizationMembership.rows[0].id]);
  // deployment_approval_visible_requirements() is removed (Aurora DSQL rejects CREATE FUNCTION
  // outright); this scoping check goes through the generated `deploymentApprovalRequirements` query
  // instead of raw SQL -- exercising the actual production path rather than a hand-copied replica.
  const projectScopedRequirements = await graphql(service, viewer,
    inboxQuery("ProjectScopedRequirements", "id", `projectId: { eq: "${project}" }`), { limit: 50, page: 0 });
  const projectScopedIds = projectScopedRequirements.deploymentApprovalRequirements.nodes.map((node) => node.id);
  assert(projectScopedIds.includes(fairnessWaitingRequirement));
  assert.equal(projectScopedIds.includes(siblingRequirement), false);
  const organizationScopedRequirements = await graphql(service, viewer,
    inboxQuery("OrganizationScopedRequirements", "id", `organizationId: { eq: "${organization}" }`), { limit: 50, page: 0 });
  const organizationScopedIds = organizationScopedRequirements.deploymentApprovalRequirements.nodes.map((node) => node.id);
  assert(organizationScopedIds.includes(fairnessWaitingRequirement));
  assert(organizationScopedIds.includes(siblingRequirement));
  // Terminal history does not belong in the bounded archive transition candidate scan. This
  // retained-history fixture asserts the partial live-cycle index before its project archives.
  const archiveScaleProject = randomUUID();
  await client.query("INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status, revision) VALUES ($1, $2, $3, 'M14 archive retained history', 'ACTIVE', 1)", [archiveScaleProject, organization, `m14-archive-scale-${run}`]);
  await client.query(`INSERT INTO deployments (id, organization_id, project_id, agent_id, agent_version_id, catalog_release_id,
      catalog_release_digest, environment, target_digest, strategy, lifecycle_status, revision, idempotency_key, requested_by,
      requested_at, updated_at, environment_definition_version_id, request_fingerprint, projection_revision)
    SELECT md5('m14-archive-scale-deployment-' || $1 || '-' || source.value)::uuid, deployment.organization_id, $2,
      deployment.agent_id, deployment.agent_version_id, deployment.catalog_release_id, deployment.catalog_release_digest,
      deployment.environment, deployment.target_digest, deployment.strategy, 'CANCELED', 1,
      'm14-archive-scale-' || $1 || '-' || source.value, deployment.requested_by,
      CURRENT_TIMESTAMP - INTERVAL '1 hour', CURRENT_TIMESTAMP, deployment.environment_definition_version_id,
      deployment.request_fingerprint, 1
    FROM deployments deployment CROSS JOIN generate_series(1, 10000) source(value) WHERE deployment.id = $3`,
    [run, archiveScaleProject, fairnessWaitingSource.id]);
  await client.query(`INSERT INTO deployment_approval_requirements (id, deployment_id, revision, organization_id, project_id,
      requested_at, required_approvers, status, expires_at, invalidated_at, invalidation_code, satisfied_participants)
    SELECT md5('m14-archive-scale-requirement-' || deployment.id::text)::uuid, deployment.id, 1, deployment.organization_id,
      deployment.project_id, deployment.requested_at, source.required_approvers, 'INVALIDATED',
      CURRENT_TIMESTAMP + INTERVAL '1 day', CURRENT_TIMESTAMP, 'TERMINAL_LIFECYCLE', '[]'::jsonb
    FROM deployments deployment JOIN deployment_approval_requirements source ON source.id = $2
    WHERE deployment.project_id = $1`, [archiveScaleProject, fairnessWaitingRequirement]);
  await archiveProjectViaApi(service, archiveScaleProject);
  const archiveScaleEvent = (await client.query("SELECT id FROM deployment_approval_project_archive_events WHERE project_id = $1", [archiveScaleProject])).rows[0].id;
  await client.query("ANALYZE deployments");
  await client.query("ANALYZE deployment_approval_requirements");
  await client.query("SET enable_seqscan = off");
  // This repeats reconcile_project_archives()'s candidate-fetch query exactly, not just its boundary
  // check: Aurora DSQL rejects CREATE FUNCTION outright, so that query expresses the archive-event
  // boundary as a JOIN on this same event's id rather than a scalar function call. Matching it here
  // means this plan check exercises the real production query shape, not an approximation.
  const archiveScalePlan = await client.query(`EXPLAIN (ANALYZE, BUFFERS)
    SELECT requirement.id
    FROM deployments deployment
      JOIN deployment_approval_requirements requirement ON requirement.deployment_id = deployment.id
      JOIN deployment_approval_project_archive_events archive_event
        ON archive_event.id = $2 AND archive_event.project_id = deployment.project_id
    WHERE deployment.project_id = $1
      AND ((deployment.project_lifecycle_revision IS NOT NULL AND archive_event.archived_project_revision IS NOT NULL
              AND deployment.project_lifecycle_revision <= archive_event.archived_project_revision)
          OR ((deployment.project_lifecycle_revision IS NULL OR archive_event.archived_project_revision IS NULL)
              AND deployment.requested_at <= archive_event.archived_at))
      AND ((deployment.lifecycle_status IN ('AWAITING_APPROVAL', 'REQUESTED') AND requirement.status = 'PENDING')
        OR (deployment.lifecycle_status IN ('APPROVED', 'REQUESTED') AND requirement.status = 'SATISFIED'))
      ORDER BY deployment.id ASC LIMIT 50`, [archiveScaleProject, archiveScaleEvent]);
  await client.query("RESET enable_seqscan");
  assert(archiveScalePlan.rows.length > 0);
  await waitForArchiveReconciliation(archiveScaleEvent);
  const highCardinalityOrganizationMembership = randomUUID();
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL)", [highCardinalityOrganizationMembership, organization, outsider]);
  await client.query("INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_MEMBER')", [highCardinalityOrganizationMembership]);
  const insertAuthorizedProjects = async (first, last) => {
    await client.query(`INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status, revision)
      SELECT md5('m14-inbox-project-' || $1 || '-' || source.value)::uuid, $2,
        'm14-inbox-project-' || $1 || '-' || source.value, 'M14 inbox authority fixture', 'ACTIVE', 1
      FROM generate_series($3::integer, $4::integer) source(value)`, [run, organization, first, last]);
    await client.query(`INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision)
      SELECT md5('m14-inbox-membership-' || $1 || '-' || source.value)::uuid,
        md5('m14-inbox-project-' || $1 || '-' || source.value)::uuid, $2, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1
      FROM generate_series($3::integer, $4::integer) source(value)`, [run, outsider, first, last]);
    await client.query(`INSERT INTO project_membership_roles (membership_id, role_code)
      SELECT md5('m14-inbox-membership-' || $1 || '-' || source.value)::uuid, 'DEPLOYMENT_APPROVER'
      FROM generate_series($2::integer, $3::integer) source(value)`, [run, first, last]);
  };
  const assertHighCardinalityPage = async (visibleRequirement) => {
    const plan = await client.query(`EXPLAIN (ANALYZE, BUFFERS)
      SELECT requirement.id FROM deployment_approval_principal_project_scopes scope
      JOIN deployment_approval_requirements requirement ON requirement.project_id = scope.project_id
      WHERE scope.principal_id = $1::uuid ORDER BY requirement.requested_at DESC, requirement.id DESC LIMIT 51`, [outsider]);
    assert(plan.rows.some((row) => row["QUERY PLAN"].includes("deployment_approval_principal_project_scopes")));
    // deployment_approval_visible_requirements() is removed (Aurora DSQL rejects CREATE FUNCTION
    // outright); pagination and scoping are exercised through the generated
    // `deploymentApprovalRequirements` query, which pages by page number, not by cursor.
    const page = await graphql(service, outsider,
      inboxQuery("HighCardinalityPage", "id"), { limit: 50, page: 0 });
    const pageIds = page.deploymentApprovalRequirements.nodes.map((node) => node.id);
    assert(pageIds.length <= 50);
    assert(pageIds.includes(visibleRequirement));
    assert.equal(pageIds.includes(siblingRequirement), false);
    assert.equal(new Set(pageIds).size, pageIds.length);
    const nextPage = await graphql(service, outsider,
      inboxQuery("HighCardinalityNextPage", "id"), { limit: 50, page: 1 });
    const nextPageIds = nextPage.deploymentApprovalRequirements.nodes.map((node) => node.id);
    assert.equal(nextPageIds.includes(visibleRequirement), false);
  };
  await insertAuthorizedProjects(1, 10_000);
  const highCardinalityProject = (await client.query("SELECT md5('m14-inbox-project-' || $1 || '-1')::uuid AS id", [run])).rows[0].id;
  const highCardinalityDeployment = randomUUID();
  await client.query(`INSERT INTO deployments (id, organization_id, project_id, agent_id, agent_version_id, catalog_release_id,
      catalog_release_digest, environment, target_digest, strategy, lifecycle_status, revision, idempotency_key, requested_by,
      requested_at, updated_at, environment_definition_version_id, request_fingerprint, projection_revision)
    SELECT $1, organization_id, $2, agent_id, agent_version_id, catalog_release_id, catalog_release_digest, environment,
      target_digest, strategy, lifecycle_status, revision, $3, requested_by, requested_at, updated_at,
      environment_definition_version_id, request_fingerprint, projection_revision
    FROM deployments WHERE id = $4`, [highCardinalityDeployment, highCardinalityProject, `m14-high-cardinality-${run}`, fairnessWaitingSource.id]);
  // See siblingDeployment's own comment above: deployments()/rawRequirements() inner-join all four of
  // these tables, so highCardinalityRequirement needs the same complete clone to resolve through the
  // real approvalInbox API.
  const highCardinalityPlanId = randomUUID();
  await client.query(`INSERT INTO deployment_plan_versions (id, deployment_id, version_number, agent_version_id, catalog_release_id,
      environment, environment_definition_version_id, agent_content_digest, catalog_release_digest, target_digest, compiler_version,
      canonical_plan, plan_digest, package_digest, package_reference, created_by)
    SELECT $1, $2, version_number, agent_version_id, catalog_release_id, environment, environment_definition_version_id,
      agent_content_digest, catalog_release_digest, target_digest, compiler_version, canonical_plan, plan_digest, package_digest,
      package_reference, created_by FROM deployment_plan_versions WHERE deployment_id = $3 AND version_number = 1`,
    [highCardinalityPlanId, highCardinalityDeployment, fairnessWaitingSource.id]);
  await client.query(`INSERT INTO deployment_plan_review_facts (plan_id, active_agent_version_number, change_summary,
      requested_dependency_versions, added_dependency_versions, removed_dependency_versions)
    SELECT $1, active_agent_version_number, change_summary, requested_dependency_versions, added_dependency_versions,
      removed_dependency_versions
    FROM deployment_plan_review_facts review JOIN deployment_plan_versions plan ON plan.id = review.plan_id
    WHERE plan.deployment_id = $2 AND plan.version_number = 1
      AND NOT EXISTS (SELECT 1 FROM deployment_plan_review_facts WHERE plan_id = $1)`, [highCardinalityPlanId, fairnessWaitingSource.id]);
  await client.query(`INSERT INTO deployment_policy_snapshots (deployment_id, policy_id, policy_revision, policy_digest, policy_matrix,
      logical_environment_class, risk, required_evidence, required_approvers, agent_version_id, environment_definition_version_id,
      target_digest, plan_digest, package_digest, binding_digest, evaluation_requirement_expires_at)
    SELECT $1, policy_id, policy_revision, policy_digest, policy_matrix, logical_environment_class, risk, required_evidence,
      required_approvers, agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest,
      binding_digest, evaluation_requirement_expires_at FROM deployment_policy_snapshots WHERE deployment_id = $2`,
    [highCardinalityDeployment, fairnessWaitingSource.id]);
  await client.query("INSERT INTO deployment_runtime_health (deployment_id, status, summary, generation) VALUES ($1, 'NOT_OBSERVED', 'No local runtime observation is available yet.', 1)", [highCardinalityDeployment]);
  const highCardinalityRequirement = randomUUID();
  await client.query(`INSERT INTO deployment_approval_requirements (id, deployment_id, revision, organization_id, project_id, requested_at,
      required_approvers, status, expires_at, satisfied_at, rejected_at, invalidated_at, invalidation_code, satisfied_participants)
    SELECT $1, $2, revision, organization_id, $3, requested_at, required_approvers, status, expires_at, satisfied_at,
      rejected_at, invalidated_at, invalidation_code, satisfied_participants
    FROM deployment_approval_requirements WHERE id = $4`, [highCardinalityRequirement, highCardinalityDeployment, highCardinalityProject, fairnessWaitingRequirement]);
  await assertHighCardinalityPage(highCardinalityRequirement);
  await insertAuthorizedProjects(10_001, 100_000);
  await assertHighCardinalityPage(highCardinalityRequirement);
  await client.query("UPDATE organization_memberships SET ended_at = CURRENT_TIMESTAMP WHERE id = $1", [highCardinalityOrganizationMembership]);
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_principal_organization_membership_scopes WHERE principal_id = $1 AND organization_id = $2", [outsider, organization])).rows[0].count, 0);
  // Ending outsider's only organization membership also invalidates every one of its 100,000 project
  // memberships' own active-organization-membership requirement: deployment_approval_capabilities()
  // counts a project role only while the owning organization membership is active, so the generated
  // list is empty and highCardinalityRequirement is not visible.
  const revokedScopePage = await graphql(service, outsider,
    inboxQuery("RevokedScopePage", "id"), { limit: 50, page: 0 });
  const revokedScopeIds = revokedScopePage.deploymentApprovalRequirements.nodes.map((node) => node.id);
  assert.equal(revokedScopeIds.includes(highCardinalityRequirement), false);
  const [futureViewer, reassignmentSource, reassignmentTarget] = [randomUUID(), randomUUID(), randomUUID()];
  for (const principal of [futureViewer, reassignmentSource, reassignmentTarget]) {
    await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, $2, $2, $2 || '@local.invalid')", [principal, `m14-scope-${principal}`]);
  }
  const futureOrganizationMembership = randomUUID();
  const futureProjectMembership = randomUUID();
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP + INTERVAL '1 hour', NULL)", [futureOrganizationMembership, organization, futureViewer]);
  await client.query("INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_MEMBER')", [futureOrganizationMembership]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP + INTERVAL '1 hour', NULL, 1)", [futureProjectMembership, highCardinalityProject, futureViewer]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'DEPLOYMENT_APPROVER')", [futureProjectMembership]);
  // Nothing refreshes this cache row on the raw INSERT above: refresh_membership_scope() and
  // refresh_role_scope() fire only for writes through the persistence layer, and no mutation path can
  // construct a future-started membership at all (a real membership write always uses
  // CURRENT_TIMESTAMP). So this fixture replicates the scope refresh's exact DELETE and INSERT
  // directly, the same way any writer capable of reaching this state would have to.
  await client.query("DELETE FROM deployment_approval_principal_project_scopes WHERE principal_id = $1 AND project_id = $2", [futureViewer, highCardinalityProject]);
  await client.query(`INSERT INTO deployment_approval_principal_project_scopes (principal_id, project_id, valid_after)
    SELECT $1, $2, MIN(GREATEST(membership.started_at, organization_membership.started_at))
    FROM project_memberships membership
      JOIN project_membership_roles role ON role.membership_id = membership.id
      JOIN projects project ON project.id = membership.project_id
      JOIN organization_memberships organization_membership
        ON organization_membership.organization_id = project.organization_id AND organization_membership.principal_id = $1
    WHERE membership.principal_id = $1 AND membership.project_id = $2 AND membership.ended_at IS NULL
      AND organization_membership.ended_at IS NULL AND role.role_code IN ('PROJECT_ADMIN', 'DEPLOYMENT_APPROVER', 'AUDITOR')
    GROUP BY membership.principal_id, membership.project_id`, [futureViewer, highCardinalityProject]);
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_principal_project_scopes WHERE principal_id = $1 AND project_id = $2 AND valid_after > CURRENT_TIMESTAMP", [futureViewer, highCardinalityProject])).rows[0].count, 1);
  // deployment_approval_visible_requirements() is removed -- see the earlier removal comment in this
  // file. futureViewer's only memberships are both future-dated (started_at > CURRENT_TIMESTAMP), so
  // no membership is active yet, so the generated list is empty, matching this file's established
  // "no scope" pattern (e.g. the StaleScope/Hidden checks above).
  const futureViewerInbox = await graphql(service, futureViewer,
    inboxQuery("FutureViewerInbox", "id"), { limit: 50, page: 0 });
  assert.deepEqual(futureViewerInbox.deploymentApprovalRequirements.nodes, []);
  const reassignmentMemberships = [];
  for (const principal of [reassignmentSource, reassignmentTarget]) {
    const organizationMembership = randomUUID();
    const projectMembership = randomUUID();
    await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL)", [organizationMembership, organization, principal]);
    await client.query("INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_MEMBER')", [organizationMembership]);
    await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1)", [projectMembership, highCardinalityProject, principal]);
    reassignmentMemberships.push(projectMembership);
  }
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'DEPLOYMENT_APPROVER')", [reassignmentMemberships[0]]);
  await client.query("UPDATE project_membership_roles SET membership_id = $1 WHERE membership_id = $2 AND role_code = 'DEPLOYMENT_APPROVER'", [reassignmentMemberships[1], reassignmentMemberships[0]]);
  // Nothing refreshes either affected principal's scope row on this raw UPDATE. Same reasoning as
  // futureViewer above: no mutation path reassigns a role's membership_id directly (a real membership
  // write replaces the role list of one membership), so this fixture replicates the scope refresh for
  // both principals directly.
  for (const principal of [reassignmentSource, reassignmentTarget]) {
    await client.query("DELETE FROM deployment_approval_principal_project_scopes WHERE principal_id = $1 AND project_id = $2", [principal, highCardinalityProject]);
    await client.query(`INSERT INTO deployment_approval_principal_project_scopes (principal_id, project_id, valid_after)
      SELECT $1, $2, MIN(GREATEST(membership.started_at, organization_membership.started_at))
      FROM project_memberships membership
        JOIN project_membership_roles role ON role.membership_id = membership.id
        JOIN projects project ON project.id = membership.project_id
        JOIN organization_memberships organization_membership
          ON organization_membership.organization_id = project.organization_id AND organization_membership.principal_id = $1
      WHERE membership.principal_id = $1 AND membership.project_id = $2 AND membership.ended_at IS NULL
        AND organization_membership.ended_at IS NULL AND role.role_code IN ('PROJECT_ADMIN', 'DEPLOYMENT_APPROVER', 'AUDITOR')
      GROUP BY membership.principal_id, membership.project_id`, [principal, highCardinalityProject]);
  }
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_principal_project_scopes WHERE principal_id = $1 AND project_id = $2", [reassignmentSource, highCardinalityProject])).rows[0].count, 0);
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_principal_project_scopes WHERE principal_id = $1 AND project_id = $2 AND valid_after <= CURRENT_TIMESTAMP", [reassignmentTarget, highCardinalityProject])).rows[0].count, 1);
  for (let index = 0; index < 50; index += 1) await legacyDeploymentFrom(fairnessWaitingSource.id, `handoff-fairness-${index}`);
  assert.equal((await client.query(`SELECT count(*)::int AS count FROM deployment_approval_requirements requirement
      JOIN deployments deployment ON deployment.id = requirement.deployment_id
      WHERE deployment.lifecycle_status = 'REQUESTED' AND requirement.status = 'PENDING' AND requirement.required_approvers = 0`)).rows[0].count >= 50, true);
  const legacyZeroDeployment = randomUUID();
  await legacyDeploymentFrom(zeroEvaluation.id, "zero-handoff", legacyZeroDeployment);
  assert.equal((await client.query("INSERT INTO deployment_outbox_events (id, deployment_id, event_type, payload, status) VALUES (gen_random_uuid(), $1, 'EXECUTE_DEPLOYMENT', '{\"mode\":\"SUCCESS\"}'::jsonb, 'PENDING')", [legacyZeroDeployment])).rowCount, 1);
  const legacyZeroRequirement = await requirementForDeployment(client, legacyZeroDeployment);
  assert.equal((await client.query("SELECT status FROM deployment_approval_requirements WHERE id = $1", [legacyZeroRequirement])).rows[0].status, "SATISFIED");
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [legacyZeroDeployment])).rows[0].lifecycle_status, "REQUESTED");
  await client.query("UPDATE deployment_outbox_events SET status = 'PROCESSING', claimed_at = CURRENT_TIMESTAMP, claimed_by = 'fixture-m13-zero-worker' WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [legacyZeroDeployment]);
  await client.query("UPDATE deployments SET lifecycle_status = 'IN_PROGRESS', revision = revision + 1 WHERE id = $1", [legacyZeroDeployment]);
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [legacyZeroDeployment])).rows[0].lifecycle_status, "IN_PROGRESS");
  await client.query("UPDATE deployments SET lifecycle_status = 'ACTIVE', revision = revision + 1 WHERE id = $1", [legacyZeroDeployment]);
  const deferredZeroDeployment = randomUUID();
  await legacyDeploymentFrom(zeroEvaluation.id, "zero-handoff-release", deferredZeroDeployment);
  const deferredZeroRequirement = await requirementForDeployment(client, deferredZeroDeployment);
  const handoffDropFunction = `m14_drop_handoff_${run}`;
  const handoffDropTrigger = `m14_drop_handoff_trigger_${run}`;
  await client.query(`INSERT INTO deployment_worker_heartbeats
      (worker_id, observed_at, last_batch_deliveries, pending_events, oldest_pending_at, state, failure_code, approval_execution_compatible)
    VALUES ('fixture-m14-handoff-race', CURRENT_TIMESTAMP, 0, 0, NULL, 'READY', NULL, TRUE)`);
  await client.query(`CREATE FUNCTION ${handoffDropFunction}() RETURNS trigger LANGUAGE plpgsql AS $$
    BEGIN IF NEW.deployment_id = '${deferredZeroDeployment}'::uuid THEN RETURN NULL; END IF; RETURN NEW; END;
  $$`);
  await client.query(`CREATE TRIGGER ${handoffDropTrigger} BEFORE INSERT ON deployment_outbox_events
    FOR EACH ROW EXECUTE FUNCTION ${handoffDropFunction}()`);
  try {
    assert.equal((await approval(service, approverOne, deferredZeroRequirement)).requirement.status, "SATISFIED");
  } finally {
    await client.query(`DROP TRIGGER IF EXISTS ${handoffDropTrigger} ON deployment_outbox_events`);
    await client.query(`DROP FUNCTION IF EXISTS ${handoffDropFunction}()`);
    await client.query("UPDATE deployment_worker_heartbeats SET approval_execution_compatible = FALSE WHERE worker_id = 'fixture-m14-handoff-race'");
  }
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [deferredZeroDeployment])).rows[0].lifecycle_status, "REQUESTED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_handoff_releases WHERE deployment_id = $1", [deferredZeroDeployment])).rows[0].count, 1);
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_outbox_events WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [deferredZeroDeployment])).rows[0].count, 0);
  const heldHandoffHealth = await fetch(endpoint.replace("/graphql", "/health/deployment-worker"));
  assert.equal(heldHandoffHealth.status, 503);
  const heldHandoffBody = await heldHandoffHealth.json();
  assert.equal(heldHandoffBody.pendingApprovalHandoffs >= 1, true);
  assert.equal(typeof heldHandoffBody.oldestApprovalHandoffAt, "string");
  worker = await startLocalDeploymentWorker(database.name, {
    HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "20",
    HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "20"
  });
  await waitForLifecycle(service, deferredZeroDeployment, "ACTIVE");
  // Renaming deployment_approval_requirements fault-injects a maintenance failure:
  // expired_approval_requirement_deployments() reads that table unconditionally on every
  // expiry-reconciliation pass (reconcile_approval_expiry(), the API-owned maintenance), so the rename
  // deterministically fails the pass this scenario exercises.
  await client.query("ALTER TABLE deployment_approval_requirements RENAME TO deployment_approval_requirements_fault");
  try {
    const failureStarted = Date.now();
    let maintenanceFailed = false;
    while (Date.now() - failureStarted < 5_000) {
      const heartbeat = await client.query("SELECT state, failure_code FROM deployment_worker_heartbeats WHERE approval_execution_compatible ORDER BY observed_at DESC LIMIT 1");
      if (heartbeat.rows[0]?.state === "DEGRADED" && heartbeat.rows[0]?.failure_code === "APPROVAL_MAINTENANCE_FAILED") {
        maintenanceFailed = true;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.equal(maintenanceFailed, true);
    const maintenanceHealth = await fetch(endpoint.replace("/graphql", "/health/deployment-worker"));
    assert.equal(maintenanceHealth.status, 503);
    assert.equal((await maintenanceHealth.json()).status, "DEGRADED");
    let apiMaintenanceHealth;
    let apiMaintenanceBody;
    const apiFailureStarted = Date.now();
    while (Date.now() - apiFailureStarted < 5_000) {
      const response = await fetch(endpoint.replace("/graphql", "/health"));
      const body = await response.json();
      if (response.status === 503
          && /^(SQLSTATE_[A-Z0-9]{5}|DATABASE_FAILURE)$/.test(body.approvalMaintenanceFailureCode ?? "")) {
        apiMaintenanceHealth = response;
        apiMaintenanceBody = body;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert(apiMaintenanceHealth, "The API health projection must retain the expiry maintenance failure.");
    assert.equal(apiMaintenanceHealth.status, 503);
    assert.equal(apiMaintenanceBody.status, "degraded");
    assert.match(apiMaintenanceBody.approvalMaintenanceFailureCode, /^(SQLSTATE_[A-Z0-9]{5}|DATABASE_FAILURE)$/);
  } finally {
    await client.query("ALTER TABLE deployment_approval_requirements_fault RENAME TO deployment_approval_requirements");
  }
  const recoveredStarted = Date.now();
  let maintenanceRecovered = false;
  while (Date.now() - recoveredStarted < 5_000) {
    const heartbeat = await client.query("SELECT state, failure_code FROM deployment_worker_heartbeats WHERE approval_execution_compatible ORDER BY observed_at DESC LIMIT 1");
    if (heartbeat.rows[0]?.state === "READY" && heartbeat.rows[0]?.failure_code === null) {
      maintenanceRecovered = true;
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert.equal(maintenanceRecovered, true);
  let recoveredApiMaintenanceHealth;
  const recoveredApiStarted = Date.now();
  while (Date.now() - recoveredApiStarted < 5_000) {
    const response = await fetch(endpoint.replace("/graphql", "/health"));
    if (response.status === 200 && (await response.clone().json()).approvalMaintenance === "ok") {
      recoveredApiMaintenanceHealth = response;
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert(recoveredApiMaintenanceHealth, "The API health projection must observe the recovered compatibility maintenance.");
  assert.equal(recoveredApiMaintenanceHealth.status, 200);
  assert.equal((await recoveredApiMaintenanceHealth.json()).approvalMaintenance, "ok");
  // The API-owned archive pass runs independently of expiry. A successful expiry pass must not
  // overwrite an archive reconciliation failure in the aggregate health response.
  await client.query("ALTER TABLE deployment_approval_project_archive_events RENAME TO deployment_approval_project_archive_events_fault");
  try {
    const archiveFailureStarted = Date.now();
    let archiveHealth;
    while (Date.now() - archiveFailureStarted < 7_500) {
      const response = await fetch(endpoint.replace("/graphql", "/health"));
      const body = await response.json();
      if (response.status === 503 && body.approvalUpgradeMaintenanceFailureCode?.startsWith("ARCHIVE_RECONCILIATION_")) {
        archiveHealth = body;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert(archiveHealth, "Archive reconciliation failure must remain visible through expiry maintenance.");
    assert.equal(archiveHealth.approvalMaintenance, "failed");
    assert.equal(archiveHealth.approvalUpgradeMaintenance, "failed");
  } finally {
    await client.query("ALTER TABLE deployment_approval_project_archive_events_fault RENAME TO deployment_approval_project_archive_events");
  }
  const archiveRecoveryStarted = Date.now();
  let archiveRecovered = false;
  while (Date.now() - archiveRecoveryStarted < 5_000) {
    const response = await fetch(endpoint.replace("/graphql", "/health"));
    const body = await response.json();
    if (response.status === 200 && body.approvalUpgradeMaintenance === "ok") {
      archiveRecovered = true;
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert.equal(archiveRecovered, true);
  await worker.stop();
  worker = undefined;
  await client.query("UPDATE deployment_worker_heartbeats SET approval_execution_compatible = FALSE, observed_at = CURRENT_TIMESTAMP - INTERVAL '1 minute'");
  await client.query("UPDATE deployment_worker_heartbeats SET observed_at = CURRENT_TIMESTAMP, state = 'READY', approval_execution_compatible = FALSE WHERE worker_id = 'fixture-m13-legacy'");
  // No "a worker holds a preclaimed event before the second approval commits, the lifecycle guard
  // rejects it" scenario here: Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, so
  // there is no outbox gate, worker gate or claim gate in the database, and
  // compatible_approval_worker() rechecks the exact claimant after locking the deployment, which
  // covers the write path. preclaimedLegacy below needs no simulated pre-claim: once both decisions
  // land, automatic_approval_handoff() finds no compatible worker ready (every heartbeat above is
  // FALSE) and defers, the same as it would for a real worker-startup race.
  const preclaimedHighVersionId = await publishHighRiskVersion(service, base.agentId, base.document, "\n# legacy claim guard");
  const preclaimedLegacy = await request(service, preclaimedHighVersionId, production, "preclaimed-legacy-worker", true, workerSafetyRequester);
  const preclaimedRequirement = await requirementForDeployment(client, preclaimedLegacy.id);
  const preclaimedFirst = await approval(service, approverOne, preclaimedRequirement);
  assert.deepEqual((await decide(service, approverOne, preclaimedRequirement, preclaimedFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const preclaimedSecond = await approval(service, approverTwo, preclaimedRequirement);
  assert.deepEqual((await decide(service, approverTwo, preclaimedRequirement, preclaimedSecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_attempts WHERE deployment_id = $1", [preclaimedLegacy.id])).rows[0].count, 0);
  const staleInboxDeployment = await request(service, highVersionId, production, "stale-inbox-expiry", true, workerSafetyRequester);
  const staleInboxRequirement = await requirementForDeployment(client, staleInboxDeployment.id);
  await client.query("SELECT pg_advisory_lock(hashtext('m14-approval-expiry-claim:' || $1::text))", [staleInboxDeployment.id]);
  try {
    await client.query("UPDATE deployment_approval_requirements SET expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second' WHERE id = $1", [staleInboxRequirement]);
    const staleInbox = await graphql(service, approverOne,
      inboxQuery("StaleInbox", "decisionAvailable id status", `projectId: { eq: "${project}" }`), { limit: 50, page: 0 });
    const staleInboxRow = staleInbox.deploymentApprovalRequirements.nodes.find((item) => item.id === staleInboxRequirement);
    assert.equal(staleInboxRow.status, "EXPIRED");
    assert.equal(staleInboxRow.decisionAvailable, false);
    assert.equal((await client.query("SELECT status FROM deployment_approval_requirements WHERE id = $1", [staleInboxRequirement])).rows[0].status, "PENDING");
  } finally {
    await client.query("SELECT pg_advisory_unlock(hashtext('m14-approval-expiry-claim:' || $1::text))", [staleInboxDeployment.id]);
  }
  const expiryWithoutWorker = await request(service, highVersionId, production, "expiry-without-worker", true, workerSafetyRequester);
  const expiryWithoutWorkerRequirement = await requirementForDeployment(client, expiryWithoutWorker.id);
  await client.query("UPDATE deployment_approval_requirements SET expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second' WHERE id = $1", [expiryWithoutWorkerRequirement]);
  await waitForRequirementStatus(expiryWithoutWorkerRequirement, "EXPIRED");
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [expiryWithoutWorker.id])).rows[0].lifecycle_status, "CANCELED");
  const deferredVersion = await publishHighRiskVersion(service, base.agentId, base.document, "\n# compatible worker handoff fixture");
  const deferredUntilCompatible = await request(service, deferredVersion, production, "deferred-until-compatible-worker", true, workerSafetyRequester);
  const deferredRequirement = await requirementForDeployment(client, deferredUntilCompatible.id);
  const deferredFirst = await approval(service, approverOne, deferredRequirement);
  assert.deepEqual((await decide(service, approverOne, deferredRequirement, deferredFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const deferredSecond = await approval(service, approverTwo, deferredRequirement);
  assert.deepEqual((await decide(service, approverTwo, deferredRequirement, deferredSecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [deferredUntilCompatible.id])).rows[0].lifecycle_status, "APPROVED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_outbox_events WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [deferredUntilCompatible.id])).rows[0].count, 0);
  worker = await startLocalDeploymentWorker(database.name, {
    HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "20",
    HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "20"
  });
  await waitForLifecycle(service, preclaimedLegacy.id, "ACTIVE");
  await waitForLifecycle(service, deferredUntilCompatible.id, "ACTIVE");

  // A satisfied requirement cannot be rewritten when its evidence is revoked before delivery. The
  // worker records a durable terminal deployment state instead of silently delivering a stranded
  // APPROVED cycle. The direct heartbeat and claim simulate the old/new worker boundary that the
  // M14 migration enforces.
  await worker.stop();
  worker = undefined;
  await client.query("UPDATE deployment_worker_heartbeats SET approval_execution_compatible = FALSE, observed_at = CURRENT_TIMESTAMP - INTERVAL '1 minute'");
  await client.query(`INSERT INTO deployment_worker_heartbeats
      (worker_id, observed_at, last_batch_deliveries, pending_events, oldest_pending_at, state, failure_code, approval_execution_compatible)
    VALUES ('fixture-m14-compatible', CURRENT_TIMESTAMP, 0, 0, NULL, 'READY', NULL, TRUE)`);
  // A SQL failure after a compatible worker claims an approved handoff must persist bounded
  // retries in a separate transaction. A later ready event proves the failing claim cannot
  // monopolize the local worker batch.
  const deliveryFailure = await request(service, highVersionId, production, "worker-database-delivery-failure", true, workerSafetyRequester);
  const deliveryFailureRequirement = await requirementForDeployment(client, deliveryFailure.id);
  const deliveryFailureFirst = await approval(service, approverOne, deliveryFailureRequirement);
  assert.deepEqual((await decide(service, approverOne, deliveryFailureRequirement, deliveryFailureFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const deliveryFailureSecond = await approval(service, approverTwo, deliveryFailureRequirement);
  assert.deepEqual((await decide(service, approverTwo, deliveryFailureRequirement, deliveryFailureSecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const deliveryRecovery = await request(service, highVersionId, production, "worker-database-delivery-fairness", true, workerSafetyRequester);
  const deliveryRecoveryRequirement = await requirementForDeployment(client, deliveryRecovery.id);
  const deliveryRecoveryFirst = await approval(service, approverOne, deliveryRecoveryRequirement);
  assert.deepEqual((await decide(service, approverOne, deliveryRecoveryRequirement, deliveryRecoveryFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const deliveryRecoverySecond = await approval(service, approverTwo, deliveryRecoveryRequirement);
  assert.deepEqual((await decide(service, approverTwo, deliveryRecoveryRequirement, deliveryRecoverySecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  // Eligibility is decided by approval_execution_eligible() in the persistence crate, not by a SQL
  // function this fixture could swap out, so it injects the same class of
  // "a SQL failure after a compatible worker claims an approved handoff" instead, via a trigger on
  // deployment_attempts -- execute()'s own first substantive write once eligibility passes -- matching
  // the identical technique the recovery-audit-failure trigger below already uses on a different table.
  // Scoped to status = 'RUNNING' (execute()'s own insert) only: the dead-letter path inserts its own
  // 'FAILED'-status attempt once retries are exhausted, and that insert must still succeed, or the
  // event can never leave PROCESSING for DEAD_LETTER.
  const deliveryFailureName = `m14_delivery_failure_${run}`;
  const deliveryFailureTrigger = `m14_delivery_failure_trigger_${run}`;
  await client.query(`CREATE FUNCTION ${deliveryFailureName}() RETURNS trigger LANGUAGE plpgsql AS $$
    BEGIN
      IF NEW.deployment_id = '${deliveryFailure.id}'::uuid AND NEW.status = 'RUNNING' THEN
        RAISE EXCEPTION 'fixture approval execution eligibility failure';
      END IF;
      RETURN NEW;
    END;
  $$`);
  await client.query(`CREATE TRIGGER ${deliveryFailureTrigger} BEFORE INSERT ON deployment_attempts
    FOR EACH ROW EXECUTE FUNCTION ${deliveryFailureName}()`);
  const recoveryAuditFailure = `m14_recovery_audit_failure_${run}`;
  const recoveryAuditTrigger = `m14_recovery_audit_trigger_${run}`;
  await client.query(`CREATE FUNCTION ${recoveryAuditFailure}() RETURNS trigger LANGUAGE plpgsql AS $$
    BEGIN
      IF NEW.action = 'OUTBOX_DELIVERY_RETRIED' AND NEW.deployment_id = '${deliveryFailure.id}'::uuid THEN
        RAISE EXCEPTION 'fixture recovery audit failure';
      END IF;
      RETURN NEW;
    END;
  $$`);
  await client.query(`CREATE TRIGGER ${recoveryAuditTrigger} BEFORE INSERT ON deployment_audit_events
    FOR EACH ROW EXECUTE FUNCTION ${recoveryAuditFailure}()`);
  worker = await startLocalDeploymentWorker(database.name, {
    HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "20",
    HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "1000"
  });
  try {
    await waitForLifecycle(service, deliveryRecovery.id, "ACTIVE");
    const firstRecoveryState = await client.query("SELECT status, attempt_count FROM deployment_outbox_events WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [deliveryFailure.id]);
    assert.deepEqual(firstRecoveryState.rows[0], { status: "PENDING", attempt_count: 1 });
    const recoveryHeartbeatStarted = Date.now();
    let recoveryFailureCode = null;
    while (Date.now() - recoveryHeartbeatStarted < 5_000) {
      recoveryFailureCode = (await client.query("SELECT failure_code FROM deployment_worker_heartbeats WHERE approval_execution_compatible ORDER BY observed_at DESC LIMIT 1")).rows[0]?.failure_code ?? null;
      if (recoveryFailureCode === "OUTBOX_RECOVERY_AUDIT_FAILED") break;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.equal(recoveryFailureCode, "OUTBOX_RECOVERY_AUDIT_FAILED");
    await worker.stop();
    worker = undefined;
    await client.query(`DROP TRIGGER ${recoveryAuditTrigger} ON deployment_audit_events`);
    await client.query(`DROP FUNCTION ${recoveryAuditFailure}()`);
    worker = await startLocalDeploymentWorker(database.name, {
      HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "20",
      HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "20"
    });
    const deliveryFailureStarted = Date.now();
    let deliveryFailureOutbox;
    while (Date.now() - deliveryFailureStarted < 12_000) {
      const row = await client.query("SELECT status, attempt_count, last_error FROM deployment_outbox_events WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [deliveryFailure.id]);
      if (row.rows[0]?.status === "DEAD_LETTER") { deliveryFailureOutbox = row.rows[0]; break; }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.deepEqual(deliveryFailureOutbox, { status: "DEAD_LETTER", attempt_count: 3, last_error: "The local worker retried a database delivery failure." });
    const recoveryAuditStarted = Date.now();
    let recoveryAudits;
    while (Date.now() - recoveryAuditStarted < 5_000) {
      const rows = await client.query(`SELECT action, count(*)::int AS count FROM deployment_audit_events
        WHERE deployment_id = $1 AND action IN ('OUTBOX_DELIVERY_RETRIED', 'OUTBOX_DEAD_LETTERED') GROUP BY action`, [deliveryFailure.id]);
      recoveryAudits = Object.fromEntries(rows.rows.map((row) => [row.action, row.count]));
      if (recoveryAudits.OUTBOX_DELIVERY_RETRIED === 2 && recoveryAudits.OUTBOX_DEAD_LETTERED === 1) break;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.deepEqual(recoveryAudits, { OUTBOX_DELIVERY_RETRIED: 2, OUTBOX_DEAD_LETTERED: 1 });
    assert.equal((await client.query(`SELECT count(*)::int AS count
      FROM deployment_outbox_delivery_audit_repairs
      WHERE deployment_id = $1 AND recorded_at IS NOT NULL`, [deliveryFailure.id])).rows[0].count, 3);
  } finally {
    if (worker) await worker.stop();
    worker = undefined;
    await client.query(`DROP TRIGGER IF EXISTS ${recoveryAuditTrigger} ON deployment_audit_events`);
    await client.query(`DROP FUNCTION IF EXISTS ${recoveryAuditFailure}()`);
    await client.query(`DROP TRIGGER IF EXISTS ${deliveryFailureTrigger} ON deployment_attempts`);
    await client.query(`DROP FUNCTION IF EXISTS ${deliveryFailureName}()`);
  }
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [deliveryFailure.id])).rows[0].lifecycle_status, "FAILED");
  const archivedAfterApprovalVersion = await publishHighRiskVersion(service, base.agentId, base.document, " archived-after-approved-handoff");
  const archivedAfterApproval = await request(service, archivedAfterApprovalVersion, production, "archived-after-approved-handoff", true, workerSafetyRequester);
  assert.deepEqual([archivedAfterApproval.deploymentPolicySnapshots.risk, archivedAfterApproval.deploymentPolicySnapshots.requiredApprovers], ["HIGH", 2]);
  const archivedAfterApprovalRequirement = await requirementForDeployment(client, archivedAfterApproval.id);
  const archivedFirst = await approval(service, approverOne, archivedAfterApprovalRequirement);
  assert.deepEqual((await decide(service, approverOne, archivedAfterApprovalRequirement, archivedFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const archivedSecond = await approval(service, approverTwo, archivedAfterApprovalRequirement);
  assert.deepEqual((await decide(service, approverTwo, archivedAfterApprovalRequirement, archivedSecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  await archiveProjectViaApi(service, project);
  worker = await startLocalDeploymentWorker(database.name, {
    HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "20",
    HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "20"
  });
  await waitForLifecycle(service, archivedAfterApproval.id, "CANCELED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_attempts WHERE deployment_id = $1", [archivedAfterApproval.id])).rows[0].count, 0);
  assert.equal((await client.query("SELECT facts->>'code' AS code FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_EXECUTION_BLOCKED'", [archivedAfterApproval.id])).rows[0].code, "PROJECT_ARCHIVED");
  await worker.stop();
  worker = undefined;
  await restoreProjectViaApi(service, project);
  await client.query("UPDATE deployment_worker_heartbeats SET approval_execution_compatible = FALSE, observed_at = CURRENT_TIMESTAMP - INTERVAL '1 minute'");
  const archiveBoundaryVersion = await publishHighRiskVersion(service, base.agentId, base.document, " archive-boundary-satisfied-handoff");
  const archiveBoundaryHandoff = await request(service, archiveBoundaryVersion, production, "archive-boundary-satisfied-handoff", true, workerSafetyRequester);
  assert.deepEqual([archiveBoundaryHandoff.deploymentPolicySnapshots.risk, archiveBoundaryHandoff.deploymentPolicySnapshots.requiredApprovers], ["HIGH", 2]);
  const archiveBoundaryRequirement = await requirementForDeployment(client, archiveBoundaryHandoff.id);
  const archiveBoundaryFirst = await approval(service, approverOne, archiveBoundaryRequirement);
  assert.deepEqual((await decide(service, approverOne, archiveBoundaryRequirement, archiveBoundaryFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const archiveBoundarySecond = await approval(service, approverTwo, archiveBoundaryRequirement);
  assert.deepEqual((await decide(service, approverTwo, archiveBoundaryRequirement, archiveBoundarySecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [archiveBoundaryHandoff.id])).rows[0].lifecycle_status, "APPROVED");
  const archiveBoundaryRevision = Number((await client.query("SELECT revision FROM projects WHERE id = $1", [project])).rows[0].revision);
  const archiveBoundaryArchive = await graphql(service, requester,
    "mutation ArchiveSatisfied($input: LifecycleAdministrationInput!) { archiveAdministrationScope(input: $input) { project { revision } problems { code } } }",
    { input: { scope: "PROJECT", scopeId: project, expectedRevision: archiveBoundaryRevision, reason: "Terminalize retained satisfied handoff" } });
  assert.deepEqual(archiveBoundaryArchive.archiveAdministrationScope.problems, []);
  await waitForLifecycle(service, archiveBoundaryHandoff.id, "CANCELED");
  const archiveBoundaryAudit = await client.query("SELECT facts->>'code' AS code, actor_principal_id FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_EXECUTION_BLOCKED' ORDER BY occurred_at DESC, id DESC LIMIT 1", [archiveBoundaryHandoff.id]);
  assert.deepEqual(archiveBoundaryAudit.rows[0], { code: "PROJECT_ARCHIVED", actor_principal_id: requester });
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_handoff_releases WHERE deployment_id = $1", [archiveBoundaryHandoff.id])).rows[0].count, 0);
  const archiveBoundaryRestore = await graphql(service, requester,
    "mutation RestoreSatisfied($input: LifecycleAdministrationInput!) { restoreAdministrationScope(input: $input) { project { lifecycleStatus } problems { code } } }",
    { input: { scope: "PROJECT", scopeId: project, expectedRevision: archiveBoundaryArchive.archiveAdministrationScope.project.revision } });
  assert.deepEqual(archiveBoundaryRestore.restoreAdministrationScope.problems, []);
  worker = await startLocalDeploymentWorker(database.name, {
    HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "20",
    HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "20"
  });
  await new Promise((resolve) => setTimeout(resolve, 250));
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [archiveBoundaryHandoff.id])).rows[0].lifecycle_status, "CANCELED");
  await worker.stop();
  worker = undefined;
  const revokedAfterApprovalVersion = await publishHighRiskVersion(service, base.agentId, base.document, " revoked-after-approved-handoff");
  const revokedAfterApproval = await request(service, revokedAfterApprovalVersion, production, "revoked-after-approved-handoff", true, workerSafetyRequester);
  assert.deepEqual([revokedAfterApproval.deploymentPolicySnapshots.risk, revokedAfterApproval.deploymentPolicySnapshots.requiredApprovers], ["HIGH", 2]);
  const revokedAfterApprovalRequirement = await requirementForDeployment(client, revokedAfterApproval.id);
  const revokedFirst = await approval(service, approverOne, revokedAfterApprovalRequirement);
  assert.deepEqual((await decide(service, approverOne, revokedAfterApprovalRequirement, revokedFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const revokedSecond = await approval(service, approverTwo, revokedAfterApprovalRequirement);
  assert.deepEqual((await decide(service, approverTwo, revokedAfterApprovalRequirement, revokedSecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  assert.equal((await client.query("SELECT status FROM deployment_approval_requirements WHERE id = $1", [revokedAfterApprovalRequirement])).rows[0].status, "SATISFIED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_outbox_events WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT' AND status = 'PENDING'", [revokedAfterApproval.id])).rows[0].count, 1);
  // No raw-SQL "claim, then the lifecycle fence rejects the worker" scenario here: Aurora DSQL
  // rejects CREATE TRIGGER and CREATE FUNCTION outright, so there is no worker gate, claim gate or
  // commit gate in the database. execute()'s own active_project(), approval_execution_eligible() and
  // compatible_approval_worker() pre-checks cover the write path; the worker started below reaches
  // this event through that path and blocks execution once it finds the revoked evidence, the same
  // way it would for any evidence invalidation discovered mid-flight.
  const approvalEvidence = await client.query("SELECT id FROM deployment_evidence_snapshots WHERE deployment_id = $1 AND evidence_kind = 'EVALUATION_PASSED'", [revokedAfterApproval.id]);
  await client.query("INSERT INTO deployment_evidence_invalidations (id, evidence_snapshot_id, kind) VALUES ($1, $2, 'REVOKED')", [randomUUID(), approvalEvidence.rows[0].id]);
  worker = await startLocalDeploymentWorker(database.name, {
    HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "20",
    HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "20"
  });
  await waitForLifecycle(service, revokedAfterApproval.id, "CANCELED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_attempts WHERE deployment_id = $1", [revokedAfterApproval.id])).rows[0].count, 0);
  const revokedExecutionAudit = await client.query("SELECT facts->>'code' AS code FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_EXECUTION_BLOCKED'", [revokedAfterApproval.id]);
  assert.deepEqual(revokedExecutionAudit.rows, [{ code: "APPROVAL_EVIDENCE_MISMATCH" }]);

  await worker.stop();
  worker = undefined;
  // No raw-SQL audit-insert watermark or correlation-fill assertions here: Aurora DSQL supports no
  // triggers, so nothing fills either field in the database. The audit write path calls
  // touch_projection() unconditionally for every audit insert, and nothing in this codebase reads
  // facts->>'correlationId' (every reader uses the correlation_id column). A raw SQL insert bypassing
  // the audit write path has no guard at all.
  const readinessVersion = await publishHighRiskVersion(service, base.agentId, base.document, "\n# worker-readiness fixture");
  const readinessDeployment = await request(service, readinessVersion, production, "worker-readiness", true, workerSafetyRequester);
  const readinessRequirement = await requirementForDeployment(client, readinessDeployment.id);
  const readinessFirst = await approval(service, approverOne, readinessRequirement);
  assert.deepEqual((await decide(service, approverOne, readinessRequirement, readinessFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const readinessSecond = await approval(service, approverTwo, readinessRequirement);
  assert.deepEqual((await decide(service, approverTwo, readinessRequirement, readinessSecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  assert.equal((await client.query("SELECT lifecycle_status FROM deployments WHERE id = $1", [readinessDeployment.id])).rows[0].lifecycle_status, "APPROVED");

  // The worker independently reconciles an expired pending requirement after the API process has
  // stopped. The database remains the source of truth, so no browser or API scheduler participates.
  const workerOnlyExpiryVersion = await publishHighRiskVersion(service, base.agentId, base.document, "\n# worker-only expiry fixture");
  const workerOnlyExpiry = await request(service, workerOnlyExpiryVersion, production, "worker-only-pending-expiry", false, workerSafetyRequester);
  const workerOnlyExpiryRequirement = await requirementForDeployment(client, workerOnlyExpiry.id);
  // The membership-scope-upgrade-not-ready fault injection formerly here (forcing
  // deployment_approval_membership_scope_upgrade_progress incomplete to make approvalInbox/
  // approvalRequirement/decide() all fail closed as not-yet-ready) is removed, not redesigned: that
  // whole readiness gate is removed too -- see V017's removal comment on deployment_approval_read_ready()
  // for the shared reasoning -- and every read/write this domain step owns is available unconditionally
  // now, with nothing left to force "not ready" against.
  await service.stop();
  service = undefined;
  worker = await startLocalDeploymentWorker(database.name, {
    HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "20",
    HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "20"
  });
  // readinessDeployment's satisfied handoff was queued (deployment_approval_handoff_releases) with no
  // worker running to release it. releaseCompatibleApprovalHandoffs() is reachable from the worker's
  // own heartbeat-triggered maintenance (recordWorkerHeartbeat(), independent of MaintenanceJobs' own
  // API-owned schedule -- see reconcileApprovalUpgrade()'s comment for what remains API-owned), so a
  // worker alone, with no API process at all, is sufficient to release and execute this deployment.
  await waitForLifecycleViaDatabase(readinessDeployment.id, "ACTIVE");
  await client.query("UPDATE deployment_approval_requirements SET expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second' WHERE id = $1", [workerOnlyExpiryRequirement]);
  await waitForRequirementStatus(workerOnlyExpiryRequirement, "EXPIRED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_EXPIRED'", [workerOnlyExpiry.id])).rows[0].count, 1);
} finally {
  try { await worker?.stop(); } finally {
    if (client) await client.end();
    if (service) await service.stop();
    await database.drop();
  }
}
