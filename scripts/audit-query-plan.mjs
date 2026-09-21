import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createIsolatedDatabase, postgresClient, startIsolatedLocalService } from "./local-service.mjs";

const organization = "10000000-0000-0000-0000-000000000001";
const project = "50000000-0000-0000-0000-000000000001";
const actor = "00000000-0000-0000-0000-000000000001";
const agent = "f0410000-0000-0000-0000-000000000001";
const event = "f0410000-0000-0000-0000-000000000002";
const correlation = "f0410000-0000-0000-0000-000000000003";
const noiseOrganization = "f0410000-0000-0000-0000-000000000004";
const noiseAgent = "f0410000-0000-0000-0000-000000000005";
const noiseProject = "f0410000-0000-0000-0000-000000000006";
const digest = "a".repeat(64);
const database = await createIsolatedDatabase("m17_audit_plan");
let service;
let client;
try {
  // `sea_orm=debug` makes the service log every statement with its values (SeaORM's `debug-print`), so the plans
  // below are those of the SQL Seaography really generates for the console's requests, tenant rule included.
  service = await startIsolatedLocalService(database.name, { RUST_LOG: "info,sea_orm=debug" });
  client = await postgresClient(database.name);
  await client.query("INSERT INTO organizations (id, slug, display_name, lifecycle_status) VALUES ($1, 'm17-audit-plan-noise', 'M17 audit planner noise', 'ACTIVE')", [noiseOrganization]);
  await client.query("INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'm17-audit-plan-noise', 'M17 audit planner noise', 'ACTIVE')", [noiseProject, noiseOrganization]);
  await client.query("INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'm17-audit-plan', 'M17 audit planner', 'ACTIVE')", [agent, project]);
  await client.query("INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'm17-audit-plan-noise', 'M17 audit planner noise', 'ACTIVE')", [noiseAgent, noiseProject]);
  // organization_id is explicit on every fixture row below: Aurora DSQL supports no triggers, so
  // nothing backfills the column from project_id, and it is NOT NULL -- a raw fixture insert must
  // supply it directly.
  await client.query("INSERT INTO agent_authoring_audit_events (id, agent_id, project_id, organization_id, actor_principal_id, action, revision, content_digest, correlation_id) VALUES ($1, $2, $3, $4, $5, 'SAVED', 1, $6, $7)", [event, agent, project, organization, actor, digest, correlation]);
  await client.query("INSERT INTO agent_authoring_audit_events (id, agent_id, project_id, organization_id, actor_principal_id, action, revision, content_digest, correlation_id, occurred_at) SELECT gen_random_uuid(), $1, $2, $3, $4, 'SAVED', series, $5, $6, CURRENT_TIMESTAMP - make_interval(secs => series) FROM generate_series(1, 512) AS series", [agent, project, organization, actor, digest, correlation]);
  await client.query("INSERT INTO agent_authoring_audit_events (id, agent_id, project_id, organization_id, actor_principal_id, action, revision, content_digest, occurred_at) SELECT gen_random_uuid(), $1, $2, $3, $4, 'SAVED', series, $5, CURRENT_TIMESTAMP - make_interval(secs => series) FROM generate_series(1, 8192) AS series", [noiseAgent, noiseProject, noiseOrganization, actor, digest]);
  await client.query("INSERT INTO administration_audit_events (id, actor_principal_id, scope_type, scope_id, action, facts, occurred_at) SELECT gen_random_uuid(), $1, 'ORGANIZATION', $2, 'ORGANIZATION_MEMBERSHIP_ADDED', '{}'::jsonb, CURRENT_TIMESTAMP - make_interval(secs => series) FROM generate_series(1, 512) AS series", [actor, organization]);
  await client.query("INSERT INTO administration_audit_events (id, actor_principal_id, scope_type, scope_id, action, facts, occurred_at) SELECT gen_random_uuid(), $1, 'ORGANIZATION', $2, 'ORGANIZATION_MEMBERSHIP_ADDED', '{}'::jsonb, CURRENT_TIMESTAMP - make_interval(secs => series) FROM generate_series(1, 8192) AS series", [actor, noiseOrganization]);
  await client.query("ANALYZE agent_authoring_audit_events");
  await client.query("ANALYZE administration_audit_events");
  // The filters the console sends (`crates/hive-console/src/api/audit.rs`): always the scope, newest first, 25 a page.
  // Ada is an organization administrator, not a platform administrator, so the tenant rule's subqueries are in the statement.
  const since = { occurredAt: { gte: "2020-01-01T00:00:00Z" } };
  const requests = [
    { name: "project", filters: { projectId: { eq: project }, ...since } },
    { name: "organization", filters: { organizationId: { eq: organization }, ...since } },
    { name: "resource", filters: { projectId: { eq: project }, resourceType: { eq: "AGENT" }, resourceId: { eq: agent }, ...since } },
    { name: "actor", filters: { projectId: { eq: project }, actorPrincipalId: { eq: actor }, ...since } },
    { name: "event-id", filters: { projectId: { eq: project }, sourceKind: { eq: "AGENT_AUTHORING" }, sourceEventId: { eq: event }, projectionId: { eq: `agent_authoring:${event}` } } },
    { name: "correlation", filters: { projectId: { eq: project }, correlationId: { eq: correlation }, ...since } }
  ];
  const document = "query AuditEvents($filters: AuditEventProjectionFilterInput) { auditEventProjection(filters: $filters, orderBy: { occurredAt: DESC, projectionId: DESC }, pagination: { page: { limit: 25, page: 0 } }) { nodes { projectionId } paginationInfo { pages current } } }";
  const logged = /DEBUG sea_orm::driver::sqlx_postgres: (SELECT [^\n]* FROM "audit_event_projection" [^\n]*)/g;
  for (const request of requests) {
    const before = service.output().length;
    const response = await fetch(`http://127.0.0.1:${service.port}/graphql`, { method: "POST", headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(actor)}` }, body: JSON.stringify({ operationName: "AuditEvents", query: document, variables: { filters: request.filters } }) });
    assert.equal(response.status, 200);
    const body = await response.json();
    assert.equal(body.errors, undefined, JSON.stringify(body.errors));
    assert.ok(body.data.auditEventProjection.nodes.length > 0, `${request.name} matched no fixture event`);
    let statements = [];
    for (let attempt = 0; attempt < 50 && statements.length < 2; attempt += 1) {
      statements = [...service.output().slice(before).matchAll(logged)].map((match) => match[1]);
      if (statements.length < 2) await new Promise((resolve) => setTimeout(resolve, 100));
    }
    const page = statements.find((statement) => statement.startsWith('SELECT "audit_event_projection"'));
    const count = statements.find((statement) => statement.startsWith("SELECT COUNT(*)"));
    assert.ok(page && count, `${request.name}: the service did not log the page and count statements:\n${statements.join("\n")}`);
    assert.match(page, /"required_capability" IN \(/, `${request.name}: the logged statement carries no tenant rule`);
    assert.match(page, /ORDER BY "audit_event_projection"."occurred_at" DESC, "audit_event_projection"."projection_id" DESC LIMIT 25/, `${request.name}: the logged statement is not the newest-first page`);
    for (const [kind, statement] of [["page", page], ["count", count]]) {
      const plan = await client.query(`EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) ${statement}`);
      const text = plan.rows.map((row) => row["QUERY PLAN"]).join("\n");
      if (process.env.HIVE_AUDIT_PLAN_PRINT) console.log(`--- ${request.name} ${kind}\n${text}`);
      assert.match(text, /Index Scan|Bitmap Index Scan|Index Only Scan/, `${request.name} ${kind} did not use a normal indexed plan:\n${text}`);
      assert.match(text, /Buffers:/, `${request.name} ${kind} omitted buffer evidence:\n${text}`);
      assert.match(text, /Execution Time:/, `${request.name} ${kind} omitted execution evidence:\n${text}`);
      assert.doesNotMatch(text, /Seq Scan[^\n]*\(actual [^\n]* rows=(?:[5-9][0-9]|[1-9][0-9]{2,})/, `${request.name} ${kind} scanned a populated unbounded source branch:\n${text}`);
    }
  }
} finally {
  if (client) await client.end();
  if (service) await service.stop();
  await database.drop();
}
const forcedPlannerSwitch = "enable" + "_seqscan";
assert.doesNotMatch(readFileSync(new URL(import.meta.url), "utf8"), new RegExp(forcedPlannerSwitch));
console.log("M17 populated normal-planner project, organization, resource, actor, event-ID, and correlation audit plans retain buffer evidence without broad source scans.");
