import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const bea = "00000000-0000-0000-0000-000000000002";
const alpha = "10000000-0000-0000-0000-000000000001";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const commandConsole = "50000000-0000-0000-0000-000000000001";
const commandNavigator = "60000000-0000-0000-0000-000000000001";
const port = 18100;

// The console context is generated reads: the requester's own principal row, the organizations and
// projects it can see, and the computed `capabilities` (the requester's codes) on each.
const contextQuery = `query ConsoleContext {
  principals { nodes { id displayName capabilities } }
  organizations(orderBy: { displayName: ASC, id: ASC }) {
    nodes { id slug capabilities projects(orderBy: { displayName: ASC, id: ASC }) { nodes { id slug capabilities } } }
  }
}`;
const preferencesQuery = "query DisplayPreferences { principalDisplayPreferences { nodes { principalId colorScheme density sidebarState } } }";
const updatePreferences = `mutation Update($input: UpdateDisplayPreferencesInput!) {
  updateDisplayPreferences(input: $input) {
    displayPreferences { colorScheme density sidebarState }
    problems { __typename code message }
  }
}`;
const updateDraft = `mutation UpdateDraft($input: UpdateAgentDraftInput!) {
  updateAgentDraft(input: $input) { agentDraft { revision } problems { __typename code message } }
}`;

async function graphql(service, principal, query, variables = {}) {
  const response = await fetch("http://127.0.0.1:" + port + "/graphql", {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: "sf_session=" + service.signFixtureSession(principal) },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.errors, undefined);
  return body.data;
}

function projectCapabilities(context, projectId) {
  return context.organizations.nodes.flatMap((organization) => organization.projects.nodes)
    .find((project) => project.id === projectId)?.capabilities;
}

const database = await createIsolatedDatabase("console_access");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  await client.query("DELETE FROM principal_display_preferences WHERE principal_id = ANY($1::uuid[])", [[ada, bea]]);
  const adaContext = await graphql(service, ada, contextQuery);
  assert.deepEqual(adaContext.principals.nodes, [{ id: ada, displayName: "Ada Lovelace", capabilities: ["PREFERENCES.UPDATE"] }]);
  assert.deepEqual(adaContext.organizations.nodes.map((organization) => organization.id), [
    alpha, "10000000-0000-0000-0000-000000000003", "10000000-0000-0000-0000-000000000002"
  ]);
  assert.equal(adaContext.organizations.nodes.some((organization) => organization.id === privateOrganization), false);
  assert(adaContext.organizations.nodes[0].capabilities.includes("ORGANIZATION.VIEW"));
  assert(projectCapabilities(adaContext, commandConsole).includes("PROJECT.VIEW"));
  assert(projectCapabilities(adaContext, commandConsole).includes("AGENT_DRAFT.UPDATE"));

  const beaContext = await graphql(service, bea, contextQuery);
  assert.deepEqual(beaContext.principals.nodes.map((principal) => principal.id), [bea], "a principal reads only its own row");
  assert.deepEqual(beaContext.organizations.nodes.map((organization) => organization.id), [privateOrganization]);
  assert.equal(projectCapabilities(beaContext, commandConsole), undefined);
  const otherPrincipal = await graphql(service, bea, `{ principals(filters: { id: { eq: "${ada}" } }) { nodes { id } } }`);
  assert.deepEqual(otherPrincipal.principals.nodes, []);

  // No stored row: the console applies its light defaults.
  assert.deepEqual((await graphql(service, ada, preferencesQuery)).principalDisplayPreferences.nodes, []);
  const saved = await graphql(service, ada, updatePreferences, {
    input: { colorScheme: "DARK", density: "COMPACT", sidebarState: "COLLAPSED" }
  });
  assert.deepEqual(saved.updateDisplayPreferences, {
    displayPreferences: { colorScheme: "DARK", density: "COMPACT", sidebarState: "COLLAPSED" }, problems: []
  });
  assert.deepEqual((await graphql(service, ada, preferencesQuery)).principalDisplayPreferences.nodes, [
    { principalId: ada, colorScheme: "DARK", density: "COMPACT", sidebarState: "COLLAPSED" }
  ]);
  const updatedInPlace = await graphql(service, ada, updatePreferences, {
    input: { colorScheme: "LIGHT", density: "COMFORTABLE", sidebarState: "EXPANDED" }
  });
  assert.deepEqual(updatedInPlace.updateDisplayPreferences, {
    displayPreferences: { colorScheme: "LIGHT", density: "COMFORTABLE", sidebarState: "EXPANDED" }, problems: []
  });
  assert.deepEqual((await graphql(service, ada, preferencesQuery)).principalDisplayPreferences.nodes, [
    { principalId: ada, colorScheme: "LIGHT", density: "COMFORTABLE", sidebarState: "EXPANDED" }
  ]);
  const contextAfterPreferences = await graphql(service, ada, contextQuery);
  assert.deepEqual(contextAfterPreferences, adaContext, "visual preferences are not an authorization input");
  assert.deepEqual((await graphql(service, bea, preferencesQuery)).principalDisplayPreferences.nodes, [],
    "preferences are visible to their own principal only");
  const columns = await client.query(`
    SELECT column_name FROM information_schema.columns
    WHERE table_name = 'principal_display_preferences'
    ORDER BY ordinal_position
  `);
  assert.deepEqual(columns.rows.map((row) => row.column_name), [
    "principal_id", "color_scheme", "density", "sidebar_state"
  ]);
  const preferenceAudit = await client.query("SELECT to_regclass('public.display_preferences_audit_events') AS audit_table");
  assert.equal(preferenceAudit.rows[0].audit_table, null);

  // The legacy editor-role row remains deliberately present: only the evaluator's canonical
  // console assignment is revoked, proving projection and direct mutation share that source.
  await client.query("DELETE FROM console_role_assignments WHERE project_id = $1 AND principal_id = $2", [commandConsole, ada]);
  const revokedContext = await graphql(service, ada, contextQuery);
  assert.equal(projectCapabilities(revokedContext, commandConsole).includes("AGENT_DRAFT.UPDATE"), false);
  const prohibited = await graphql(service, ada, updateDraft, {
    input: { projectId: commandConsole, agentId: commandNavigator, expectedRevision: 1, document: { general: { displayName: "No write" } } }
  });
  assert.deepEqual(prohibited.updateAgentDraft.problems, [{
    __typename: "Problem", code: "FORBIDDEN", message: "You do not have permission to edit this draft."
  }]);
} finally {
  await client.end();
  await service.stop();
  await database.drop();
}
