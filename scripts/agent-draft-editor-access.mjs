import { readFileSync } from "node:fs";
import { buildSchema } from "graphql";
import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const bea = "00000000-0000-0000-0000-000000000002";
const alpha = "10000000-0000-0000-0000-000000000001";
const commandConsole = "50000000-0000-0000-0000-000000000001";
const fleetAnalytics = "50000000-0000-0000-0000-000000000002";
const privateProject = "50000000-0000-0000-0000-000000000003";
const commandNavigator = "60000000-0000-0000-0000-000000000001";
const privateAgent = "60000000-0000-0000-0000-000000000004";
const readerMembership = "77000000-0000-0000-0000-000000000001";
const port = 18097;

// The generated `AgentDrafts` object: its columns, its `agents` relation and its computed fields.
const draftFields = "agentId document revision validationStatus validatedAt canUpdate validationDiagnostics agents { slug displayName lifecycleStatus }";
const problemFields = "__typename code message resourceId expectedRevision actualRevision";
// `Agents.draft` is the stored draft, or the default draft of an agent that has none yet.
const draftQuery = "query Draft($projectId: String!, $agentId: String!) { agents(filters: { id: { eq: $agentId }, projectId: { eq: $projectId } }) { nodes { draft { " + draftFields + " } } } }";

function draftOf(response) {
  assert.equal(response.errors, undefined);
  assert.equal(response.data.agents.nodes.length, 1);
  return response.data.agents.nodes[0].draft;
}
const updateMutation = "mutation Update($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { " + draftFields + " } problems { " + problemFields + " } } }";
const validateMutation = "mutation Validate($input: ValidateAgentDraftInput!) { validateAgentDraft(input: $input) { agentDraft { " + draftFields + " } problems { " + problemFields + " } } }";

async function graphql(service, principal, query, variables) {
  const response = await fetch("http://127.0.0.1:" + port + "/graphql", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Cookie: "sf_session=" + service.signFixtureSession(principal)
    },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  return response.json();
}

function pause(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function waitForRoleLockWaiters(client, minimum) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const waiters = await client.query(`
      SELECT count(*)::int AS count
      FROM pg_stat_activity
      WHERE datname = current_database()
        AND wait_event_type = 'Lock'
        AND query LIKE '%console_role_assignments%'
      `);
    if (waiters.rows[0].count >= minimum) {
      return;
    }
    await pause(25);
  }
  throw new Error("The editor-role revocation race did not reach its required lock state.");
}

async function raceRevocationBeforeCommand(client, command) {
  const roleLock = await postgresClient(database.name);
  const revoker = await postgresClient(database.name);
  try {
    await roleLock.query("BEGIN");
    await roleLock.query(
      "SELECT 1 FROM console_role_assignments WHERE project_id = $1 AND principal_id = $2 AND role_code = 'AGENT_DEVELOPER' FOR UPDATE",
      [commandConsole, ada]
    );
    const revocation = revoker.query(
      "DELETE FROM console_role_assignments WHERE project_id = $1 AND principal_id = $2 AND role_code = 'AGENT_DEVELOPER'",
      [commandConsole, ada]
    );
    await waitForRoleLockWaiters(client, 1);
    let commandFinished = false;
    const result = command().finally(() => { commandFinished = true; });
    await waitForRoleLockWaiters(client, 2);
    assert.equal(commandFinished, false, "A command must wait for a concurrent editor-role revocation.");
    await roleLock.query("COMMIT");
    const removed = await revocation;
    assert.equal(removed.rowCount, 1);
    return result;
  } finally {
    await roleLock.query("ROLLBACK").catch(() => {});
    await roleLock.end();
    await revoker.end();
  }
}

async function draftAndAuditState(client) {
  const state = await client.query(`
    SELECT draft.document::text, draft.revision, draft.validation_status, draft.validation_diagnostics::text,
           (SELECT count(*) FROM agent_draft_audit_events event WHERE event.agent_id = draft.agent_id) AS audit_count
    FROM agent_drafts draft
    WHERE draft.agent_id = $1
    `, [commandNavigator]);
  return state.rows[0];
}

