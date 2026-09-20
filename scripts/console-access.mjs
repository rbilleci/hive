import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const bea = "00000000-0000-0000-0000-000000000002";
const alpha = "10000000-0000-0000-0000-000000000001";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const commandConsole = "50000000-0000-0000-0000-000000000001";
const commandNavigator = "60000000-0000-0000-0000-000000000001";
const port = 18100;

const contextQuery = `query ConsoleContext {
  consoleContext {
    principal { id displayName }
    organizations { id slug projects { id slug } }
    capabilities { code scopeType scopeId }
    revision
  }
}`;
const preferencesQuery = "query DisplayPreferences { displayPreferences { colorScheme density sidebarState } }";
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

const database = await createIsolatedDatabase("console_access");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  await client.query("DELETE FROM principal_display_preferences WHERE principal_id = ANY($1::uuid[])", [[ada, bea]]);
  const adaContext = (await graphql(service, ada, contextQuery)).consoleContext;
  assert.equal(adaContext.principal.id, ada);
  assert.deepEqual(adaContext.organizations.map((organization) => organization.id), [
    alpha, "10000000-0000-0000-0000-000000000003", "10000000-0000-0000-0000-000000000002"
  ]);
  assert.equal(adaContext.organizations.some((organization) => organization.id === privateOrganization), false);
  assert(adaContext.capabilities.some((capability) => capability.code === "PROJECT.VIEW" && capability.scopeId === commandConsole));
  assert(adaContext.capabilities.some((capability) => capability.code === "AGENT_DRAFT.UPDATE" && capability.scopeId === commandConsole));
  assert.match(adaContext.revision, /^[a-f0-9]{64}$/);

  const beaContext = (await graphql(service, bea, contextQuery)).consoleContext;
  assert.deepEqual(beaContext.organizations.map((organization) => organization.id), [privateOrganization]);
  assert.equal(beaContext.capabilities.some((capability) => capability.scopeId === commandConsole), false);

  assert.deepEqual((await graphql(service, ada, preferencesQuery)).displayPreferences, {
    colorScheme: "LIGHT", density: "COMFORTABLE", sidebarState: "EXPANDED"
  });
  const saved = await graphql(service, ada, updatePreferences, {
    input: { colorScheme: "DARK", density: "COMPACT", sidebarState: "COLLAPSED" }
  });
  assert.deepEqual(saved.updateDisplayPreferences, {
    displayPreferences: { colorScheme: "DARK", density: "COMPACT", sidebarState: "COLLAPSED" }, problems: []
  });
  const updatedInPlace = await graphql(service, ada, updatePreferences, {
    input: { colorScheme: "LIGHT", density: "COMFORTABLE", sidebarState: "EXPANDED" }
  });
  assert.deepEqual(updatedInPlace.updateDisplayPreferences, {
    displayPreferences: { colorScheme: "LIGHT", density: "COMFORTABLE", sidebarState: "EXPANDED" }, problems: []
  });
  const contextAfterPreferences = (await graphql(service, ada, contextQuery)).consoleContext;
  assert.equal(contextAfterPreferences.revision, adaContext.revision, "visual preferences are not an authorization input");
  assert.deepEqual(contextAfterPreferences.capabilities, adaContext.capabilities, "visual preferences do not change capabilities");
  assert.deepEqual((await graphql(service, bea, preferencesQuery)).displayPreferences, {
    colorScheme: "LIGHT", density: "COMFORTABLE", sidebarState: "EXPANDED"
  });
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
  const revokedContext = (await graphql(service, ada, contextQuery)).consoleContext;
  assert.equal(revokedContext.capabilities.some((capability) => capability.code === "AGENT_DRAFT.UPDATE" && capability.scopeId === commandConsole), false);
  const prohibited = await graphql(service, ada, updateDraft, {
    input: { projectId: commandConsole, agentId: commandNavigator, expectedRevision: 1, document: { general: { displayName: "No write" } } }
  });
  assert.deepEqual(prohibited.updateAgentDraft.problems, [{
    __typename: "AgentDraftAuthorizationProblem", code: "FORBIDDEN", message: "You do not have permission to edit this draft."
  }]);
} finally {
  await client.end();
  await service.stop();
  await database.drop();
}
