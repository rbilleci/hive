import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { createIsolatedDatabase, postgresClient, startIsolatedLocalService } from "./local-service.mjs";

const principal = "00000000-0000-0000-0000-000000000001";
const outsider = "d1300000-0000-0000-0000-000000000005";
const viewer = "00000000-0000-0000-0000-000000000002";
const organization = "10000000-0000-0000-0000-000000000001";
const project = "50000000-0000-0000-0000-000000000001";
let endpoint;
async function graphql(service, identity, operationName, query, variables) {
  const response = await fetch(endpoint, { method: "POST", headers: { "Content-Type": "application/json", "User-Agent": "M17 local audit fixture", Cookie: `sf_session=${service.signFixtureSession(identity)}` }, body: JSON.stringify({ operationName, query, variables }) });
  assert.equal(response.status, 200); return { body: await response.json(), requestId: response.headers.get("x-request-id") };
}
const filters = { projectId: { eq: project }, occurredAt: { gte: "2020-01-01T00:00:00Z" } };
const eventFields = "projectionId action outcome organizationId projectId requestId correlationId graphqlOperation sourceIp userAgent sensitiveFieldsRedacted occurredAt resourceType resourceId resourceReferences safeChangedFields";
// The generated read, as the console sends it: newest first, the key as the tie-break.
const eventsQuery = (name, fields = eventFields) => `query ${name}($filters: AuditEventProjectionFilterInput, $limit: Int!, $page: Int!) { auditEventProjection(filters: $filters, orderBy: { occurredAt: DESC, projectionId: DESC }, pagination: { page: { limit: $limit, page: $page } }) { nodes { ${fields} } paginationInfo { pages current total } } }`;
const database = await createIsolatedDatabase("m17_audit_access");
let service; let client;
try {
  service = await startIsolatedLocalService(database.name); endpoint = `http://127.0.0.1:${service.port}/graphql`; client = await postgresClient(database.name);
  await client.query("INSERT INTO platform_role_assignments (principal_id, role_code) VALUES ($1, 'PLATFORM_ADMIN')", [principal]);
  const created = await graphql(service, principal, "CreateAuditAgent", "mutation CreateAuditAgent($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId } problems { code } } }", { input: { projectId: project, displayName: "M17 audit fixture", slug: `m17-${randomUUID().slice(0, 8)}` } });
  assert.deepEqual(created.body.data.createAgentDraft.problems, []); assert.match(created.requestId ?? "", /^[0-9a-f-]{36}$/);
  const second = await graphql(service, principal, "CreateSecondAuditAgent", "mutation CreateSecondAuditAgent($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId } problems { code } } }", { input: { projectId: project, displayName: "M17 audit fixture two", slug: `m17-${randomUUID().slice(0, 8)}` } });
  assert.deepEqual(second.body.data.createAgentDraft.problems, []);
  const listed = await graphql(service, principal, "AuditEvents", eventsQuery("AuditEvents"), { filters: { ...filters, correlationId: { eq: created.requestId } }, limit: 10, page: 0 });
  assert.equal(listed.body.errors, undefined, JSON.stringify(listed.body.errors)); const event = listed.body.data.auditEventProjection.nodes.find((item) => item.action === "AGENT_CREATED");
  assert(event); assert.equal(event.projectId, project); assert.equal(event.organizationId, organization); assert.equal(event.graphqlOperation, "CreateAuditAgent"); assert.equal(event.sourceIp, "127.0.0.1"); assert.equal(event.userAgent, "M17 local audit fixture");
  assert.equal(event.sensitiveFieldsRedacted, false, "nothing was withheld from a principal that holds AUDIT_SENSITIVE.VIEW");
  assert.equal(event.correlationId, created.requestId); assert.equal(event.requestId, created.requestId); assert.equal(event.resourceType, "AGENT"); assert.ok(event.resourceReferences.some((reference) => reference.type === "AGENT"));
  assert.ok(listed.body.data.auditEventProjection.nodes.every((item) => item.correlationId === created.requestId), "correlation filtering must return only the recorded request's events");
  // Paging reaches every event exactly once, newest first, and the total does not move with the page.
  const firstPage = await graphql(service, principal, "AuditEventsPaged", eventsQuery("AuditEventsPaged", "projectionId occurredAt"), { filters, limit: 1, page: 0 });
  const { pages, total } = firstPage.body.data.auditEventProjection.paginationInfo; assert.ok(total >= 2, "the fixture writes at least two events"); assert.equal(pages, total);
  const seen = [];
  for (let page = 0; page < pages; page += 1) {
    const result = await graphql(service, principal, "AuditEventsPaged", eventsQuery("AuditEventsPaged", "projectionId occurredAt"), { filters, limit: 1, page });
    assert.equal(result.body.data.auditEventProjection.paginationInfo.total, total, "the total must retain the identical scope and filter when the page changes");
    seen.push(...result.body.data.auditEventProjection.nodes);
  }
  assert.equal(seen.length, total); assert.equal(new Set(seen.map((item) => item.projectionId)).size, total, "paging must reach every event exactly once");
  for (let index = 1; index < seen.length; index += 1) assert.ok(Date.parse(seen[index - 1].occurredAt) >= Date.parse(seen[index].occurredAt), "events are listed newest first");
  const beyond = await graphql(service, principal, "AuditEventsPaged", eventsQuery("AuditEventsPaged", "projectionId occurredAt"), { filters, limit: 1, page: pages });
  assert.deepEqual(beyond.body.data.auditEventProjection.nodes, []);
  const succeeded = await graphql(service, principal, "SucceededAudit", eventsQuery("SucceededAudit", "outcome"), { filters: { ...filters, outcome: { eq: "SUCCEEDED" } }, limit: 10, page: 0 });
  assert.ok(succeeded.body.data.auditEventProjection.nodes.length > 0); assert.ok(succeeded.body.data.auditEventProjection.nodes.every((item) => item.outcome === "SUCCEEDED"));
  const failed = await graphql(service, principal, "FailedAudit", eventsQuery("FailedAudit", "outcome"), { filters: { ...filters, outcome: { eq: "FAILED" } }, limit: 10, page: 0 });
  assert.ok(failed.body.data.auditEventProjection.nodes.every((item) => item.outcome === "FAILED"));
  const [sourceKind, sourceEventId] = [event.projectionId.split(":")[0].toUpperCase(), event.projectionId.split(":")[1]];
  const detail = await graphql(service, principal, "AuditEvent", eventsQuery("AuditEvent"), { filters: { projectId: { eq: project }, projectionId: { eq: event.projectionId }, sourceKind: { eq: sourceKind }, sourceEventId: { eq: sourceEventId } }, limit: 1, page: 0 });
  assert.deepEqual(detail.body.data.auditEventProjection.nodes, [event]);
  const viewerMembership = randomUUID();
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP, 1)", [randomUUID(), organization, viewer]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP, 1)", [viewerMembership, project, viewer]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AUDITOR')", [viewerMembership]);
  const redacted = await graphql(service, viewer, "RedactedAudit", eventsQuery("RedactedAudit"), { filters: { ...filters, projectionId: { eq: event.projectionId } }, limit: 1, page: 0 });
  const redactedEvent = redacted.body.data.auditEventProjection.nodes[0];
  assert.equal(redactedEvent.projectionId, event.projectionId); assert.equal(redactedEvent.sourceIp, null); assert.equal(redactedEvent.userAgent, null); assert.equal(redactedEvent.sensitiveFieldsRedacted, true);
  // The stored columns are not part of the generated API: they cannot be selected, filtered or ordered on.
  for (const [name, query] of [
    ["select", "query Raw { auditEventProjection { nodes { source_ip } } }"],
    ["select capability", "query Raw { auditEventProjection { nodes { requiredCapability } } }"],
    ["filter", "query Raw { auditEventProjection(filters: { sourceIp: { eq: \"127.0.0.1\" } }) { nodes { projectionId } } }"],
    ["filter user agent", "query Raw { auditEventProjection(filters: { userAgent: { eq: \"M17 local audit fixture\" } }) { nodes { projectionId } } }"],
    ["order", "query Raw { auditEventProjection(orderBy: { sourceIp: ASC }) { nodes { projectionId } } }"]
  ]) {
    const raw = await graphql(service, viewer, "Raw", query);
    assert.ok(raw.body.errors?.length, `${name}: the raw sensitive column must not be reachable`); assert.equal(raw.body.data ?? null, null);
  }
  // A project auditor with no scope filter at all still reads only that project's events, and none of the organization's own.
  await client.query("INSERT INTO administration_audit_events (id, actor_principal_id, scope_type, scope_id, action, facts) VALUES ($1, $2, 'ORGANIZATION', $3, 'ORGANIZATION_MEMBERSHIP_ADDED', '{}'::jsonb)", [randomUUID(), principal, organization]);
  const unscoped = await graphql(service, viewer, "UnscopedAudit", eventsQuery("UnscopedAudit", "projectId organizationId"), { filters: {}, limit: 50, page: 0 });
  assert.equal(unscoped.body.errors, undefined, JSON.stringify(unscoped.body.errors)); assert.ok(unscoped.body.data.auditEventProjection.nodes.length > 0); assert.ok(unscoped.body.data.auditEventProjection.nodes.every((item) => item.projectId === project), "a project grant must not reach another scope");
  const organizationEvents = await graphql(service, principal, "OrganizationAudit", eventsQuery("OrganizationAudit", "projectId"), { filters: { organizationId: { eq: organization } }, limit: 50, page: 0 });
  assert.ok(organizationEvents.body.data.auditEventProjection.nodes.some((item) => item.projectId === null), "the administrator reads the organization's own events");
  const denied = await graphql(service, outsider, "OutsiderAudit", eventsQuery("OutsiderAudit", "projectionId"), { filters, limit: 50, page: 0 });
  assert.equal(denied.body.errors, undefined, JSON.stringify(denied.body.errors)); assert.deepEqual(denied.body.data.auditEventProjection.nodes, []); assert.equal(denied.body.data.auditEventProjection.paginationInfo.total, 0);
  const deniedUnscoped = await graphql(service, outsider, "OutsiderAudit", eventsQuery("OutsiderAudit", "projectionId"), { filters: {}, limit: 50, page: 0 });
  assert.equal(deniedUnscoped.body.data.auditEventProjection.paginationInfo.total, 0, "a principal with no audit grant reads no event");
  const sourceCount = await client.query("SELECT count(*)::integer AS count FROM agent_authoring_audit_events WHERE project_id = $1", [project]);
  const refused = await graphql(service, principal, "RefusedAuditMutation", "mutation RefusedAuditMutation($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { agentId } problems { code } } }", { input: { projectId: project, agentId: created.body.data.createAgentDraft.agentDraft.agentId, expectedRevision: 999999, document: {} } });
  assert.equal(refused.body.errors, undefined, JSON.stringify(refused.body.errors)); assert.ok(refused.body.data.updateAgentDraft.problems.length > 0, "a refused mutation must report a domain problem");
  const sourceCountAfterRefusal = await client.query("SELECT count(*)::integer AS count FROM agent_authoring_audit_events WHERE project_id = $1", [project]);
  assert.equal(sourceCountAfterRefusal.rows[0].count, sourceCount.rows[0].count, "a refused mutation must not append an audit fact");
  const fallbackSlug = `m17-fallback-${randomUUID().slice(0, 8)}`;
  const namedResponse = await fetch(endpoint, { method: "POST", headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` }, body: JSON.stringify({ query: "mutation M17OperationNameFallback($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId } problems { code } } }", variables: { input: { projectId: project, displayName: "M17 operation fallback", slug: fallbackSlug } } }) });
  assert.equal(namedResponse.status, 200); const namedBody = await namedResponse.json(); assert.equal(namedBody.errors, undefined, JSON.stringify(namedBody.errors)); assert.deepEqual(namedBody.data.createAgentDraft.problems, []);
  const namedOperation = await client.query("SELECT graphql_operation FROM agent_authoring_audit_events WHERE agent_id = $1 ORDER BY occurred_at DESC LIMIT 1", [namedBody.data.createAgentDraft.agentDraft.agentId]);
  assert.equal(namedOperation.rows[0].graphql_operation, "M17OperationNameFallback", "the server must derive a supplied named operation when operationName is absent");
  const forgedOperation = await fetch(endpoint, { method: "POST", headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` }, body: JSON.stringify({ operationName: "ForgedAuditOperation", query: "mutation ActualAuditOperation($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId } problems { code } } }", variables: { input: { projectId: project, displayName: "M17 forged operation", slug: `m17-forged-${randomUUID().slice(0, 8)}` } } }) });
  assert.equal(forgedOperation.status, 400); const forgedBody = await forgedOperation.json();
  assert.equal(forgedBody.errors?.[0]?.message, "The GraphQL operationName does not match the document.");
  // No immutability-trigger check here: Aurora DSQL supports no triggers, so the migrations define no
  // agent_authoring_audit_events_immutable guard, and nothing UPDATEs or DELETEs an audit-event row --
  // there is no application-level operation for such a guard to protect.
  await client.query("DROP VIEW audit_event_projection");
  const unavailable = await fetch(endpoint, { method: "POST", headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` }, body: JSON.stringify({ operationName: "UnavailableAudit", query: eventsQuery("UnavailableAudit", "projectionId"), variables: { filters, limit: 1, page: 0 } }) });
  assert.equal(unavailable.status, 503); const unavailableBody = await unavailable.json(); assert.equal(unavailableBody.errors?.[0]?.message, "The service is temporarily unavailable.");
  assert.doesNotMatch(JSON.stringify(unavailableBody), /audit_event_projection|relation/, "the database's own message must not reach the client");
} finally { if (client) await client.end(); if (service) await service.stop(); await database.drop(); }
console.log("M17 audit tenant, paging, correlation, outcome, atomicity, redaction, operation fallback, and unavailability checks passed.");
