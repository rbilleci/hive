import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const bea = "00000000-0000-0000-0000-000000000002";
const alpha = "10000000-0000-0000-0000-000000000001";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const project = "50000000-0000-0000-0000-000000000003";
const membership = "a1200000-0000-0000-0000-000000000001";
const authorMembership = "a1200000-0000-0000-0000-000000000002";
const authorRole = "a1200000-0000-0000-0000-000000000003";
const port = 18112;
const fixtureName = "M12 Fixture " + Date.now().toString().slice(-8);

const draftFields = "agentId slug displayName revision validationStatus validationDiagnostics { code severity message path } canUpdate canPublish latestVersion document";
const versionFields = "id agentId number canonicalDocument contentDigest dependencies catalogReleaseId catalogReleaseDigest publishedBy publishedAt";
const problemFields = "__typename code message ... on AgentDraftRevisionConflict { resourceId expectedRevision actualRevision }";

async function graphql(service, principal, query, variables) {
  const response = await fetch("http://127.0.0.1:" + port + "/graphql", {
    method: "POST", headers: { "Content-Type": "application/json", Cookie: "sf_session=" + service.signFixtureSession(principal) },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  return response.json();
}

const database = await createIsolatedDatabase("agent_authoring_access");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  // Published versions deliberately cannot be deleted. Keep these proof records in
  // a private project and withdraw Ada's temporary authority before another suite
  // observes the shared fixture catalog.
  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL) ON CONFLICT (id) DO NOTHING",
    [authorMembership, privateOrganization, ada]
  );
  await client.query(
    "INSERT INTO console_role_assignments (id, principal_id, organization_id, project_id, role_code) VALUES ($1, $2, NULL, $3, 'AGENT_DEVELOPER') ON CONFLICT (id) DO NOTHING",
    [authorRole, ada, project]
  );
  const created = await graphql(service, ada,
    "mutation Create($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { " + draftFields + " } agentVersion { id } problems { " + problemFields + " } } }",
    { input: { projectId: project, displayName: fixtureName } });
  assert.deepEqual(created.data.createAgentDraft.problems, []);
  const draft = created.data.createAgentDraft.agentDraft;
  assert.equal(draft.revision, 1);
  assert.equal(draft.canUpdate, true);
  assert.equal(draft.canPublish, true);
  const agent = draft.agentId;

  const warningDocument = {
    general: { displayName: fixtureName, description: "" },
    instructions: { source: "# Local only\nNo remote renderer.", language: "markdown" },
    harness: { source: "def run(value):\n    return value\n", language: "python" },
    model: { reference: "model:local-safe-chat@v2" }, limits: { maxTokens: 2048 },
    dependencies: ["model:local-safe-chat@v2", "tool:http-metadata@v1"]
  };
  const saved = await graphql(service, ada,
    "mutation Save($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { " + draftFields + " } problems { " + problemFields + " } } }",
    { input: { projectId: project, agentId: agent, expectedRevision: 1, document: warningDocument } });
  assert.equal(saved.data.updateAgentDraft.agentDraft.revision, 2);
  const validated = await graphql(service, ada,
    "mutation Validate($input: ValidateAgentDraftInput!) { validateAgentDraft(input: $input) { agentDraft { " + draftFields + " } problems { " + problemFields + " } } }",
    { input: { projectId: project, agentId: agent, expectedRevision: 2 } });
  assert.equal(validated.data.validateAgentDraft.agentDraft.validationStatus, "VALID");
  assert.deepEqual(validated.data.validateAgentDraft.agentDraft.validationDiagnostics.map((value) => value.code), ["DESCRIPTION_RECOMMENDED"]);
  const beforeBlocked = await client.query("SELECT count(*)::int AS versions, revision FROM agent_versions RIGHT JOIN agent_drafts ON agent_drafts.agent_id = agent_versions.agent_id WHERE agent_drafts.agent_id = $1 GROUP BY revision", [agent]);
  const blocked = await graphql(service, ada,
    "mutation Publish($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentDraft { revision } agentVersion { id } problems { " + problemFields + " } } }",
    { input: { projectId: project, agentId: agent, expectedRevision: 3, warningsAcknowledged: false } });
  assert.equal(blocked.data.publishAgentDraft.problems[0].code, "WARNING_ACKNOWLEDGEMENT_REQUIRED");
  assert.deepEqual(await client.query("SELECT count(*)::int AS versions, revision FROM agent_versions RIGHT JOIN agent_drafts ON agent_drafts.agent_id = agent_versions.agent_id WHERE agent_drafts.agent_id = $1 GROUP BY revision", [agent]), beforeBlocked);

  const firstPublish = await graphql(service, ada,
    "mutation Publish($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentDraft { revision latestVersion } agentVersion { " + versionFields + " } problems { " + problemFields + " } } }",
    { input: { projectId: project, agentId: agent, expectedRevision: 3, warningsAcknowledged: true } });
  assert.deepEqual(firstPublish.data.publishAgentDraft.problems, []);
  const first = firstPublish.data.publishAgentDraft.agentVersion;
  assert.equal(first.number, 1);
  assert.deepEqual(first.dependencies, ["model:local-safe-chat@v2", "tool:http-metadata@v1"]);
  assert.equal(first.catalogReleaseId, "local-2026-08-10");
  assert.equal(firstPublish.data.publishAgentDraft.agentDraft.revision, 3, "publishing must not rewrite the draft");

  const secondDocument = { ...warningDocument, general: { displayName: fixtureName, description: "Documented immutable release." } };
  const savedSecond = await graphql(service, ada,
    "mutation Save($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { " + problemFields + " } } }",
    { input: { projectId: project, agentId: agent, expectedRevision: 3, document: secondDocument } });
  const validatedSecond = await graphql(service, ada,
    "mutation Validate($input: ValidateAgentDraftInput!) { validateAgentDraft(input: $input) { agentDraft { revision validationStatus } problems { " + problemFields + " } } }",
    { input: { projectId: project, agentId: agent, expectedRevision: savedSecond.data.updateAgentDraft.agentDraft.revision } });
  const secondPublish = await graphql(service, ada,
    "mutation Publish($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentVersion { " + versionFields + " } problems { " + problemFields + " } } }",
    { input: { projectId: project, agentId: agent, expectedRevision: validatedSecond.data.validateAgentDraft.agentDraft.revision, warningsAcknowledged: false } });
  const second = secondPublish.data.publishAgentDraft.agentVersion;
  assert.equal(second.number, 2);

  const review = await graphql(service, ada,
    "query Review($projectId: ID!, $agentId: ID!) { agentDraftReview(projectId: $projectId, agentId: $agentId) { contentDigest catalogReleaseId dependencies changedSections diagnostics { code severity } } }",
    { projectId: project, agentId: agent });
  assert.equal(review.data.agentDraftReview.catalogReleaseId, "local-2026-08-10");
  assert.deepEqual(review.data.agentDraftReview.dependencies, first.dependencies);
  const versions = await graphql(service, ada,
    "query Versions($projectId: ID!, $agentId: ID!, $from: ID!, $to: ID!) { agentVersions(projectId: $projectId, agentId: $agentId) { " + versionFields + " } compareAgentVersions(projectId: $projectId, agentId: $agentId, fromVersionId: $from, toVersionId: $to) { changedSections } }",
    { projectId: project, agentId: agent, from: first.id, to: second.id });
  assert.deepEqual(versions.data.agentVersions.map((value) => value.number), [2, 1]);
  assert.deepEqual(versions.data.compareAgentVersions.changedSections, ["general"]);
  // No raw-SQL rewrite-rejection check here: agent_versions_no_update no longer exists under Aurora
  // DSQL compatibility (V012 stopped creating it), and PostgresAgentDraftRepository never UPDATEs or
  // DELETEs agent_versions rows in the first place -- there is no application-level operation left to
  // guard.
  const audit = await client.query("SELECT action FROM agent_authoring_audit_events WHERE agent_id = $1 ORDER BY occurred_at, id", [agent]);
  assert.deepEqual(audit.rows.map((row) => row.action), ["CREATED", "SAVED", "VALIDATED", "PUBLISHED", "SAVED", "VALIDATED", "PUBLISHED"]);

  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL)", [membership, alpha, bea]);
  const denied = await graphql(service, bea,
    "mutation Publish($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentVersion { id } problems { " + problemFields + " } } }",
    { input: { projectId: project, agentId: agent, expectedRevision: 5, warningsAcknowledged: true } });
  assert.equal(denied.data.publishAgentDraft.problems[0].code, "FORBIDDEN");
  const hidden = await graphql(service, ada,
    "query Hidden($projectId: ID!, $agentId: ID!, $versionId: ID!) { agentVersion(projectId: $projectId, agentId: $agentId, versionId: $versionId) { id } }",
    { projectId: "50000000-0000-0000-0000-000000000002", agentId: agent, versionId: first.id });
  assert.equal(hidden.data.agentVersion, null);
  await client.query("UPDATE agents SET lifecycle_status = 'ARCHIVED' WHERE id = $1", [agent]);
  const archivedWrite = await graphql(service, ada,
    "mutation Save($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { " + problemFields + " } } }",
    { input: { projectId: project, agentId: agent, expectedRevision: 5, document: secondDocument } });
  assert.equal(archivedWrite.data.updateAgentDraft.problems[0].code, "FORBIDDEN");
  await client.query("UPDATE agents SET lifecycle_status = 'ACTIVE' WHERE id = $1", [agent]);
} finally {
  await client.query("DELETE FROM console_role_assignments WHERE id = $1", [authorRole]);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [authorMembership]);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [membership]);
  await client.end();
  await service.stop();
  await database.drop();
}
