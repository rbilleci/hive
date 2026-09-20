import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alphaProject = "50000000-0000-0000-0000-000000000001";
const alphaMembership = "20000000-0000-0000-0000-000000000001";
const privateProject = "50000000-0000-0000-0000-000000000003";
const revokedOrganization = "63000000-0000-0000-0000-000000000001";
const revokedProject = "63000000-0000-0000-0000-000000000002";
const revokedMembership = "63000000-0000-0000-0000-000000000003";
const publishedAgent = "61000000-0000-0000-0000-000000000105";
const port = 18089;

// The console's ProjectAgents selection: Seaography's generated `projects` with its `agents`
// relation field, and each agent's newest `agentVersions` row for the version and model columns.
const query = [
  "query ProjectAgents($id: String!, $filters: AgentsFilterInput, $limit: Int!, $page: Int!) {",
  "  projects(filters: { id: { eq: $id } }) { nodes {",
  "    id slug displayName lifecycleStatus",
  "    agents(filters: $filters, orderBy: { displayName: ASC, id: ASC }, pagination: { page: { limit: $limit, page: $page } }) {",
  "      nodes { id slug displayName lifecycleStatus",
  "        agentVersions(orderBy: { versionNumber: DESC }, pagination: { page: { limit: 1, page: 0 } }) { nodes { versionNumber canonicalDocument } } }",
  "      paginationInfo { pages current total }",
  "    }",
  "  } }",
  "}"
].join("\n");

async function graphql(service, projectId, limit = 25, page = 0, filters = {}) {
  const response = await fetch("http://127.0.0.1:" + port + "/graphql", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Cookie: "sf_session=" + service.signFixtureSession(ada)
    },
    body: JSON.stringify({ query, variables: { id: projectId, filters, limit, page } })
  });
  assert.equal(response.status, 200);
  return response.json();
}

function agents(result) {
  return result.data.projects.nodes[0].agents;
}

/** The pattern the console sends for a literal search: `%`, `_` and `\` escaped. */
function likePattern(text) {
  return "%" + text.replace(/[\\%_]/g, (character) => "\\" + character) + "%";
}