async function restoreEditorRole(client) {
  await client.query(
    "INSERT INTO console_role_assignments (id, principal_id, organization_id, project_id, role_code) VALUES ($1, $2, NULL, $3, 'AGENT_DEVELOPER') ON CONFLICT DO NOTHING",
    ["81000000-0000-0000-0000-000000000005", ada, commandConsole]
  );
}

function input(revision, document) {
  return { projectId: commandConsole, agentId: commandNavigator, expectedRevision: revision, ...(document === undefined ? {} : { document }) };
}

const database = await createIsolatedDatabase("agent_draft_editor_access");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  const first = await graphql(service, ada, draftQuery, { projectId: commandConsole, agentId: commandNavigator });
  const firstDraft = draftOf(first);
  assert.equal(firstDraft.agentId, commandNavigator);
  assert.equal(firstDraft.canUpdate, true);
  assert.equal(firstDraft.revision, 1);
  assert.equal(firstDraft.validationStatus, "NOT_VALIDATED");
  assert.deepEqual(firstDraft.validationDiagnostics, []);
  assert.equal(firstDraft.document.general.displayName, "Feedback Triage Agent");
  assert.deepEqual(firstDraft.agents, { slug: "feedback-triage-agent", displayName: "Feedback Triage Agent", lifecycleStatus: "ACTIVE" });
  // Reading the default draft stores nothing.
  assert.equal((await client.query("SELECT count(*)::int AS drafts FROM agent_drafts WHERE agent_id = $1", [commandNavigator])).rows[0].drafts, 0);

  const completeDocument = {
    general: { displayName: "Draft Navigator", description: "Routes fleet requests safely." },
    instructions: { system: "Route requests through the approved command protocol." },
    harness: { key: "approved-harness" },
    model: { profile: "command-model-v1" },
    tools: [{ name: "directory" }],
    limits: { maxTokens: 2048 },
    evaluations: { required: true }
  };
  const saved = await graphql(service, ada, updateMutation, { input: input(1, completeDocument) });
  assert.equal(saved.errors, undefined);
  assert.deepEqual(saved.data.updateAgentDraft.problems, []);
  assert.equal(saved.data.updateAgentDraft.agentDraft.revision, 2);
  assert.equal(saved.data.updateAgentDraft.agentDraft.validationStatus, "NOT_VALIDATED");
  assert.equal(saved.data.updateAgentDraft.agentDraft.agents.slug, "feedback-triage-agent");
  assert.equal(draftOf(await graphql(service, ada, draftQuery, { projectId: commandConsole, agentId: commandNavigator })).revision, 2);
  assert.deepEqual(saved.data.updateAgentDraft.agentDraft.document, completeDocument);

  const validated = await graphql(service, ada, validateMutation, { input: input(2) });
  assert.equal(validated.errors, undefined);
  assert.equal(validated.data.validateAgentDraft.agentDraft.revision, 3);
  assert.equal(validated.data.validateAgentDraft.agentDraft.validationStatus, "VALID");
  assert.deepEqual(validated.data.validateAgentDraft.agentDraft.validationDiagnostics, []);

  const invalidDocument = { ...completeDocument, general: { displayName: "", description: "" } };
  const invalidSave = await graphql(service, ada, updateMutation, { input: input(3, invalidDocument) });
  assert.equal(invalidSave.data.updateAgentDraft.agentDraft.revision, 4);
  const invalidValidation = await graphql(service, ada, validateMutation, { input: input(4) });
  const invalidDraft = invalidValidation.data.validateAgentDraft.agentDraft;
  assert.equal(invalidDraft.revision, 5);
  assert.equal(invalidDraft.validationStatus, "INVALID");
  assert.deepEqual(invalidDraft.validationDiagnostics.map((diagnostic) => [diagnostic.severity, diagnostic.path]), [
    ["ERROR", ["general", "displayName"]],
    ["WARNING", ["general", "description"]]
  ]);

  const staleValidation = await graphql(service, ada, validateMutation, { input: input(4) });
  assert.equal(staleValidation.data.validateAgentDraft.agentDraft, null);
  assert.deepEqual(staleValidation.data.validateAgentDraft.problems, [{
    __typename: "Problem",
    code: "REVISION_CONFLICT",
    message: "This draft changed after you opened it.",
    resourceId: commandNavigator,
    expectedRevision: 4,
    actualRevision: 5
  }]);
  const audit = await client.query(
    "SELECT action, revision FROM agent_draft_audit_events WHERE agent_id = $1 ORDER BY revision", [commandNavigator]
  );
  assert.deepEqual(audit.rows, [
    { action: "UPDATED", revision: "2" }, { action: "VALIDATED", revision: "3" },
    { action: "UPDATED", revision: "4" }, { action: "VALIDATED", revision: "5" }
  ]);

  const beforeRevokedUpdate = await draftAndAuditState(client);
  const revokedUpdate = await raceRevocationBeforeCommand(client, () => graphql(service, ada, updateMutation, {
    input: input(5, completeDocument)
  }));
  assert.deepEqual(revokedUpdate.data.updateAgentDraft, {
    agentDraft: null,
    problems: [{
      __typename: "Problem", code: "FORBIDDEN", message: "You do not have permission to edit this draft.",
      resourceId: null, expectedRevision: null, actualRevision: null
    }]
  });
  assert.deepEqual(await draftAndAuditState(client), beforeRevokedUpdate);

  await restoreEditorRole(client);
  const beforeRevokedValidation = await draftAndAuditState(client);
  const revokedValidation = await raceRevocationBeforeCommand(client, () => graphql(service, ada, validateMutation, {
    input: input(5)
  }));
  assert.deepEqual(revokedValidation.data.validateAgentDraft, {
    agentDraft: null,
    problems: [{
      __typename: "Problem", code: "FORBIDDEN", message: "You do not have permission to edit this draft.",
      resourceId: null, expectedRevision: null, actualRevision: null
    }]
  });
  assert.deepEqual(await draftAndAuditState(client), beforeRevokedValidation);
  await restoreEditorRole(client);

  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL)",
    [readerMembership, alpha, bea]
  );
  const reader = await graphql(service, bea, draftQuery, { projectId: commandConsole, agentId: commandNavigator });
  assert.equal(draftOf(reader).canUpdate, false);
  const readerWrite = await graphql(service, bea, validateMutation, { input: input(5) });
  assert.deepEqual(readerWrite.data.validateAgentDraft.problems, [{
    __typename: "Problem", code: "FORBIDDEN", message: "You do not have permission to edit this draft.",
    resourceId: null, expectedRevision: null, actualRevision: null
  }]);

  for (const [projectId, agentId] of [
    [fleetAnalytics, commandNavigator], [privateProject, privateAgent], [commandConsole, "77000000-0000-0000-0000-000000000099"],
    ["not-a-uuid", commandNavigator], [commandConsole, "not-a-uuid"]
  ]) {
    const unavailable = await graphql(service, ada, draftQuery, { projectId, agentId });
    if (projectId === "not-a-uuid" || agentId === "not-a-uuid") {
      // A malformed id is a Seaography type-conversion error, not "no row"; the console never sends one.
      assert.equal(unavailable.data, null);
      assert.equal(unavailable.errors.length, 1);
    } else {
      assert.deepEqual(unavailable.data, { agents: { nodes: [] } });
    }
    const unavailableWrite = await graphql(service, ada, validateMutation, {
      input: { projectId, agentId, expectedRevision: 5 }
    });
    assert.deepEqual(unavailableWrite.data.validateAgentDraft.problems, [{
      __typename: "Problem", code: "NOT_FOUND", message: "This agent is unavailable.",
      resourceId: null, expectedRevision: null, actualRevision: null
    }]);
  }

  const schema = await graphql(service, ada, "query MutationSchema { __schema { mutationType { fields { name } } } }", {});
  // Field order is not part of a GraphQL contract; the committed SDL is the reference set.
  const contractMutations = buildSchema(readFileSync("schema/hive.graphql", "utf8")).getMutationType().getFields();
  assert.deepEqual(schema.data.__schema.mutationType.fields.map((field) => field.name).sort(), Object.keys(contractMutations).sort());
} finally {
  await restoreEditorRole(client);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [readerMembership]);
  await client.query("DELETE FROM agent_draft_audit_events WHERE agent_id = $1", [commandNavigator]);
  await client.query("DELETE FROM agent_drafts WHERE agent_id = $1", [commandNavigator]);
  await client.end();
  await service.stop();
  await database.drop();
}
