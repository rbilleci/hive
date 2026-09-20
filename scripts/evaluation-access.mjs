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

async function definition(service, id) {
  const data = await graphql(service, requester, "query Definition($id: ID!) { evaluationDefinition(definitionId: $id) { id projectId slug draft { revision validationStatus diagnostics { code } } latestVersion { id number } } }", { id });
  return data.evaluationDefinition;
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

async function waitFor(service, runId, expectedStatus, expectedOutcome) {
  const deadline = Date.now() + 25_000;
  while (Date.now() < deadline) {
    const data = await graphql(service, requester, "query Run($id: ID!) { evaluationRun(runId: $id) { id projectId lifecycleStatus generation outcomeCategory outcomeCode durationMillis failureSummary sourceRunId target { agentContentDigest } cases(first: 50) { edges { node { lifecycleStatus passed failureCode } } } metrics(first: 50) { edges { node { code value threshold passed } } } artifacts(first: 50) { edges { node { kind contentDigest mediaType byteLength } } } audit(first: 50) { edges { node { action summary } } } deploymentEvidenceDisposition } }", { id: runId });
    const value = data.evaluationRun;
    if (value?.lifecycleStatus === expectedStatus && value?.outcomeCategory === expectedOutcome) return value;
    await new Promise((resolve) => setTimeout(resolve, 140));
  }
  throw new Error(`Evaluation ${runId} did not reach ${expectedStatus}/${expectedOutcome}.`);
}

async function queue(service, versionId, target) {
  const value = await graphql(service, requester,
    "mutation Run($input: RunEvaluationInput!) { runEvaluation(input: $input) { run { id } problems { code } } }",
    { input: { projectId: project, definitionVersionId: versionId, targetKind: target.kind, targetId: target.id, environmentDefinitionVersionId: target.environmentDefinitionVersionId, idempotencyKey: randomUUID() } });
  assert.deepEqual(value.runEvaluation.problems, []);
  return value.runEvaluation.run.id;
}

async function publishUpdatedVersion(service, definitionId, document) {
  const current = await graphql(service, requester,
    "query Draft($id: ID!) { evaluationDefinition(definitionId: $id) { draft { revision } } }", { id: definitionId });
  const saved = await graphql(service, requester,
    "mutation Save($input: UpdateEvaluationDefinitionDraftInput!) { updateEvaluationDefinitionDraft(input: $input) { definition { draft { revision validationStatus } } problems { code } } }",
    { input: { definitionId, expectedRevision: current.evaluationDefinition.draft.revision, document, idempotencyKey: randomUUID() } });
  assert.deepEqual(saved.updateEvaluationDefinitionDraft.problems, []);
  const published = await graphql(service, requester,
    "mutation Publish($input: PublishEvaluationDefinitionDraftInput!) { publishEvaluationDefinitionDraft(input: $input) { version { id } problems { code } } }",
    { input: { definitionId, expectedRevision: saved.updateEvaluationDefinitionDraft.definition.draft.revision, idempotencyKey: randomUUID() } });
  assert.deepEqual(published.publishEvaluationDefinitionDraft.problems, []);
  return published.publishEvaluationDefinitionDraft.version.id;
}

async function allTargetPages(service, definitionVersionId) {
  let after = null;
  const values = [];
  do {
    const data = await graphql(service, requester,
      "query TargetPage($project: ID!, $version: ID!, $after: String) { evaluationTargets(projectId: $project, definitionVersionId: $version, after: $after, first: 1) { edges { cursor node { kind id agentVersionId environmentDefinitionVersionId logicalEnvironmentClass displayName } } hasNextPage endCursor } }",
      { project, version: definitionVersionId, after });
    const page = data.evaluationTargets;
    assert(page);
    values.push(...page.edges.map((edge) => edge.node));
    after = page.hasNextPage ? page.endCursor : null;
  } while (after !== null);
  return values;
}

async function authoritativeTargetRows(client) {
  const rows = await client.query(`SELECT target_kind AS kind, target_id AS id, agent_version_id AS "agentVersionId", environment_definition_version_id AS "environmentDefinitionVersionId", logical_environment_class AS "logicalEnvironmentClass", display_name AS "displayName" FROM (
    SELECT 'AGENT_VERSION'::text AS target_kind, versioned.id AS target_id, versioned.id AS agent_version_id, environment.id AS environment_definition_version_id, environment.logical_environment_class, agent.display_name
    FROM agent_versions versioned JOIN agents agent ON agent.id = versioned.agent_id
    JOIN environment_definition_versions environment ON environment.catalog_release_id = versioned.catalog_release_id WHERE agent.project_id = $1
    UNION ALL
    SELECT 'DEPLOYMENT'::text, deployment.id, deployment.agent_version_id, environment.id, environment.logical_environment_class, 'Deployment ' || deployment.id::text
    FROM deployments deployment JOIN environment_definition_versions environment ON environment.id = deployment.environment_definition_version_id WHERE deployment.project_id = $1
  ) source ORDER BY kind, "displayName", id, "environmentDefinitionVersionId"`, [project]);
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
    "mutation Publish($input: PublishEvaluationDefinitionDraftInput!) { publishEvaluationDefinitionDraft(input: $input) { version { id number } problems { code } } }",
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
  const definitionPage = await graphql(service, requester,
    "query Definitions($project: ID!, $after: String) { evaluationDefinitions(projectId: $project, after: $after, first: 1) { edges { cursor node { id } } hasNextPage endCursor } }",
    { project, after: null });
  assert.equal(definitionPage.evaluationDefinitions.edges.length, 1);
  assert.equal(definitionPage.evaluationDefinitions.hasNextPage, true);
  const repeatedDefinitionPage = await graphql(service, requester,
    "query Definitions($project: ID!, $after: String) { evaluationDefinitions(projectId: $project, after: $after, first: 1) { edges { cursor node { id } } hasNextPage endCursor } }",
    { project, after: null });
  assert.deepEqual(repeatedDefinitionPage.evaluationDefinitions, definitionPage.evaluationDefinitions);
  const nextDefinitionPage = await graphql(service, requester,
    "query Definitions($project: ID!, $after: String) { evaluationDefinitions(projectId: $project, after: $after, first: 1) { edges { cursor node { id } } hasNextPage endCursor } }",
    { project, after: definitionPage.evaluationDefinitions.endCursor });
  assert.notEqual(nextDefinitionPage.evaluationDefinitions.edges[0]?.node.id, definitionPage.evaluationDefinitions.edges[0].node.id);
  const transplantedDefinitionCursor = await graphql(service, requester,
    "query Versions($definition: ID!, $after: String) { evaluationDefinitionVersions(definitionId: $definition, after: $after, first: 1) { edges { node { id } } } }",
    { definition: definitionId, after: definitionPage.evaluationDefinitions.endCursor });
  assert.equal(transplantedDefinitionCursor.evaluationDefinitionVersions, null);
  const viewerMembership = randomUUID();
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, revision) VALUES ($1, '10000000-0000-0000-0000-000000000001', $2, CURRENT_TIMESTAMP, 1)", [randomUUID(), viewer]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP, 1)", [viewerMembership, project, viewer]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AUDITOR')", [viewerMembership]);
  const redacted = await graphql(service, viewer,
    "query Redacted($definition: ID!, $version: ID!) { evaluationDefinition(definitionId: $definition) { draft { canonicalDocument diagnostics { code } } latestVersion { canonicalDocument } } evaluationDefinitionVersion(versionId: $version) { canonicalDocument } }",
    { definition: definitionId, version: versionId });
  assert.equal(redacted.evaluationDefinition.draft.canonicalDocument, "");
  assert.deepEqual(redacted.evaluationDefinition.draft.diagnostics, []);
  assert.equal(redacted.evaluationDefinition.latestVersion.canonicalDocument, "");
  assert.equal(redacted.evaluationDefinitionVersion.canonicalDocument, "");
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
  const targets = await graphql(service, requester,
    "query Targets($project: ID!, $version: ID!) { evaluationTargets(projectId: $project, definitionVersionId: $version, first: 50) { edges { node { kind id agentVersionId environmentDefinitionVersionId logicalEnvironmentClass } } } }",
    { project, version: versionId });
  const target = targets.evaluationTargets.edges.map((edge) => edge.node).find((value) => value.kind === "AGENT_VERSION");
  assert(target, "The local fixture must contain an immutable agent version target.");
  const productionTarget = targets.evaluationTargets.edges.map((edge) => edge.node).find((value) => value.kind === "AGENT_VERSION" && value.logicalEnvironmentClass === "PRODUCTION");
  assert(productionTarget, "The local fixture must contain a production immutable target.");
  const firstTargetPage = await graphql(service, requester,
    "query TargetPage($project: ID!, $version: ID!, $after: String) { evaluationTargets(projectId: $project, definitionVersionId: $version, after: $after, first: 1) { edges { cursor node { kind id environmentDefinitionVersionId } } hasNextPage endCursor } }",
    { project, version: versionId, after: null });
  assert.equal(firstTargetPage.evaluationTargets.edges.length, 1);
  assert.equal(firstTargetPage.evaluationTargets.hasNextPage, true);
  const repeatedTargetPage = await graphql(service, requester,
    "query TargetPage($project: ID!, $version: ID!, $after: String) { evaluationTargets(projectId: $project, definitionVersionId: $version, after: $after, first: 1) { edges { cursor node { kind id environmentDefinitionVersionId } } hasNextPage endCursor } }",
    { project, version: versionId, after: null });
  assert.deepEqual(repeatedTargetPage.evaluationTargets, firstTargetPage.evaluationTargets);
  const nextTargetPage = await graphql(service, requester,
    "query TargetPage($project: ID!, $version: ID!, $after: String) { evaluationTargets(projectId: $project, definitionVersionId: $version, after: $after, first: 1) { edges { cursor node { kind id environmentDefinitionVersionId } } hasNextPage endCursor } }",
    { project, version: versionId, after: firstTargetPage.evaluationTargets.endCursor });
  assert.notDeepEqual(nextTargetPage.evaluationTargets.edges[0]?.node, firstTargetPage.evaluationTargets.edges[0].node);
  const malformedTargetPage = await graphql(service, requester,
    "query TargetPage($project: ID!, $version: ID!, $after: String) { evaluationTargets(projectId: $project, definitionVersionId: $version, after: $after, first: 1) { edges { node { id } } } }",
    { project, version: versionId, after: "not-a-cursor" });
  assert.equal(malformedTargetPage.evaluationTargets, null);
  const decodedTargetCursor = JSON.parse(Buffer.from(firstTargetPage.evaluationTargets.endCursor, "base64url").toString("utf8"));
  const malformedCursor = (keys) => Buffer.from(JSON.stringify({ ...decodedTargetCursor, keys })).toString("base64url");
  const unexpectedTargetKind = await graphql(service, requester,
    "query TargetPage($project: ID!, $version: ID!, $after: String) { evaluationTargets(projectId: $project, definitionVersionId: $version, after: $after, first: 1) { edges { node { id } } } }",
    { project, version: versionId, after: malformedCursor(["UNSUPPORTED_TARGET", ...decodedTargetCursor.keys.slice(1)]) });
  assert.equal(unexpectedTargetKind.evaluationTargets, null);
  const blankTargetDisplayName = await graphql(service, requester,
    "query TargetPage($project: ID!, $version: ID!, $after: String) { evaluationTargets(projectId: $project, definitionVersionId: $version, after: $after, first: 1) { edges { node { id } } } }",
    { project, version: versionId, after: malformedCursor([decodedTargetCursor.keys[0], "", ...decodedTargetCursor.keys.slice(2)]) });
  assert.equal(blankTargetDisplayName.evaluationTargets, null);
  const deploymentRequest = await graphql(service, requester,
    "mutation Deploy($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id lifecycleStatus } problems { code } } }",
    { input: { agentVersionId, environmentDefinitionVersionId: productionTarget.environmentDefinitionVersionId, strategy: "ROLLING", idempotencyKey: randomUUID() } });
  assert.deepEqual(deploymentRequest.deployAgentVersion.problems, []);
  const deploymentId = deploymentRequest.deployAgentVersion.deployment.id;
  const deploymentTargets = await graphql(service, requester,
    "query Targets($project: ID!, $version: ID!) { evaluationTargets(projectId: $project, definitionVersionId: $version, first: 50) { edges { node { kind id environmentDefinitionVersionId } } } }",
    { project, version: versionId });
  const deploymentTarget = deploymentTargets.evaluationTargets.edges.map((edge) => edge.node).find((value) => value.kind === "DEPLOYMENT" && value.id === deploymentId);
  assert(deploymentTarget, "A deployment target must retain its exact immutable environment.");
  const deploymentProjection = await client.query("SELECT count(*)::int AS count FROM evaluation_target_projections WHERE target_kind = 'DEPLOYMENT' AND target_id = $1 AND environment_definition_version_id = $2", [deploymentId, deploymentTarget.environmentDefinitionVersionId]);
  assert.equal(deploymentProjection.rows[0].count, 1);
  const traversedTargets = await allTargetPages(service, versionId);
  const authoritativeTargets = await authoritativeTargetRows(client);
  assert.deepEqual(traversedTargets.map((target) => ({ kind: target.kind, id: target.id, agentVersionId: target.agentVersionId, environmentDefinitionVersionId: target.environmentDefinitionVersionId, logicalEnvironmentClass: target.logicalEnvironmentClass, displayName: target.displayName })), authoritativeTargets);
  const queued = await graphql(service, requester,
    "mutation Run($input: RunEvaluationInput!) { runEvaluation(input: $input) { run { id lifecycleStatus generation } problems { code } } }",
    { input: { projectId: project, definitionVersionId: versionId, targetKind: target.kind, targetId: target.id, environmentDefinitionVersionId: target.environmentDefinitionVersionId, idempotencyKey: randomUUID() } });
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
  assert.equal(completed.cases.edges[0].node.passed, true);
  assert.equal(completed.metrics.edges[0].node.passed, true);
  assert.equal(completed.artifacts.edges[0].node.kind, "LOCAL_SUMMARY");
  assert(completed.audit.edges.every(({ node }) => !node.summary.includes("ready")));
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
  assert.equal(mismatch.cases.edges[0].node.failureCode, "EXACT_MATCH_FAILED");
  const targetFailureVersion = await publishUpdatedVersion(service, definitionId,
    "{\"schemaVersion\":\"hive.evaluation-definition/v1\",\"cases\":[{\"key\":\"target\",\"prompt\":\"Return ready.\",\"expectedOutput\":\"ready\",\"fixture\":{\"targetFailureCode\":\"TARGET_UNAVAILABLE\"}}],\"metrics\":[{\"code\":\"EXACT_MATCH_RATE\",\"threshold\":1}],\"requiredArtifacts\":[\"LOCAL_SUMMARY\"],\"compatibleTargetKinds\":[\"AGENT_VERSION\"],\"compatibleLogicalEnvironmentClasses\":[\"DEVELOPMENT\",\"STAGING\",\"PRODUCTION\"],\"localRunner\":{\"adapter\":\"LOCAL_PROMPT_CASE_V1\"}}");
  await waitFor(service, await queue(service, targetFailureVersion, target), "FAILED", "TARGET_FAILED");
  const runnerFailureVersion = await publishUpdatedVersion(service, definitionId,
    "{\"schemaVersion\":\"hive.evaluation-definition/v1\",\"cases\":[{\"key\":\"runner\",\"prompt\":\"Return ready.\",\"expectedOutput\":\"ready\",\"fixture\":{\"output\":\"ready\"}}],\"metrics\":[{\"code\":\"EXACT_MATCH_RATE\",\"threshold\":1}],\"requiredArtifacts\":[\"LOCAL_SUMMARY\"],\"compatibleTargetKinds\":[\"AGENT_VERSION\"],\"compatibleLogicalEnvironmentClasses\":[\"DEVELOPMENT\",\"STAGING\",\"PRODUCTION\"],\"localRunner\":{\"adapter\":\"LOCAL_PROMPT_CASE_V1\",\"failureFixture\":\"RUNNER_FAILURE\"}}");
  await waitFor(service, await queue(service, runnerFailureVersion, target), "FAILED", "RUNNER_FAILED");
  const versionPage = await graphql(service, requester,
    "query Versions($definition: ID!, $after: String) { evaluationDefinitionVersions(definitionId: $definition, after: $after, first: 1) { edges { cursor node { id number } } hasNextPage endCursor } }",
    { definition: definitionId, after: null });
  assert.equal(versionPage.evaluationDefinitionVersions.edges.length, 1);
  assert.equal(versionPage.evaluationDefinitionVersions.hasNextPage, true);
  const nextVersionPage = await graphql(service, requester,
    "query Versions($definition: ID!, $after: String) { evaluationDefinitionVersions(definitionId: $definition, after: $after, first: 1) { edges { cursor node { id number } } hasNextPage endCursor } }",
    { definition: definitionId, after: versionPage.evaluationDefinitionVersions.endCursor });
  assert.notEqual(nextVersionPage.evaluationDefinitionVersions.edges[0]?.node.id, versionPage.evaluationDefinitionVersions.edges[0].node.id);
  const transplantedTargetCursor = await graphql(service, requester,
    "query TargetPage($project: ID!, $version: ID!, $after: String) { evaluationTargets(projectId: $project, definitionVersionId: $version, after: $after, first: 1) { edges { node { id } } } }",
    { project, version: mismatchVersion, after: firstTargetPage.evaluationTargets.endCursor });
  assert.equal(transplantedTargetCursor.evaluationTargets, null);
  const hidden = await graphql(service, outsider, "query Hidden($id: ID!) { evaluationDefinition(definitionId: $id) { id } }", { id: definitionId });
  assert.equal(hidden.evaluationDefinition, null);
  const crossTenant = await graphql(service, outsider, "query Private($project: ID!) { evaluationDefinitions(projectId: $project, first: 1) { edges { node { id } } } }", { project: privateProject });
  assert.equal(crossTenant.evaluationDefinitions, null);
} finally {
  if (worker) await worker.stop();
  if (service) await service.stop();
  if (client) await client.end();
  await database.drop();
}