const database = await createIsolatedDatabase("hive_project_agents");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  await client.query(
    "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) "
      + "SELECT ('61000000-0000-0000-0000-' || lpad(number::text, 12, '0'))::uuid, $1, "
      + "'agent-page-' || lpad(number::text, 3, '0'), 'Agent Page ' || lpad(number::text, 3, '0'), "
      + "CASE WHEN number % 3 = 0 THEN 'ARCHIVED' WHEN number % 2 = 0 THEN 'DEPRECATED' ELSE 'ACTIVE' END "
      + "FROM generate_series(1, 28) AS fixture(number) ON CONFLICT (id) DO NOTHING",
    [alphaProject]
  );
  await client.query(
    "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES "
      + "('61000000-0000-0000-0000-000000000101', $1, 'literal-percent-underscore', 'Literal %_ Signal', 'ACTIVE'), "
      + "('61000000-0000-0000-0000-000000000102', $1, 'literal-near-match', 'Literal ZZ Signal', 'ACTIVE'), "
      + "('61000000-0000-0000-0000-000000000103', $1, 'duplicate-earlier', 'Same Agent', 'ACTIVE'), "
      + "('61000000-0000-0000-0000-000000000104', $1, 'duplicate-later', 'Same Agent', 'ACTIVE') "
      + "ON CONFLICT (id) DO NOTHING",
    [alphaProject]
  );
  await client.query(
    "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES "
      + "($1, $2, 'published-fixture', 'Published Fixture', 'ACTIVE') ON CONFLICT (id) DO NOTHING",
    [publishedAgent, alphaProject]
  );
  await client.query(
    "INSERT INTO agent_versions (id, agent_id, version_number, canonical_document, content_digest, "
      + "catalog_release_id, catalog_release_digest, published_by, published_at) VALUES "
      + "('61000000-0000-0000-0000-000000000201', $1, 1, $2::jsonb, repeat('a', 64), "
      + "'local-2026-08-10', repeat('a', 64), $3, CURRENT_TIMESTAMP - INTERVAL '1 hour'), "
      + "('61000000-0000-0000-0000-000000000202', $1, 2, $4::jsonb, repeat('b', 64), "
      + "'local-2026-08-10', repeat('b', 64), $3, CURRENT_TIMESTAMP)",
    [
      publishedAgent,
      JSON.stringify({ model: { reference: "model:fixture-reasoner@v1" } }),
      ada,
      JSON.stringify({ model: { reference: "model:fixture-reasoner@v2" } })
    ]
  );
  await client.query(
    "INSERT INTO organizations (id, slug, display_name, lifecycle_status) VALUES ($1, 'revoked-agent-list', 'Revoked agent list', 'ACTIVE')",
    [revokedOrganization]
  );
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'revoked-agent-list', 'Revoked agent list', 'ACTIVE')",
    [revokedProject, revokedOrganization]
  );
  await client.query(
    "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ('63000000-0000-0000-0000-000000000004', $1, 'revoked-agent', 'Revoked Agent', 'ACTIVE')",
    [revokedProject]
  );
  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', CURRENT_TIMESTAMP)",
    [revokedMembership, revokedOrganization, ada]
  );

  const first = await graphql(service, alphaProject);
  assert.equal(first.errors, undefined);
  assert.equal(first.data.projects.nodes[0].id, alphaProject);
  assert.deepEqual(agents(first).paginationInfo, { pages: 2, current: 0, total: 36 });
  const firstNodes = agents(first).nodes;
  assert.equal(firstNodes.length, 25);
  assert(firstNodes.every((agent) => agent.agentVersions.nodes.length === 0));
  // Ordered by display name, then id: the entities declare their primary key last, so the id the
  // console always adds is the final tie-break for rows that share a name.
  const firstPairs = firstNodes.map((agent) => agent.displayName + "\u0000" + agent.id);
  assert.deepEqual(firstPairs, [...firstPairs].sort());

  const second = await graphql(service, alphaProject, 25, 1);
  assert.equal(second.errors, undefined);
  assert.deepEqual(agents(second).paginationInfo, { pages: 2, current: 1, total: 36 });
  const allIds = firstNodes.concat(agents(second).nodes).map((agent) => agent.id);
  assert.equal(new Set(allIds).size, 36);

  const publishedNode = agents(second).nodes.find((agent) => agent.id === publishedAgent);
  assert.equal(publishedNode.agentVersions.nodes.length, 1);
  assert.equal(publishedNode.agentVersions.nodes[0].versionNumber, 2, "the higher of the two seeded version_number rows must win");
  assert.equal(publishedNode.agentVersions.nodes[0].canonicalDocument.model.reference, "model:fixture-reasoner@v2");

  const deprecated = await graphql(service, alphaProject, 50, 0, { lifecycleStatus: { eq: "DEPRECATED" } });
  assert.equal(deprecated.errors, undefined);
  assert(agents(deprecated).nodes.length > 0);
  assert(agents(deprecated).nodes.every((agent) => agent.lifecycleStatus === "DEPRECATED"));

  const literalSearch = await graphql(service, alphaProject, 50, 0, { displayName: { ilike: likePattern("%_") } });
  assert.equal(literalSearch.errors, undefined);
  assert.deepEqual(agents(literalSearch).nodes.map((agent) => agent.slug), ["literal-percent-underscore"]);

  await client.query("UPDATE organization_memberships SET ended_at = CURRENT_TIMESTAMP WHERE id = $1", [alphaMembership]);
  const revokedAlpha = await graphql(service, alphaProject);
  assert.equal(revokedAlpha.errors, undefined);
  assert.deepEqual(revokedAlpha.data, { projects: { nodes: [] } });
  await client.query("UPDATE organization_memberships SET ended_at = NULL WHERE id = $1", [alphaMembership]);

  for (const projectId of [privateProject, revokedProject, "50000000-0000-0000-0000-000000000099"]) {
    const inaccessible = await graphql(service, projectId);
    assert.equal(inaccessible.errors, undefined);
    assert.deepEqual(inaccessible.data, { projects: { nodes: [] } });
  }
  // The agents and versions of a hidden project are hidden when read directly, too.
  const direct = await fetch("http://127.0.0.1:" + port + "/graphql", {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: "sf_session=" + service.signFixtureSession(ada) },
    body: JSON.stringify({ query: "query Direct($project: String!) { agents(filters: { projectId: { eq: $project } }) { nodes { id } } }", variables: { project: revokedProject } })
  }).then((response) => response.json());
  assert.deepEqual(direct.data.agents.nodes, []);

  const indexes = await client.query(
    "SELECT indexname FROM pg_indexes WHERE schemaname = 'public' AND tablename = 'agents'"
  );
  assert(indexes.rows.some((row) => row.indexname === "agents_directory_keyset"));
  assert(indexes.rows.some((row) => row.indexname === "agents_directory_lifecycle_keyset"));
} finally {
  // publishedAgent is excluded here and reaped only by the isolated database drop below, mirroring
  // agent-authoring-access.mjs — kept distinct from the bulk cleanup rather than relying on a
  // referential-integrity constraint to reject it, since Aurora DSQL migration V012 no longer defines
  // one for agent_versions.agent_id.
  await client.query("DELETE FROM agents WHERE id::text LIKE '61000000-0000-0000-0000-%' AND id != $1", [publishedAgent]);
  await client.query("DELETE FROM agents WHERE id = '63000000-0000-0000-0000-000000000004'");
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [revokedMembership]);
  await client.query("DELETE FROM projects WHERE id = $1", [revokedProject]);
  await client.query("DELETE FROM organizations WHERE id = $1", [revokedOrganization]);
  await client.end();
  await service.stop();
  await database.drop();
}
