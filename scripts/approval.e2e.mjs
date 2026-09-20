import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startIsolatedLocalService } from "./local-service.mjs";

const requester = "d1410000-0000-0000-0000-000000000001";
const approver = "d1410000-0000-0000-0000-000000000002";
const revokedApprover = "d1410000-0000-0000-0000-000000000003";
const organization = "10000000-0000-0000-0000-000000000004";
const project = "50000000-0000-0000-0000-000000000003";
let origin = "";

async function graphql(service, principal, query, variables) {
  const response = await fetch(`${origin}/graphql`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  assert.match(response.headers.get("x-request-id") ?? "", /^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i);
  const body = await response.json();
  assert.equal(body.errors, undefined, JSON.stringify(body.errors));
  return body.data;
}

async function publishedVersion(service) {
  const created = await graphql(service, requester,
    "mutation Create($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId revision } problems { code } } }",
    { input: { projectId: project, displayName: "M14 browser approval fixture" } });
  assert.deepEqual(created.createAgentDraft.problems, []);
  const agentId = created.createAgentDraft.agentDraft.agentId;
  const document = {
    general: { displayName: "M14 browser approval fixture", description: "Browser approval fixture." },
    instructions: { source: "# Browser fixture", language: "markdown" }, harness: { source: "def run(value): return value", language: "python" },
    model: { reference: "model:local-safe-chat@v2" }, tools: { source: "{}", language: "json" }, skills: { source: "export const skills = [];", language: "javascript" },
    capabilities: { source: "export type Capability = string;", language: "typescript" }, subagents: { enabled: false }, memory: { strategy: "project" },
    guardrails: { source: "# local guardrail checks", language: "shell" }, identity: { persona: "" }, observability: { source: "<observability/>", language: "xml" },
    limits: { maxTokens: 256 }, evaluations: { required: false }, dependencies: ["model:local-safe-chat@v2"]
  };
  const saved = await graphql(service, requester,
    "mutation Save($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: created.createAgentDraft.agentDraft.revision, document } });
  assert.deepEqual(saved.updateAgentDraft.problems, []);
  const validated = await graphql(service, requester,
    "mutation Validate($input: ValidateAgentDraftInput!) { validateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: saved.updateAgentDraft.agentDraft.revision } });
  assert.deepEqual(validated.validateAgentDraft.problems, []);
  const published = await graphql(service, requester,
    "mutation Publish($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentVersion { id } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: validated.validateAgentDraft.agentDraft.revision, warningsAcknowledged: true } });
  assert.deepEqual(published.publishAgentDraft.problems, []);
  return published.publishAgentDraft.agentVersion.id;
}

async function stagingEnvironment(service, versionId) {
  const values = await graphql(service, requester,
    "query Environments($version: ID!) { deploymentEnvironmentDefinitionVersions(agentVersionId: $version, first: 50) { edges { node { id logicalEnvironmentClass } } } }", { version: versionId });
  const staging = values.deploymentEnvironmentDefinitionVersions.edges.map((edge) => edge.node).find((value) => value.logicalEnvironmentClass === "STAGING");
  assert(staging);
  return staging.id;
}

async function recordEvaluationPassed(deploymentId) {
  const policy = await client.query("SELECT binding_digest, agent_version_id, environment_definition_version_id, target_digest, plan_digest, package_digest, evaluation_requirement_expires_at FROM deployment_policy_snapshots WHERE deployment_id = $1", [deploymentId]);
  assert.equal(policy.rowCount, 1);
  const fact = policy.rows[0];
  await client.query(`INSERT INTO deployment_evidence_snapshots (id, deployment_id, evidence_kind, evidence_digest, expires_at, agent_version_id,
      environment_definition_version_id, target_digest, plan_digest, package_digest, binding_digest)
    VALUES ($1, $2, 'EVALUATION_PASSED', encode(sha256(convert_to($3 || '|EVALUATION_PASSED', 'UTF8')), 'hex'), $4, $5, $6, $7, $8, $9, $3)`,
    [crypto.randomUUID(), deploymentId, fact.binding_digest, fact.evaluation_requirement_expires_at, fact.agent_version_id,
      fact.environment_definition_version_id, fact.target_digest, fact.plan_digest, fact.package_digest]);
}

const database = await createIsolatedDatabase("hive_m14_e2e");
let service;
let client;
try {
  service = await startIsolatedLocalService(database.name);
  origin = `http://127.0.0.1:${service.port}`;
  client = await postgresClient(database.name);
  const roleAssignmentPlatform = "d1500000-0000-0000-0000-000000000003";
  await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, 'm14-browser-platform', 'm14-browser-platform', 'm14-browser-platform@local.invalid') ON CONFLICT (id) DO NOTHING", [roleAssignmentPlatform]);
  await client.query("INSERT INTO platform_role_assignments (principal_id, role_code) VALUES ($1, 'PLATFORM_ADMIN') ON CONFLICT DO NOTHING", [roleAssignmentPlatform]);
  await client.query("SELECT set_config('hive.m14_approval_role_assignment_actor', $1, FALSE)", [roleAssignmentPlatform]);
  const projectMemberships = new Map();
  for (const [id, subject] of [[requester, "m14-browser-requester"], [approver, "m14-browser-approver"], [revokedApprover, "m14-browser-revoked-approver"]]) {
    await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, $2, $2, $2 || '@local.invalid') ON CONFLICT (id) DO NOTHING", [id, subject]);
    const organizationMembership = crypto.randomUUID();
    await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL)", [organizationMembership, organization, id]);
    await client.query("INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_MEMBER')", [organizationMembership]);
    const projectMembership = crypto.randomUUID();
    await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1)", [projectMembership, project, id]);
    await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, $2)", [projectMembership, id === requester ? "AGENT_DEVELOPER" : "DEPLOYMENT_APPROVER"]);
    projectMemberships.set(id, projectMembership);
  }
  const versionId = await publishedVersion(service);
  const environmentId = await stagingEnvironment(service, versionId);
  const request = await graphql(service, requester,
    "mutation Deploy($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id lifecycleStatus } problems { code } } }",
    { input: { agentVersionId: versionId, environmentDefinitionVersionId: environmentId, strategy: "ROLLING", idempotencyKey: "m14-browser-decision" } });
  assert.deepEqual(request.deployAgentVersion.problems, []);
  assert.equal(request.deployAgentVersion.deployment.lifecycleStatus, "AWAITING_APPROVAL");
  const deploymentId = request.deployAgentVersion.deployment.id;
  // M16 owns evaluation authoring. This browser fixture appends the exact-bound fact so M14 can
  // exercise its inbox and immutable decision flow after an evaluation exists.
  await recordEvaluationPassed(deploymentId);
  const requirement = await client.query("SELECT id FROM deployment_approval_requirements WHERE deployment_id = $1", [deploymentId]);
  assert.equal(requirement.rowCount, 1);
  const requirementId = requirement.rows[0].id;
  const detailPath = `/projects/${project}/deployments/${deploymentId}/approvals/${requirementId}`;
  const routeRetryA = await graphql(service, requester,
    "mutation DeployRouteRetryA($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }",
    { input: { agentVersionId: versionId, environmentDefinitionVersionId: environmentId, strategy: "ROLLING", idempotencyKey: "m14-browser-route-retry-a" } });
  assert.deepEqual(routeRetryA.deployAgentVersion.problems, []);
  await recordEvaluationPassed(routeRetryA.deployAgentVersion.deployment.id);
  const routeRetryARequirement = await client.query("SELECT id FROM deployment_approval_requirements WHERE deployment_id = $1", [routeRetryA.deployAgentVersion.deployment.id]);
  const routeRetryB = await graphql(service, requester,
    "mutation DeployRouteRetryB($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }",
    { input: { agentVersionId: versionId, environmentDefinitionVersionId: environmentId, strategy: "ROLLING", idempotencyKey: "m14-browser-route-retry-b" } });
  assert.deepEqual(routeRetryB.deployAgentVersion.problems, []);
  await recordEvaluationPassed(routeRetryB.deployAgentVersion.deployment.id);
  const routeRetryBRequirement = await client.query("SELECT id FROM deployment_approval_requirements WHERE deployment_id = $1", [routeRetryB.deployAgentVersion.deployment.id]);
  const routeRetryAPath = `/projects/${project}/deployments/${routeRetryA.deployAgentVersion.deployment.id}/approvals/${routeRetryARequirement.rows[0].id}`;
  const routeRetryBPath = `/projects/${project}/deployments/${routeRetryB.deployAgentVersion.deployment.id}/approvals/${routeRetryBRequirement.rows[0].id}`;
  // approvalInbox used to return null -- rendered by the browser as "Approvals are unavailable.", with
  // no "Approval inbox" heading at all -- until deployment_approval_read_ready() reported true, gating
  // on MaintenanceJobs' background reconciliation completing. That compatibility-backfill gate is
  // removed, not ported -- see PostgresDeploymentRepository.reconcileApprovalUpgrade()'s comment for
  // the shared reasoning -- so approvalInbox() no longer gates on anything: it is available as soon as
  // this fixture's own setup calls above (already awaited) commit.
  const browser = await launchBrowser();
  try {
    const timeoutContext = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await timeoutContext.addCookies([{ name: "sf_session", value: service.signFixtureSession(approver), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const timeoutPage = await timeoutContext.newPage();
    await timeoutPage.goto(`${origin}/approvals`);
    await timeoutPage.getByRole("heading", { name: "Approval inbox" }).waitFor();
    await timeoutPage.getByRole("link", { name: /M14 browser approval fixture · v1/ }).first().waitFor();
    await timeoutPage.evaluate(() => {
      const originalFetch = window.fetch.bind(window);
      window.fetch = (input, init) => {
        if (typeof input === "string" && input.endsWith("/graphql")) return new Promise((_, reject) => {
          init.signal?.addEventListener("abort", () => reject(new DOMException("", "AbortError")), { once: true });
        });
        return originalFetch(input, init);
      };
    });
    await timeoutPage.getByRole("button", { name: "Refresh approvals" }).click();
    await timeoutPage.getByText("We could not refresh approvals. Displayed rows remain from the last successful request.").waitFor({ timeout: 15_000 });
    await timeoutContext.close();

    const retryContext = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await retryContext.addCookies([{ name: "sf_session", value: service.signFixtureSession(approver), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const retryPage = await retryContext.newPage();
    await retryPage.route("**/graphql", (route) => route.request().postData()?.includes("ApprovalRequirement") ? route.abort() : route.continue());
    await retryPage.goto(`${origin}${detailPath}`);
    await retryPage.getByText("We could not load this approval requirement.").waitFor();
    await retryPage.unroute("**/graphql");
    await retryPage.getByRole("button", { name: "Retry approval requirement" }).click();
    await retryPage.getByRole("heading", { name: "M14 browser approval fixture · v1" }).waitFor();
    await retryContext.close();

    const requesterContext = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await requesterContext.addCookies([{ name: "sf_session", value: service.signFixtureSession(requester), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const requesterPage = await requesterContext.newPage();
    await requesterPage.goto(`${origin}${detailPath}`);
    await requesterPage.getByText("This approval requirement is unavailable.").waitFor();
    assert.equal(await requesterPage.getByRole("button", { name: "Approve deployment" }).count(), 0);
    assert.equal(await requesterPage.getByText(/DEPLOYMENT_APPROVER|eligible|requester cannot/i).count(), 0);
    await requesterContext.close();

    const paginationContext = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await paginationContext.addCookies([{ name: "sf_session", value: service.signFixtureSession(approver), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const paginationPage = await paginationContext.newPage();
    let initialInbox;
    let releaseDuplicatePage;
    const duplicatePageRelease = new Promise((resolve) => { releaseDuplicatePage = resolve; });
    let notifyDuplicatePage;
    const duplicatePageStarted = new Promise((resolve) => { notifyDuplicatePage = resolve; });
    let duplicatePageRequests = 0;
    await paginationPage.route("**/graphql", async (route) => {
      const body = route.request().postData() ?? "";
      if (!body.includes("query ApprovalInbox")) return route.continue();
      const request = JSON.parse(body);
      if (!request.variables.after) {
        const response = await route.fetch();
        const payload = await response.json();
        initialInbox = payload.data.approvalInbox;
        payload.data.approvalInbox.pageInfo = { hasNextPage: true, endCursor: "duplicate-page" };
        return route.fulfill({ response, body: JSON.stringify(payload) });
      }
      assert.equal(request.variables.after, "duplicate-page");
      duplicatePageRequests += 1;
      if (duplicatePageRequests === 2) notifyDuplicatePage();
      await duplicatePageRelease;
      const edge = structuredClone(initialInbox.edges[0]);
      edge.cursor = "duplicate-page-next";
      edge.node.requirement.id = crypto.randomUUID();
      edge.node.deployment.id = crypto.randomUUID();
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ data: {
        approvalInbox: { edges: [edge], pageInfo: { hasNextPage: false, endCursor: "duplicate-page-next" } }
      } }) });
    });
    await paginationPage.goto(`${origin}/approvals`);
    await paginationPage.getByRole("heading", { name: "Approval inbox" }).waitFor();
    await paginationPage.getByRole("button", { name: "Load more approvals" }).evaluate((button) => { button.click(); button.click(); });
    await duplicatePageStarted;
    releaseDuplicatePage();
    await paginationPage.waitForFunction(() => document.querySelectorAll(".approval-list li").length === 4);
    const paginationLinks = await paginationPage.locator(".approval-list li a").evaluateAll((links) => links.map((link) => link.getAttribute("href")));
    assert.equal(new Set(paginationLinks).size, paginationLinks.length);
    await paginationContext.close();

    const revocationContext = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await revocationContext.addCookies([{ name: "sf_session", value: service.signFixtureSession(revokedApprover), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const revocationPage = await revocationContext.newPage();
    await revocationPage.goto(`${origin}${detailPath}`);
    await revocationPage.getByRole("button", { name: "Refresh requirement" }).waitFor();
    await revocationPage.getByText("Select a standard review code.").waitFor();
    let releaseStaleApprovalResponse;
    const staleApprovalResponse = new Promise((resolve) => { releaseStaleApprovalResponse = resolve; });
    let startStaleApprovalResponse;
    const staleApprovalStarted = new Promise((resolve) => { startStaleApprovalResponse = resolve; });
    let stallApprovalRefresh = false;
    await revocationPage.route("**/graphql", async (route) => {
      if (stallApprovalRefresh && route.request().postData()?.includes("ApprovalRequirement")) {
        startStaleApprovalResponse();
        await staleApprovalResponse;
      }
      await route.continue();
    });
    stallApprovalRefresh = true;
    await revocationPage.getByRole("button", { name: "Refresh requirement" }).click();
    await staleApprovalStarted;
    await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1 AND role_code = 'DEPLOYMENT_APPROVER'", [projectMemberships.get(revokedApprover)]);
    await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AUDITOR')", [projectMemberships.get(revokedApprover)]);
    await revocationPage.evaluate(() => window.dispatchEvent(new Event("focus")));
    releaseStaleApprovalResponse();
    await revocationPage.getByText("A decision is unavailable for this requirement.").waitFor({ timeout: 15_000 });
    assert.equal(await revocationPage.getByRole("button", { name: "Approve deployment" }).isDisabled(), true);
    await revocationContext.close();

    const approverContext = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await approverContext.addCookies([{ name: "sf_session", value: service.signFixtureSession(approver), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const page = await approverContext.newPage();
    await page.goto(`${origin}/approvals`);
    await page.getByRole("heading", { name: "Approval inbox" }).waitFor();
    await Promise.all([page.waitForURL((url) => url.pathname === detailPath), page.locator(`a[href="${detailPath}"]`).click()]);
    await page.getByRole("button", { name: "Refresh requirement" }).waitFor();
    // A browser that had cached this M14 detail must withdraw it when a rollback M13 API rejects
    // the approval field at GraphQL validation. No decision mutation is attempted after the read.
    await page.route("**/graphql", (route) => route.request().postData()?.includes("ApprovalRequirement")
      ? route.fulfill({ contentType: "application/json", body: JSON.stringify({ errors: [{ message: 'Cannot query field "approvalRequirement" on type "Query".' }] }) })
      : route.continue());
    await page.getByRole("button", { name: "Refresh requirement" }).click();
    await page.getByText("This approval requirement is unavailable.").waitFor();
    assert.equal(await page.getByRole("button", { name: "Approve deployment" }).count(), 0);
    await page.unroute("**/graphql");
    await page.goto(`${origin}/approvals`);
    await page.getByRole("heading", { name: "Approval inbox" }).waitFor();
    await page.route("**/graphql", (route) => route.request().postData()?.includes("ApprovalInbox")
      ? route.fulfill({ contentType: "application/json", body: JSON.stringify({ errors: [{ message: 'Cannot query field "approvalInbox" on type "Query".' }] }) })
      : route.continue());
    await page.getByRole("button", { name: "Refresh approvals" }).click();
    await page.getByText("Approvals are unavailable.").waitFor();
    assert.equal(await page.getByRole("link", { name: /M14 browser approval fixture · v1/ }).count(), 0);
    await page.unroute("**/graphql");
    await page.goto(`${origin}/approvals`);
    await Promise.all([page.waitForURL((url) => url.pathname === detailPath), page.locator(`a[href="${detailPath}"]`).click()]);
    await page.getByRole("button", { name: "Refresh requirement" }).waitFor();
    await page.getByText("Select a standard review code.").waitFor();
    await page.getByText(/EVALUATION_PASSED: VALID/).waitFor();
    const decisionResponse = page.waitForResponse((response) => response.url().endsWith("/graphql")
      && response.request().postData()?.includes("mutation DecideDeploymentApproval"));
    await page.getByRole("button", { name: "Approve deployment" }).click();
    const correlationId = (await decisionResponse).headers()["x-request-id"];
    assert.match(correlationId ?? "", /^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i);
    await page.getByText("The immutable approval was recorded.").waitFor();
    const persistedCorrelation = await client.query(`SELECT decision.correlation_id::text AS decision_correlation_id,
        audit.facts->>'correlationId' AS audit_correlation_id
      FROM deployment_approval_decisions decision
      JOIN deployment_audit_events audit ON audit.deployment_id = $1 AND audit.action = 'APPROVAL_RECORDED'
      WHERE decision.approval_requirement_id = $2
      ORDER BY audit.occurred_at DESC, audit.id DESC LIMIT 1`, [deploymentId, requirementId]);
    assert.deepEqual(persistedCorrelation.rows[0], { decision_correlation_id: correlationId, audit_correlation_id: correlationId });
    await page.getByText(/SATISFIED · expires/).waitFor();
    await page.goto(`${origin}/organizations/${organization}/approvals`);
    await page.getByRole("heading", { name: "Approval inbox" }).waitFor();
    await page.locator(`a[href="${detailPath}"]`).waitFor();
    await page.goto(`${origin}${routeRetryAPath}`);
    await page.getByText("Select a standard review code.").waitFor();
    let resolveLostResponse;
    const lostResponse = new Promise((resolve) => { resolveLostResponse = resolve; });
    let lostIdempotencyKey = "";
    let firstDecision = true;
    await page.route("**/graphql", async (route) => {
      if (firstDecision && route.request().postData()?.includes("mutation DecideDeploymentApproval")) {
        firstDecision = false;
        lostIdempotencyKey = JSON.parse(route.request().postData() ?? "{}").variables.input.idempotencyKey;
        await route.fetch();
        resolveLostResponse();
        await new Promise(() => {});
      } else await route.continue();
    });
    await page.getByRole("button", { name: "Approve deployment" }).click();
    await lostResponse;
    await page.goto(`${origin}${routeRetryBPath}`);
    await page.getByText("Select a standard review code.").waitFor();
    assert.match(await page.getByRole("link", { name: "Open immutable deployment audit correlation" }).getAttribute("href") ?? "", new RegExp(routeRetryB.deployAgentVersion.deployment.id));
    const routeRetryDecision = page.waitForRequest((request) => request.url().endsWith("/graphql")
      && request.postData()?.includes("mutation DecideDeploymentApproval") && !firstDecision);
    await page.getByRole("button", { name: "Approve deployment" }).click();
    const routeRetryIdempotencyKey = JSON.parse((await routeRetryDecision).postData() ?? "{}").variables.input.idempotencyKey;
    assert.notEqual(routeRetryIdempotencyKey, lostIdempotencyKey);
    await page.getByText("The immutable approval was recorded.").waitFor();
    await approverContext.close();
  } finally {
    await browser.close();
  }
} finally {
  if (client) await client.end();
  if (service) await service.stop();
  await database.drop();
}
