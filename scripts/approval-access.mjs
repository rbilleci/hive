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

const deploymentFields = "id projectId lifecycleStatus revision policy { policyDigest policyRevision risk requiredEvidence requiredApprovers }";
const requirementFields = "id deploymentId projectId revision status expiresAt requesterId requester { id subject } requiredDistinctApproverCount qualifyingApprovalCount satisfiedParticipantIds satisfiedParticipants { id subject } approvalSnapshot { policyDigest policyRevision environmentClass risk riskLevel rule { requiredEvidence requiredDistinctApproverCount } target { agentVersionId agentVersionDigest environmentDefinitionVersionId environmentDefinitionDigest targetDigest deploymentPlanDigest artifactDigest } evidence { kind digest bindingDigest expiresAt state } expiresAt } decisions(first: 20) { edges { cursor node { id actorPrincipalId decision comment rejectionReason eligibilityCheckedAt decidedAt } } pageInfo { hasNextPage endCursor } }";
const approvalItemFields = `decisionAvailable eligible requirement { ${requirementFields} } deployment { ${deploymentFields} }`;

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
    "query Environments($version: ID!) { deploymentEnvironmentDefinitionVersions(agentVersionId: $version, first: 50) { edges { node { id logicalEnvironmentClass } } } }",
    { version: versionId });
  const value = result.deploymentEnvironmentDefinitionVersions.edges.map((edge) => edge.node).find((entry) => entry.logicalEnvironmentClass === logicalClass);
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
  if (evaluationPassed && result.deployAgentVersion.deployment.policy.requiredEvidence.includes("EVALUATION_PASSED")) {
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
  // deployment_approval_evaluation_handoff_trigger used to call automatic_handoff on this INSERT,
  // regardless of writer; it is removed (Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright
  // -- see V017's removal comment). The real M16 write path
  // (PostgresEvaluationRepository.appendEvaluationPassedEvidence-shaped method) now calls
  // PostgresDeploymentRepository.touchProjection()/.automaticApprovalHandoff() directly instead, but
  // this fixture bypasses that Java path entirely with a raw INSERT. automatic_handoff() is removed too
  // (ported to the private automaticApprovalHandoff(), unreachable from this script -- see
  // legacyDeploymentFrom()'s own comment), so this replicates only the one branch this fixture's own
  // deployments (always zero-approver, always still PENDING/non-terminal/unexpired at this point) can
  // reach: the still-PENDING requirement becomes SATISFIED now that every required evidence kind --
  // including the EVALUATION_PASSED row just inserted above -- is present, and a
  // deployment_approval_handoff_releases row queues the resulting execution for a compatible worker's
  // own maintenance pass, exactly as automatic_handoff()'s zero-approver branch would.
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
  // deployment_approval_requirement_compatibility_trigger used to create this row automatically on
  // this INSERT; it is removed (Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright -- see
  // V017's removal comment). deployment_approval_ensure_requirement() is removed too, ported to
  // PostgresDeploymentRepository.ensureRequirement() -- a private Java method this script cannot call
  // directly -- so this fixture replicates its one observable effect for a fresh, non-archived,
  // REQUESTED-lifecycle row instead: clone the source deployment's own already-resolved requirement
  // shape (its required_approvers and whatever terminal/pending outcome a real deploy() already gave
  // it), since a real ensureRequirement() call for this brand-new row would find the identical
  // deployment/policy facts (copied from the same source immediately above) and reach the same result.
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
  // deployment_approval_evaluation_handoff_trigger used to call automatic_handoff on this INSERT too,
  // whenever a copied row carried evidence_kind = 'EVALUATION_PASSED' -- see V017's removal comment.
  // automatic_handoff() is removed too, ported to PostgresDeploymentRepository.automaticApprovalHandoff()
  // -- also a private Java method this script cannot call directly. Its own effect beyond the
  // requirement shape already cloned above is: for a SATISFIED requirement on a REQUESTED/APPROVED
  // deployment, queue a deployment_approval_handoff_releases row so a compatible worker's own
  // maintenance pass (releaseCompatibleApprovalHandoffs()) picks it up and enqueues EXECUTE_DEPLOYMENT --
  // the same durable queue a real automatic_handoff() call leaves behind when it cannot enqueue
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
    `query Requirement($id: ID!) { approvalRequirement(approvalRequirementId: $id) { ${approvalItemFields} } }`, { id: requirementId });
  return result.approvalRequirement;
}

async function requirementForDeployment(client, deploymentId) {
  const result = await client.query("SELECT id FROM deployment_approval_requirements WHERE deployment_id = $1", [deploymentId]);
  assert.equal(result.rowCount, 1);
  return result.rows[0].id;
}

async function decide(service, principal, requirementId, revision, decision, comment = null, rejectionReason = null, idempotencyKey = randomUUID()) {
  return graphql(service, principal,
    `mutation Decide($input: DecideDeploymentApprovalInput!) { decideDeploymentApproval(input: $input) { decision { id decision comment rejectionReason } requirement { ${requirementFields} } deployment { ${deploymentFields} } problems { __typename code message ... on ApprovalRequirementRevisionConflict { resourceId expectedRevision actualRevision } } } }`,
    { input: { approvalRequirementId: requirementId, expectedRevision: revision, decision, comment, rejectionReason, idempotencyKey } });
}

