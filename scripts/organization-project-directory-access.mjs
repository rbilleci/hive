import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const revokedOrganization = "73000000-0000-0000-0000-000000000001";
const revokedMembership = "73000000-0000-0000-0000-000000000002";
const port = 18085;

// The console's OrganizationProjects selection: Seaography's generated `organizations` with its
// `projects` relation field, the lifecycle filter, the literal `ilike` search and page pagination.
const query = [
  "query OrganizationProjects($id: String!, $filters: ProjectsFilterInput, $limit: Int!, $page: Int!) {",
  "  organizations(filters: { id: { eq: $id } }) { nodes {",
  "    id",
  "    projects(filters: $filters, orderBy: { displayName: ASC, id: ASC }, pagination: { page: { limit: $limit, page: $page } }) {",
  "      nodes { id slug displayName lifecycleStatus }",
  "      paginationInfo { pages current total }",
  "    }",
  "  } }",
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

function projects(result) {
  return result.data.organizations.nodes[0].projects;
}

/** The pattern the console sends for a literal search: `%`, `_` and `\` escaped. */
function likePattern(text) {
  return "%" + text.replace(/[\\%_]/g, (character) => "\\" + character) + "%";
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

  const first = await graphql(service.signFixtureSession, ada, { id: alpha, filters: {}, limit: 25, page: 0 });
  assert.equal(first.errors, undefined);
  assert.equal(first.data.organizations.nodes[0].id, alpha);
  assert.deepEqual(projects(first).paginationInfo, { pages: 2, current: 0, total: 34 });
  const firstNodes = projects(first).nodes;
  assert.equal(firstNodes.length, 25);
  // Ordered by display name, then id: the entities declare their primary key last, so the id the
  // console always adds is the final tie-break for rows that share a name.
  const firstPairs = firstNodes.map((project) => project.displayName + "\u0000" + project.id);
  assert.deepEqual(firstPairs, [...firstPairs].sort());

  const second = await graphql(service.signFixtureSession, ada, { id: alpha, filters: {}, limit: 25, page: 1 });
  assert.equal(second.errors, undefined);
  assert.deepEqual(projects(second).paginationInfo, { pages: 2, current: 1, total: 34 });
  const allIds = firstNodes.concat(projects(second).nodes).map((project) => project.id);
  assert.equal(new Set(allIds).size, 34);

  const active = await graphql(service.signFixtureSession, ada, {
    id: alpha, filters: { lifecycleStatus: { eq: "ACTIVE" } }, limit: 50, page: 0
  });
  assert.equal(active.errors, undefined);
  assert(projects(active).nodes.length > 0);
  assert(projects(active).nodes.every((project) => project.lifecycleStatus === "ACTIVE"));

  const literalSearch = await graphql(service.signFixtureSession, ada, {
    id: alpha, filters: { displayName: { ilike: likePattern("%_") } }, limit: 50, page: 0
  });
  assert.equal(literalSearch.errors, undefined);
  assert.deepEqual(projects(literalSearch).nodes.map((project) => project.slug), ["literal-percent-underscore"]);
  const caseInsensitive = await graphql(service.signFixtureSession, ada, {
    id: alpha, filters: { displayName: { ilike: likePattern("literal zz") } }, limit: 50, page: 0
  });
  assert.deepEqual(projects(caseInsensitive).nodes.map((project) => project.slug), ["literal-near-match"]);

  await client.query("UPDATE organization_memberships SET ended_at = CURRENT_TIMESTAMP WHERE id = $1", [revokedMembership]);
  for (const organizationId of [privateOrganization, revokedOrganization, "10000000-0000-0000-0000-000000000099"]) {
    const inaccessible = await graphql(service.signFixtureSession, ada, { id: organizationId, filters: {}, limit: 25, page: 0 });
    assert.equal(inaccessible.errors, undefined);
    assert.deepEqual(inaccessible.data, { organizations: { nodes: [] } });
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
