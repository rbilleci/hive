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
  service = await startIsolatedLocalService(database.name);
  client = await postgresClient(database.name);
  await client.query("INSERT INTO organizations (id, slug, display_name, lifecycle_status) VALUES ($1, 'm17-audit-plan-noise', 'M17 audit planner noise', 'ACTIVE')", [noiseOrganization]);
  await client.query("INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'm17-audit-plan-noise', 'M17 audit planner noise', 'ACTIVE')", [noiseProject, noiseOrganization]);
  await client.query("INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'm17-audit-plan', 'M17 audit planner', 'ACTIVE')", [agent, project]);
  await client.query("INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'm17-audit-plan-noise', 'M17 audit planner noise', 'ACTIVE')", [noiseAgent, noiseProject]);
  // organization_id is explicit on every fixture row below: Aurora DSQL compatibility removed
  // audit_copy_authoring_organization(), the trigger that used to backfill it from project_id, and the
  // column stays NOT NULL (see V040's comment), so raw fixture inserts must supply it directly now.
  await client.query("INSERT INTO agent_authoring_audit_events (id, agent_id, project_id, organization_id, actor_principal_id, action, revision, content_digest, correlation_id) VALUES ($1, $2, $3, $4, $5, 'SAVED', 1, $6, $7)", [event, agent, project, organization, actor, digest, correlation]);
  await client.query("INSERT INTO agent_authoring_audit_events (id, agent_id, project_id, organization_id, actor_principal_id, action, revision, content_digest, correlation_id, occurred_at) SELECT gen_random_uuid(), $1, $2, $3, $4, 'SAVED', series, $5, $6, CURRENT_TIMESTAMP - make_interval(secs => series) FROM generate_series(1, 512) AS series", [agent, project, organization, actor, digest, correlation]);
  await client.query("INSERT INTO agent_authoring_audit_events (id, agent_id, project_id, organization_id, actor_principal_id, action, revision, content_digest, occurred_at) SELECT gen_random_uuid(), $1, $2, $3, $4, 'SAVED', series, $5, CURRENT_TIMESTAMP - make_interval(secs => series) FROM generate_series(1, 8192) AS series", [noiseAgent, noiseProject, noiseOrganization, actor, digest]);
  await client.query("INSERT INTO administration_audit_events (id, actor_principal_id, scope_type, scope_id, action, facts, occurred_at) SELECT gen_random_uuid(), $1, 'ORGANIZATION', $2, 'ORGANIZATION_MEMBERSHIP_ADDED', '{}'::jsonb, CURRENT_TIMESTAMP - make_interval(secs => series) FROM generate_series(1, 512) AS series", [actor, organization]);
  await client.query("INSERT INTO administration_audit_events (id, actor_principal_id, scope_type, scope_id, action, facts, occurred_at) SELECT gen_random_uuid(), $1, 'ORGANIZATION', $2, 'ORGANIZATION_MEMBERSHIP_ADDED', '{}'::jsonb, CURRENT_TIMESTAMP - make_interval(secs => series) FROM generate_series(1, 8192) AS series", [actor, noiseOrganization]);
  await client.query("ANALYZE agent_authoring_audit_events");
  await client.query("ANALYZE administration_audit_events");
  const predicates = [
    { name: "project", sql: "project_id = $1", values: [project] },
    { name: "organization", sql: "organization_id = $1", values: [organization] },
    { name: "resource", sql: "project_id = $1 AND resource_type = 'AGENT' AND resource_id = $2", values: [project, agent] },
    { name: "actor", sql: "project_id = $1 AND actor_principal_id = $2", values: [project, actor] },
    { name: "event-id", sql: "source_kind = 'AGENT_AUTHORING' AND source_event_id = $1", values: [event] },
    { name: "correlation", sql: "project_id = $1 AND correlation_id = $2", values: [project, correlation] }
  ];
  for (const predicate of predicates) {
    const plan = await client.query(`EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) SELECT projection_id FROM audit_event_projection WHERE ${predicate.sql} ORDER BY occurred_at DESC, projection_id DESC LIMIT 25`, predicate.values);
    const text = plan.rows.map((row) => row["QUERY PLAN"]).join("\n");
    assert.match(text, /Index Scan|Bitmap Index Scan|Index Only Scan/, `${predicate.name} did not use a normal indexed plan:\n${text}`);
    assert.match(text, /Buffers:/, `${predicate.name} omitted buffer evidence:\n${text}`);
    assert.match(text, /Execution Time:/, `${predicate.name} omitted execution evidence:\n${text}`);
    assert.doesNotMatch(text, /Seq Scan[^\n]*\(actual [^\n]* rows=(?:[5-9][0-9]|[1-9][0-9]{2,})/, `${predicate.name} scanned a populated unbounded source branch:\n${text}`);
  }
} finally {
  if (client) await client.end();
  if (service) await service.stop();
  await database.drop();
}
const forcedPlannerSwitch = "enable" + "_seqscan";
assert.doesNotMatch(readFileSync(new URL(import.meta.url), "utf8"), new RegExp(forcedPlannerSwitch));
console.log("M17 populated normal-planner project, organization, resource, actor, event-ID, and correlation audit plans retain buffer evidence without broad source scans.");
