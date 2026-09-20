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

// Payloads return the generated `AgentDrafts` / `AgentVersions` objects, relations and computed fields included.
const draftFields = "agentId revision validationStatus validationDiagnostics canUpdate canPublish document agents { slug displayName agentVersions(orderBy: { versionNumber: DESC }) { nodes { versionNumber } } }";
const versionFields = "id agentId versionNumber canonicalDocument contentDigest dependencyVersions catalogReleaseId catalogReleaseDigest publishedBy publishedAt";
const problemFields = "__typename code message resourceId expectedRevision actualRevision";

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
  assert.equal(draft.agents.displayName, fixtureName);
  assert.deepEqual(draft.agents.agentVersions.nodes, []);
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
    "mutation Publish($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentDraft { revision agents { agentVersions(orderBy: { versionNumber: DESC }) { nodes { versionNumber } } } } agentVersion { " + versionFields + " } problems { " + problemFields + " } } }",
    { input: { projectId: project, agentId: agent, expectedRevision: 3, warningsAcknowledged: true } });
  assert.deepEqual(firstPublish.data.publishAgentDraft.problems, []);
  const first = firstPublish.data.publishAgentDraft.agentVersion;
  assert.equal(first.versionNumber, 1);
  assert.deepEqual(first.dependencyVersions, ["model:local-safe-chat@v2", "tool:http-metadata@v1"]);
  assert.deepEqual(firstPublish.data.publishAgentDraft.agentDraft.agents.agentVersions.nodes, [{ versionNumber: 1 }]);
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
  assert.equal(second.versionNumber, 2);

  const reviewed = await graphql(service, ada,
    "query Review($agent: AgentsFilterInput!) { agents(filters: $agent) { nodes { draft { revision review { contentDigest catalogReleaseId catalogReleaseDigest dependencies changedSections diagnostics { code severity message path } } } } } }",
    { agent: { id: { eq: agent }, projectId: { eq: project } } });
  assert.equal(reviewed.errors, undefined);
  const review = reviewed.data.agents.nodes[0].draft.review;
  assert.equal(review.catalogReleaseId, "local-2026-08-10");
  assert.equal(review.catalogReleaseDigest, second.catalogReleaseDigest);
  assert.deepEqual(review.dependencies, first.dependencyVersions);
  // The draft is what version 2 was published from: same digest, nothing changed since.
  assert.equal(review.contentDigest, second.contentDigest);
  assert.deepEqual(review.changedSections, []);
  assert.deepEqual(review.diagnostics, []);
  const versions = await graphql(service, ada,
    "query Versions($agentId: String!, $from: String!, $to: String!) { agentVersions(filters: { agentId: { eq: $agentId } }, orderBy: { versionNumber: DESC, id: ASC }) { nodes { id versionNumber contentDigest dependencyVersions agents { projectId slug } } } compared: agentVersions(filters: { id: { eq: $to }, agentId: { eq: $agentId } }) { nodes { comparison(fromVersionId: $from) { from { id versionNumber } changedSections } same: comparison(fromVersionId: $to) { changedSections } foreign: comparison(fromVersionId: $agentId) { changedSections } malformed: comparison(fromVersionId: \"not-a-uuid\") { changedSections } } } }",
    { agentId: agent, from: first.id, to: second.id });
  assert.equal(versions.errors, undefined);
  const generatedVersions = versions.data.agentVersions.nodes;
  assert.deepEqual(generatedVersions.map((value) => value.versionNumber), [2, 1]);
  assert.deepEqual(generatedVersions.map((value) => value.id), [second.id, first.id]);
  assert.equal(generatedVersions[1].contentDigest, first.contentDigest);
  assert.deepEqual(generatedVersions[1].dependencyVersions, first.dependencyVersions);
  assert.equal(generatedVersions[0].agents.projectId, project);
  // A version compared with itself changed nothing; an id that is not a version of this agent compares to null.
  assert.deepEqual(versions.data.compared.nodes, [{
    comparison: { from: { id: first.id, versionNumber: 1 }, changedSections: ["general"] }, same: { changedSections: [] }, foreign: null, malformed: null
  }]);
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
  // A principal with no membership in the agent's organization sees none of its versions.
  const hidden = await graphql(service, "99999999-9999-9999-9999-999999999999",
    "query Hidden($agentId: String!) { agentVersions(filters: { agentId: { eq: $agentId } }) { nodes { id } } }",
    { agentId: agent });
  assert.deepEqual(hidden.data.agentVersions.nodes, []);
  const hiddenDraft = await graphql(service, "99999999-9999-9999-9999-999999999999",
    "query HiddenDraft($agentId: String!) { agentDrafts(filters: { agentId: { eq: $agentId } }) { nodes { agentId } } agents(filters: { id: { eq: $agentId } }) { nodes { draft { agentId } } } }",
    { agentId: agent });
  assert.deepEqual(hiddenDraft.data, { agentDrafts: { nodes: [] }, agents: { nodes: [] } });
  // A member without an editor role reads the draft and is told it may not write it.
  const readOnly = await graphql(service, bea,
    "query ReadOnly($agentId: String!) { agentDrafts(filters: { agentId: { eq: $agentId } }) { nodes { revision canUpdate canPublish } } }",
    { agentId: agent });
  assert.deepEqual(readOnly.data.agentDrafts.nodes, [{ revision: 5, canUpdate: false, canPublish: false }]);
  await client.query("UPDATE agents SET lifecycle_status = 'ARCHIVED' WHERE id = $1", [agent]);
  const archivedWrite = await graphql(service, ada,
    "mutation Save($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { " + problemFields + " } } }",
    { input: { projectId: project, agentId: agent, expectedRevision: 5, document: secondDocument } });
  assert.equal(archivedWrite.data.updateAgentDraft.problems[0].code, "FORBIDDEN");
  const archivedRead = await graphql(service, ada,
    "query Archived($agentId: String!) { agentDrafts(filters: { agentId: { eq: $agentId } }) { nodes { canUpdate canPublish } } }",
    { agentId: agent });
  assert.deepEqual(archivedRead.data.agentDrafts.nodes, [{ canUpdate: false, canPublish: false }]);
  await client.query("UPDATE agents SET lifecycle_status = 'ACTIVE' WHERE id = $1", [agent]);
} finally {
  await client.query("DELETE FROM console_role_assignments WHERE id = $1", [authorRole]);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [authorMembership]);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [membership]);
  await client.end();
  await service.stop();
  await database.drop();
}
