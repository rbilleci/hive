import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { createIsolatedDatabase, postgresClient, startIsolatedLocalService } from "./local-service.mjs";

const administrator = "00000000-0000-0000-0000-000000000001";
const organization = "10000000-0000-0000-0000-000000000001";
const project = "50000000-0000-0000-0000-000000000001";
let endpoint = "";

async function gql(service, query, variables) {
  const response = await fetch(endpoint, { method: "POST", headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(administrator)}` }, body: JSON.stringify({ query, variables }) });
  assert.equal(response.status, 200);
  return response.json();
}

async function gqlFailure(service, query, variables) {
  const response = await fetch(endpoint, { method: "POST", headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(administrator)}` }, body: JSON.stringify({ query, variables }) });
  return { status: response.status, body: await response.json() };
}

async function withAuditInsertFault(client, table, action) {
  const trigger = `m17_fault_${table}`;
  await client.query("CREATE OR REPLACE FUNCTION m17_injected_audit_insert_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'M17 injected audit insertion fault' USING ERRCODE = 'P0001'; END; $$");
  await client.query(`CREATE TRIGGER ${trigger} BEFORE INSERT ON ${table} FOR EACH ROW EXECUTE FUNCTION m17_injected_audit_insert_fault()`);
  try { await action(); }
  finally {
    await client.query(`DROP TRIGGER IF EXISTS ${trigger} ON ${table}`);
    await client.query("DROP FUNCTION IF EXISTS m17_injected_audit_insert_fault()");
  }
}

function publishDocument(displayName) {
  return {
    general: { displayName, description: "M17 local transaction fixture." },
    instructions: { source: "# local fixture", language: "markdown" }, harness: { source: "def run(value): return value", language: "python" },
    model: { reference: "model:local-safe-chat@v2" }, tools: { source: "{}", language: "json" }, skills: { source: "export const skills = [];", language: "javascript" },
    capabilities: { source: "export type Capability = string;", language: "typescript" }, subagents: { enabled: false }, memory: { strategy: "project" },
    guardrails: { source: "# local guardrail", language: "shell" }, identity: { persona: "" }, observability: { source: "<observability/>", language: "xml" },
    limits: { maxTokens: 256 }, evaluations: { required: false }, dependencies: ["model:local-safe-chat@v2"]
  };
}

async function publishAgent(service, suffix) {
  const created = await gql(service, "mutation CreateTransactionAgent($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId revision } problems { code } } }", { input: { projectId: project, displayName: `M17 transaction ${suffix}`, slug: `m17-transaction-${suffix}` } });
  assert.deepEqual(created.data.createAgentDraft.problems, []);
  const agentId = created.data.createAgentDraft.agentDraft.agentId;
  const saved = await gql(service, "mutation SaveTransactionAgent($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }", { input: { projectId: project, agentId, expectedRevision: created.data.createAgentDraft.agentDraft.revision, document: publishDocument(`M17 transaction ${suffix}`) } });
  assert.deepEqual(saved.data.updateAgentDraft.problems, []);
  const validated = await gql(service, "mutation ValidateTransactionAgent($input: ValidateAgentDraftInput!) { validateAgentDraft(input: $input) { agentDraft { revision validationStatus } problems { code } } }", { input: { projectId: project, agentId, expectedRevision: saved.data.updateAgentDraft.agentDraft.revision } });
  assert.equal(validated.data.validateAgentDraft.agentDraft.validationStatus, "VALID");
  const published = await gql(service, "mutation PublishTransactionAgent($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentVersion { id } problems { code } } }", { input: { projectId: project, agentId, expectedRevision: validated.data.validateAgentDraft.agentDraft.revision, warningsAcknowledged: true } });
  assert.deepEqual(published.data.publishAgentDraft.problems, []);
  return published.data.publishAgentDraft.agentVersion.id;
}

