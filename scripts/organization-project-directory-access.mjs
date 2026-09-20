import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const revokedOrganization = "73000000-0000-0000-0000-000000000001";
const revokedMembership = "73000000-0000-0000-0000-000000000002";
const port = 18085;

const query = [
  "query OrganizationProjects($id: ID!, $first: Int, $after: String, $last: Int, $before: String, $filter: OrganizationProjectFilter) {",
  "  organization(id: $id) {",
  "    id",
  "    projects(first: $first, after: $after, last: $last, before: $before, filter: $filter) {",
  "      edges { cursor node { id slug displayName lifecycleStatus } }",
  "      pageInfo { hasNextPage hasPreviousPage endCursor startCursor }",
  "      totalCount",
  "    }",
  "  }",
  "}"
].join("\n");

async function graphql(signFixtureSession, principal, variables) {
  const response = await fetch("http://127.0.0.1:" + port + "/graphql", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Cookie: "sf_session=" + signFixtureSession(principal)
    },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  return response.json();
}

function nodes(result) {
  return result.data.organization.projects.edges.map((edge) => edge.node);
}

const database = await createIsolatedDatabase("hive_project_directory");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) "
      + "SELECT ('72000000-0000-0000-0000-' || lpad(number::text, 12, '0'))::uuid, $1, "
      + "'directory-page-' || lpad(number::text, 3, '0'), 'Directory project ' || lpad(number::text, 3, '0'), "
      + "CASE WHEN number % 2 = 0 THEN 'ARCHIVED' ELSE 'ACTIVE' END "
      + "FROM generate_series(1, 28) AS fixture(number) ON CONFLICT (id) DO NOTHING",
    [alpha]
  );
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES "
      + "('72000000-0000-0000-0000-000000000101', $1, 'literal-percent-underscore', 'Literal %_ Signal', 'ACTIVE'), "
      + "('72000000-0000-0000-0000-000000000102', $1, 'literal-near-match', 'Literal ZZ Signal', 'ACTIVE'), "
      + "('72000000-0000-0000-0000-000000000104', $1, 'duplicate-later', 'Same name', 'ACTIVE'), "
      + "('72000000-0000-0000-0000-000000000103', $1, 'duplicate-earlier', 'Same name', 'ACTIVE') "
      + "ON CONFLICT (id) DO NOTHING",
    [alpha]
  );
  await client.query(
    "INSERT INTO organizations (id, slug, display_name, lifecycle_status) VALUES ($1, 'revoked-directory', 'Revoked directory', 'ACTIVE') "
      + "ON CONFLICT (id) DO NOTHING",
    [revokedOrganization]
  );
  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) "
      + "VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL) ON CONFLICT (id) DO NOTHING",
    [revokedMembership, revokedOrganization, ada]
  );

  const first = await graphql(service.signFixtureSession, ada, {
    id: alpha, first: 25, after: null, filter: { lifecycleStatus: null, search: null }
  });
  assert.equal(first.errors, undefined);
  assert.equal(first.data.organization.id, alpha);
  assert.equal(first.data.organization.projects.pageInfo.hasNextPage, true);
  assert.equal(first.data.organization.projects.totalCount, 34);
  const firstNodes = nodes(first);
  const firstPairs = firstNodes.map((project) => project.displayName + "\u0000" + project.id);
  assert.deepEqual(firstPairs, [...firstPairs].sort());

  const second = await graphql(service.signFixtureSession, ada, {
    id: alpha,
    first: 25,
    after: first.data.organization.projects.pageInfo.endCursor,
    filter: { lifecycleStatus: null, search: null }
  });
  assert.equal(second.errors, undefined);
  const allIds = firstNodes.concat(nodes(second)).map((project) => project.id);
  assert.equal(new Set(allIds).size, 34);
  assert.equal(second.data.organization.projects.pageInfo.hasNextPage, false);

  assert.equal(first.data.organization.projects.pageInfo.hasPreviousPage, false);
  assert.equal(first.data.organization.projects.pageInfo.startCursor, first.data.organization.projects.edges[0].cursor);
  assert.equal(second.data.organization.projects.pageInfo.hasPreviousPage, true);

  const back = await graphql(service.signFixtureSession, ada, {
    id: alpha, first: 25, after: null, last: 25,
    before: second.data.organization.projects.pageInfo.startCursor,
    filter: { lifecycleStatus: null, search: null }
  });
  assert.equal(back.errors, undefined);
  assert.equal(back.data.organization.projects.pageInfo.hasPreviousPage, false);
  assert.equal(back.data.organization.projects.pageInfo.hasNextPage, true);
  assert.deepEqual(nodes(back).map((project) => project.id), firstNodes.map((project) => project.id));

  const bothDirections = await graphql(service.signFixtureSession, ada, {
    id: alpha, first: 25, after: first.data.organization.projects.pageInfo.endCursor, last: 25,
    before: second.data.organization.projects.pageInfo.startCursor,
    filter: { lifecycleStatus: null, search: null }
  });
  assert.notEqual(bothDirections.errors, undefined);

  const active = await graphql(service.signFixtureSession, ada, {
    id: alpha, first: 50, after: null, filter: { lifecycleStatus: "ACTIVE", search: null }
  });
  assert.equal(active.errors, undefined);
  assert(active.data.organization.projects.edges.every((edge) => edge.node.lifecycleStatus === "ACTIVE"));

  const literalSearch = await graphql(service.signFixtureSession, ada, {
    id: alpha, first: 50, after: null, filter: { lifecycleStatus: null, search: "%_" }
  });
  assert.equal(literalSearch.errors, undefined);
  assert.deepEqual(nodes(literalSearch).map((project) => project.slug), ["literal-percent-underscore"]);

  const wrongFilterCursor = await graphql(service.signFixtureSession, ada, {
    id: alpha,
    first: 25,
    after: first.data.organization.projects.pageInfo.endCursor,
    filter: { lifecycleStatus: "ACTIVE", search: null }
  });
  assert.notEqual(wrongFilterCursor.errors, undefined);

  await client.query("UPDATE organization_memberships SET ended_at = CURRENT_TIMESTAMP WHERE id = $1", [revokedMembership]);
  for (const organizationId of [
    privateOrganization,
    revokedOrganization,
    "10000000-0000-0000-0000-000000000099",
    "not-a-uuid"
  ]) {
    const inaccessible = await graphql(service.signFixtureSession, ada, {
      id: organizationId, first: 25, after: null, filter: { lifecycleStatus: null, search: null }
    });
    assert.equal(inaccessible.errors, undefined);
    assert.deepEqual(inaccessible.data, { organization: null });
  }

  const indexes = await client.query(
    "SELECT indexname FROM pg_indexes WHERE schemaname = 'public' AND tablename = 'projects'"
  );
  assert(indexes.rows.some((row) => row.indexname === "projects_directory_keyset"));
  assert(indexes.rows.some((row) => row.indexname === "projects_directory_lifecycle_keyset"));
} finally {
  await client.query("DELETE FROM projects WHERE id::text LIKE '72000000-0000-0000-0000-%'");
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [revokedMembership]);
  await client.query("DELETE FROM organizations WHERE id = $1", [revokedOrganization]);
  await client.end();
  await service.stop();
  await database.drop();
}
