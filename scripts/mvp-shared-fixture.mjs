import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { createIsolatedDatabase, postgresClient, startIsolatedLocalService, startLocalDeploymentWorker, startLocalEvaluationWorker } from "./local-service.mjs";

const requester = "00000000-0000-0000-0000-000000000001";
const approver = "f0420000-0000-0000-0000-000000000001";
const organization = "10000000-0000-0000-0000-000000000001";
const project = "50000000-0000-0000-0000-000000000001";
const run = Date.now().toString(36);
const document = { general: { displayName: "M17 shared fixture", description: "Local MVP acceptance fixture." }, instructions: { source: "# local", language: "markdown" }, harness: { source: "def run(value): return value", language: "python" }, model: { reference: "model:local-safe-chat@v2" }, tools: { source: "{}", language: "json" }, skills: { source: "export const skills = [];", language: "javascript" }, capabilities: { source: "export type Capability = string;", language: "typescript" }, subagents: { enabled: false }, memory: { strategy: "project" }, guardrails: { source: "# local", language: "shell" }, identity: { persona: "" }, observability: { source: "<local/>", language: "xml" }, limits: { maxTokens: 256 }, evaluations: { required: false }, dependencies: ["model:local-safe-chat@v2"] };
const evaluationDocument = JSON.stringify({ schemaVersion: "hive.evaluation-definition/v1", cases: [{ key: "local", prompt: "Return ready.", expectedOutput: "ready", fixture: { output: "ready" } }], metrics: [{ code: "EXACT_MATCH_RATE", threshold: 1 }], requiredArtifacts: ["LOCAL_SUMMARY"], compatibleTargetKinds: ["AGENT_VERSION", "DEPLOYMENT"], compatibleLogicalEnvironmentClasses: ["DEVELOPMENT", "STAGING", "PRODUCTION"], localRunner: { adapter: "LOCAL_PROMPT_CASE_V1" } });
const database = await createIsolatedDatabase("m17_shared_mvp");
let service;
let client;
let deploymentWorker;
let evaluationWorker;
let endpoint;

