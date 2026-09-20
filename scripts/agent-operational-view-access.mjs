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

const query = [
  "query AgentOperationalView($projectId: ID!, $agentId: ID!) {",
  "  agentOperationalView(projectId: $projectId, agentId: $agentId) {",
  "    id slug displayName lifecycleStatus",
  "    draftValidation { status errorCount warningCount validatedAt }",
  "    latestPublishedVersion { status version publishedAt }",
  "    aliasTargets { totalCount activeCount }",
  "    activeDeployment { status observedAt }",
  "    recentEvaluation { outcome completedAt }",
  "    runtimeHealth { status observedAt freshness }",
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
  const result = overview.data.agentOperationalView;
  assert.deepEqual(Object.keys(result), [
    "id", "slug", "displayName", "lifecycleStatus", "draftValidation", "latestPublishedVersion",
    "aliasTargets", "activeDeployment", "recentEvaluation", "runtimeHealth"
  ]);
  assert.equal(result.id, commandNavigator);
  assert.equal(result.displayName, "Feedback Triage Agent");
  assert.deepEqual(Object.keys(result.draftValidation), ["status", "errorCount", "warningCount", "validatedAt"]);
  assert.deepEqual(Object.keys(result.latestPublishedVersion), ["status", "version", "publishedAt"]);
  assert.deepEqual(Object.keys(result.aliasTargets), ["totalCount", "activeCount"]);
  assert.deepEqual(Object.keys(result.activeDeployment), ["status", "observedAt"]);
  assert.deepEqual(Object.keys(result.recentEvaluation), ["outcome", "completedAt"]);
  assert.deepEqual(Object.keys(result.runtimeHealth), ["status", "observedAt", "freshness"]);
  assert.equal(result.draftValidation.status, "VALID");
  assert.equal(result.latestPublishedVersion.version, "v1.4.0");
  assert.equal(result.aliasTargets.totalCount, 3);
  assert.equal(result.activeDeployment.status, "ACTIVE");
  assert.equal(result.recentEvaluation.outcome, "PASSED");
  assert.equal(result.runtimeHealth.status, "HEALTHY");
  assert.equal(result.runtimeHealth.freshness, "FRESH");

  const empty = await graphql(service, commandConsole, emptyAgent);
  assert.equal(empty.errors, undefined);
  assert.deepEqual(empty.data.agentOperationalView.draftValidation, {
    status: "NOT_VALIDATED", errorCount: 0, warningCount: 0, validatedAt: null
  });
  assert.deepEqual(empty.data.agentOperationalView.latestPublishedVersion, {
    status: "NO_PUBLISHED_VERSION", version: null, publishedAt: null
  });
  assert.deepEqual(empty.data.agentOperationalView.runtimeHealth, {
    status: "UNKNOWN", observedAt: null, freshness: "UNKNOWN"
  });

  for (const [projectId, agentId] of [
    [fleetAnalytics, commandNavigator],
    [privateProject, privateAgent],
    [revokedProject, revokedAgent],
    [commandConsole, "67000000-0000-0000-0000-000000000099"],
    ["not-a-uuid", commandNavigator],
    [commandConsole, "not-a-uuid"]
  ]) {
    const inaccessible = await graphql(service, projectId, agentId);
    assert.equal(inaccessible.errors, undefined);
    assert.deepEqual(inaccessible.data, { agentOperationalView: null });
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