async function waitForLifecycle(service, deploymentId, lifecycle) {
  const started = Date.now();
  while (Date.now() - started < 25_000) {
    const result = await graphql(service, requester,
      "query Deployment($id: ID!) { deploymentProjection(deploymentId: $id, first: 1) { deployment { id lifecycleStatus } } }", { id: deploymentId });
    if (result.deploymentProjection?.deployment.lifecycleStatus === lifecycle) return;
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

// deployment_approval_reconcile_project_archives_page() is removed, ported to
// PostgresDeploymentRepository.reconcileProjectArchives() -- a private method run only from
// reconcileApprovalUpgrade(), itself only reachable through MaintenanceJobs' own 1-second scheduled
// tick (running inside the API process this whole file already drives through `service`), not through
// any raw-SQL-callable surface this script can invoke directly. Polling alone is correct: that tick
// runs continuously for the lifetime of `service`, with or without a local deployment worker started.
async function waitForArchiveReconciliation(eventId) {
  const started = Date.now();
  while (Date.now() - started < 25_000) {
    const event = await client.query("SELECT processed_at IS NOT NULL AS processed FROM deployment_approval_project_archive_events WHERE id = $1", [eventId]);
    if (event.rows[0]?.processed) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`Archive event ${eventId} did not reconcile.`);
}

// deployment_approval_project_archive_requested_trigger/_requested() are removed, ported to
// PostgresAdministrationRepository.recordProjectArchiveEvent() -- reachable only through
// PostgresAdministrationRepository.lifecycle(), not through a raw UPDATE projects any more (see V023's
// removal comment). These two helpers replace every raw "UPDATE projects SET lifecycle_status = ..."
// this file used to rely on for that side effect, routing archive/restore through the real
// archiveAdministrationScope/restoreAdministrationScope mutations instead so the archive event this
// domain's revision-boundary scenarios depend on actually gets recorded.
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

// deployment_approval_ensure_requirement()'s final redefinition is removed, ported to
// PostgresDeploymentRepository.ensureRequirement()/invalidateArchivedApprovalRequirement()/
// recordTerminalInvalidation() -- all private Java methods this script cannot call directly (see
// legacyDeploymentFrom()'s own comment). This replicates the full port for fixtures that construct a
// deployment row directly (bypassing insertApprovalRequirement()) and need the same dynamic archived-
// or-terminal-lifecycle determination a real call would make, not just a clone of an already-resolved
// source's requirement shape (legacyDeploymentFrom()'s simpler replacement, sufficient only when the
// source's own outcome can be copied verbatim). The archived check below inlines
// deployment_approval_archive_boundary()'s final (V027, revision-aware) predicate directly rather than
// calling it: Aurora DSQL rejects CREATE FUNCTION outright, so the P-10 domain-closure step removed the
// SQL declaration and ported the identical predicate to
// PostgresDeploymentRepository.deploymentArchiveBoundary() -- this fixture mirrors that Java port
// exactly rather than that private method, which this script cannot call directly.
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

// deployment_approval_automatic_handoff()'s zero-approver-satisfy branch, for a fixture-constructed
// deployment (see ensureRequirementViaSql() above) whose requirement is still PENDING with zero
// required approvers and evidence already ready by construction -- see legacyDeploymentFrom()'s own
// comment for the same replacement applied to its own SATISFIED-on-copy shortcut.
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
  // No raw-SQL "direct writer gets rejected" assertion here: Aurora DSQL rejects CREATE TRIGGER/CREATE
  // FUNCTION outright, so deployment_approval_project_role_assignment_guard_trigger is gone --
  // PostgresAdministrationRepository.approvalRoleTransitionAllowed() is the sole remaining enforcement
  // point, and it only guards writes that go through the repository, not a raw SQL INSERT. The
  // requesterSelfGrant assertion below exercises that same invariant through the real GraphQL/
  // repository path.
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
  assert.deepEqual([developmentMedium.policy.risk, developmentMedium.policy.requiredEvidence, developmentMedium.policy.requiredApprovers], ["MEDIUM", ["CHANGE_SUMMARY_READY", "PLAN_VALIDATED"], 0]);
  const developmentMediumRequirement = await requirementForDeployment(client, developmentMedium.id);
  assert.equal((await approval(service, requester, developmentMediumRequirement)).requirement.status, "SATISFIED");
  await waitForLifecycle(service, developmentMedium.id, "ACTIVE");
  // No raw-SQL "invalidation bumps the projection" assertion here: Aurora DSQL rejects CREATE TRIGGER/
  // CREATE FUNCTION outright, so deployment_approval_evidence_invalidation_handoff_trigger is gone --
  // see V017's removal comment. Nothing in this codebase writes deployment_evidence_invalidations
  // (confirmed by grep: no GraphQL mutation, no repository INSERT); this scenario simulated a
  // hypothetical direct writer the same way the now-removed M13-writer scenarios elsewhere in this
  // file did, and it exercised the same touch_projection()-only effect reconcilePending()/
  // automaticApprovalHandoff() would already no-op on for an ACTIVE (terminal) deployment.
  const developmentLow = await request(service, base.versionId, development, "development-low");
  assert.deepEqual([developmentLow.policy.risk, developmentLow.policy.requiredEvidence, developmentLow.policy.requiredApprovers], ["LOW", ["PLAN_VALIDATED"], 0]);
  await waitForLifecycle(service, developmentLow.id, "ACTIVE");

  const stagingMedium = await request(service, base.versionId, staging, "staging-medium");
  assert.deepEqual([stagingMedium.policy.risk, stagingMedium.policy.requiredEvidence, stagingMedium.policy.requiredApprovers], ["MEDIUM", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 1]);
  await approveAndActivate(service, client, stagingMedium);
  const stagingLow = await request(service, base.versionId, staging, "staging-low");
  assert.deepEqual([stagingLow.policy.risk, stagingLow.policy.requiredEvidence, stagingLow.policy.requiredApprovers], ["LOW", ["CHANGE_SUMMARY_READY", "PLAN_VALIDATED"], 0]);
  await waitForLifecycle(service, stagingLow.id, "ACTIVE");

  const productionMedium = await request(service, base.versionId, production, "production-medium");
  assert.deepEqual([productionMedium.policy.risk, productionMedium.policy.requiredEvidence, productionMedium.policy.requiredApprovers], ["MEDIUM", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 1]);
  await approveAndActivate(service, client, productionMedium);
  const productionLow = await request(service, base.versionId, production, "production-low");
  assert.deepEqual([productionLow.policy.risk, productionLow.policy.requiredEvidence, productionLow.policy.requiredApprovers], ["LOW", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 1]);
  await approveAndActivate(service, client, productionLow);

  const highVersionId = await publishHighRiskVersion(service, base.agentId, base.document);
  const developmentHigh = await request(service, highVersionId, development, "development-high");
  assert.deepEqual([developmentHigh.policy.risk, developmentHigh.policy.requiredEvidence, developmentHigh.policy.requiredApprovers], ["HIGH", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 1]);
  await approveAndActivate(service, client, developmentHigh);
  const stagingHigh = await request(service, highVersionId, staging, "staging-high");
  assert.deepEqual([stagingHigh.policy.risk, stagingHigh.policy.requiredEvidence, stagingHigh.policy.requiredApprovers], ["HIGH", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 1]);
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
  // This scenario used to simulate a retained M13 writer archiving a project through a raw UPDATE
  // projects that bypassed PostgresAdministrationRepository.lifecycle() entirely, relying on
  // deployment_approval_project_archive_requested_trigger (bound directly to the projects table) to
  // still record the archive event, with an actor left NULL and later resolved from a matching
  // administration_audit_events row through deployment_approval_project_archive_actor_attributions/
  // deployment_approval_record_project_archive_actor(). Both the trigger and the fallback-attribution
  // mechanism it fed are removed, not ported -- see V027's and V032's own removal comments -- and the
  // scenario itself is now structurally unreachable, not just untested: recordProjectArchiveEvent()
  // (PostgresAdministrationRepository.java) is the only writer of archive events in this rewrite, is
  // only ever reached through lifecycle()'s own Java-validated PROJECT-archive path, and always
  // supplies the caller's real actor as a direct, non-null parameter -- there is no remaining path that
  // creates an archive event without an actor already in hand.
  const productionHigh = await request(service, highVersionId, production, "production-high");
  assert.deepEqual([productionHigh.policy.risk, productionHigh.policy.requiredEvidence, productionHigh.policy.requiredApprovers], ["HIGH", ["CHANGE_SUMMARY_READY", "EVALUATION_PASSED", "PLAN_VALIDATED"], 2]);
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
  assert.deepEqual(changedCommentRetry.decideDeploymentApproval.problems[0], { __typename: "ApprovalIdempotencyProblem", code: "IDEMPOTENCY_CONFLICT",
    message: "This idempotency key belongs to a different approval decision." });
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
  // No scope-cache-refreshed-to-zero assertion here: the scope-cache triggers are removed (Aurora
  // DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright -- see V017's removal comment), and
  // PostgresAdministrationRepository's refreshMembershipScope()/refreshRoleScope() only fire for
  // writes through the repository, not this raw SQL DELETE. The explicit DELETE below clears the
  // cache row before capabilityLoss needs it gone; decide()'s own NOT_FOUND check on a real
  // capability re-derivation, not the discovery cache, is what actually matters there.
  // A stale compact discovery row cannot supply the required P-10 inbox authority. The global
  // request remains non-disclosing before the keyset predicate evaluates the stale row.
  await client.query("INSERT INTO deployment_approval_principal_project_scopes (principal_id, project_id, valid_after) VALUES ($1, $2, CURRENT_TIMESTAMP) ON CONFLICT (principal_id, project_id) DO NOTHING", [approverTwo, project]);
  const staleScopeInbox = await graphql(service, approverTwo,
    "query StaleScope { approvalInbox(first: 50) { edges { node { requirement { id } } } } }", {});
  assert.equal(staleScopeInbox.approvalInbox, null);
  await client.query("DELETE FROM deployment_approval_principal_project_scopes WHERE principal_id = $1 AND project_id = $2", [approverTwo, project]);
  const capabilityLoss = await decide(service, approverTwo, productionHighRequirement, productionHighFirst.requirement.revision, "APPROVE");
  assert.equal(capabilityLoss.decideDeploymentApproval.problems[0].code, "NOT_FOUND");
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'DEPLOYMENT_APPROVER')", [roleMembership.rows[0].membership_id]);
  // No scope-cache-refreshed-to-one assertion here, for the same reason as the DELETE above -- this
  // raw SQL INSERT bypasses the repository too. secondDecision below only needs decide()'s own
  // real-time capability check to see the restored role, not the discovery cache.
  // No raw-SQL direct-writer rejection check here: deployment_approval_requirement_transition_trigger
  // no longer exists under Aurora DSQL compatibility (V017 stopped creating it), and the guarantee it
  // enforced against the application's own write path is upheld by construction, not by a trigger --
  // see V033's removal comment for the induction argument (recordApprovalDecision() derives
  // satisfied_participants from deployment_approval_decisions itself, whose UNIQUE constraint makes a
  // duplicate participant structurally impossible). A raw SQL statement bypassing the application
  // entirely, the scenario this assertion forced, has no equivalent guard once the database no longer
  // performs any of this logic -- there is no application-level operation left to test.
  // Hold the local executor after the second immutable decision so this archive-boundary test
  // exercises the satisfied handoff before any compatible worker may begin execution.
  await worker.stop();
  worker = undefined;
  const secondDecision = await decide(service, approverTwo, productionHighRequirement, productionHighFirst.requirement.revision, "APPROVE", "REVIEWED_CHANGE_SCOPE");
  assert.deepEqual(secondDecision.decideDeploymentApproval.problems, []);
  assert.equal(secondDecision.decideDeploymentApproval.requirement.status, "SATISFIED");
  assert.deepEqual(secondDecision.decideDeploymentApproval.requirement.satisfiedParticipantIds.sort(), [approverOne, approverTwo].sort());
  // deployment_approval_replay_receipt_backfill_progress/_page() are removed, not ported: see V031's
  // own removal comment -- auditApprovalReplay()/insertDecision() (PostgresDeploymentRepository.java)
  // write deployment_approval_replay_receipts synchronously for every decision and every APPROVAL_
  // REPLAYED audit fact from the start, so no V029-predates-V030 backfill scenario can occur in this
  // greenfield rewrite. secondDecisionRequest's own receipt (written synchronously when secondDecision
  // was created above) is exactly what recoveredReplay below exercises -- no backfill catch-up needed.
  const secondDecisionRequest = await client.query("SELECT request_key FROM deployment_approval_decisions WHERE id = $1", [secondDecision.decideDeploymentApproval.decision.id]);
  const historicalReplayCount = await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_REPLAYED'", [productionHigh.id]);
  const recoveredReplay = await decide(service, approverTwo, productionHighRequirement, productionHighFirst.requirement.revision,
    "APPROVE", "REVIEWED_CHANGE_SCOPE", null, secondDecisionRequest.rows[0].request_key);
  assert.deepEqual(recoveredReplay.decideDeploymentApproval.problems, []);
  assert.equal(recoveredReplay.decideDeploymentApproval.decision.id, secondDecision.decideDeploymentApproval.decision.id);
  // This first replay of secondDecisionRequest's own key appends exactly one APPROVAL_REPLAYED fact
  // (auditApprovalReplay(), PostgresDeploymentRepository.java) -- unlike the removed historical-backfill
  // scenario this fixture used to also cover, there is no pre-existing replayed fact for this key yet.
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_REPLAYED'", [productionHigh.id])).rows[0].count,
    historicalReplayCount.rows[0].count + 1);
  // A response lost after the first commit retries the same immutable request identity even after
  // the second decision terminalizes the requirement.
  const replayedDecision = await decide(service, approverOne, productionHighRequirement, productionHighFirst.requirement.revision,
    "APPROVE", "REVIEWED_CHANGE_SCOPE", null, firstDecisionKey);
  const replayedDecisionCorrelation = lastGraphqlRequestId;
  assert.deepEqual(replayedDecision.decideDeploymentApproval.problems, []);
  assert.equal(replayedDecision.decideDeploymentApproval.decision.id, firstDecision.decideDeploymentApproval.decision.id);
  const decisionHistoryFirst = await graphql(service, approverOne,
    "query DecisionHistory($id: ID!, $after: String) { approvalRequirement(approvalRequirementId: $id) { requirement { decisions(after: $after, first: 1) { edges { cursor node { id actorPrincipalId } } pageInfo { hasNextPage endCursor } } } } }",
    { id: productionHighRequirement, after: null });
  const firstHistoryPage = decisionHistoryFirst.approvalRequirement.requirement.decisions;
  assert.equal(firstHistoryPage.edges.length, 1);
  assert.equal(firstHistoryPage.pageInfo.hasNextPage, true);
  assert.equal(firstHistoryPage.pageInfo.endCursor, firstHistoryPage.edges[0].cursor);
  const decisionHistorySecond = await graphql(service, approverOne,
    "query DecisionHistory($id: ID!, $after: String) { approvalRequirement(approvalRequirementId: $id) { requirement { decisions(after: $after, first: 1) { edges { cursor node { id actorPrincipalId } } pageInfo { hasNextPage endCursor } } } } }",
    { id: productionHighRequirement, after: firstHistoryPage.pageInfo.endCursor });
  const secondHistoryPage = decisionHistorySecond.approvalRequirement.requirement.decisions;
  assert.equal(secondHistoryPage.edges.length, 1);
  assert.equal(secondHistoryPage.pageInfo.hasNextPage, false);
  assert.notEqual(secondHistoryPage.edges[0].node.id, firstHistoryPage.edges[0].node.id);
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
    "query ReplayTimeline($id: ID!) { deploymentProjection(deploymentId: $id, first: 100) { timeline { edges { node { stage status message source } } } } }",
    { id: productionHigh.id });
  const replayTimelineEvent = replayTimeline.deploymentProjection.timeline.edges.map((edge) => edge.node)
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
    "query CanceledInbox { approvalInbox(first: 50, projectId: \"50000000-0000-0000-0000-000000000003\") { edges { node { decisionAvailable requirement { id status } deployment { lifecycleStatus } } } } }", {});
  const canceledInboxItem = canceledInbox.approvalInbox.edges.map((edge) => edge.node)
    .find((item) => item.requirement.id === canceledInboxRequirement);
  assert.deepEqual(canceledInboxItem, { decisionAvailable: false,
    requirement: { id: canceledInboxRequirement, status: "PENDING" }, deployment: { lifecycleStatus: "CANCELED" } });
  const foreignDecisionHistory = await graphql(service, approverOne,
    "query DecisionHistory($id: ID!, $after: String) { approvalRequirement(approvalRequirementId: $id) { requirement { decisions(after: $after, first: 1) { edges { node { id } } } } } }",
    { id: await requirementForDeployment(client, stagingMedium.id), after: firstHistoryPage.pageInfo.endCursor });
  assert.equal(foreignDecisionHistory.approvalRequirement.requirement.decisions.edges.length, 0);
  // No raw-SQL immutability checks here: deployment_approval_decisions_no_update/_no_delete are
  // removed (Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright -- see V017's removal
  // comment), and insertDecision() (PostgresDeploymentRepository.java) is this table's only writer --
  // a single, unconditional INSERT, never an UPDATE or DELETE -- so there is no application-level
  // operation left to test.

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
  // No pre-decide() status/audit-count assertions here: deployment_approval_evidence_invalidation_
  // handoff_trigger is removed (see V017's removal comment) and nothing writes
  // deployment_evidence_invalidations from Java, so a raw-SQL insert no longer proactively reconciles
  // the still-PENDING requirement. decide()'s own real-time evidence check -- which reconciles as a
  // side effect of evaluating the attempt, the same pattern missingRequirement above already relies
  // on -- is what actually catches this now.
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
  // No DISABLE/ENABLE TRIGGER bracket needed here: deployment_approval_requirement_transition_trigger
  // no longer exists under Aurora DSQL compatibility, so there is no trigger left to reject this raw
  // expires_at rewrite (which the removed trigger's frozen-facts guard used to require disabling first).
  await client.query("UPDATE deployment_approval_requirements SET expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second' WHERE id = $1", [inboxExpiredRequirement]);
  await waitForRequirementStatus(inboxExpiredRequirement, "EXPIRED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_audit_events WHERE deployment_id = $1 AND action = 'APPROVAL_EXPIRED'", [inboxExpiredDeployment.id])).rows[0].count, 1);

  const mismatchDeployment = await request(service, highVersionId, production, "evidence-mismatch");
  const mismatchRequirement = await invalidateEvidence(client, mismatchDeployment.id, async () => {
    await client.query("DROP RULE IF EXISTS deployment_evidence_snapshots_no_update ON deployment_evidence_snapshots");
    // The mismatch check binds evidence to the frozen policy facts (binding, target, plan, package).
    // It no longer recomputes evidence_digest: that SQL needed pgcrypto, which Aurora DSQL rejects.
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
  const expired = await decide(service, approverOne, expiredRequirement, (await approval(service, approverOne, expiredRequirement)).requirement.revision, "APPROVE");
  assert.equal(expired.decideDeploymentApproval.problems[0].code, "APPROVAL_REQUIREMENT_EXPIRED");
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
  assert.deepEqual([zeroEvaluation.policy.risk, zeroEvaluation.policy.requiredApprovers], ["LOW", 0]);
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
  // No append-only DELETE check here: deployment_approval_requirements_no_delete no longer exists
  // under Aurora DSQL compatibility (V017 stopped creating it), and no Java code path ever DELETEs
  // from this table in the first place -- there is no application-level operation left to guard.

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
  // digest_trigger no longer exists under Aurora DSQL compatibility (V017 stopped creating it), and this
  // simulated pre-V017 row has no other source for the value the assertion below expects.
  await client.query(`INSERT INTO deployment_policy_snapshots (deployment_id, policy_id, policy_revision, policy_digest, policy_matrix,
      logical_environment_class, risk, required_evidence, required_approvers, agent_version_id, environment_definition_version_id,
      target_digest, plan_digest, package_digest, binding_digest, evaluation_requirement_expires_at, risk_verification_digest)
    SELECT $1, policy_id, policy_revision, policy_digest, policy_matrix, logical_environment_class, risk, required_evidence,
      required_approvers, agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest,
      binding_digest, evaluation_requirement_expires_at, encode(sha256(convert_to(risk || '|' || binding_digest, 'UTF8')), 'hex')
    FROM deployment_policy_snapshots WHERE deployment_id = $2`, [m13CompatibleDeployment, zeroEvaluation.id]);
  // deployment_approval_ensure_requirement() is removed (ported to
  // PostgresDeploymentRepository.ensureRequirement(), a private Java method this script cannot call
  // directly -- see legacyDeploymentFrom()'s own fix above). Unlike that fixture, this row is not a
  // clone of an already-resolved source: it is a brand-new requirement for a deployment inserted
  // directly into a terminal (CANCELED) lifecycle_status, so this replicates ensureRequirement()'s own
  // terminal-lifecycle INVALIDATED branch (deployment.lifecycle_status IN (...) THEN 'INVALIDATED') and
  // recordTerminalInvalidation()'s timeline-sequence claim plus APPROVAL_INVALIDATED audit fact.
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

  // The three historical-backfill scenarios formerly here (a predecessor deployment discovered by the
  // resumable deployment_approval_compatibility_backfill_page() cursor, in PENDING/APPROVED/terminal
  // shape) are removed, not redesigned: that page and its deployment_approval_compatibility_progress
  // cursor are themselves removed -- see PostgresDeploymentRepository.reconcileApprovalUpgrade()'s
  // comment for the shared reasoning -- and the scenario they covered (a deployment somehow missing its
  // requirement row) is now structurally unreachable, not just untested: insertApprovalRequirement()
  // creates that row synchronously for every deployment this rewrite's single write path ever produces,
  // and legacyDeploymentFrom() (this fixture's own simulated-legacy-writer helper) now does too -- see
  // its own comment for what replaced its raw calls to the same two removed SQL functions this backfill
  // page used internally.

  // No "retained M13 claimant already holds PROCESSING before V017 creates its requirement" scenario
  // here: it tested deployment_approval_outbox_gate_trigger (disabled to bypass insert validation)
  // and deployment_approval_execution_worker_gate_trigger (expected to reject a raw SQL IN_PROGRESS
  // transition at the database level). Both are removed -- Aurora DSQL rejects CREATE TRIGGER/CREATE
  // FUNCTION outright -- and there is no M13 claimant in this rewrite for either guard to fail closed
  // against; see V017's removal comments (execute()'s own activeProject()/approvalExecutionEligible()/
  // compatibleApprovalWorker() pre-checks already cover the Java write path these triggers used to
  // double-check).

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
  // deployment_approval_requirement_compatibility_trigger used to create this row automatically on
  // the policy-snapshot INSERT above; it is removed -- see V017's removal comment and
  // ensureRequirementViaSql()'s own comment for the replacement. This deployment is inserted REQUESTED,
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
  // No premature-EXECUTE_DEPLOYMENT-event-suppressed assertion here: deployment_approval_outbox_gate_
  // trigger is removed (see V017's removal comment), and there is no M13 writer left in this rewrite
  // to insert that event prematurely in the first place -- automaticApprovalHandoff() (called from
  // decide() below) is the only Java path that ever enqueues an EXECUTE_DEPLOYMENT event, and it only
  // does so once the requirement is actually SATISFIED.
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
  // No raw-SQL frozen-fact rejection check here: deployment_approval_requirement_transition_trigger no
  // longer exists under Aurora DSQL compatibility. transitionRequirement() (PostgresDeploymentRepository
  // .java) never includes required_approvers in its UPDATE's SET clause, so the application's own write
  // path cannot change it either way -- but a raw SQL statement bypassing the application entirely, the
  // scenario this assertion forced, has no equivalent guard once the database no longer performs any of
  // this logic.

  // A project role can predate its organization membership. V018 must avoid an organization-wide
  // scope rewrite while still discovering the principal after the membership becomes active.
  await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, 'm14-late-joiner', 'm14-late-joiner', 'm14-late-joiner@local.invalid')", [lateJoiner]);
  const lateProjectMembership = randomUUID();
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1)", [lateProjectMembership, project, lateJoiner]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'DEPLOYMENT_APPROVER')", [lateProjectMembership]);
  const lateBeforeOrganization = await graphql(service, lateJoiner,
    "query LateBefore($project: ID!) { approvalInbox(projectId: $project, first: 1) { edges { node { requirement { id } } } } }", { project });
  assert.equal(lateBeforeOrganization.approvalInbox, null);
  const lateOrganizationMembership = randomUUID();
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL)", [lateOrganizationMembership, organization, lateJoiner]);
  await client.query("INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_MEMBER')", [lateOrganizationMembership]);
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_approval_principal_project_scopes WHERE principal_id = $1 AND project_id = $2", [lateJoiner, project])).rows[0].count, 0);
  const lateAfterOrganization = await graphql(service, lateJoiner,
    "query LateAfter($project: ID!) { approvalInbox(projectId: $project, first: 50) { edges { node { requirement { id } } } } }", { project });
  assert(lateAfterOrganization.approvalInbox.edges.some((edge) => edge.node.requirement.id === retriedRequirement.requirement.id));

  const inbox = await graphql(service, approverOne,
    `query Inbox($organization: ID!, $project: ID!) { global: approvalInbox(first: 50) { edges { node { ${approvalItemFields} } } } organization: approvalInbox(organizationId: $organization, first: 50) { edges { node { requirement { id } } } } project: approvalInbox(projectId: $project, first: 50) { edges { node { requirement { id } } } } }`,
    { organization, project });
  assert(inbox.global.edges.some((edge) => edge.node.requirement.id === retriedRequirement.requirement.id));
  assert(inbox.organization.edges.some((edge) => edge.node.requirement.id === retriedRequirement.requirement.id));
  assert(inbox.project.edges.some((edge) => edge.node.requirement.id === retriedRequirement.requirement.id));
  const redactedPlan = await graphql(service, approverOne,
    "query ApprovalPlan($id: ID!) { approvalRequirement(approvalRequirementId: $id) { deployment { plan { canonicalPlan } } } }",
    { id: retriedRequirement.requirement.id });
  assert.equal(redactedPlan.approvalRequirement.deployment.plan.canonicalPlan, null);
  const hidden = await graphql(service, outsider,
    `query Hidden($id: ID!, $project: ID!) { approvalRequirement(approvalRequirementId: $id) { requirement { id } } approvalInbox(projectId: $project, first: 1) { edges { node { requirement { id } } } } }`,
    { id: retriedRequirement.requirement.id, project });
  assert.equal(hidden.approvalRequirement, null);
  assert.equal(hidden.approvalInbox, null);
  const developerInbox = await graphql(service, developerOnly,
    "query DeveloperInbox($organization: ID!) { global: approvalInbox(first: 1) { edges { node { requirement { id } } } } organization: approvalInbox(organizationId: $organization, first: 1) { edges { node { requirement { id } } } } }", { organization });
  assert.equal(developerInbox.global, null);
  assert.equal(developerInbox.organization, null);
  const malformedScope = await graphql(service, approverOne,
    "query MalformedScope { approvalInbox(projectId: \"invalid\", first: 1) { edges { node { requirement { id } } } } }", {});
  assert.equal(malformedScope.approvalInbox, null);
  const recursiveDecision = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(approverOne)}` },
    body: JSON.stringify({ query: "query RecursiveDecision($id: ID!) { approvalRequirement(approvalRequirementId: $id) { requirement { decisions(first: 1) { edges { node { requirement { id } } } } } } }", variables: { id: retriedRequirement.requirement.id } })
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
  // No project_lifecycle_revision-stamped assertion here: deployment_approval_legacy_archive_insert_
  // guard_trigger is removed -- see V024's removal comment -- so nothing stamps this column anymore.
  // The boundary checks below still hold on requested_at's own timestamp ordering (these two raw
  // inserts are sequential, not concurrent, around a real archive/restore in between).
  await archiveProjectViaApi(service, revisionBoundaryProject);
  await restoreProjectViaApi(service, revisionBoundaryProject);
  await insertRevisionBoundaryDeployment(revisionBoundaryAfter, "after");
  // boundary below inlines deployment_approval_archive_boundary()'s final (V027, revision-aware)
  // predicate as a correlated EXISTS -- see ensureRequirementViaSql()'s own comment for why this script
  // mirrors PostgresDeploymentRepository.deploymentArchiveBoundary()'s Java port directly rather than
  // calling a SQL declaration Aurora DSQL no longer allows to exist.
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
  // No project_lifecycle_revision immutability check here: deployment_approval_project_lifecycle_
  // revision_immutable_trigger is removed (see V027's removal comment) -- the column is never written
  // by anything now, so there is no application-level operation left to guard.
  // No archive-event-immutability assert.rejects() checks here either (identity/boundary facts staying
  // unchanged, delivery only transitioning outside reconciliation, no DELETE ever taking effect):
  // deployment_approval_archive_event_immutable_trigger and deployment_approval_project_archive_events_
  // no_delete are both removed -- see V027's own removal comment for why both guarantees hold by
  // construction (recordProjectArchiveEvent() only INSERTs, reconcileProjectArchives() is the only
  // UPDATE anywhere in this codebase and only ever sets processed_at once, nothing ever DELETEs) rather
  // than by a DB-level guard a raw SQL statement could still be aimed at to prove rejected.
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
  // deployment_approval_requirement_compatibility_trigger used to call this on the policy-snapshot
  // INSERT above; it is removed -- see V017's removal comment and ensureRequirementViaSql()'s own
  // comment for the replacement.
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
  // deployment_approval_evaluation_handoff_trigger used to call automatic_handoff on the evidence
  // copy above too; needed here since this zero-approver requirement only reaches SATISFIED once
  // automatic_handoff (not just ensure_requirement) runs -- see legacyDeploymentFrom()'s own identical
  // fix for the reasoning.
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
  // deployments()/rawRequirements() (PostgresDeploymentRepository.java) inner-join deployment_plan_versions,
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
  // outright); its Java port, PostgresDeploymentRepository.approvalInboxRequirementIds(), is private, so
  // this scoping check now goes through the real approvalInbox GraphQL query instead of raw SQL --
  // exercising the actual production path rather than a hand-copied replica.
  const projectScopedRequirements = await graphql(service, viewer,
    "query ProjectScopedRequirements($project: ID!) { approvalInbox(projectId: $project, first: 50) { edges { node { requirement { id } } } } }", { project });
  const projectScopedIds = projectScopedRequirements.approvalInbox.edges.map((edge) => edge.node.requirement.id);
  assert(projectScopedIds.includes(fairnessWaitingRequirement));
  assert.equal(projectScopedIds.includes(siblingRequirement), false);
  const organizationScopedRequirements = await graphql(service, viewer,
    "query OrganizationScopedRequirements($organization: ID!) { approvalInbox(organizationId: $organization, first: 50) { edges { node { requirement { id } } } } }", { organization });
  const organizationScopedIds = organizationScopedRequirements.approvalInbox.edges.map((edge) => edge.node.requirement.id);
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
  // Mirrors reconcileProjectArchives()'s own candidate-fetch query exactly (PostgresDeploymentRepository
  // .java), not just its boundary check: deployment_approval_archive_event_boundary() is inlined there
  // as a JOIN on this same event's id, not a scalar function call (Aurora DSQL rejects CREATE FUNCTION
  // outright), so this plan check exercises the real production query shape rather than an approximation.
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
    // outright); its Java port, PostgresDeploymentRepository.approvalInboxRequirementIds(), is private,
    // so pagination/scoping is exercised through the real approvalInbox GraphQL query instead.
    const page = await graphql(service, outsider,
      "query HighCardinalityPage { approvalInbox(first: 50) { edges { node { requirement { id } } } pageInfo { endCursor } } }", {});
    const pageIds = page.approvalInbox.edges.map((edge) => edge.node.requirement.id);
    assert(pageIds.length <= 50);
    assert(pageIds.includes(visibleRequirement));
    assert.equal(pageIds.includes(siblingRequirement), false);
    assert.equal(new Set(pageIds).size, pageIds.length);
    const nextPage = await graphql(service, outsider,
      "query HighCardinalityNextPage($after: String!) { approvalInbox(after: $after, first: 50) { edges { node { requirement { id } } } } }",
      { after: page.approvalInbox.pageInfo.endCursor });
    const nextPageIds = nextPage.approvalInbox.edges.map((edge) => edge.node.requirement.id);
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
  // deployment_approval_visible_requirements() is removed -- see the earlier removal comment in this
  // file. Ending outsider's only organization membership also invalidates every one of its 100,000
  // project memberships' own active-organization-membership requirement (project_view's definition,
  // now PostgresEffectiveCapabilityEvaluator.deploymentApprovalCapabilities()), so
  // hasApprovalInboxScope() plausibly finds no candidate at all here -- tolerate approvalInbox itself
  // coming back null (no scope whatsoever) as well as a non-null connection with zero matching edges;
  // either result correctly proves highCardinalityRequirement is no longer visible.
  const revokedScopePage = await graphql(service, outsider,
    "query RevokedScopePage { approvalInbox(first: 50) { edges { node { requirement { id } } } } }", {});
  const revokedScopeIds = revokedScopePage.approvalInbox?.edges.map((edge) => edge.node.requirement.id) ?? [];
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
  // deployment_approval_project_role_scope_trigger used to refresh this cache row on the raw INSERT
  // above; it is removed (see V017's removal comment). PostgresAdministrationRepository's own
  // refreshProjectScope()/refreshRoleScope() only fire for writes through the repository, and there is
  // no real mutation path that can construct a future-started membership in the first place (addMembership()
  // always uses CURRENT_TIMESTAMP), so this fixture replicates refreshProjectScope()'s exact DELETE+INSERT
  // directly, the same way it would need to for any writer capable of reaching this state.
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
  // hasApprovalInboxScope() finds no candidate at all -- approvalInbox resolves to null at the top
  // level, matching this file's established "no scope" pattern (e.g. the StaleScope/Hidden checks above).
  const futureViewerInbox = await graphql(service, futureViewer,
    "query FutureViewerInbox { approvalInbox(first: 50) { edges { node { requirement { id } } } } }", {});
  assert.equal(futureViewerInbox.approvalInbox, null);
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
  // deployment_approval_project_role_scope_trigger used to refresh both the old and new membership's
  // owning principal's scope row on this raw UPDATE; it is removed (see V017's removal comment).
  // Same reasoning as futureViewer's fix above -- no real mutation path reassigns a role's
  // membership_id directly (replaceMembership() only replaces a role LIST for one membership), so this
  // fixture replicates refreshProjectScope() for both affected principals directly.
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
  // This used to rename deployment_approval_compatibility_progress -- the singleton checkpoint the old
  // scheduled compatibility-backfill phase read -- to fault-inject a maintenance failure. That table
  // and phase are both removed (see PostgresDeploymentRepository.reconcileApprovalUpgrade()'s comment),
  // so this now targets deployment_approval_requirements instead: the one table
  // expiredApprovalRequirementDeployments() unconditionally reads on every expiry-reconciliation pass
  // (reconcileApprovalExpiry(), the API-owned maintenance still live after that removal), so renaming it
  // still deterministically fails the identical maintenance pass this scenario exercises.
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
  // No "retained worker holds a preclaimed event before the second approval commits, the lifecycle
  // guard rejects it" scenario here: it tested deployment_approval_outbox_gate_trigger (disabled to
  // bypass insert validation for a simulated incompatible-worker claim) and
  // deployment_approval_execution_worker_gate_trigger's "M14-compatible worker" rejection message and
  // deployment_approval_outbox_claim_gate_trigger's claim-revert-with-delay behavior. All three are
  // removed -- Aurora DSQL rejects CREATE TRIGGER/CREATE FUNCTION outright -- and
  // compatibleApprovalWorker() ("Rechecks the exact claimant after locking the deployment so a
  // pre-approval M13 claim cannot execute it") already covers the Java write path these triggers used
  // to double-check; see V017's removal comments. preclaimedLegacy is kept below (unlike the deleted
  // scenario above, deferred-until-compatible-worker waits on it reaching ACTIVE later) but no longer
  // needs a simulated pre-claim: once both decisions land, automaticApprovalHandoff() finds no
  // compatible worker ready yet (every heartbeat above is FALSE) and defers the same as it would for
  // this domain's actual worker-startup race, with no raw SQL needed to force that state.
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
      "query StaleInbox($project: ID!) { approvalInbox(projectId: $project, first: 50) { edges { node { decisionAvailable requirement { id status } } } } }",
      { project });
    const staleInboxRow = staleInbox.approvalInbox.edges.map((edge) => edge.node).find((item) => item.requirement.id === staleInboxRequirement);
    assert.equal(staleInboxRow.requirement.status, "EXPIRED");
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
  // deployment_approval_execution_eligible() is removed (ported to
  // PostgresDeploymentRepository.approvalExecutionEligible() -- see V017's removal comment), so this
  // fixture can no longer fault-inject by swapping that SQL function out. It injects the same class of
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
  assert.deepEqual([archivedAfterApproval.policy.risk, archivedAfterApproval.policy.requiredApprovers], ["HIGH", 2]);
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
  assert.deepEqual([archiveBoundaryHandoff.policy.risk, archiveBoundaryHandoff.policy.requiredApprovers], ["HIGH", 2]);
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
  assert.deepEqual([revokedAfterApproval.policy.risk, revokedAfterApproval.policy.requiredApprovers], ["HIGH", 2]);
  const revokedAfterApprovalRequirement = await requirementForDeployment(client, revokedAfterApproval.id);
  const revokedFirst = await approval(service, approverOne, revokedAfterApprovalRequirement);
  assert.deepEqual((await decide(service, approverOne, revokedAfterApprovalRequirement, revokedFirst.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  const revokedSecond = await approval(service, approverTwo, revokedAfterApprovalRequirement);
  assert.deepEqual((await decide(service, approverTwo, revokedAfterApprovalRequirement, revokedSecond.requirement.revision, "APPROVE")).decideDeploymentApproval.problems, []);
  assert.equal((await client.query("SELECT status FROM deployment_approval_requirements WHERE id = $1", [revokedAfterApprovalRequirement])).rows[0].status, "SATISFIED");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM deployment_outbox_events WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT' AND status = 'PENDING'", [revokedAfterApproval.id])).rows[0].count, 1);
  // No raw-SQL "claim, then the lifecycle fence rejects a retained worker" scenario here:
  // deployment_approval_execution_worker_gate_trigger, deployment_approval_outbox_claim_gate_trigger,
  // and deployment_approval_execution_commit_gate_trigger are all removed (Aurora DSQL rejects CREATE
  // TRIGGER/CREATE FUNCTION outright -- see V017's/V020's removal comments), and there is no retained
  // worker left in this rewrite for any of them to reject. execute()'s own activeProject()/
  // approvalExecutionEligible()/compatibleApprovalWorker() pre-checks cover the real Java write path;
  // the worker started below reaches this event through that real path and calls
  // blockApprovalExecution() once it finds the revoked evidence, the same way it would for any
  // evidence invalidation discovered mid-flight.
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

  // The predecessor-database simulation formerly here -- stopping the API and worker, dropping every
  // "_progress"/backfill/legacy-correlation table this domain step removes, deleting their owning
  // migrations' hive_schema_migrations markers, and restarting the API to observe the resumable
  // backfill machinery replay and catch up every table -- is removed, not redesigned: that whole
  // migration-replay backfill layer is itself removed -- see
  // PostgresDeploymentRepository.reconcileApprovalUpgrade()'s comment for the shared reasoning -- and
  // the scenario is now structurally unreachable, not just untested, for the same reason every other
  // backfill scenario this file removed is: every fact those migrations backfilled is instead written
  // synchronously, from the start, by this rewrite's single unified write path.
  await worker.stop();
  worker = undefined;
  // No raw-SQL audit-insert watermark/correlation-fill assertions here:
  // deployment_approval_audit_projection_watermark_trigger and deployment_approval_audit_correlation_
  // insert are both removed (see V018's/V025's removal comments) -- audit() (PostgresDeploymentRepository
  // .java) already calls touchProjection() unconditionally for every real audit insert, and nothing
  // reads facts->>'correlationId' anywhere in this codebase (the real correlation_id column is what
  // every actual reader uses). A raw SQL insert bypassing audit() entirely, the scenario these
  // assertions forced, has no equivalent guard once the database no longer performs any of this logic.
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
