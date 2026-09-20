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
const filter = { projectId: project, occurredAfter: "2020-01-01T00:00:00Z" };
const eventFields = "id action outcome projectId requestId correlationId graphqlOperation sourceIp userAgent sensitiveFieldsRedacted occurredAt resource { type id } references { type id }";
const database = await createIsolatedDatabase("m17_audit_access");
let service; let client;
try {
  service = await startIsolatedLocalService(database.name); endpoint = `http://127.0.0.1:${service.port}/graphql`; client = await postgresClient(database.name);
  await client.query("INSERT INTO platform_role_assignments (principal_id, role_code) VALUES ($1, 'PLATFORM_ADMIN')", [principal]);
  const created = await graphql(service, principal, "CreateAuditAgent", "mutation CreateAuditAgent($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId } problems { code } } }", { input: { projectId: project, displayName: "M17 audit fixture", slug: `m17-${randomUUID().slice(0, 8)}` } });
  assert.deepEqual(created.body.data.createAgentDraft.problems, []); assert.match(created.requestId ?? "", /^[0-9a-f-]{36}$/);
  const listed = await graphql(service, principal, "AuditEvents", `query AuditEvents($filter: AuditEventFilter!) { auditEvents(filter: $filter, first: 1, includeTotalCount: true) { edges { cursor node { ${eventFields} } } pageInfo { hasNextPage endCursor } totalCount } }`, { filter });
  assert.equal(listed.body.errors, undefined, JSON.stringify(listed.body.errors)); const edge = listed.body.data.auditEvents.edges.find((item) => item.node.action === "AGENT_CREATED") ?? listed.body.data.auditEvents.edges[0];
  assert(edge); assert.equal(edge.node.projectId, project); assert.equal(edge.node.graphqlOperation, "CreateAuditAgent"); assert.equal(edge.node.sourceIp, "127.0.0.1"); assert.equal(edge.node.userAgent, "M17 local audit fixture");
  assert.match(edge.node.correlationId, /^[0-9a-f-]{36}$/); assert.ok(edge.node.references.some((reference) => reference.type === "AGENT"));
  const paged = await graphql(service, principal, "AuditEventsAfter", `query AuditEventsAfter($filter: AuditEventFilter!, $after: String!) { auditEvents(filter: $filter, first: 1, after: $after, includeTotalCount: true) { edges { node { id } } totalCount } }`, { filter, after: edge.cursor });
  assert.equal(paged.body.data.auditEvents.totalCount, listed.body.data.auditEvents.totalCount, "totalCount must retain the identical scope and filter when pagination changes");
  const changedOutcome = await fetch(endpoint, { method: "POST", headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` }, body: JSON.stringify({ operationName: "ChangedOutcomeCursor", query: "query ChangedOutcomeCursor($filter: AuditEventFilter!, $after: String!) { auditEvents(filter: $filter, first: 1, after: $after) { edges { node { id } } } }", variables: { filter: { ...filter, outcome: "FAILED" }, after: edge.cursor } }) });
  assert.equal(changedOutcome.status, 200); const changedOutcomeBody = await changedOutcome.json();
  assert.ok(changedOutcomeBody.errors?.length, "changing an outcome filter must reject an existing cursor");
  const correlated = await graphql(service, principal, "CorrelatedAudit", `query CorrelatedAudit($filter: AuditEventFilter!) { auditEvents(filter: $filter, first: 10) { edges { node { id correlationId } } } }`, { filter: { ...filter, correlationId: created.requestId } });
  assert.ok(correlated.body.data.auditEvents.edges.some((item) => item.node.id === edge.node.id), "correlation filtering must return the recorded event");
  const succeeded = await graphql(service, principal, "SucceededAudit", `query SucceededAudit($filter: AuditEventFilter!) { auditEvents(filter: $filter, first: 10) { edges { node { outcome } } } }`, { filter: { ...filter, outcome: "SUCCEEDED" } });
  assert.ok(succeeded.body.data.auditEvents.edges.length > 0); assert.ok(succeeded.body.data.auditEvents.edges.every((item) => item.node.outcome === "SUCCEEDED"));
  const detail = await graphql(service, principal, "AuditEvent", `query AuditEvent($filter: AuditEventFilter!, $eventId: ID!) { auditEvent(filter: $filter, eventId: $eventId) { ${eventFields} } }`, { filter, eventId: edge.node.id });
  assert.equal(detail.body.data.auditEvent.id, edge.node.id);
  const viewerMembership = randomUUID();
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP, 1)", [randomUUID(), organization, viewer]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP, 1)", [viewerMembership, project, viewer]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AUDITOR')", [viewerMembership]);
  const redacted = await graphql(service, viewer, "RedactedAudit", `query RedactedAudit($filter: AuditEventFilter!) { auditEvents(filter: $filter, first: 1) { edges { node { ${eventFields} } } } }`, { filter });
  const redactedEvent = redacted.body.data.auditEvents.edges[0].node;
  assert.equal(redactedEvent.sourceIp, null); assert.equal(redactedEvent.userAgent, null); assert.equal(redactedEvent.sensitiveFieldsRedacted, true);
  const malformed = await graphql(service, principal, "BadCursor", "query BadCursor($filter: AuditEventFilter!) { auditEvents(filter: $filter, after: \"bad\") { edges { cursor } } }", { filter });
  assert.ok(malformed.body.errors?.length, "malformed audit cursors must fail before broad SQL");
  const denied = await graphql(service, outsider, "OutsiderAudit", "query OutsiderAudit($filter: AuditEventFilter!) { auditEvents(filter: $filter) { edges { node { id } } } }", { filter });
  assert.equal(denied.body.data, null); assert.equal(denied.body.errors?.[0]?.message, "Audit history is unavailable.");
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
  // No immutability-trigger check here: agent_authoring_audit_events_immutable no longer exists under
  // Aurora DSQL compatibility (V040 stopped creating it, and no Java code path ever UPDATEs or DELETEs
  // an audit-event row in the first place -- there is no application-level operation left to guard).
  await client.query("DROP VIEW audit_event_projection");
  const unavailable = await fetch(endpoint, { method: "POST", headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` }, body: JSON.stringify({ operationName: "UnavailableAudit", query: "query UnavailableAudit($filter: AuditEventFilter!) { auditEvents(filter: $filter) { edges { node { id } } } }", variables: { filter } }) });
  assert.equal(unavailable.status, 503); const unavailableBody = await unavailable.json(); assert.equal(unavailableBody.errors?.[0]?.message, "Audit history is temporarily unavailable.");
} finally { if (client) await client.end(); if (service) await service.stop(); await database.drop(); }
console.log("M17 audit tenant, cursor, correlation, outcome, atomicity, redaction, operation fallback, and unavailability checks passed.");
