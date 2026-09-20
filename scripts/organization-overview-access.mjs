import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const noMembership = "00000000-0000-0000-0000-000000000099";
const alpha = "10000000-0000-0000-0000-000000000001";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const temporaryProject = "59999999-0000-0000-0000-000000000001";
const duplicateProject = "59999999-0000-0000-0000-000000000002";
const invalidLifecycleProject = "59999999-0000-0000-0000-000000000003";
const query = [
  "query Organization($id: ID!, $first: Int!, $after: String) {",
  "  organization(id: $id) {",
  "    id slug displayName lifecycleStatus",
  "    projects(first: $first, after: $after) {",
  "      edges { cursor node { id slug displayName lifecycleStatus } }",
  "      pageInfo { hasNextPage endCursor }",
  "      totalCount",
  "    }",
  "  }",
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
  const first = await graphql(port, service.signFixtureSession, ada, { id: alpha, first: 1, after: null });
  assert.equal(first.errors, undefined);
  assert.equal(first.data.organization.id, alpha);
  assert.equal(first.data.organization.slug, "product");
  assert.equal(first.data.organization.displayName, "Product");
  assert.equal(first.data.organization.lifecycleStatus, "ACTIVE");
  assert.deepEqual(first.data.organization.projects.edges.map((edge) => edge.node.slug), ["customer-feedback-copilot"]);
  assert.equal(first.data.organization.projects.totalCount, 2);
  assert.equal(first.data.organization.projects.pageInfo.hasNextPage, true);

  const second = await graphql(port, service.signFixtureSession, ada, {
    id: alpha, first: 1, after: first.data.organization.projects.pageInfo.endCursor
  });
  assert.equal(second.errors, undefined);
  assert.deepEqual(second.data.organization.projects.edges.map((edge) => edge.node.slug), ["usage-analytics"]);
  assert.equal(second.data.organization.projects.pageInfo.hasNextPage, false);

  const inaccessible = await graphql(port, service.signFixtureSession, ada, {
    id: privateOrganization, first: 1, after: null
  });
  const absent = await graphql(port, service.signFixtureSession, ada, {
    id: "10000000-0000-0000-0000-000000000099", first: 1, after: null
  });
  const malformed = await graphql(port, service.signFixtureSession, ada, { id: "not-a-uuid", first: 1, after: null });
  const noAccess = await graphql(port, service.signFixtureSession, noMembership, { id: alpha, first: 1, after: null });
  for (const result of [inaccessible, absent, malformed, noAccess]) {
    assert.equal(result.errors, undefined);
    assert.deepEqual(result.data, { organization: null });
  }

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
