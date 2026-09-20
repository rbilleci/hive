import assert from "node:assert/strict";
import { createIsolatedDatabase, postgresClient, startIsolatedLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const bea = "00000000-0000-0000-0000-000000000002";
const alpha = "10000000-0000-0000-0000-000000000001";
let endpoint = "";

// Reads are the generated API: the scope row through `organizations` / `projects`, its memberships,
// budget policy, approval policy and settings connections through the row's relations.
const memberFields = "id principalId endedAt revision roleCodes principals{email lastSeenAt}";
const projectFields = "id lifecycleStatus revision capabilities assignableRoles availablePrincipals{id}"
  + " projectMemberships(orderBy:{startedAt:DESC,id:ASC}){nodes{" + memberFields + "}}"
  + " projectBudgetPolicies{currentVersion{revision currency monthlyLimitCents} status{state reason}}"
  + " projectApprovalPolicies{currentVersion{revision digest}}"
  + " projectSettingsConnections(orderBy:{displayName:ASC,id:ASC}){nodes{id displayName credentialStatus lifecycleStatus revision}}";
const organizationFields = "id revision capabilities assignableRoles availablePrincipals{id}"
  + " organizationMemberships(orderBy:{startedAt:DESC,id:ASC}){nodes{" + memberFields + " projectAccessSummary}}";
const organizationQuery = "query($id:String!){organizations(filters:{id:{eq:$id}}){nodes{" + organizationFields + "}}}";
const projectQuery = "query($id:String!){projects(filters:{id:{eq:$id}}){nodes{" + projectFields + "}}}";
const payload = "{organization{" + organizationFields + "} project{" + projectFields + "} problems{__typename code message expectedRevision actualRevision}}";
const create = "mutation($input:CreateProjectInput!){createProject(input:$input)" + payload + "}";
const add = "mutation($input:AdministrationMembershipInput!){addAdministrationMembership(input:$input)" + payload + "}";
const replace = "mutation($input:ReplaceAdministrationMembershipInput!){replaceAdministrationMembershipRoles(input:$input)" + payload + "}";
const end = "mutation($input:EndAdministrationMembershipInput!){endAdministrationMembership(input:$input)" + payload + "}";
const budget = "mutation($input:UpdateProjectBudgetPolicyInput!){updateProjectBudgetPolicy(input:$input)" + payload + "}";
const approval = "mutation($input:UpdateProjectApprovalPolicyInput!){updateProjectApprovalPolicy(input:$input)" + payload + "}";
const archive = "mutation($input:LifecycleAdministrationInput!){archiveAdministrationScope(input:$input)" + payload + "}";
const restore = "mutation($input:LifecycleAdministrationInput!){restoreAdministrationScope(input:$input)" + payload + "}";
const general = "mutation($input:UpdateProjectGeneralInput!){updateProjectGeneral(input:$input)" + payload + "}";
const connection = "mutation($input:SaveProjectSettingsConnectionInput!){saveProjectSettingsConnection(input:$input)" + payload + "}";

// The approval policy matrix travels as a list of cells.
const cells = (matrix) => Object.entries(matrix).map(([cell, rule]) => ({ cell, ...rule }));
const budgetStatus = async (service, principal, id) =>
  (await gql(service, principal, projectQuery, { id })).projects.nodes[0].projectBudgetPolicies.status;

const baselineMatrix = {
  DEVELOPMENT_LOW: { requiredEvidence: ["PLAN_VALIDATED"], requiredApprovers: 0 },
  DEVELOPMENT_MEDIUM: { requiredEvidence: ["PLAN_VALIDATED", "CHANGE_SUMMARY_READY"], requiredApprovers: 0 },
  DEVELOPMENT_HIGH: { requiredEvidence: ["PLAN_VALIDATED", "CHANGE_SUMMARY_READY", "EVALUATION_PASSED"], requiredApprovers: 1 },
  STAGING_LOW: { requiredEvidence: ["PLAN_VALIDATED", "CHANGE_SUMMARY_READY"], requiredApprovers: 0 },
  STAGING_MEDIUM: { requiredEvidence: ["PLAN_VALIDATED", "CHANGE_SUMMARY_READY", "EVALUATION_PASSED"], requiredApprovers: 1 },
  STAGING_HIGH: { requiredEvidence: ["PLAN_VALIDATED", "CHANGE_SUMMARY_READY", "EVALUATION_PASSED"], requiredApprovers: 1 },
  PRODUCTION_LOW: { requiredEvidence: ["PLAN_VALIDATED", "CHANGE_SUMMARY_READY", "EVALUATION_PASSED"], requiredApprovers: 1 },
  PRODUCTION_MEDIUM: { requiredEvidence: ["PLAN_VALIDATED", "CHANGE_SUMMARY_READY", "EVALUATION_PASSED"], requiredApprovers: 1 },
  PRODUCTION_HIGH: { requiredEvidence: ["PLAN_VALIDATED", "CHANGE_SUMMARY_READY", "EVALUATION_PASSED"], requiredApprovers: 2 }
};

async function gql(service, principal, query, variables) {
  const response = await fetch(endpoint + "/graphql", {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: "sf_session=" + service.signFixtureSession(principal) },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.errors, undefined);
  return body.data;
}

const database = await createIsolatedDatabase("hive_m09_access");
let service = await startIsolatedLocalService(database.name);
endpoint = "http://127.0.0.1:" + service.port;
const client = await postgresClient(database.name);
let projectId = null;
try {
  const organization = (await gql(service, ada, organizationQuery, { id: alpha })).organizations.nodes[0];
  assert(organization.capabilities.includes("PROJECT.CREATE"));
  const adaMembership = organization.organizationMemberships.nodes.find((entry) => entry.roleCodes.includes("ORGANIZATION_ADMIN"));
  assert.equal(adaMembership.principals.email, "ada.lovelace@local.invalid");
  assert(organization.availablePrincipals.some((entry) => entry.id === ada));
  assert(organization.assignableRoles.includes("ORGANIZATION_ADMIN"));
  assert.equal(typeof adaMembership.principals.lastSeenAt, "string");
  assert(adaMembership.projectAccessSummary.includes("All organization projects (ORGANIZATION_ADMIN)"));
  const organizationRoles = (await gql(service, ada, replace, { input: {
    scope: "ORGANIZATION", scopeId: alpha, membershipId: adaMembership.id, roleCodes: ["AUDITOR", "ORGANIZATION_ADMIN"], expectedRevision: adaMembership.revision
  } })).replaceAdministrationMembershipRoles;
  assert.deepEqual(organizationRoles.organization.organizationMemberships.nodes.find((entry) => entry.id === adaMembership.id).roleCodes, ["AUDITOR", "ORGANIZATION_ADMIN"]);
  const invalidOrganizationArchive = (await gql(service, ada, archive, { input: {
    scope: "ORGANIZATION", scopeId: alpha, expectedRevision: organization.revision, reason: "must type current slug", confirmation: "wrong-slug"
  } })).archiveAdministrationScope;
  assert.equal(invalidOrganizationArchive.problems[0].code, "PROTECTED_LIFECYCLE");
  assert.deepEqual((await gql(service, bea, organizationQuery, { id: alpha })).organizations.nodes, []);
  assert.deepEqual((await gql(service, ada, organizationQuery, { id: "10000000-0000-0000-0000-000000000099" })).organizations.nodes, []);
  // Membership rows, their roles and the members' principals are for a holder of the membership view
  // capability only. Bea is no member of alpha, and nothing of it reaches her through any root.
  const rootsQuery = "query($id:String!){organizationMemberships(filters:{organizationId:{eq:$id}}){nodes{id}} organizationMembershipRoles{nodes{membershipId}} projectMemberships{nodes{id}} principals{nodes{id}}}";
  const beaRoots = await gql(service, bea, rootsQuery, { id: alpha });
  assert.deepEqual(beaRoots.organizationMemberships.nodes, []);
  assert.deepEqual(beaRoots.organizationMembershipRoles.nodes, []);
  assert.deepEqual(beaRoots.projectMemberships.nodes, []);
  assert.deepEqual(beaRoots.principals.nodes.map((entry) => entry.id), [bea]);
  // Ada is a plain ORGANIZATION_MEMBER of the support organization: she reads the organization, not its members.
  const support = "10000000-0000-0000-0000-000000000002";
  const plainMember = (await gql(service, ada, organizationQuery, { id: support })).organizations.nodes[0];
  assert(plainMember.capabilities.includes("ORGANIZATION.VIEW"));
  assert(!plainMember.capabilities.includes("ORGANIZATION_MEMBERSHIP.VIEW"));
  assert.deepEqual(plainMember.organizationMemberships.nodes, []);
  assert.deepEqual(plainMember.availablePrincipals, []);
  assert.deepEqual((await gql(service, ada, rootsQuery, { id: support })).organizationMemberships.nodes, []);
  const deniedCreate = (await gql(service, bea, create, { input: {
    organizationId: alpha, expectedRevision: organization.revision, slug: "hidden-project", displayName: "Hidden", description: "must not disclose"
  } })).createProject;
  assert.equal(deniedCreate.problems[0].code, "FORBIDDEN");
  assert.equal(deniedCreate.problems[0].message, "This administration resource is unavailable.");

  const created = (await gql(service, ada, create, { input: {
    organizationId: alpha, expectedRevision: organization.revision, slug: "m09-check", displayName: "M09 Check", description: "Local check"
  } })).createProject;
  assert.deepEqual(created.problems, []);
  projectId = created.project.id;
  assert(created.project.availablePrincipals.some((entry) => entry.id === ada));
  assert(created.project.assignableRoles.includes("AGENT_DEVELOPER"));

  const added = (await gql(service, ada, add, { input: {
    scope: "PROJECT", scopeId: projectId, principalId: ada, roleCodes: ["PROJECT_ADMIN"], expectedScopeRevision: created.project.revision
  } })).addAdministrationMembership;
  assert.deepEqual(added.problems, []);
  assert(added.project.capabilities.includes("PROJECT_BUDGET.UPDATE"));
  assert(added.project.capabilities.includes("PROJECT.UPDATE"));
  assert(!added.project.capabilities.some((capability) => capability.startsWith("PROJECT_CONNECTION.")));
  const membership = added.project.projectMemberships.nodes.find((entry) => entry.roleCodes.includes("PROJECT_ADMIN"));

  const replaced = (await gql(service, ada, replace, { input: {
    scope: "PROJECT", scopeId: projectId, membershipId: membership.id, roleCodes: ["AUDITOR", "PROJECT_ADMIN"], expectedRevision: membership.revision
  } })).replaceAdministrationMembershipRoles;
  assert.deepEqual(replaced.problems, []);
  const replacedMembership = replaced.project.projectMemberships.nodes.find((entry) => entry.id === membership.id);
  assert.deepEqual(replacedMembership.roleCodes, ["AUDITOR", "PROJECT_ADMIN"]);

  const savedConnection = (await gql(service, ada, connection, { input: {
    projectId, expectedRevision: 0, displayName: "Fixture tool", definitionVersion: "fixture-tool/v1", environment: "DEVELOPMENT",
    credentialStatus: "UNBOUND", lifecycleStatus: "ACTIVE"
  } })).saveProjectSettingsConnection;
  assert.deepEqual(savedConnection.problems, []);
  const projectConnection = savedConnection.project.projectSettingsConnections.nodes[0];
  assert.equal(projectConnection.credentialStatus, "UNBOUND");

  const staleBudget = (await gql(service, ada, budget, { input: {
    projectId, expectedRevision: 1, currency: "USD", monthlyLimitCents: 100000, warningThresholdCents: 50000, reason: "stale"
  } })).updateProjectBudgetPolicy;
  assert.equal(staleBudget.problems[0].code, "REVISION_CONFLICT");
  const savedBudget = (await gql(service, ada, budget, { input: {
    projectId, expectedRevision: 0, currency: "USD", monthlyLimitCents: 100000, warningThresholdCents: 50000, reason: "local test"
  } })).updateProjectBudgetPolicy;
  assert.equal(savedBudget.project.projectBudgetPolicies.currentVersion.revision, 1);
  assert.equal(savedBudget.project.projectBudgetPolicies.status.state, "UNKNOWN");

  await client.query("INSERT INTO frozen_spend_import_batches (id, project_id, period_start, period_end, currency, state, amount_cents, includes_estimates, data_as_of, completed_at) VALUES (gen_random_uuid(), $1, date_trunc('month', CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC', date_trunc('month', CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' + INTERVAL '1 month', 'USD', 'COMPLETE', 12345, TRUE, CURRENT_TIMESTAMP - INTERVAL '1 hour', CURRENT_TIMESTAMP - INTERVAL '1 hour')", [projectId]);
  assert.equal((await budgetStatus(service, ada, projectId)).state, "NORMAL");
  await client.query("UPDATE frozen_spend_import_batches SET state = 'FAILED' WHERE project_id = $1", [projectId]);
  let status = await budgetStatus(service, ada, projectId);
  assert.deepEqual(status, { state: "UNKNOWN", reason: "IMPORT_FAILED" });
  await client.query("UPDATE frozen_spend_import_batches SET state = 'COMPLETE', completed_at = CURRENT_TIMESTAMP - INTERVAL '2 days' WHERE project_id = $1", [projectId]);
  status = await budgetStatus(service, ada, projectId);
  assert.deepEqual(status, { state: "UNKNOWN", reason: "STALE_DATA" });
  await client.query("UPDATE frozen_spend_import_batches SET completed_at = CURRENT_TIMESTAMP, currency = 'EUR' WHERE project_id = $1", [projectId]);
  status = await budgetStatus(service, ada, projectId);
  assert.deepEqual(status, { state: "UNKNOWN", reason: "CURRENCY_MISMATCH" });
  await client.query("UPDATE frozen_spend_import_batches SET currency = 'USD' WHERE project_id = $1", [projectId]);
  assert.equal((await budgetStatus(service, ada, projectId)).state, "NORMAL");

  const strongerMatrix = { ...baselineMatrix, DEVELOPMENT_LOW: { requiredEvidence: ["PLAN_VALIDATED", "CHANGE_SUMMARY_READY"], requiredApprovers: 0 } };
  const strengthened = (await gql(service, ada, approval, { input: { projectId, expectedRevision: 1, matrix: cells(strongerMatrix), reason: "Strengthen local evidence" } })).updateProjectApprovalPolicy;
  assert.equal(strengthened.project.projectApprovalPolicies.currentVersion.revision, 2);
  const stableDigest = strengthened.project.projectApprovalPolicies.currentVersion.digest;
  const idempotent = (await gql(service, ada, approval, { input: { projectId, expectedRevision: 2, matrix: cells(strongerMatrix), reason: "Repeat" } })).updateProjectApprovalPolicy;
  assert.equal(idempotent.project.projectApprovalPolicies.currentVersion.revision, 2);
  assert.equal(idempotent.project.projectApprovalPolicies.currentVersion.digest, stableDigest);
  await service.stop();
  service = await startIsolatedLocalService(database.name);
  endpoint = "http://127.0.0.1:" + service.port;
  const afterRestart = (await gql(service, ada, approval, { input: { projectId, expectedRevision: 2, matrix: cells(strongerMatrix), reason: "Repeat after restart" } })).updateProjectApprovalPolicy;
  assert.equal(afterRestart.project.projectApprovalPolicies.currentVersion.revision, 2);
  assert.equal(afterRestart.project.projectApprovalPolicies.currentVersion.digest, stableDigest);
  const weakening = (await gql(service, ada, approval, { input: {
    projectId, expectedRevision: 2, matrix: cells({ ...strongerMatrix, PRODUCTION_HIGH: { requiredEvidence: ["PLAN_VALIDATED"], requiredApprovers: 0 } }), reason: "weaken"
  } })).updateProjectApprovalPolicy;
  assert.equal(weakening.problems[0].code, "POLICY_WEAKENING");

  const ended = (await gql(service, ada, end, { input: {
    scope: "PROJECT", scopeId: projectId, membershipId: membership.id, expectedRevision: replacedMembership.revision, reason: "Access review"
  } })).endAdministrationMembership;
  assert.deepEqual(ended.problems, []);
  assert(!ended.project.capabilities.includes("PROJECT_BUDGET.UPDATE"));
  const capabilityLoss = (await gql(service, ada, budget, { input: {
    projectId, expectedRevision: 1, currency: "USD", monthlyLimitCents: 110000, warningThresholdCents: 55000, reason: "must not write"
  } })).updateProjectBudgetPolicy;
  assert.equal(capabilityLoss.problems[0].code, "FORBIDDEN");

  const archived = (await gql(service, ada, archive, { input: {
    scope: "PROJECT", scopeId: projectId, expectedRevision: created.project.revision, reason: "Lifecycle check"
  } })).archiveAdministrationScope;
  assert.equal(archived.project.lifecycleStatus, "ARCHIVED");
  const blockedWrite = (await gql(service, ada, general, { input: {
    projectId, expectedRevision: archived.project.revision, displayName: "No archived edit", description: "refused"
  } })).updateProjectGeneral;
  assert.equal(blockedWrite.problems[0].code, "PROTECTED_LIFECYCLE");
  const safetyReduction = (await gql(service, ada, connection, { input: {
    projectId, connectionId: projectConnection.id, expectedRevision: projectConnection.revision, displayName: "Fixture tool",
    definitionVersion: "fixture-tool/v1", environment: "DEVELOPMENT", credentialStatus: "UNBOUND", lifecycleStatus: "DISABLED"
  } })).saveProjectSettingsConnection;
  assert.deepEqual(safetyReduction.problems, []);
  assert.equal(safetyReduction.project.projectSettingsConnections.nodes[0].lifecycleStatus, "DISABLED");
  const restored = (await gql(service, ada, restore, { input: {
    scope: "PROJECT", scopeId: projectId, expectedRevision: archived.project.revision
  } })).restoreAdministrationScope;
  assert.equal(restored.project.lifecycleStatus, "ACTIVE");

  const audits = await client.query("SELECT id, action, facts FROM administration_audit_events WHERE scope_id = $1 ORDER BY occurred_at", [projectId]);
  const actions = audits.rows.map((entry) => entry.action);
  for (const action of ["PROJECT_CREATED", "PROJECT_MEMBERSHIP_ADDED", "PROJECT_MEMBERSHIP_ROLES_REPLACED", "PROJECT_MEMBERSHIP_ENDED", "PROJECT_BUDGET_POLICY_UPDATED", "PROJECT_APPROVAL_POLICY_UPDATED", "PROJECT_ARCHIVED", "PROJECT_RESTORED"]) assert(actions.includes(action));
  const policyAudit = audits.rows.find((entry) => entry.action === "PROJECT_APPROVAL_POLICY_UPDATED");
  assert.equal(policyAudit.facts.action, "PROJECT_APPROVAL_POLICY_UPDATED");
  assert.equal(policyAudit.facts.scopeId, projectId);
  assert.equal(policyAudit.facts.actorPrincipalId, ada);
  // The Aurora DSQL alignment removed every trigger and rule, so storage no longer rejects a direct
  // UPDATE or DELETE here; append-only behavior is an application guarantee (no repository mutates these rows).
  assert.equal((await client.query("SELECT action FROM administration_audit_events WHERE id = $1", [policyAudit.id])).rows[0].action, "PROJECT_APPROVAL_POLICY_UPDATED");
} finally {
  await client.end();
  await service.stop();
  await database.drop();
}
