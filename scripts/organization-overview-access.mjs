import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const noMembership = "00000000-0000-0000-0000-000000000099";
const alpha = "10000000-0000-0000-0000-000000000001";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const temporaryProject = "59999999-0000-0000-0000-000000000001";
const duplicateProject = "59999999-0000-0000-0000-000000000002";
const invalidLifecycleProject = "59999999-0000-0000-0000-000000000003";
// The console's OrganizationOverview selection: Seaography's generated `organizations` with its
// `projects` relation field.
const query = [
  "query Organization($id: String!, $limit: Int!, $page: Int!) {",
  "  organizations(filters: { id: { eq: $id } }) { nodes {",
  "    id slug displayName lifecycleStatus",
  "    projects(orderBy: { displayName: ASC, id: ASC }, pagination: { page: { limit: $limit, page: $page } }) {",
  "      nodes { id slug displayName lifecycleStatus }",
  "      paginationInfo { pages current total }",
  "    }",
  "  } }",
  "}"
].join("\n");

async function graphql(port, signFixtureSession, principal, variables) {
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

const port = 18083;
const database = await createIsolatedDatabase("hive_organization_overview");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  const first = await graphql(port, service.signFixtureSession, ada, { id: alpha, limit: 1, page: 0 });
  assert.equal(first.errors, undefined);
  const organization = first.data.organizations.nodes[0];
  assert.equal(organization.id, alpha);
  assert.equal(organization.slug, "product");
  assert.equal(organization.displayName, "Product");
  assert.equal(organization.lifecycleStatus, "ACTIVE");
  assert.deepEqual(organization.projects.nodes.map((node) => node.slug), ["customer-feedback-copilot"]);
  assert.deepEqual(organization.projects.paginationInfo, { pages: 2, current: 0, total: 2 });

  const second = await graphql(port, service.signFixtureSession, ada, { id: alpha, limit: 1, page: 1 });
  assert.equal(second.errors, undefined);
  assert.deepEqual(second.data.organizations.nodes[0].projects.nodes.map((node) => node.slug), ["usage-analytics"]);

  // An organization the principal is not a member of, one that does not exist, and any
  // organization for a principal with no membership are all the same answer: no row.
  const inaccessible = await graphql(port, service.signFixtureSession, ada, { id: privateOrganization, limit: 1, page: 0 });
  const absent = await graphql(port, service.signFixtureSession, ada, { id: "10000000-0000-0000-0000-000000000099", limit: 1, page: 0 });
  const noAccess = await graphql(port, service.signFixtureSession, noMembership, { id: alpha, limit: 1, page: 0 });
  for (const result of [inaccessible, absent, noAccess]) {
    assert.equal(result.errors, undefined);
    assert.deepEqual(result.data, { organizations: { nodes: [] } });
  }
  // Scoping also holds for a project reached directly rather than through its organization.
  const directProjects = await fetch("http://127.0.0.1:" + port + "/graphql", {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: "sf_session=" + service.signFixtureSession(noMembership) },
    body: JSON.stringify({ query: "{ projects { nodes { id } } }" })
  }).then((response) => response.json());
  assert.deepEqual(directProjects.data.projects.nodes, []);

  const indexes = await client.query(
    "SELECT indexname, indexdef FROM pg_indexes WHERE schemaname = 'public' AND tablename = 'projects'"
  );
  assert.match(indexes.rows.find((row) => row.indexname === "projects_organization_slug_case_insensitive").indexdef, /lower\(slug\)/i);
  assert.match(indexes.rows.find((row) => row.indexname === "projects_organization_lifecycle_display_name").indexdef,
    /organization_id, lifecycle_status, display_name/i);
  const foreignKey = await client.query(
    "SELECT confdeltype FROM pg_constraint WHERE conrelid = 'projects'::regclass AND contype = 'f'"
  );
  // Aurora DSQL supports no FOREIGN KEY, so the migrations declare none (check:dsql-conformance).
  assert.deepEqual(foreignKey.rows.map((row) => row.confdeltype), []);

  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, $3, $4, 'ACTIVE')",
    [temporaryProject, alpha, "Case-Sensitive", "Temporary project"]
  );
  await assert.rejects(
    client.query(
      "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, $3, $4, 'ACTIVE')",
      [duplicateProject, alpha, "case-sensitive", "Duplicate project"]
    ),
    (error) => error.code === "23505"
  );
  await assert.rejects(
    client.query(
      "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, $3, $4, 'PENDING')",
      [invalidLifecycleProject, alpha, "invalid-lifecycle", "Invalid lifecycle"]
    ),
    (error) => error.code === "23514"
  );
} finally {
  await client.query("DELETE FROM projects WHERE id = ANY($1::uuid[])",
    [[temporaryProject, duplicateProject, invalidLifecycleProject]]);
  await client.end();
  await service.stop();
  await database.drop();
}
