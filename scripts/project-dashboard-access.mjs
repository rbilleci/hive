import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const stranger = "99999999-9999-9999-9999-999999999999";
const alphaProject = "50000000-0000-0000-0000-000000000001";
const privateProject = "50000000-0000-0000-0000-000000000003";
const unavailableCostProject = "75000000-0000-0000-0000-000000000004";
const revokedOrganization = "75000000-0000-0000-0000-000000000001";
const revokedProject = "75000000-0000-0000-0000-000000000002";
const revokedMembership = "75000000-0000-0000-0000-000000000003";
const port = 18087;

// The dashboard is the generated field over the `project_dashboard_projection` view.
const query = [
  "query ProjectDashboard($id: String!) {",
  "  projectDashboardProjection(filters: { projectId: { eq: $id } }) {",
  "    nodes {",
  "      projectId slug displayName lifecycleStatus",
  "      activeAgents activeDeployments failedDeployments pendingApprovals unhealthyResources",
  "      costAvailability costPeriodStart costPeriodEnd costCurrency currentPeriodCostCents costDataAsOf",
  "    }",
  "  }",
  "}"
].join("\n");

async function graphql(service, projectId, principal = ada) {
  const response = await fetch("http://127.0.0.1:" + port + "/graphql", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Cookie: "sf_session=" + service.signFixtureSession(principal)
    },
    body: JSON.stringify({ query, variables: { id: projectId } })
  });
  assert.equal(response.status, 200);
  return response.json();
}

const database = await createIsolatedDatabase("project_dashboard_access");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  const dashboard = await graphql(service, alphaProject);
  assert.equal(dashboard.errors, undefined);
  assert.equal(dashboard.data.projectDashboardProjection.nodes.length, 1);
  const row = dashboard.data.projectDashboardProjection.nodes[0];
  assert.deepEqual(row, {
    projectId: alphaProject,
    slug: "customer-feedback-copilot",
    displayName: "Customer Feedback Copilot",
    lifecycleStatus: "ACTIVE",
    activeAgents: 3,
    activeDeployments: 2,
    failedDeployments: 1,
    pendingApprovals: 4,
    unhealthyResources: 1,
    costAvailability: "AVAILABLE",
    costPeriodStart: row.costPeriodStart,
    costPeriodEnd: row.costPeriodEnd,
    costCurrency: "USD",
    currentPeriodCostCents: 12345,
    costDataAsOf: row.costDataAsOf
  });
  // The schema turns on Seaography's RFC 3339 timestamps, for example `2026-09-01T00:00:00+00:00`.
  const timestamp = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?([+-]\d{2}:\d{2}|Z)$/;
  const instant = (value) => new Date(value);
  for (const value of [row.costPeriodStart, row.costPeriodEnd, row.costDataAsOf]) {
    assert.match(value, timestamp);
    assert(!Number.isNaN(instant(value).getTime()));
  }
  assert(instant(row.costPeriodStart) < instant(row.costPeriodEnd));

  const projection = await client.query(
    "SELECT definition FROM pg_views WHERE schemaname = 'public' AND viewname = 'project_dashboard_projection'"
  );
  assert.match(projection.rows[0].definition, /project_dashboard_metrics/i);

  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, '10000000-0000-0000-0000-000000000001', 'unavailable-cost', 'Unavailable cost', 'ACTIVE')",
    [unavailableCostProject]
  );
  await client.query(
    "INSERT INTO project_dashboard_metrics (project_id, cost_availability) VALUES ($1, 'UNAVAILABLE')",
    [unavailableCostProject]
  );
  const unavailableCost = await graphql(service, unavailableCostProject);
  assert.equal(unavailableCost.errors, undefined);
  const { costAvailability, costPeriodStart, costPeriodEnd, costCurrency, currentPeriodCostCents, costDataAsOf } =
    unavailableCost.data.projectDashboardProjection.nodes[0];
  assert.deepEqual(
    { costAvailability, costPeriodStart, costPeriodEnd, costCurrency, currentPeriodCostCents, costDataAsOf },
    {
      costAvailability: "UNAVAILABLE",
      costPeriodStart: null,
      costPeriodEnd: null,
      costCurrency: null,
      currentPeriodCostCents: null,
      costDataAsOf: null
    }
  );
  await assert.rejects(
    client.query("UPDATE project_dashboard_metrics SET cost_availability = 'AVAILABLE' WHERE project_id = $1", [unavailableCostProject]),
    (error) => error.code === "23514"
  );

  await client.query(
    "INSERT INTO organizations (id, slug, display_name, lifecycle_status) VALUES ($1, 'revoked-dashboard', 'Revoked dashboard', 'ACTIVE')",
    [revokedOrganization]
  );
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'revoked-dashboard', 'Revoked dashboard', 'ACTIVE')",
    [revokedProject, revokedOrganization]
  );
  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', CURRENT_TIMESTAMP)",
    [revokedMembership, revokedOrganization, ada]
  );
  for (const projectId of [privateProject, revokedProject, "50000000-0000-0000-0000-000000000099"]) {
    const inaccessible = await graphql(service, projectId);
    assert.equal(inaccessible.errors, undefined);
    assert.deepEqual(inaccessible.data, { projectDashboardProjection: { nodes: [] } });
  }
  // A principal with no membership reads no row, even for a project that has one.
  const outsider = await graphql(service, alphaProject, stranger);
  assert.equal(outsider.errors, undefined);
  assert.deepEqual(outsider.data, { projectDashboardProjection: { nodes: [] } });
  // An id that is not a UUID is refused by the generated filter: an error and no data.
  const malformed = await graphql(service, "not-a-uuid");
  assert.equal(malformed.data, null);
  assert.equal(malformed.errors.length, 1);
} finally {
  await client.query("DELETE FROM projects WHERE id = $1", [unavailableCostProject]);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [revokedMembership]);
  await client.query("DELETE FROM projects WHERE id = $1", [revokedProject]);
  await client.query("DELETE FROM organizations WHERE id = $1", [revokedOrganization]);
  await client.end();
  await service.stop();
  await database.drop();
}
