import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const commandConsole = "50000000-0000-0000-0000-000000000001";
const fleetAnalytics = "50000000-0000-0000-0000-000000000002";
const commandNavigator = "60000000-0000-0000-0000-000000000001";
const privateProject = "50000000-0000-0000-0000-000000000003";
const privateAgent = "60000000-0000-0000-0000-000000000004";
const emptyAgent = "67000000-0000-0000-0000-000000000001";
const revokedOrganization = "67000000-0000-0000-0000-000000000002";
const revokedProject = "67000000-0000-0000-0000-000000000003";
const revokedAgent = "67000000-0000-0000-0000-000000000004";
const revokedMembership = "67000000-0000-0000-0000-000000000005";
const port = 18091;

// The generated `agentOperationalViewProjection` field over the view of the same name.
const columns = [
  "agentId", "slug", "displayName", "lifecycleStatus",
  "draftValidationStatus", "draftErrorCount", "draftWarningCount", "draftValidatedAt",
  "publishedVersionStatus", "publishedVersion", "publishedAt",
  "aliasTargetCount", "activeAliasTargetCount",
  "deploymentStatus", "deploymentObservedAt",
  "evaluationOutcome", "evaluationCompletedAt",
  "runtimeHealth", "runtimeObservedAt", "runtimeFreshness"
];
const query = [
  "query AgentOperationalView($projectId: String!, $agentId: String!) {",
  "  agentOperationalViewProjection(filters: { projectId: { eq: $projectId }, agentId: { eq: $agentId } }) {",
  "    nodes { " + columns.join(" ") + " }",
  "  }",
  "}"
].join("\n");

async function graphql(service, projectId, agentId) {
  const response = await fetch("http://127.0.0.1:" + port + "/graphql", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Cookie: "sf_session=" + service.signFixtureSession(ada)
    },
    body: JSON.stringify({ query, variables: { projectId, agentId } })
  });
  assert.equal(response.status, 200);
  return response.json();
}

const database = await createIsolatedDatabase("agent_operational_view_access");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  await client.query(
    "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'empty-overview', 'Empty overview', 'ACTIVE')",
    [emptyAgent, commandConsole]
  );
  await client.query(
    "INSERT INTO organizations (id, slug, display_name, lifecycle_status) VALUES ($1, 'revoked-overview', 'Revoked overview', 'ACTIVE')",
    [revokedOrganization]
  );
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'revoked-overview', 'Revoked overview', 'ACTIVE')",
    [revokedProject, revokedOrganization]
  );
  await client.query(
    "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'revoked-overview', 'Revoked overview', 'ACTIVE')",
    [revokedAgent, revokedProject]
  );
  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', CURRENT_TIMESTAMP)",
    [revokedMembership, revokedOrganization, ada]
  );

  const overview = await graphql(service, commandConsole, commandNavigator);
  assert.equal(overview.errors, undefined);
  assert.equal(overview.data.agentOperationalViewProjection.nodes.length, 1);
  const result = overview.data.agentOperationalViewProjection.nodes[0];
  assert.deepEqual(Object.keys(result), columns);
  assert.equal(result.agentId, commandNavigator);
  assert.equal(result.displayName, "Feedback Triage Agent");
  assert.equal(result.draftValidationStatus, "VALID");
  assert.equal(result.publishedVersionStatus, "PUBLISHED");
  assert.equal(result.publishedVersion, "v1.4.0");
  assert.equal(result.aliasTargetCount, 3);
  assert.equal(result.deploymentStatus, "ACTIVE");
  assert.equal(result.evaluationOutcome, "PASSED");
  assert.equal(result.runtimeHealth, "HEALTHY");
  assert.equal(result.runtimeFreshness, "FRESH");
  assert.match(result.runtimeObservedAt, /^\d{4}-\d{2}-\d{2}T/, "generated timestamps are RFC 3339");

  const empty = await graphql(service, commandConsole, emptyAgent);
  assert.equal(empty.errors, undefined);
  assert.deepEqual(empty.data.agentOperationalViewProjection.nodes, [{
    agentId: emptyAgent, slug: "empty-overview", displayName: "Empty overview", lifecycleStatus: "ACTIVE",
    draftValidationStatus: "NOT_VALIDATED", draftErrorCount: 0, draftWarningCount: 0, draftValidatedAt: null,
    publishedVersionStatus: "NO_PUBLISHED_VERSION", publishedVersion: null, publishedAt: null,
    aliasTargetCount: 0, activeAliasTargetCount: 0,
    deploymentStatus: "NOT_DEPLOYED", deploymentObservedAt: null,
    evaluationOutcome: "NO_EVALUATION", evaluationCompletedAt: null,
    runtimeHealth: "UNKNOWN", runtimeObservedAt: null, runtimeFreshness: "UNKNOWN"
  }]);

  for (const [projectId, agentId] of [
    [fleetAnalytics, commandNavigator],
    [privateProject, privateAgent],
    [revokedProject, revokedAgent],
    [commandConsole, "67000000-0000-0000-0000-000000000099"]
  ]) {
    const inaccessible = await graphql(service, projectId, agentId);
    assert.equal(inaccessible.errors, undefined);
    assert.deepEqual(inaccessible.data, { agentOperationalViewProjection: { nodes: [] } });
  }
  // A malformed id is a Seaography type-conversion error, not "no row"; the console never sends one.
  for (const [projectId, agentId] of [["not-a-uuid", commandNavigator], [commandConsole, "not-a-uuid"]]) {
    const malformed = await graphql(service, projectId, agentId);
    assert.notEqual(malformed.errors, undefined);
    assert.equal(malformed.data?.agentOperationalViewProjection ?? null, null);
  }
} finally {
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [revokedMembership]);
  await client.query("DELETE FROM projects WHERE id = $1", [revokedProject]);
  await client.query("DELETE FROM organizations WHERE id = $1", [revokedOrganization]);
  await client.query("DELETE FROM agents WHERE id = $1", [emptyAgent]);
  await client.end();
  await service.stop();
  await database.drop();
}
