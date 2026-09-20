import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const query = [
  "query Selector($first: Int!, $after: String, $filter: AccessibleOrganizationsFilter) {",
  "  accessibleOrganizations(first: $first, after: $after, filter: $filter) {",
  "    edges { cursor node { slug displayName lifecycleStatus } }",
  "    pageInfo { hasNextPage endCursor }",
  "    totalCount",
  "  }",
  "}"
].join("\n");

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
  return payload.data.accessibleOrganizations;
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

  const firstPage = await graphql(port, service.signFixtureSession, {
    first: 1, filter: { includeArchived: false }
  });
  assert.deepEqual(firstPage.edges.map((edge) => edge.node.slug), ["product"]);
  assert.equal(firstPage.totalCount, 2);
  assert.equal(firstPage.pageInfo.hasNextPage, true);
  assert.ok(firstPage.pageInfo.endCursor);

  const secondPage = await graphql(port, service.signFixtureSession, {
    first: 1,
    after: firstPage.pageInfo.endCursor,
    filter: { includeArchived: false }
  });
  assert.deepEqual(secondPage.edges.map((edge) => edge.node.slug), ["support"]);
  assert.equal(secondPage.pageInfo.hasNextPage, false);
  const visibleSlugs = firstPage.edges.concat(secondPage.edges).map((edge) => edge.node.slug);
  assert.equal(new Set(visibleSlugs).size, visibleSlugs.length);

  const archived = await graphql(port, service.signFixtureSession, {
    first: 50, filter: { includeArchived: true }
  });
  assert.deepEqual(archived.edges.map((edge) => edge.node.slug), [
    "product", "quality-assurance", "support"
  ]);
  assert.equal(archived.totalCount, 3);

} finally {
  await client.end();
  await service.stop();
  await database.drop();
}
