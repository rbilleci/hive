import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const bea = "00000000-0000-0000-0000-000000000002";
const alpha = "10000000-0000-0000-0000-000000000001";
const project = "50000000-0000-0000-0000-000000000001";
const privateProject = "50000000-0000-0000-0000-000000000003";
const port = 18118;
const fixture = Date.now().toString(36);
const promptName = "M11 Fixture Prompt " + fixture;
const profileName = "M11 Fixture Profile " + fixture;
const policyName = "M11 Fixture Policy " + fixture;
const crossProjectPromptName = "M11 Private Prompt " + fixture;
const archivedName = "M11 Archived Prompt " + fixture;
const collisionName = "Collision Identity " + fixture;
const collisionIdentity = "collision-identity-" + fixture;
const trailingName = "Trailing Identity " + fixture + "-";
const trailingIdentity = "trailing-identity-" + fixture;
const beaPrivateAssignment = randomUUID();
// Generated `ReusableResources` and `ProjectToolConnections` rows; `draft`, `dependentResources`, `arguments`,
// `remoteUrl` and `status` are computed fields.
const resourceFields = "id name identity resourceKind currentDraftRevision currentPublishedVersion draft { contentDigest validationStatus diagnostics } dependentResources reusableResourceVersions(orderBy: { version: DESC }) { nodes { version contentDigest dependencies } }";
const mcpFields = "id projectId serverId name definitionIdentity definitionVersion environment enabled transportType stdioCommand arguments remoteUrl redactedBindings declaredTools declaredResources declaredPrompts lifecycleStatus status revision dependentResources";
const payloadFields = "resource { " + resourceFields + " } mcpServer { " + mcpFields + " } tool { id name redactedSecretReference revision definitionIdentity definitionVersion environment lifecycleStatus } problems { code message resourceId expectedRevision actualRevision }";

async function gql(service, principal, query, variables) {
  const response = await fetch("http://127.0.0.1:" + port + "/graphql", { method: "POST", headers: { "Content-Type": "application/json", Cookie: "sf_session=" + service.signFixtureSession(principal) }, body: JSON.stringify({ query, variables }) });
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.errors, undefined);
  return body.data;
}

// The catalog has no owner, so the console reads the organization next to it: no organization node is "unavailable".
const catalog = "query($organizationId:String!){organizations(filters:{id:{eq:$organizationId}}){nodes{id}} catalogProjectionHeads(filters:{id:{eq:\"local\"}}){nodes{catalogReleases{id source sourceDigest catalogDefinitions(orderBy:{definitionKind:ASC,identity:ASC,version:ASC}){nodes{identity version definitionKind contentDigest availableEnvironments}} catalogEnvironments(orderBy:{environment:ASC}){nodes{environment}}}}}}";
const create = "mutation($input:CreateReusableResourceInput!){createReusableResource(input:$input){" + payloadFields + "}}";
const update = "mutation($input:UpdateReusableResourceDraftInput!){updateReusableResourceDraft(input:$input){" + payloadFields + "}}";
const validate = "mutation($input:ReusableResourceRevisionInput!){validateReusableResource(input:$input){" + payloadFields + "}}";
const publish = "mutation($input:ReusableResourceRevisionInput!){publishReusableResource(input:$input){" + payloadFields + "}}";
// Project rows are read through `projects`: a project the principal cannot see has no node, which is not an empty list.
const tools = "query($projectId:String!){projects(filters:{id:{eq:$projectId}}){nodes{projectToolConnections(orderBy:{name:ASC,id:ASC}){nodes{id name redactedSecretReference revision}}}} projectToolConnections(filters:{projectId:{eq:$projectId}}){nodes{id}}}";
const resource = "query($projectId:String!,$resourceId:String!){projects(filters:{id:{eq:$projectId}}){nodes{reusableResources(filters:{id:{eq:$resourceId}}){nodes{id identity dependentResources}}}}}";
const resources = "query($projectId:String!){projects(filters:{id:{eq:$projectId}}){nodes{reusableResources(orderBy:{identity:ASC,id:ASC}){nodes{id}}}} reusableResources(filters:{projectId:{eq:$projectId}}){nodes{id}}}";
const saveTool = "mutation($input:SaveProjectToolConnectionMetadataInput!){saveProjectToolConnectionMetadata(input:$input){" + payloadFields + "}}";
const mcpServers = "query($projectId:String!){projects(filters:{id:{eq:$projectId}}){nodes{projectToolConnections(orderBy:{name:ASC,id:ASC}){nodes{" + mcpFields + "}}}}}";
const readResource = async (principal, projectId, resourceId) => (await gql(service, principal, resource, { projectId, resourceId })).projects.nodes[0].reusableResources.nodes[0];
const readMcpServers = async (principal, projectId) => (await gql(service, principal, mcpServers, { projectId })).projects.nodes[0].projectToolConnections.nodes;
const createMcp = "mutation($input:CreateProjectMcpServerInput!){createProjectMcpServer(input:$input){" + payloadFields + "}}";
const updateMcp = "mutation($input:UpdateProjectMcpServerInput!){updateProjectMcpServer(input:$input){" + payloadFields + "}}";

