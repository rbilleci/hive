import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
// The console's AccessibleOrganizations selection, against Seaography's generated `organizations`.
const query = [
  "query Selector($filters: OrganizationsFilterInput, $limit: Int!, $page: Int!) {",
  "  organizations(filters: $filters, orderBy: { displayName: ASC }, pagination: { page: { limit: $limit, page: $page } }) {",
  "    nodes { slug displayName lifecycleStatus }",
  "    paginationInfo { pages current total }",
  "  }",
  "}"
].join("\n");
const activeOnly = { lifecycleStatus: { ne: "ARCHIVED" } };

async function graphql(port, signFixtureSession, variables) {
  const response = await fetch("http://127.0.0.1:" + port + "/graphql", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Cookie: "sf_session=" + signFixtureSession(ada)
    },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  const payload = await response.json();
  assert.equal(payload.errors, undefined);
  return payload.data.organizations;
}

const port = 18081;
const database = await createIsolatedDatabase("organization_access");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  const applicationShell = await fetch("http://127.0.0.1:" + port + "/organizations");
  assert.equal(applicationShell.status, 200);
  assert.match(await applicationShell.text(), /<div id="root"><\/div>/);

  await assert.rejects(
    client.query(
      "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at, active_marker) "
        + "VALUES ($1, $2, $3, CURRENT_TIMESTAMP, NULL, TRUE)",
      [
        "20000000-0000-0000-0000-000000000099",
        "10000000-0000-0000-0000-000000000001",
        ada
      ]
    ),
    (error) => error.code === "23505"
  );

  const firstPage = await graphql(port, service.signFixtureSession, { filters: activeOnly, limit: 1, page: 0 });
  assert.deepEqual(firstPage.nodes.map((node) => node.slug), ["product"]);
  assert.deepEqual(firstPage.paginationInfo, { pages: 2, current: 0, total: 2 });

  const secondPage = await graphql(port, service.signFixtureSession, { filters: activeOnly, limit: 1, page: 1 });
  assert.deepEqual(secondPage.nodes.map((node) => node.slug), ["support"]);
  assert.deepEqual(secondPage.paginationInfo, { pages: 2, current: 1, total: 2 });

  const archived = await graphql(port, service.signFixtureSession, { filters: {}, limit: 50, page: 0 });
  assert.deepEqual(archived.nodes.map((node) => node.slug), ["product", "quality-assurance", "support"]);
  assert.equal(archived.paginationInfo.total, 3);

} finally {
  await client.end();
  await service.stop();
  await database.drop();
}
