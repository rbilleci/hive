import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alphaProject = "50000000-0000-0000-0000-000000000001";
const privateProject = "50000000-0000-0000-0000-000000000003";
const unavailableCostProject = "75000000-0000-0000-0000-000000000004";
const revokedOrganization = "75000000-0000-0000-0000-000000000001";
const revokedProject = "75000000-0000-0000-0000-000000000002";
const revokedMembership = "75000000-0000-0000-0000-000000000003";
const port = 18087;

const query = [
  "query ProjectDashboard($id: ID!) {",
  "  projectDashboard(id: $id) {",
  "    id slug displayName lifecycleStatus",
  "    activeAgents activeDeployments failedDeployments pendingApprovals unhealthyResources",
  "    currentPeriodCost { availability periodStart periodEnd currency amountCents dataAsOf }",
  "  }",
  "}"
].join("\n");

async function graphql(service, projectId) {
  const response = await fetch("http://127.0.0.1:" + port + "/graphql", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Cookie: "sf_session=" + service.signFixtureSession(ada)
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
  assert.deepEqual(dashboard.data.projectDashboard, {
    id: alphaProject,
    slug: "customer-feedback-copilot",
    displayName: "Customer Feedback Copilot",
    lifecycleStatus: "ACTIVE",
    activeAgents: 3,
    activeDeployments: 2,
    failedDeployments: 1,
    pendingApprovals: 4,
    unhealthyResources: 1,
    currentPeriodCost: {
      availability: "AVAILABLE",
      periodStart: dashboard.data.projectDashboard.currentPeriodCost.periodStart,
      periodEnd: dashboard.data.projectDashboard.currentPeriodCost.periodEnd,
      currency: "USD",
      amountCents: 12345,
      dataAsOf: dashboard.data.projectDashboard.currentPeriodCost.dataAsOf
    }
  });
  const cost = dashboard.data.projectDashboard.currentPeriodCost;
  assert.notEqual(cost.periodStart, null);
  assert.notEqual(cost.periodEnd, null);
  assert.notEqual(cost.dataAsOf, null);
  assert(new Date(cost.periodStart) < new Date(cost.periodEnd));

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
  assert.deepEqual(unavailableCost.data.projectDashboard.currentPeriodCost, {
    availability: "UNAVAILABLE",
    periodStart: null,
    periodEnd: null,
    currency: null,
    amountCents: null,
    dataAsOf: null
  });
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
  for (const projectId of [
    privateProject,
    revokedProject,
    "50000000-0000-0000-0000-000000000099",
    "not-a-uuid"
  ]) {
    const inaccessible = await graphql(service, projectId);
    assert.equal(inaccessible.errors, undefined);
    assert.deepEqual(inaccessible.data, { projectDashboard: null });
  }
} finally {
  await client.query("DELETE FROM projects WHERE id = $1", [unavailableCostProject]);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [revokedMembership]);
  await client.query("DELETE FROM projects WHERE id = $1", [revokedProject]);
  await client.query("DELETE FROM organizations WHERE id = $1", [revokedOrganization]);
  await client.end();
  await service.stop();
  await database.drop();
}
