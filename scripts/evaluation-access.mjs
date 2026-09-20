import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { createIsolatedDatabase, postgresClient, startIsolatedLocalService, startLocalEvaluationWorker } from "./local-service.mjs";

const requester = "00000000-0000-0000-0000-000000000001";
const outsider = "d1300000-0000-0000-0000-000000000005";
const viewer = "00000000-0000-0000-0000-000000000002";
const project = "50000000-0000-0000-0000-000000000001";
const privateProject = "50000000-0000-0000-0000-000000000003";
const runSuffix = Date.now().toString(36);
let endpoint = "";

async function graphql(service, principal, query, variables) {
  const response = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.errors, undefined, JSON.stringify(body.errors));
  return body.data;
}

async function publishAgentFixture(service, displayName = "M16 local evaluation fixture", slug = `m16-evaluation-${runSuffix}`) {
  const created = await graphql(service, requester,
    "mutation Create($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId revision } problems { code } } }",
    { input: { projectId: project, displayName, slug } });
  assert.deepEqual(created.createAgentDraft.problems, []);
  const agentId = created.createAgentDraft.agentDraft.agentId;
  const document = {
    general: { displayName, description: "Deterministic local evaluation target." },
    instructions: { source: "# local evaluation fixture", language: "markdown" }, harness: { source: "def run(value): return value", language: "python" },
    model: { reference: "model:local-safe-chat@v2" }, tools: { source: "{}", language: "json" }, skills: { source: "export const skills = [];", language: "javascript" },
    capabilities: { source: "export type Capability = string;", language: "typescript" }, subagents: { enabled: false }, memory: { strategy: "project" },
    guardrails: { source: "# local guardrail", language: "shell" }, identity: { persona: "" }, observability: { source: "<observability/>", language: "xml" },
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
  return published.publishAgentDraft.agentVersion.id;
}

// The run and its four fact lists, every one a generated entity read ordered by its own key.
const RUN_DETAIL = `query Run($id: String!) {
  evaluationRuns(filters: { id: { eq: $id } }) { nodes { id projectId lifecycleStatus generation outcomeCategory outcomeCode durationMillis failureSummary sourceRunId deploymentEvidenceDisposition target { agentContentDigest } } }
  evaluationCaseRuns(filters: { runId: { eq: $id } }, orderBy: { ordinal: ASC, id: ASC }) { nodes { lifecycleStatus passed failureCode } }
  evaluationMetricResults(filters: { runId: { eq: $id } }, orderBy: { metricCode: ASC, id: ASC }) { nodes { metricCode value threshold passed } }
  evaluationArtifactMetadata(filters: { runId: { eq: $id } }, orderBy: { artifactKind: ASC, id: ASC }) { nodes { artifactKind contentDigest mediaType byteLength } }
  evaluationAuditEvents(filters: { runId: { eq: $id } }, orderBy: { occurredAt: DESC, id: DESC }) { nodes { action summary } }
}`;

async function waitFor(service, runId, expectedStatus, expectedOutcome) {
  const deadline = Date.now() + 25_000;
  while (Date.now() < deadline) {
    const data = await graphql(service, requester, RUN_DETAIL, { id: runId });
    const value = data.evaluationRuns.nodes[0];
    if (value?.lifecycleStatus === expectedStatus && value?.outcomeCategory === expectedOutcome) {
      return { ...value, cases: data.evaluationCaseRuns.nodes, metrics: data.evaluationMetricResults.nodes, artifacts: data.evaluationArtifactMetadata.nodes, audit: data.evaluationAuditEvents.nodes };
    }
    await new Promise((resolve) => setTimeout(resolve, 140));
  }
  throw new Error(`Evaluation ${runId} did not reach ${expectedStatus}/${expectedOutcome}.`);
}

async function queue(service, versionId, target) {
  const value = await graphql(service, requester,
    "mutation Run($input: RunEvaluationInput!) { runEvaluation(input: $input) { run { id } problems { code } } }",
    { input: { projectId: project, definitionVersionId: versionId, targetKind: target.targetKind, targetId: target.targetId, environmentDefinitionVersionId: target.environmentDefinitionVersionId, idempotencyKey: randomUUID() } });
  assert.deepEqual(value.runEvaluation.problems, []);
  return value.runEvaluation.run.id;
}

async function publishUpdatedVersion(service, definitionId, document) {
  const current = await graphql(service, requester,
    "query Draft($id: String!) { evaluationDefinitions(filters: { id: { eq: $id } }) { nodes { draft { revision } } } }", { id: definitionId });
  const saved = await graphql(service, requester,
    "mutation Save($input: UpdateEvaluationDefinitionDraftInput!) { updateEvaluationDefinitionDraft(input: $input) { definition { draft { revision validationStatus } } problems { code } } }",
    { input: { definitionId, expectedRevision: current.evaluationDefinitions.nodes[0].draft.revision, document, idempotencyKey: randomUUID() } });
  assert.deepEqual(saved.updateEvaluationDefinitionDraft.problems, []);
  const published = await graphql(service, requester,
    "mutation Publish($input: PublishEvaluationDefinitionDraftInput!) { publishEvaluationDefinitionDraft(input: $input) { version { id } problems { code } } }",
    { input: { definitionId, expectedRevision: saved.updateEvaluationDefinitionDraft.definition.draft.revision, idempotencyKey: randomUUID() } });
  assert.deepEqual(published.publishEvaluationDefinitionDraft.problems, []);
  return published.publishEvaluationDefinitionDraft.version.id;
}

// The candidate targets of a published version: a computed field on the project, over the
// generated `evaluationTargetProjections` rows. The set is bounded by the project's own agent
// versions and deployments, so the server answers all of it at once.
async function compatibleTargets(service, principal, definitionVersionId) {
  const data = await graphql(service, principal,
    "query Targets($project: String!, $version: String!) { projects(filters: { id: { eq: $project } }) { nodes { compatibleEvaluationTargets(definitionVersionId: $version) { targetKind targetId agentVersionId environmentDefinitionVersionId logicalEnvironmentClass displayName } } } }",
    { project, version: definitionVersionId });
  return data.projects.nodes[0]?.compatibleEvaluationTargets ?? null;
}

async function authoritativeTargetRows(client) {
  const rows = await client.query(`SELECT target_kind AS "targetKind", target_id AS "targetId", agent_version_id AS "agentVersionId", environment_definition_version_id AS "environmentDefinitionVersionId", logical_environment_class AS "logicalEnvironmentClass", display_name AS "displayName" FROM (
    SELECT 'AGENT_VERSION'::text AS target_kind, versioned.id AS target_id, versioned.id AS agent_version_id, environment.id AS environment_definition_version_id, environment.logical_environment_class, agent.display_name
    FROM agent_versions versioned JOIN agents agent ON agent.id = versioned.agent_id
    JOIN environment_definition_versions environment ON environment.catalog_release_id = versioned.catalog_release_id WHERE agent.project_id = $1
    UNION ALL
    SELECT 'DEPLOYMENT'::text, deployment.id, deployment.agent_version_id, environment.id, environment.logical_environment_class, 'Deployment ' || deployment.id::text
    FROM deployments deployment JOIN environment_definition_versions environment ON environment.id = deployment.environment_definition_version_id WHERE deployment.project_id = $1
  ) source ORDER BY "targetKind", "displayName", "targetId", "environmentDefinitionVersionId"`, [project]);
  return rows.rows;
}

const database = await createIsolatedDatabase("hive_m16_access");
let service;
let worker;
let client;
try {
  service = await startIsolatedLocalService(database.name);
  endpoint = `http://127.0.0.1:${service.port}/graphql`;
  client = await postgresClient(database.name);
  await client.query("INSERT INTO platform_role_assignments (principal_id, role_code) VALUES ($1, 'PLATFORM_ADMIN')", [requester]);
  const agentVersionId = await publishAgentFixture(service, "M16 equal evaluation target", `m16-evaluation-primary-${runSuffix}`);
  await publishAgentFixture(service, "M16 equal evaluation target", `m16-evaluation-secondary-${runSuffix}`);
  const createInput = { projectId: project, slug: `local-evaluation-${runSuffix}`, document: null, idempotencyKey: `create-${runSuffix}` };
  const created = await graphql(service, requester,
    "mutation Create($input: CreateEvaluationDefinitionInput!) { createEvaluationDefinition(input: $input) { definition { id draft { revision validationStatus } } problems { code } } }",
    { input: createInput });
  assert.deepEqual(created.createEvaluationDefinition.problems, []);
  const definitionId = created.createEvaluationDefinition.definition.id;
  assert.equal(created.createEvaluationDefinition.definition.draft.validationStatus, "VALID");
  const createdReplay = await graphql(service, requester,
    "mutation Create($input: CreateEvaluationDefinitionInput!) { createEvaluationDefinition(input: $input) { definition { id } problems { code } } }", { input: createInput });
  assert.equal(createdReplay.createEvaluationDefinition.definition.id, definitionId);
  const publishInput = { definitionId, expectedRevision: created.createEvaluationDefinition.definition.draft.revision, idempotencyKey: `initial-publish-${runSuffix}` };
  const published = await graphql(service, requester,
    "mutation Publish($input: PublishEvaluationDefinitionDraftInput!) { publishEvaluationDefinitionDraft(input: $input) { version { id versionNumber } problems { code } } }",
    { input: publishInput });
  assert.deepEqual(published.publishEvaluationDefinitionDraft.problems, []);
  const versionId = published.publishEvaluationDefinitionDraft.version.id;
  const publishedReplay = await graphql(service, requester,
    "mutation Publish($input: PublishEvaluationDefinitionDraftInput!) { publishEvaluationDefinitionDraft(input: $input) { version { id } problems { code } } }", { input: publishInput });
  assert.equal(publishedReplay.publishEvaluationDefinitionDraft.version.id, versionId);
  const secondDefinition = await graphql(service, requester,
    "mutation CreateSecond($input: CreateEvaluationDefinitionInput!) { createEvaluationDefinition(input: $input) { definition { id } problems { code } } }",
    { input: { projectId: project, slug: `page-evaluation-${runSuffix}`, document: null, idempotencyKey: `page-${runSuffix}` } });
  assert.deepEqual(secondDefinition.createEvaluationDefinition.problems, []);
  const DEFINITION_PAGE = "query Definitions($project: String!, $page: Int!) { evaluationDefinitions(filters: { projectId: { eq: $project } }, orderBy: { createdAt: DESC, id: DESC }, pagination: { page: { limit: 1, page: $page } }) { nodes { id } paginationInfo { pages current total } } }";
  const definitionPage = await graphql(service, requester, DEFINITION_PAGE, { project, page: 0 });
  assert.equal(definitionPage.evaluationDefinitions.nodes.length, 1);
  assert(definitionPage.evaluationDefinitions.paginationInfo.pages > 1);
  const repeatedDefinitionPage = await graphql(service, requester, DEFINITION_PAGE, { project, page: 0 });
  assert.deepEqual(repeatedDefinitionPage.evaluationDefinitions, definitionPage.evaluationDefinitions);
  const nextDefinitionPage = await graphql(service, requester, DEFINITION_PAGE, { project, page: 1 });
  assert.notEqual(nextDefinitionPage.evaluationDefinitions.nodes[0]?.id, definitionPage.evaluationDefinitions.nodes[0].id);
  // Every definition of the project is reached exactly once across the pages, in one order.
  const seenDefinitions = [];
  for (let number = 0; number < definitionPage.evaluationDefinitions.paginationInfo.pages; number += 1) {
    const value = await graphql(service, requester, DEFINITION_PAGE, { project, page: number });
    seenDefinitions.push(...value.evaluationDefinitions.nodes.map((node) => node.id));
  }
  assert.equal(new Set(seenDefinitions).size, seenDefinitions.length);
  assert.equal(seenDefinitions.length, definitionPage.evaluationDefinitions.paginationInfo.total);
  assert(seenDefinitions.includes(definitionId));
  const viewerMembership = randomUUID();
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, revision) VALUES ($1, '10000000-0000-0000-0000-000000000001', $2, CURRENT_TIMESTAMP, 1)", [randomUUID(), viewer]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP, 1)", [viewerMembership, project, viewer]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AUDITOR')", [viewerMembership]);
  const redacted = await graphql(service, viewer,
    "query Redacted($definition: String!, $version: String!) { evaluationDefinitions(filters: { id: { eq: $definition } }) { nodes { canAuthor canPublish draft { canonicalDocument diagnostics { code } } latestVersion { canonicalDocument } } } evaluationDefinitionVersions(filters: { id: { eq: $version } }) { nodes { canonicalDocument } } }",
    { definition: definitionId, version: versionId });
  const redactedDefinition = redacted.evaluationDefinitions.nodes[0];
  assert.equal(redactedDefinition.canAuthor, false);
  assert.equal(redactedDefinition.canPublish, false);
  assert.equal(redactedDefinition.draft.canonicalDocument, "");
  assert.deepEqual(redactedDefinition.draft.diagnostics, []);
  assert.equal(redactedDefinition.latestVersion.canonicalDocument, "");
  assert.equal(redacted.evaluationDefinitionVersions.nodes[0].canonicalDocument, "");
  const authored = await graphql(service, requester,
    "query Authored($definition: String!) { evaluationDefinitions(filters: { id: { eq: $definition } }) { nodes { canAuthor canPublish draft { canonicalDocument } } } }",
    { definition: definitionId });
  assert.equal(authored.evaluationDefinitions.nodes[0].canAuthor, true);
  assert.equal(authored.evaluationDefinitions.nodes[0].canPublish, true);
  assert(authored.evaluationDefinitions.nodes[0].draft.canonicalDocument.length > 0);
  const agentProjection = await client.query("SELECT count(*)::int AS count FROM evaluation_target_projections WHERE target_kind = 'AGENT_VERSION' AND target_id = $1", [agentVersionId]);
  assert(agentProjection.rows[0].count > 0, "Publishing an agent version must commit its derived target rows.");
  const visibleDenied = await graphql(service, viewer,
    "mutation Save($input: UpdateEvaluationDefinitionDraftInput!) { updateEvaluationDefinitionDraft(input: $input) { problems { code } } }",
    { input: { definitionId, expectedRevision: created.createEvaluationDefinition.definition.draft.revision, document: "{}", idempotencyKey: randomUUID() } });
  assert.deepEqual(visibleDenied.updateEvaluationDefinitionDraft.problems.map((problem) => problem.code), ["FORBIDDEN"]);
  const hiddenDenied = await graphql(service, outsider,
    "mutation Save($input: UpdateEvaluationDefinitionDraftInput!) { updateEvaluationDefinitionDraft(input: $input) { problems { code } } }",
    { input: { definitionId, expectedRevision: created.createEvaluationDefinition.definition.draft.revision, document: "{}", idempotencyKey: randomUUID() } });
  assert.deepEqual(hiddenDenied.updateEvaluationDefinitionDraft.problems.map((problem) => problem.code), ["NOT_FOUND"]);
  const targets = await compatibleTargets(service, requester, versionId);
  const target = targets.find((value) => value.targetKind === "AGENT_VERSION");
  assert(target, "The local fixture must contain an immutable agent version target.");
  const productionTarget = targets.find((value) => value.targetKind === "AGENT_VERSION" && value.logicalEnvironmentClass === "PRODUCTION");
  assert(productionTarget, "The local fixture must contain a production immutable target.");
  assert.deepEqual(await compatibleTargets(service, requester, versionId), targets, "The candidate target list is stable.");
  // A version this project does not own offers no target here.
  assert.deepEqual(await compatibleTargets(service, requester, randomUUID()), []);
  // `EVALUATION_RUN.RUN` gates the list: the AUDITOR holds only the two view capabilities.
  assert.deepEqual(await compatibleTargets(service, viewer, versionId), []);
  // An outsider sees no project here, so there is nothing to ask.
  assert.equal(await compatibleTargets(service, outsider, versionId), null);
  const deploymentRequest = await graphql(service, requester,
    "mutation Deploy($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id lifecycleStatus } problems { code } } }",
    { input: { agentVersionId, environmentDefinitionVersionId: productionTarget.environmentDefinitionVersionId, strategy: "ROLLING", idempotencyKey: randomUUID() } });
  assert.deepEqual(deploymentRequest.deployAgentVersion.problems, []);
  const deploymentId = deploymentRequest.deployAgentVersion.deployment.id;
  const deploymentTargets = await compatibleTargets(service, requester, versionId);
  const deploymentTarget = deploymentTargets.find((value) => value.targetKind === "DEPLOYMENT" && value.targetId === deploymentId);
  assert(deploymentTarget, "A deployment target must retain its exact immutable environment.");
  const deploymentProjection = await client.query("SELECT count(*)::int AS count FROM evaluation_target_projections WHERE target_kind = 'DEPLOYMENT' AND target_id = $1 AND environment_definition_version_id = $2", [deploymentId, deploymentTarget.environmentDefinitionVersionId]);
  assert.equal(deploymentProjection.rows[0].count, 1);
  const traversedTargets = await compatibleTargets(service, requester, versionId);
  const authoritativeTargets = await authoritativeTargetRows(client);
  assert.deepEqual(traversedTargets, authoritativeTargets);
  const queued = await graphql(service, requester,
    "mutation Run($input: RunEvaluationInput!) { runEvaluation(input: $input) { run { id lifecycleStatus generation } problems { code } } }",
    { input: { projectId: project, definitionVersionId: versionId, targetKind: target.targetKind, targetId: target.targetId, environmentDefinitionVersionId: target.environmentDefinitionVersionId, idempotencyKey: randomUUID() } });
  assert.deepEqual(queued.runEvaluation.problems, []);
  const canceled = await graphql(service, requester,
    "mutation Cancel($input: CancelEvaluationInput!) { cancelEvaluation(input: $input) { run { id lifecycleStatus generation outcomeCategory } problems { code } } }",
    { input: { runId: queued.runEvaluation.run.id, expectedGeneration: queued.runEvaluation.run.generation, idempotencyKey: randomUUID(), reason: "must-not-reach-audit" } });
  assert.deepEqual(canceled.cancelEvaluation.problems, []);
  assert.equal(canceled.cancelEvaluation.run.lifecycleStatus, "CANCELED");
  assert.equal(canceled.cancelEvaluation.run.outcomeCategory, "CANCELED");
  worker = await startLocalEvaluationWorker(database.name);
  const passingRunId = await queue(service, versionId, target);
  const completed = await waitFor(service, passingRunId, "COMPLETED", "PASSED");
  assert.equal(completed.cases[0].passed, true);
  assert.equal(completed.metrics[0].passed, true);
  assert.equal(completed.artifacts[0].artifactKind, "LOCAL_SUMMARY");
  assert(completed.audit.every((event) => !event.summary.includes("ready")));
  const deploymentCompleted = await waitFor(service, await queue(service, versionId, deploymentTarget), "COMPLETED", "PASSED");
  assert.equal(deploymentCompleted.deploymentEvidenceDisposition, "APPENDED");
  const rerun = await graphql(service, requester,
    "mutation Rerun($input: RerunEvaluationInput!) { rerunEvaluation(input: $input) { run { id sourceRunId } problems { code } } }",
    { input: { runId: completed.id, idempotencyKey: randomUUID() } });
  assert.deepEqual(rerun.rerunEvaluation.problems, []);
  assert.equal(rerun.rerunEvaluation.run.sourceRunId, completed.id);
  await waitFor(service, rerun.rerunEvaluation.run.id, "COMPLETED", "PASSED");
  const mismatchVersion = await publishUpdatedVersion(service, definitionId,
    "{\"schemaVersion\":\"hive.evaluation-definition/v1\",\"cases\":[{\"key\":\"mismatch\",\"prompt\":\"Return ready.\",\"expectedOutput\":\"ready\",\"fixture\":{\"output\":\"not-ready\"}}],\"metrics\":[{\"code\":\"EXACT_MATCH_RATE\",\"threshold\":1}],\"requiredArtifacts\":[\"LOCAL_SUMMARY\"],\"compatibleTargetKinds\":[\"AGENT_VERSION\"],\"compatibleLogicalEnvironmentClasses\":[\"DEVELOPMENT\",\"STAGING\",\"PRODUCTION\"],\"localRunner\":{\"adapter\":\"LOCAL_PROMPT_CASE_V1\"}}");
  const mismatch = await waitFor(service, await queue(service, mismatchVersion, target), "COMPLETED", "CASE_FAILED");
  assert.equal(mismatch.cases[0].failureCode, "EXACT_MATCH_FAILED");
  const targetFailureVersion = await publishUpdatedVersion(service, definitionId,
    "{\"schemaVersion\":\"hive.evaluation-definition/v1\",\"cases\":[{\"key\":\"target\",\"prompt\":\"Return ready.\",\"expectedOutput\":\"ready\",\"fixture\":{\"targetFailureCode\":\"TARGET_UNAVAILABLE\"}}],\"metrics\":[{\"code\":\"EXACT_MATCH_RATE\",\"threshold\":1}],\"requiredArtifacts\":[\"LOCAL_SUMMARY\"],\"compatibleTargetKinds\":[\"AGENT_VERSION\"],\"compatibleLogicalEnvironmentClasses\":[\"DEVELOPMENT\",\"STAGING\",\"PRODUCTION\"],\"localRunner\":{\"adapter\":\"LOCAL_PROMPT_CASE_V1\"}}");
  await waitFor(service, await queue(service, targetFailureVersion, target), "FAILED", "TARGET_FAILED");
  const runnerFailureVersion = await publishUpdatedVersion(service, definitionId,
    "{\"schemaVersion\":\"hive.evaluation-definition/v1\",\"cases\":[{\"key\":\"runner\",\"prompt\":\"Return ready.\",\"expectedOutput\":\"ready\",\"fixture\":{\"output\":\"ready\"}}],\"metrics\":[{\"code\":\"EXACT_MATCH_RATE\",\"threshold\":1}],\"requiredArtifacts\":[\"LOCAL_SUMMARY\"],\"compatibleTargetKinds\":[\"AGENT_VERSION\"],\"compatibleLogicalEnvironmentClasses\":[\"DEVELOPMENT\",\"STAGING\",\"PRODUCTION\"],\"localRunner\":{\"adapter\":\"LOCAL_PROMPT_CASE_V1\",\"failureFixture\":\"RUNNER_FAILURE\"}}");
  await waitFor(service, await queue(service, runnerFailureVersion, target), "FAILED", "RUNNER_FAILED");
  const VERSION_PAGE = "query Versions($definition: String!, $page: Int!) { evaluationDefinitionVersions(filters: { definitionId: { eq: $definition } }, orderBy: { versionNumber: DESC, id: DESC }, pagination: { page: { limit: 1, page: $page } }) { nodes { id versionNumber } paginationInfo { pages current total } } }";
  const versionPage = await graphql(service, requester, VERSION_PAGE, { definition: definitionId, page: 0 });
  assert.equal(versionPage.evaluationDefinitionVersions.nodes.length, 1);
  assert(versionPage.evaluationDefinitionVersions.paginationInfo.pages > 1);
  const nextVersionPage = await graphql(service, requester, VERSION_PAGE, { definition: definitionId, page: 1 });
  assert.notEqual(nextVersionPage.evaluationDefinitionVersions.nodes[0]?.id, versionPage.evaluationDefinitionVersions.nodes[0].id);
  // Newest version first, and every version reached exactly once.
  assert(versionPage.evaluationDefinitionVersions.nodes[0].versionNumber > nextVersionPage.evaluationDefinitionVersions.nodes[0].versionNumber);
  const seenVersions = [];
  for (let number = 0; number < versionPage.evaluationDefinitionVersions.paginationInfo.pages; number += 1) {
    const value = await graphql(service, requester, VERSION_PAGE, { definition: definitionId, page: number });
    seenVersions.push(...value.evaluationDefinitionVersions.nodes.map((node) => node.id));
  }
  assert.equal(new Set(seenVersions).size, seenVersions.length);
  assert.equal(seenVersions.length, versionPage.evaluationDefinitionVersions.paginationInfo.total);
  assert(seenVersions.includes(mismatchVersion));
  // The version comparison is a computed field of the left version, refused across definitions.
  const comparison = await graphql(service, requester,
    "query Compare($left: String!, $right: String!) { evaluationDefinitionVersions(filters: { id: { eq: $left } }) { nodes { comparison(rightVersionId: $right) { left { id } right { id } } } } }",
    { left: versionId, right: mismatchVersion });
  assert.deepEqual(comparison.evaluationDefinitionVersions.nodes[0].comparison, { left: { id: versionId }, right: { id: mismatchVersion } });
  const foreignComparison = await graphql(service, requester,
    "query Compare($left: String!, $right: String!) { evaluationDefinitionVersions(filters: { id: { eq: $left } }) { nodes { comparison(rightVersionId: $right) { right { id } } } } }",
    { left: versionId, right: randomUUID() });
  assert.equal(foreignComparison.evaluationDefinitionVersions.nodes[0].comparison, null);
  // One version's run usage.
  const usage = await graphql(service, requester,
    "query Usage($version: String!) { evaluationRuns(filters: { definitionVersionId: { eq: $version } }, orderBy: { createdAt: DESC, id: DESC }, pagination: { page: { limit: 50, page: 0 } }) { nodes { id definitionVersionId } } }",
    { version: versionId });
  assert(usage.evaluationRuns.nodes.length > 0);
  assert(usage.evaluationRuns.nodes.every((run) => run.definitionVersionId === versionId));
  // The status filter.
  const completedRuns = await graphql(service, requester,
    "query Runs($project: String!) { evaluationRuns(filters: { projectId: { eq: $project }, lifecycleStatus: { eq: \"COMPLETED\" } }, orderBy: { createdAt: DESC, id: DESC }) { nodes { id lifecycleStatus } } }",
    { project });
  assert(completedRuns.evaluationRuns.nodes.length > 0);
  assert(completedRuns.evaluationRuns.nodes.every((run) => run.lifecycleStatus === "COMPLETED"));
  // Tenant isolation: an outsider reads no evaluation row of this project or the private one.
  const hidden = await graphql(service, outsider, "query Hidden($id: String!) { evaluationDefinitions(filters: { id: { eq: $id } }) { nodes { id } } }", { id: definitionId });
  assert.deepEqual(hidden.evaluationDefinitions.nodes, []);
  const crossTenant = await graphql(service, outsider, "query Private($project: String!) { evaluationDefinitions(filters: { projectId: { eq: $project } }) { nodes { id } } }", { project: privateProject });
  assert.deepEqual(crossTenant.evaluationDefinitions.nodes, []);
  const hiddenRuns = await graphql(service, outsider, "query HiddenRuns { evaluationRuns { nodes { id } } evaluationCaseRuns { nodes { id } } evaluationMetricResults { nodes { id } } evaluationArtifactMetadata { nodes { id } } evaluationAuditEvents { nodes { id } } evaluationTargetSnapshots { nodes { runId } } evaluationTargetProjections { nodes { targetId } } }", {});
  for (const field of ["evaluationRuns", "evaluationCaseRuns", "evaluationMetricResults", "evaluationArtifactMetadata", "evaluationAuditEvents", "evaluationTargetSnapshots", "evaluationTargetProjections"]) {
    assert.deepEqual(hiddenRuns[field].nodes, [], `an outsider must read no ${field} row`);
  }
} finally {
  if (worker) await worker.stop();
  if (service) await service.stop();
  if (client) await client.end();
  await database.drop();
}