const database = await createIsolatedDatabase("configuration_access");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
let resourceId = null;
let profileId = null;
let policyId = null;
let toolId = null;
let mcpId = null;
let privatePromptId = null;
let archivedId = null;
try {
  const adaCatalog = await gql(service, ada, catalog, { organizationId: alpha });
  assert.deepEqual(adaCatalog.organizations.nodes, [{ id: alpha }]);
  const release = adaCatalog.catalogProjectionHeads.nodes[0].catalogReleases;
  assert.equal(release.id, "local-2026-08-10");
  assert.match(release.source, /^git:/);
  assert.match(release.sourceDigest, /^[0-9a-f]{64}$/);
  assert(release.catalogDefinitions.nodes.some((definition) => definition.definitionKind === "model" && definition.identity === "local-safe-chat"));
  assert.deepEqual(release.catalogEnvironments.nodes.map((entry) => entry.environment), ["DEVELOPMENT", "PRODUCTION", "STAGING"]);
  // No raw-SQL immutability probe here: catalog_releases_no_update/catalog_definitions_no_update no
  // longer exist under Aurora DSQL compatibility (V011 stopped creating them), and
  // PostgresConfigurationRepository has no write path into either table in the first place -- there is
  // no application-level operation left to guard.
  assert.deepEqual((await gql(service, bea, catalog, { organizationId: alpha })).organizations.nodes, [], "the catalog of an organization the principal is not a member of is unavailable");
  const stranger = await gql(service, randomUUID(), catalog, { organizationId: alpha });
  assert.deepEqual(stranger.organizations.nodes, []);
  assert.deepEqual(stranger.catalogProjectionHeads.nodes, [], "CATALOG.VIEW needs an active organization membership");

  const denied = (await gql(service, bea, create, { input: { projectId: project, kind: "PROMPT", name: "Denied resource", content: "no access", dependencies: [] } })).createReusableResource;
  assert.equal(denied.problems[0].code, "FORBIDDEN");
  const beaResources = await gql(service, bea, resources, { projectId: project });
  assert.deepEqual(beaResources.projects.nodes, [], "unauthorized project resource reads are not represented as empty lists");
  assert.deepEqual(beaResources.reusableResources.nodes, [], "the generated root is scoped to the principal's projects");
  const beaTools = await gql(service, bea, tools, { projectId: project });
  assert.deepEqual(beaTools.projects.nodes, [], "unauthorized project tool reads are not represented as empty lists");
  assert.deepEqual(beaTools.projectToolConnections.nodes, [], "the generated root is scoped to the principal's projects");
  assert.deepEqual((await gql(service, bea, mcpServers, { projectId: project })).projects.nodes, [], "unauthorized MCP server reads are not represented as empty lists");
  const before = await client.query("SELECT count(*)::int AS count FROM reusable_resources WHERE project_id = $1", [project]);
  const invalid = (await gql(service, ada, create, { input: { projectId: project, kind: "MODEL_PROFILE", name: "Broken profile", content: "maxTokens:512\nenvironment:DEVELOPMENT", dependencies: ["model:does-not-exist@v1"] } })).createReusableResource;
  assert.equal(invalid.problems[0].code, "INVALID_DRAFT");
  const after = await client.query("SELECT count(*)::int AS count FROM reusable_resources WHERE project_id = $1", [project]);
  assert.equal(after.rows[0].count, before.rows[0].count, "failed creation changed no identity or draft state");

  const created = (await gql(service, ada, create, { input: { projectId: project, kind: "PROMPT", name: promptName, content: "Hello {{operator}}", dependencies: [] } })).createReusableResource;
  assert.deepEqual(created.problems, []); resourceId = created.resource.id;
  assert.equal(created.resource.draft.validationStatus, "UNVALIDATED");
  const updated = (await gql(service, ada, update, { input: { projectId: project, resourceId, expectedRevision: 1, content: "Hello {{operator}}", dependencies: [] } })).updateReusableResourceDraft;
  assert.equal(updated.resource.currentDraftRevision, 2);
  const stale = (await gql(service, ada, update, { input: { projectId: project, resourceId, expectedRevision: 1, content: "stale", dependencies: [] } })).updateReusableResourceDraft;
  assert.equal(stale.problems[0].code, "REVISION_CONFLICT");
  assert.deepEqual([stale.problems[0].resourceId, stale.problems[0].expectedRevision, stale.problems[0].actualRevision], [resourceId, 1, 2]);
  const validated = (await gql(service, ada, validate, { input: { projectId: project, resourceId, expectedRevision: 2 } })).validateReusableResource;
  assert.equal(validated.resource.draft.validationStatus, "VALID");
  const published = (await gql(service, ada, publish, { input: { projectId: project, resourceId, expectedRevision: 2 } })).publishReusableResource;
  assert.equal(published.resource.currentPublishedVersion, 1);
  assert.equal(published.resource.reusableResourceVersions.nodes[0].version, 1);
  assert.match(published.resource.reusableResourceVersions.nodes[0].contentDigest, /^[0-9a-f]{64}$/);
  const immutable = await client.query("SELECT resource_id FROM reusable_resource_versions WHERE resource_id = $1", [resourceId]);
  // No raw-SQL rewrite-rejection check here: reusable_resource_versions_no_update no longer exists
  // under Aurora DSQL compatibility (V011 stopped creating it), and PostgresConfigurationRepository
  // never UPDATEs or DELETEs reusable_resource_versions in the first place -- there is no
  // application-level operation left to guard.
  assert.equal(immutable.rowCount, 1);
  const history = "query($id:String!){reusableResourceDrafts(filters:{resourceId:{eq:$id}},orderBy:{revision:ASC}){nodes{revision validationStatus}} reusableResourceVersions(filters:{resourceId:{eq:$id}}){nodes{version}}}";
  const adaHistory = await gql(service, ada, history, { id: resourceId });
  assert.deepEqual(adaHistory.reusableResourceDrafts.nodes, [{ revision: 1, validationStatus: "UNVALIDATED" }, { revision: 2, validationStatus: "VALID" }]);
  assert.deepEqual(adaHistory.reusableResourceVersions.nodes, [{ version: 1 }]);
  const beaHistory = await gql(service, bea, history, { id: resourceId });
  assert.deepEqual([beaHistory.reusableResourceDrafts.nodes, beaHistory.reusableResourceVersions.nodes], [[], []], "drafts and versions are visible with their resource only");
  const promptReference = "prompt:" + published.resource.identity + "@v1";
  const policy = (await gql(service, ada, create, { input: { projectId: project, kind: "POLICY", name: policyName, content: "severity:LOW\nscope:project", dependencies: [promptReference] } })).createReusableResource;
  policyId = policy.resource.id;
  const validPolicy = (await gql(service, ada, validate, { input: { projectId: project, resourceId: policyId, expectedRevision: 1 } })).validateReusableResource;
  assert.equal(validPolicy.resource.draft.validationStatus, "VALID", "an exact published typed prompt reference resolves");
  assert.deepEqual((await readResource(ada, project, resourceId)).dependentResources, [policyName], "the referenced prompt exposes its authorized dependency usage");

  await client.query("INSERT INTO console_role_assignments (id, principal_id, organization_id, project_id, role_code) VALUES ($1, $2, NULL, $3, 'AGENT_DEVELOPER')", [beaPrivateAssignment, bea, privateProject]);
  const privatePrompt = (await gql(service, bea, create, { input: { projectId: privateProject, kind: "PROMPT", name: crossProjectPromptName, content: "Private {{operator}}", dependencies: [] } })).createReusableResource;
  assert.deepEqual(privatePrompt.problems, []); privatePromptId = privatePrompt.resource.id;
  const privateValidated = (await gql(service, bea, validate, { input: { projectId: privateProject, resourceId: privatePromptId, expectedRevision: 1 } })).validateReusableResource;
  assert.equal(privateValidated.resource.draft.validationStatus, "VALID");
  const privatePublished = (await gql(service, bea, publish, { input: { projectId: privateProject, resourceId: privatePromptId, expectedRevision: 1 } })).publishReusableResource;
  assert.equal(privatePublished.resource.currentPublishedVersion, 1);
  const crossReference = "prompt:" + privatePublished.resource.identity + "@v1";
  const crossProjectDependency = (await gql(service, ada, create, { input: { projectId: project, kind: "POLICY", name: "Cross Project Policy " + fixture, content: "severity:LOW\nscope:project", dependencies: [crossReference] } })).createReusableResource;
  assert.equal(crossProjectDependency.problems[0].code, "INVALID_DRAFT", "a typed reusable-resource dependency cannot cross projects");

  const collision = (await gql(service, ada, create, { input: { projectId: project, kind: "PROMPT", name: collisionName, content: "Collision {{operator}}", dependencies: [] } })).createReusableResource;
  assert.equal(collision.resource.identity, collisionIdentity, "a display name receives one explicit normalized identity");
  assert.equal((await gql(service, ada, validate, { input: { projectId: project, resourceId: collision.resource.id, expectedRevision: 1 } })).validateReusableResource.resource.draft.validationStatus, "VALID");
  assert.equal((await gql(service, ada, publish, { input: { projectId: project, resourceId: collision.resource.id, expectedRevision: 1 } })).publishReusableResource.resource.currentPublishedVersion, 1);
  const collisionDuplicate = (await gql(service, ada, create, { input: { projectId: project, kind: "PROMPT", name: collisionIdentity, content: "Duplicate {{operator}}", dependencies: [] } })).createReusableResource;
  assert.equal(collisionDuplicate.problems[0].code, "INVALID_INPUT", "colliding display names are rejected by their canonical identity");
  const collisionWrongVersion = (await gql(service, ada, create, { input: { projectId: project, kind: "POLICY", name: "Collision Wrong Version " + fixture, content: "severity:LOW\nscope:project", dependencies: ["prompt:" + collisionIdentity + "@v2"] } })).createReusableResource;
  assert.equal(collisionWrongVersion.problems[0].code, "INVALID_DRAFT", "a resource reference resolves only its exact published version");
  const collisionConsumerName = "Collision Consumer " + fixture;
  const collisionConsumer = (await gql(service, ada, create, { input: { projectId: project, kind: "POLICY", name: collisionConsumerName, content: "severity:LOW\nscope:project", dependencies: ["prompt:" + collisionIdentity + "@v1"] } })).createReusableResource;
  assert.equal((await gql(service, ada, validate, { input: { projectId: project, resourceId: collisionConsumer.resource.id, expectedRevision: 1 } })).validateReusableResource.resource.draft.validationStatus, "VALID");
  assert.deepEqual((await readResource(ada, project, collision.resource.id)).dependentResources, [collisionConsumerName], "reverse usage is bound to the canonical identity and exact version");

  const trailing = (await gql(service, ada, create, { input: { projectId: project, kind: "PROMPT", name: trailingName, content: "Trailing {{operator}}", dependencies: [] } })).createReusableResource;
  assert.equal(trailing.resource.identity, trailingIdentity, "trailing punctuation is excluded from the canonical identity");
  assert.equal((await gql(service, ada, validate, { input: { projectId: project, resourceId: trailing.resource.id, expectedRevision: 1 } })).validateReusableResource.resource.draft.validationStatus, "VALID");
  assert.equal((await gql(service, ada, publish, { input: { projectId: project, resourceId: trailing.resource.id, expectedRevision: 1 } })).publishReusableResource.resource.currentPublishedVersion, 1);
  const nonCanonicalTrailing = (await gql(service, ada, create, { input: { projectId: project, kind: "POLICY", name: "Trailing Invalid Reference " + fixture, content: "severity:LOW\nscope:project", dependencies: ["prompt:" + trailingIdentity + "-@v1"] } })).createReusableResource;
  assert.equal(nonCanonicalTrailing.problems[0].code, "INVALID_INPUT", "a noncanonical typed resource identity is rejected before persistence");
  const trailingConsumerName = "Trailing Consumer " + fixture;
  const trailingConsumer = (await gql(service, ada, create, { input: { projectId: project, kind: "POLICY", name: trailingConsumerName, content: "severity:LOW\nscope:project", dependencies: ["prompt:" + trailingIdentity + "@v1"] } })).createReusableResource;
  assert.equal((await gql(service, ada, validate, { input: { projectId: project, resourceId: trailingConsumer.resource.id, expectedRevision: 1 } })).validateReusableResource.resource.draft.validationStatus, "VALID");
  assert.deepEqual((await readResource(ada, project, trailing.resource.id)).dependentResources, [trailingConsumerName], "reverse usage uses the same canonical identity as resource lookup");

  const profile = (await gql(service, ada, create, { input: { projectId: project, kind: "MODEL_PROFILE", name: profileName, content: "maxTokens:512\nenvironment:DEVELOPMENT", dependencies: ["model:local-safe-chat@v2"] } })).createReusableResource;
  profileId = profile.resource.id;
  const validProfile = (await gql(service, ada, validate, { input: { projectId: project, resourceId: profileId, expectedRevision: 1 } })).validateReusableResource;
  assert.equal(validProfile.resource.draft.validationStatus, "VALID");

  const archived = (await gql(service, ada, create, { input: { projectId: project, kind: "PROMPT", name: archivedName, content: "Archived {{operator}}", dependencies: [] } })).createReusableResource;
  archivedId = archived.resource.id;
  await client.query("UPDATE reusable_resources SET lifecycle_status = 'ARCHIVED' WHERE id = $1", [archivedId]);
  const archivedValidation = (await gql(service, ada, validate, { input: { projectId: project, resourceId: archivedId, expectedRevision: 1 } })).validateReusableResource;
  assert.equal(archivedValidation.problems[0].code, "PROTECTED_LIFECYCLE", "archived resources cannot validate");
  const archivedPublish = (await gql(service, ada, publish, { input: { projectId: project, resourceId: archivedId, expectedRevision: 1 } })).publishReusableResource;
  assert.equal(archivedPublish.problems[0].code, "PROTECTED_LIFECYCLE", "archived resources cannot publish");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM reusable_resource_versions WHERE resource_id = $1", [archivedId])).rows[0].count, 0, "blocked lifecycle operations leave version state unchanged");

  const mcpInput = { projectId: project, serverId: "browser-metadata-" + fixture, name: "Browser Metadata " + fixture,
    definition: "tool:http-metadata@v1", environment: "DEVELOPMENT", enabled: true,
    transportType: "STDIO", command: "local-mcp", arguments: ["--read-only"], remoteUrl: null,
    redactedBindings: ["redacted://local/browser-token"], tools: ["read_metadata"], resources: ["browser://page"], prompts: ["summarize_page"] };
  const deniedMcp = (await gql(service, bea, createMcp, { input: { ...mcpInput, serverId: "denied-" + fixture, name: "Denied MCP " + fixture } })).createProjectMcpServer;
  assert.equal(deniedMcp.problems[0].code, "FORBIDDEN");
  const beforeMcp = await client.query("SELECT count(*)::int AS count FROM project_tool_connections WHERE project_id = $1", [project]);
  const secretLiteral = (await gql(service, ada, createMcp, { input: { ...mcpInput, serverId: "unsafe-" + fixture,
    name: "Unsafe MCP " + fixture, arguments: ["token=plain-text-value"] } })).createProjectMcpServer;
  assert.equal(secretLiteral.problems[0].code, "INVALID_INPUT", "MCP arguments cannot carry a secret-looking literal");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM project_tool_connections WHERE project_id = $1", [project])).rows[0].count,
    beforeMcp.rows[0].count, "a rejected MCP descriptor changes no connection metadata");
  const splitSecret = (await gql(service, ada, createMcp, { input: { ...mcpInput, serverId: "split-unsafe-" + fixture,
    name: "Split Unsafe MCP " + fixture, arguments: ["--token", "plain-text-value"] } })).createProjectMcpServer;
  assert.equal(splitSecret.problems[0].code, "INVALID_INPUT", "M12UXC-MCP-SECRET-PERSIST: split secret flags are refused");
  assert.equal((await client.query("SELECT count(*)::int AS count FROM project_tool_connections WHERE project_id = $1", [project])).rows[0].count,
    beforeMcp.rows[0].count, "a rejected split secret changes no connection metadata");
  for (const scheme of ["bearer", "basic"]) {
    const splitCredential = (await gql(service, ada, createMcp, { input: { ...mcpInput,
      serverId: scheme + "-unsafe-" + fixture, name: scheme + " Unsafe MCP " + fixture,
      arguments: ["--" + scheme, "plain-text-value"] } })).createProjectMcpServer;
    assert.equal(splitCredential.problems[0].code, "INVALID_INPUT",
      `M12UXC-MCP-SECRET-PERSIST: split --${scheme} credentials are refused`);
    assert.equal((await client.query("SELECT count(*)::int AS count FROM project_tool_connections WHERE project_id = $1", [project])).rows[0].count,
      beforeMcp.rows[0].count, `a rejected split --${scheme} credential changes no connection metadata`);
  }
  for (const [suffix, remoteUrl] of [["userinfo", "https://user:plain-text-value@mcp.invalid/"],
    ["query", "https://mcp.invalid/metadata?api_key=plain-text-value"]]) {
    const unsafeRemote = (await gql(service, ada, createMcp, { input: { ...mcpInput,
      serverId: suffix + "-unsafe-" + fixture, name: suffix + " Unsafe MCP " + fixture,
      transportType: "REMOTE", command: null, arguments: [], remoteUrl } })).createProjectMcpServer;
    assert.equal(unsafeRemote.problems[0].code, "INVALID_INPUT", `M12UXC-MCP-SECRET-PERSIST: ${suffix} credentials are refused`);
    assert.equal((await client.query("SELECT count(*)::int AS count FROM project_tool_connections WHERE project_id = $1", [project])).rows[0].count,
      beforeMcp.rows[0].count, `a rejected ${suffix} URL changes no connection metadata`);
  }
  const createdMcp = (await gql(service, ada, createMcp, { input: mcpInput })).createProjectMcpServer;
  assert.deepEqual(createdMcp.problems, []); mcpId = createdMcp.mcpServer.id;
  assert.equal(createdMcp.mcpServer.status, "NOT_CHECKED");
  assert.equal(createdMcp.mcpServer.remoteUrl, null);
  assert.deepEqual(createdMcp.mcpServer.arguments, ["--read-only"]);
  assert.deepEqual(createdMcp.mcpServer.declaredTools, ["read_metadata"]);
  assert.deepEqual(createdMcp.mcpServer.redactedBindings, ["redacted://local/browser-token"]);
  const descriptor = await gql(service, ada, "{ __type(name: \"ProjectToolConnections\") { fields { name } } filter: __type(name: \"ProjectToolConnectionsFilterInput\") { inputFields { name } } }");
  assert.equal(descriptor.__type.fields.some((field) => field.name === "stdioArguments"), false, "stored stdio arguments are readable only through the computed arguments field");
  assert.equal(descriptor.filter.inputFields.some((field) => /stdioArguments|remoteUrl/.test(field.name)), false, "stored arguments and URLs cannot be probed through a filter");
  assert.equal(Object.keys(createdMcp.mcpServer).some((key) => /credential|password|secretValue|headerValue|environmentValue/i.test(key)), false,
    "the MCP descriptor exposes no secret-bearing field");
  await assert.rejects(
    client.query(`UPDATE project_tool_connections SET stdio_arguments = '["--token", "plain-text-value"]'::jsonb WHERE id = $1`, [mcpId]),
    (error) => error.code === "23514",
    "M12UXC-MCP-SECRET-PERSIST: storage rejects split secret arguments independently of request validation"
  );
  assert.deepEqual((await client.query("SELECT stdio_arguments FROM project_tool_connections WHERE id = $1", [mcpId])).rows[0].stdio_arguments,
    ["--read-only"], "the storage refusal preserves the descriptor");
  for (const scheme of ["bearer", "basic"]) {
    await assert.rejects(
      client.query("UPDATE project_tool_connections SET stdio_arguments = $2::jsonb WHERE id = $1", [mcpId, JSON.stringify(["--" + scheme, "plain-text-value"])]),
      (error) => error.code === "23514",
      `M12UXC-MCP-SECRET-PERSIST: storage rejects split --${scheme} credentials`
    );
    assert.deepEqual((await client.query("SELECT stdio_arguments FROM project_tool_connections WHERE id = $1", [mcpId])).rows[0].stdio_arguments,
      ["--read-only"], `the split --${scheme} storage refusal preserves the descriptor`);
  }
  await assert.rejects(
    client.query("UPDATE project_tool_connections SET transport_type = 'REMOTE', stdio_command = NULL, stdio_arguments = '{}', remote_url = 'https://user:plain-text-value@mcp.invalid/' WHERE id = $1", [mcpId]),
    (error) => error.code === "23514",
    "M12UXC-MCP-SECRET-PERSIST: storage rejects credential-bearing remote URLs"
  );
  assert.equal((await client.query("SELECT transport_type FROM project_tool_connections WHERE id = $1", [mcpId])).rows[0].transport_type,
    "STDIO", "the remote URL storage refusal preserves the descriptor");
  // Restores the migration's own definition afterwards, so this check never carries a stale copy of it.
  const secretArgumentsCheck = (await client.query("SELECT pg_get_constraintdef(oid) AS definition FROM pg_constraint WHERE conname = 'project_tool_connections_no_secret_arguments_check'")).rows[0].definition;
  await client.query("ALTER TABLE project_tool_connections DROP CONSTRAINT project_tool_connections_no_secret_arguments_check");
  try {
    await client.query(`UPDATE project_tool_connections SET stdio_arguments = '["--bearer", "legacy-plain-text-value"]'::jsonb WHERE id = $1`, [mcpId]);
    const legacySafe = (await readMcpServers(ada, project)).find((entry) => entry.id === mcpId);
    assert.deepEqual(legacySafe.arguments, [], "legacy split bearer material is removed from descriptor reads");
    assert.equal(legacySafe.status, "INCOMPLETE", "a redacted legacy descriptor reports incomplete metadata");
  } finally {
    await client.query(`UPDATE project_tool_connections SET stdio_arguments = '["--read-only"]'::jsonb WHERE id = $1`, [mcpId]);
    await client.query("ALTER TABLE project_tool_connections ADD CONSTRAINT project_tool_connections_no_secret_arguments_check " + secretArgumentsCheck);
  }
  const changedMcp = (await gql(service, ada, updateMcp, { input: { projectId: project, id: mcpId,
    expectedRevision: 1, name: mcpInput.name, definition: mcpInput.definition, environment: "DEVELOPMENT",
    enabled: false, transportType: "REMOTE", command: null, arguments: [], remoteUrl: "https://mcp.invalid/metadata",
    redactedBindings: mcpInput.redactedBindings, tools: mcpInput.tools, resources: mcpInput.resources,
    prompts: mcpInput.prompts, lifecycleStatus: "ACTIVE" } })).updateProjectMcpServer;
  assert.deepEqual(changedMcp.problems, []);
  assert.equal(changedMcp.mcpServer.status, "DISABLED");
  assert.equal(changedMcp.mcpServer.stdioCommand, null);
  assert.equal(changedMcp.mcpServer.remoteUrl, "https://mcp.invalid/metadata");
  assert.equal(changedMcp.mcpServer.revision, 2);
  const staleMcp = (await gql(service, ada, updateMcp, { input: { projectId: project, id: mcpId,
    expectedRevision: 1, name: mcpInput.name, definition: mcpInput.definition, environment: "DEVELOPMENT",
    enabled: true, transportType: "STDIO", command: "local-mcp", arguments: [], remoteUrl: null,
    redactedBindings: [], tools: [], resources: [], prompts: [], lifecycleStatus: "ACTIVE" } })).updateProjectMcpServer;
  assert.equal(staleMcp.problems[0].code, "REVISION_CONFLICT", "MCP updates require the expected revision");
  assert.equal((await readMcpServers(ada, project)).find((entry) => entry.id === mcpId).status, "DISABLED");

  const saved = (await gql(service, ada, saveTool, { input: { projectId: project, expectedRevision: 0, name: "M11 Fixture Tool", definition: "tool:http-metadata@v1", environment: "DEVELOPMENT", redactedSecretReference: "redacted://local/m11-fixture", lifecycleStatus: "ACTIVE", rotationSummary: "Recorded rotation metadata only" } })).saveProjectToolConnectionMetadata;
  assert.deepEqual(saved.problems, []); toolId = saved.tool.id;
  assert.equal(saved.tool.redactedSecretReference, "redacted://local/m11-fixture");
  assert.equal(Object.keys(saved.tool).some((key) => /credentialValue|token|password|secretValue/i.test(key)), false);
  const listed = (await gql(service, ada, tools, { projectId: project })).projects.nodes[0].projectToolConnections.nodes.find((entry) => entry.id === toolId);
  assert.equal(listed.redactedSecretReference, "redacted://local/m11-fixture");
  const lifecycleUpdated = (await gql(service, ada, saveTool, { input: { projectId: project, toolId, expectedRevision: 1, name: "M11 Fixture Tool", definition: "tool:http-metadata@v1", environment: "DEVELOPMENT", redactedSecretReference: "redacted://local/m11-fixture", lifecycleStatus: "DISABLED", rotationSummary: "Disabled during local lifecycle review" } })).saveProjectToolConnectionMetadata;
  assert.equal(lifecycleUpdated.tool.lifecycleStatus, "DISABLED");
  assert.equal(lifecycleUpdated.tool.revision, 2);
  const audit = await client.query("SELECT action FROM configuration_audit_events WHERE subject_id IN ($1, $2, $3, $4)", [resourceId, profileId, policyId, toolId]);
  assert(audit.rows.some((entry) => entry.action === "REUSABLE_RESOURCE_PUBLISHED"));
  const mcpAudit = await client.query("SELECT action FROM configuration_audit_events WHERE subject_id = $1 ORDER BY occurred_at", [mcpId]);
  assert.deepEqual(mcpAudit.rows.map((entry) => entry.action), ["MCP_SERVER_CREATED", "MCP_SERVER_UPDATED"]);
  // No append-only DELETE check here: configuration_audit_events_no_delete no longer exists under
  // Aurora DSQL compatibility (V011 stopped creating it), and no Java code path ever DELETEs from this
  // table in the first place -- there is no application-level operation left to guard.
} finally {
  // Published versions and their audit facts are intentionally retained as immutable local evidence.
  if (toolId) await client.query("DELETE FROM project_tool_connections WHERE id = $1", [toolId]);
  if (mcpId) await client.query("DELETE FROM project_tool_connections WHERE id = $1", [mcpId]);
  await client.query("DELETE FROM console_role_assignments WHERE id = $1", [beaPrivateAssignment]);
  await client.end();
  await service.stop();
  await database.drop();
}