const database = await createIsolatedDatabase("m17_audit_transaction");
let service;
let client;
try {
  service = await startIsolatedLocalService(database.name);
  endpoint = `http://127.0.0.1:${service.port}/graphql`;
  client = await postgresClient(database.name);
  await client.query("INSERT INTO platform_role_assignments (principal_id, role_code) VALUES ($1, 'PLATFORM_ADMIN')", [administrator]);

  const agentSlug = `m17-fault-agent-${randomUUID().slice(0, 8)}`;
  await withAuditInsertFault(client, "agent_authoring_audit_events", async () => {
    const response = await gql(service, "mutation FaultAgent($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId } problems { code } } }", { input: { projectId: project, displayName: "M17 fault agent", slug: agentSlug } });
    assert.ok(response.errors?.length, "the injected agent audit fault must fail the mutation");
  });
  assert.equal((await client.query("SELECT count(*)::int AS count FROM agents WHERE project_id = $1 AND slug = $2", [project, agentSlug])).rows[0].count, 0);

  const organizationState = await gql(service, "query FaultOrganization($id: String!) { organizations(filters: { id: { eq: $id } }) { nodes { revision } } }", { id: organization });
  const projectSlug = `m17-fault-project-${randomUUID().slice(0, 8)}`;
  await withAuditInsertFault(client, "administration_audit_events", async () => {
    const response = await gql(service, "mutation FaultAdministration($input: CreateProjectInput!) { createProject(input: $input) { project { id } problems { code } } }", { input: { organizationId: organization, expectedRevision: organizationState.data.organizations.nodes[0].revision, slug: projectSlug, displayName: "M17 fault project", description: "Injected audit fault fixture" } });
    assert.ok(response.errors?.length, "the injected administration audit fault must fail the mutation");
  });
  assert.equal((await client.query("SELECT count(*)::int AS count FROM projects WHERE organization_id = $1 AND slug = $2", [organization, projectSlug])).rows[0].count, 0);

  const resourceName = `M17 Fault Resource ${randomUUID().slice(0, 8)}`;
  await withAuditInsertFault(client, "configuration_audit_events", async () => {
    const response = await gql(service, "mutation FaultConfiguration($input: CreateReusableResourceInput!) { createReusableResource(input: $input) { resource { id } problems { code } } }", { input: { projectId: project, kind: "PROMPT", name: resourceName, content: "M17 fault {{operator}}", dependencies: [] } });
    assert.ok(response.errors?.length, "the injected configuration audit fault must fail the mutation");
  });
  assert.equal((await client.query("SELECT count(*)::int AS count FROM reusable_resources WHERE project_id = $1 AND name = $2", [project, resourceName])).rows[0].count, 0);

  const definitionSlug = `m17-fault-evaluation-${randomUUID().slice(0, 8)}`;
  await withAuditInsertFault(client, "evaluation_audit_events", async () => {
    const response = await gql(service, "mutation FaultEvaluation($input: CreateEvaluationDefinitionInput!) { createEvaluationDefinition(input: $input) { definition { id } problems { code } } }", { input: { projectId: project, slug: definitionSlug, document: null, idempotencyKey: randomUUID() } });
    assert.equal(response.data.createEvaluationDefinition.problems[0]?.code, "UNAVAILABLE", "the injected evaluation audit fault must refuse the mutation");
  });
  assert.equal((await client.query("SELECT count(*)::int AS count FROM evaluation_definitions WHERE project_id = $1 AND slug = $2", [project, definitionSlug])).rows[0].count, 0);

  const agentVersionId = await publishAgent(service, randomUUID().slice(0, 8));
  const environments = await gql(service, "query FaultDeploymentEnvironment($version: String!) { agentVersions(filters: { id: { eq: $version } }, pagination: { page: { limit: 1, page: 0 } }) { nodes { catalogReleases { environmentDefinitionVersions(orderBy: { stableDefinitionId: ASC, version: ASC, id: ASC }, pagination: { page: { limit: 50, page: 0 } }) { nodes { id logicalEnvironmentClass stableDefinitionId version catalogReleaseDigest } } } } } }", { version: agentVersionId });
  const environmentId = environments.data.agentVersions.nodes[0]?.catalogReleases?.environmentDefinitionVersions.nodes.find((node) => node.logicalEnvironmentClass === "DEVELOPMENT")?.id;
  assert.ok(environmentId, "the fixture must expose a DEVELOPMENT environment");
  const deploymentCount = await client.query("SELECT count(*)::int AS count FROM deployments WHERE project_id = $1", [project]);
  await withAuditInsertFault(client, "deployment_audit_events", async () => {
    const response = await gqlFailure(service, "mutation FaultDeployment($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }", { input: { agentVersionId, environmentDefinitionVersionId: environmentId, strategy: "ROLLING", idempotencyKey: randomUUID() } });
    assert.equal(response.status, 503); assert.ok(response.body.errors?.length, "the injected deployment audit fault must fail the mutation");
  });
  const deploymentCountAfter = await client.query("SELECT count(*)::int AS count FROM deployments WHERE project_id = $1", [project]);
  assert.equal(deploymentCountAfter.rows[0].count, deploymentCount.rows[0].count);
} finally {
  if (client) await client.end();
  if (service) await service.stop();
  await database.drop();
}
console.log("M17 injected audit-insertion faults roll back agent, administration, configuration, evaluation, and deployment mutations.");