async function graphql(principal, operationName, query, variables) {
  const response = await fetch(endpoint, { method: "POST", headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` }, body: JSON.stringify({ operationName, query, variables }) });
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.errors, undefined, JSON.stringify(body.errors));
  return { data: body.data, requestId: response.headers.get("x-request-id") };
}

async function waitForDeployment(id, status) {
  const deadline = Date.now() + 25_000;
  while (Date.now() < deadline) {
    const value = await graphql(requester, "SharedDeployment", "query SharedDeployment($id: ID!) { deploymentProjection(deploymentId: $id, first: 50) { deployment { id revision lifecycleStatus runtimeHealth { status } rollbackTarget { agentVersionId } } } }", { id });
    const deployment = value.data.deploymentProjection?.deployment;
    if (deployment?.lifecycleStatus === status) return deployment;
    await new Promise((resolve) => setTimeout(resolve, 140));
  }
  throw new Error(`Shared MVP deployment ${id} did not reach ${status}.`);
}

async function waitForEvaluation(id) {
  const deadline = Date.now() + 25_000;
  while (Date.now() < deadline) {
    const value = await graphql(requester, "SharedEvaluation", "query SharedEvaluation($id: String!) { evaluationRuns(filters: { id: { eq: $id } }) { nodes { lifecycleStatus outcomeCategory } } }", { id });
    const run = value.data.evaluationRuns.nodes[0];
    if (run?.lifecycleStatus === "COMPLETED") return run;
    await new Promise((resolve) => setTimeout(resolve, 140));
  }
  throw new Error(`Shared MVP evaluation ${id} did not complete.`);
}

try {
  service = await startIsolatedLocalService(database.name); endpoint = `http://127.0.0.1:${service.port}/graphql`;
  client = await postgresClient(database.name);
  deploymentWorker = await startLocalDeploymentWorker(database.name, { HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "100", HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "80" });
  evaluationWorker = await startLocalEvaluationWorker(database.name, { HIVE_EVALUATION_WORKER_INITIAL_DELAY_MILLIS: "100", HIVE_EVALUATION_WORKER_INTERVAL_MILLIS: "80" });
  await client.query("INSERT INTO platform_role_assignments (principal_id, role_code) VALUES ($1, 'PLATFORM_ADMIN') ON CONFLICT DO NOTHING", [requester]);
  await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, 'm17-shared-approver', 'M17 Shared Approver', 'm17-shared-approver@local.invalid') ON CONFLICT (id) DO NOTHING", [approver]);
  await client.query("SELECT set_config('hive.m14_approval_role_assignment_actor', $1, FALSE)", [requester]);
  const organizationMembership = randomUUID(); const projectMembership = randomUUID();
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1)", [organizationMembership, organization, approver]);
  await client.query("INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_MEMBER')", [organizationMembership]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1)", [projectMembership, project, approver]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'DEPLOYMENT_APPROVER')", [projectMembership]);
  await client.query("SELECT set_config('hive.m14_approval_role_assignment_actor', '', FALSE)");

  const created = await graphql(requester, "SharedCreateAgent", "mutation SharedCreateAgent($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId revision } problems { code } } }", { input: { projectId: project, displayName: "M17 shared fixture", slug: `m17-shared-${run}` } });
  assert.deepEqual(created.data.createAgentDraft.problems, []);
  const agentId = created.data.createAgentDraft.agentDraft.agentId;
  const saved = await graphql(requester, "SharedSaveAgent", "mutation SharedSaveAgent($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }", { input: { projectId: project, agentId, expectedRevision: 1, document } });
  assert.deepEqual(saved.data.updateAgentDraft.problems, []);
  const validated = await graphql(requester, "SharedValidateAgent", "mutation SharedValidateAgent($input: ValidateAgentDraftInput!) { validateAgentDraft(input: $input) { agentDraft { revision validationStatus } problems { code } } }", { input: { projectId: project, agentId, expectedRevision: saved.data.updateAgentDraft.agentDraft.revision } });
  assert.equal(validated.data.validateAgentDraft.agentDraft.validationStatus, "VALID");
  const published = await graphql(requester, "SharedPublishAgent", "mutation SharedPublishAgent($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentVersion { id } problems { code } } }", { input: { projectId: project, agentId, expectedRevision: validated.data.validateAgentDraft.agentDraft.revision, warningsAcknowledged: true } });
  assert.deepEqual(published.data.publishAgentDraft.problems, []);
  const versionId = published.data.publishAgentDraft.agentVersion.id;
  const environments = await graphql(requester, "SharedEnvironments", "query SharedEnvironments($id: ID!) { deploymentEnvironmentDefinitionVersions(agentVersionId: $id, first: 20) { edges { node { id logicalEnvironmentClass } } } }", { id: versionId });
  const environment = (kind) => environments.data.deploymentEnvironmentDefinitionVersions.edges.map((edge) => edge.node).find((node) => node.logicalEnvironmentClass === kind).id;
  const development = environment("DEVELOPMENT"); const production = environment("PRODUCTION");

  const definition = await graphql(requester, "SharedCreateEvaluation", "mutation SharedCreateEvaluation($input: CreateEvaluationDefinitionInput!) { createEvaluationDefinition(input: $input) { definition { id draft { revision } } problems { code } } }", { input: { projectId: project, slug: `m17-shared-evaluation-${run}`, document: evaluationDocument, idempotencyKey: `m17-shared-definition-${run}` } });
  assert.deepEqual(definition.data.createEvaluationDefinition.problems, []);
  const evaluationVersion = await graphql(requester, "SharedPublishEvaluation", "mutation SharedPublishEvaluation($input: PublishEvaluationDefinitionDraftInput!) { publishEvaluationDefinitionDraft(input: $input) { version { id } problems { code } } }", { input: { definitionId: definition.data.createEvaluationDefinition.definition.id, expectedRevision: definition.data.createEvaluationDefinition.definition.draft.revision, idempotencyKey: `m17-shared-evaluation-publish-${run}` } });
  assert.deepEqual(evaluationVersion.data.publishEvaluationDefinitionDraft.problems, []);
  const agentEvaluation = await graphql(requester, "SharedRunAgentEvaluation", "mutation SharedRunAgentEvaluation($input: RunEvaluationInput!) { runEvaluation(input: $input) { run { id } problems { code } } }", { input: { projectId: project, definitionVersionId: evaluationVersion.data.publishEvaluationDefinitionDraft.version.id, targetKind: "AGENT_VERSION", targetId: versionId, environmentDefinitionVersionId: development, idempotencyKey: `m17-shared-agent-evaluation-${run}` } });
  assert.deepEqual(agentEvaluation.data.runEvaluation.problems, []); assert.equal((await waitForEvaluation(agentEvaluation.data.runEvaluation.run.id)).outcomeCategory, "PASSED");

  const requested = await graphql(requester, "SharedDeploy", "mutation SharedDeploy($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }", { input: { agentVersionId: versionId, environmentDefinitionVersionId: development, strategy: "REPLACE", idempotencyKey: `m17-shared-deploy-${run}` } });
  assert.deepEqual(requested.data.deployAgentVersion.problems, []);
  const active = await waitForDeployment(requested.data.deployAgentVersion.deployment.id, "ACTIVE"); assert.equal(active.runtimeHealth.status, "HEALTHY");
  const promoted = await graphql(requester, "SharedPromote", "mutation SharedPromote($input: PromoteDeploymentInput!) { promoteDeployment(input: $input) { deployment { id } problems { code } } }", { input: { deploymentId: active.id, expectedRevision: active.revision, idempotencyKey: `m17-shared-promote-${run}` } });
  assert.deepEqual(promoted.data.promoteDeployment.problems, []);

  const staged = await graphql(requester, "SharedApprovalDeployment", "mutation SharedApprovalDeployment($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }", { input: { agentVersionId: versionId, environmentDefinitionVersionId: production, strategy: "ROLLING", idempotencyKey: `m17-shared-approval-${run}` } });
  assert.deepEqual(staged.data.deployAgentVersion.problems, []);
  const deploymentEvaluation = await graphql(requester, "SharedRunDeploymentEvaluation", "mutation SharedRunDeploymentEvaluation($input: RunEvaluationInput!) { runEvaluation(input: $input) { run { id } problems { code } } }", { input: { projectId: project, definitionVersionId: evaluationVersion.data.publishEvaluationDefinitionDraft.version.id, targetKind: "DEPLOYMENT", targetId: staged.data.deployAgentVersion.deployment.id, environmentDefinitionVersionId: production, idempotencyKey: `m17-shared-deployment-evaluation-${run}` } });
  assert.deepEqual(deploymentEvaluation.data.runEvaluation.problems, []); assert.equal((await waitForEvaluation(deploymentEvaluation.data.runEvaluation.run.id)).outcomeCategory, "PASSED");
  const requirement = (await client.query("SELECT id, revision FROM deployment_approval_requirements WHERE deployment_id = $1", [staged.data.deployAgentVersion.deployment.id])).rows[0];
  assert(requirement, "The shared staging deployment must retain an approval requirement.");
  const approvalState = await graphql(approver, "SharedApprovalState", "query SharedApprovalState($id: ID!) { approvalRequirement(approvalRequirementId: $id) { decisionAvailable eligible requirement { status requiredDistinctApproverCount qualifyingApprovalCount } } }", { id: requirement.id });
  assert.equal(approvalState.data.approvalRequirement?.decisionAvailable, true, JSON.stringify(approvalState.data.approvalRequirement));
  const approved = await graphql(approver, "SharedApprove", "mutation SharedApprove($input: DecideDeploymentApprovalInput!) { decideDeploymentApproval(input: $input) { requirement { status } problems { code } } }", { input: { approvalRequirementId: requirement.id, expectedRevision: Number(requirement.revision), decision: "APPROVE", comment: "REVIEWED_CHANGE_SCOPE", idempotencyKey: randomUUID() } });
  assert.deepEqual(approved.data.decideDeploymentApproval.problems, []);

  const failed = await graphql(requester, "SharedFailure", "mutation SharedFailure($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }", { input: { agentVersionId: versionId, environmentDefinitionVersionId: development, strategy: "CANARY", idempotencyKey: `m17-shared-failure-${run}` } });
  assert.deepEqual(failed.data.deployAgentVersion.problems, []);
  await client.query("UPDATE deployment_outbox_events SET payload = '{\"mode\":\"FAILURE\"}'::jsonb WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [failed.data.deployAgentVersion.deployment.id]);
  const failedDetail = await waitForDeployment(failed.data.deployAgentVersion.deployment.id, "FAILED");
  const rolledBack = await graphql(requester, "SharedRollback", "mutation SharedRollback($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { deployment { id } problems { code } } }", { input: { deploymentId: failedDetail.id, targetAgentVersionId: active.rollbackTarget?.agentVersionId ?? versionId, expectedRevision: failedDetail.revision, reason: "Restore the observed local target.", productionConfirmation: "M15 local command binding", idempotencyKey: `m17-shared-rollback-${run}` } });
  assert.deepEqual(rolledBack.data.rollbackDeployment.problems, []);

  const audit = await graphql(requester, "SharedAuditTrace", "query SharedAuditTrace($filters: AuditEventProjectionFilterInput) { auditEventProjection(filters: $filters, orderBy: { occurredAt: DESC, projectionId: DESC }, pagination: { page: { limit: 100, page: 0 } }) { nodes { action correlationId resourceType resourceId resourceReferences } } }", { filters: { projectId: { eq: project }, occurredAt: { gte: "2020-01-01T00:00:00Z" } } });
  const events = audit.data.auditEventProjection.nodes.map((node) => ({ ...node, references: node.resourceReferences }));
  assert(events.some((event) => event.correlationId === created.requestId && event.references.some((reference) => reference.type === "AGENT" && reference.id === agentId)));
  assert(events.some((event) => event.references.some((reference) => reference.type === "AGENT_VERSION" && reference.id === versionId)));
  assert(events.some((event) => event.references.some((reference) => reference.type === "DEPLOYMENT" && reference.id === failedDetail.id)));
  assert(events.some((event) => event.references.some((reference) => reference.type === "EVALUATION_DEFINITION" && reference.id === definition.data.createEvaluationDefinition.definition.id)));
} finally {
  if (evaluationWorker) await evaluationWorker.stop();
  if (deploymentWorker) await deploymentWorker.stop();
  if (client) await client.end();
  if (service) await service.stop();
  await database.drop();
}
console.log("M17 shared local fixture publishes, evaluates, deploys, approves, observes, promotes, rolls back, and traces typed audit resources and correlation.");
process.exit(0);
