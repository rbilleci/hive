import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startIsolatedLocalService, startLocalDeploymentWorker } from "./local-service.mjs";

const requester = "d1310000-0000-0000-0000-000000000004";
const recoveryActor = "d1310000-0000-0000-0000-000000000005";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const project = "50000000-0000-0000-0000-000000000003";
const membership = "d1310000-0000-0000-0000-000000000001";
const role = "d1310000-0000-0000-0000-000000000002";
const requesterProjectMembership = "d1310000-0000-0000-0000-000000000003";
const recoveryActorOrganizationMembership = "d1310000-0000-0000-0000-000000000005";
const recoveryActorProjectMembership = "d1310000-0000-0000-0000-000000000006";
const otherProject = "d1310000-0000-0000-0000-000000000020";
const otherProjectMembership = "d1310000-0000-0000-0000-000000000021";
const recoveryConfirmationSentinel = "m15-production-confirmation-redaction-sentinel";
let origin = "";

async function graphql(service, query, variables, principal = requester) {
  const response = await graphqlResponse(service, query, variables, principal);
  assert.equal(response.status, 200);
  const payload = await response.json();
  assert.equal(payload.errors, undefined, JSON.stringify(payload.errors));
  return payload;
}

async function graphqlResponse(service, query, variables, principal = requester) {
  return fetch(`${origin}/graphql`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` },
    body: JSON.stringify({ query, variables })
  });
}

function recoveryLogs(service) {
  return service.output().split(/\r?\n/).filter((line) => line.includes("event=deployment_recovery"));
}

async function publishedVersion(service) {
  const created = await graphql(service,
    "mutation Create($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId revision } problems { code } } }",
    { input: { projectId: project, displayName: "M13 browser fixture" } });
  assert.deepEqual(created.data.createAgentDraft.problems, []);
  const agentId = created.data.createAgentDraft.agentDraft.agentId;
  const document = {
    general: { displayName: "M13 browser fixture", description: "Browser deployment fixture." },
    instructions: { source: "# Browser fixture", language: "markdown" }, harness: { source: "def run(value): return value", language: "python" },
    model: { reference: "model:local-safe-chat@v2" }, tools: { source: "{}", language: "json" }, skills: { source: "export const skills = [];", language: "javascript" },
    capabilities: { source: "export type Capability = string;", language: "typescript" }, subagents: { enabled: false }, memory: { strategy: "project" },
    guardrails: { source: "# local guardrail checks", language: "shell" }, identity: { persona: "" }, observability: { source: "<observability/>", language: "xml" },
    limits: { maxTokens: 256 }, evaluations: { required: false }, dependencies: ["model:local-safe-chat@v2"]
  };
  const saved = await graphql(service,
    "mutation Save($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: created.data.createAgentDraft.agentDraft.revision, document } });
  assert.deepEqual(saved.data.updateAgentDraft.problems, []);
  const validated = await graphql(service,
    "mutation Validate($input: ValidateAgentDraftInput!) { validateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: saved.data.updateAgentDraft.agentDraft.revision } });
  assert.deepEqual(validated.data.validateAgentDraft.problems, []);
  const published = await graphql(service,
    "mutation Publish($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentVersion { id } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: validated.data.validateAgentDraft.agentDraft.revision, warningsAcknowledged: true } });
  assert.deepEqual(published.data.publishAgentDraft.problems, []);
  return { agentId, versionId: published.data.publishAgentDraft.agentVersion.id };
}

const database = await createIsolatedDatabase("hive_m13_e2e");
let service;
let client;
let worker;
try {
  service = await startIsolatedLocalService(database.name, { HIVE_DEPLOYMENT_WORKER_STALE_MILLIS: "500" });
  origin = `http://127.0.0.1:${service.port}`;
  client = await postgresClient(database.name);
  worker = await startLocalDeploymentWorker(database.name, {
    HIVE_DEPLOYMENT_WORKER_INITIAL_DELAY_MILLIS: "600000",
    HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "250"
  });
  await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, 'm13-browser-fixture', 'M13 Browser Fixture', 'm13-browser-fixture@local.invalid') ON CONFLICT (id) DO NOTHING", [requester]);
  await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, 'm15-recovery-fixture', 'M15 Recovery Fixture', 'm15-recovery-fixture@local.invalid') ON CONFLICT (id) DO NOTHING", [recoveryActor]);
  await client.query("DELETE FROM console_role_assignments WHERE id = $1", [role]);
  await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1", [requesterProjectMembership]);
  await client.query("DELETE FROM project_memberships WHERE id = $1", [requesterProjectMembership]);
  await client.query("DELETE FROM organization_membership_roles WHERE membership_id = $1", [membership]);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [membership]);
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL) ON CONFLICT (id) DO NOTHING", [membership, privateOrganization, requester]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1)", [requesterProjectMembership, project, requester]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AGENT_DEVELOPER')", [requesterProjectMembership]);
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL) ON CONFLICT (id) DO NOTHING", [recoveryActorOrganizationMembership, privateOrganization, recoveryActor]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1) ON CONFLICT (id) DO NOTHING", [recoveryActorProjectMembership, project, recoveryActor]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AGENT_DEVELOPER')", [recoveryActorProjectMembership]);
  await client.query("INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'm13-route-scope', 'M13 Route Scope', 'ACTIVE')", [otherProject, privateOrganization]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, ended_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL, 1)", [otherProjectMembership, otherProject, requester]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AGENT_DEVELOPER')", [otherProjectMembership]);
  const { agentId, versionId } = await publishedVersion(service);
  const requestPath = `/projects/${project}/agents/${agentId}/versions/${versionId}/deploy`;
  const browser = await launchBrowser();
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await context.addCookies([{ name: "sf_session", value: service.signFixtureSession(requester), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const page = await context.newPage();

    await page.goto(`${origin}/projects/${project}/agents/${agentId}/versions/${versionId}`);
    await page.getByRole("heading", { name: "M13 browser fixture · v1" }).waitFor();
    await page.getByRole("link", { name: "Request deployment from this immutable version" }).click();
    await page.getByRole("heading", { name: "Request deployment" }).waitFor();
    await page.getByRole("heading", { name: "Frozen request preview" }).waitFor();
    const staleRequestVersion = await graphql(service,
      "query AgentVersion($projectId: ID!, $agentId: ID!, $versionId: ID!) { agentVersion(projectId: $projectId, agentId: $agentId, versionId: $versionId) { id agentId number slug displayName canonicalDocument contentDigest dependencies catalogReleaseId catalogReleaseDigest publishedBy publishedAt } }",
      { projectId: project, agentId, versionId });
    let releaseRequestRoute;
    const deferredRequestRoute = new Promise((resolve) => { releaseRequestRoute = resolve; });
    await page.route("**/graphql", async (route) => {
      const body = route.request().postData();
      if (body?.includes("query AgentVersion") && body.includes(`\"projectId\":\"${project}\"`)) { releaseRequestRoute(route); return; }
      await route.fallback();
    });
    await page.goto(`${origin}${requestPath}`);
    const delayedRequestRoute = await deferredRequestRoute;
    await page.goto(`${origin}/projects/${otherProject}/agents/${agentId}/versions/${versionId}/deploy`);
    await page.getByText("This immutable version is unavailable.").waitFor();
    await delayedRequestRoute.fulfill({ contentType: "application/json", body: JSON.stringify(staleRequestVersion) });
    await page.getByText("This immutable version is unavailable.").waitFor();
    await page.getByRole("heading", { name: "Frozen request preview" }).count().then((count) => assert.equal(count, 0));
    await page.getByRole("button", { name: "Request deployment" }).count().then((count) => assert.equal(count, 0));
    await page.unroute("**/graphql");
    await page.goto(`${origin}${requestPath}`);
    await page.getByRole("heading", { name: "Frozen request preview" }).waitFor();
    const previewEnvironmentId = await page.getByRole("combobox").first().inputValue();
    const previewFields = "environmentDefinitionVersion { id stableDefinitionId version displayName logicalEnvironmentClass catalogReleaseId catalogReleaseDigest contentDigest } strategy risk policyDigest policyRevision requiredEvidence requiredApprovers planDigest packageDigest catalogReleaseId catalogReleaseDigest agentContentDigest targetDigest bindingDigest currentTarget { aliasName deploymentId agentVersionId agentVersionNumber targetDigest requestedAt } requirementExpiresAt warnings compatibility";
    const stalePreview = await graphql(service,
      `query DeploymentPreview($agentVersionId: ID!, $environmentDefinitionVersionId: ID!, $strategy: DeploymentStrategy!) { deploymentPreview(agentVersionId: $agentVersionId, environmentDefinitionVersionId: $environmentDefinitionVersionId, strategy: $strategy) { ${previewFields} } }`,
      { agentVersionId: versionId, environmentDefinitionVersionId: previewEnvironmentId, strategy: "REPLACE" });
    const currentPreview = await graphql(service,
      `query DeploymentPreview($agentVersionId: ID!, $environmentDefinitionVersionId: ID!, $strategy: DeploymentStrategy!) { deploymentPreview(agentVersionId: $agentVersionId, environmentDefinitionVersionId: $environmentDefinitionVersionId, strategy: $strategy) { ${previewFields} } }`,
      { agentVersionId: versionId, environmentDefinitionVersionId: previewEnvironmentId, strategy: "CANARY" });
    const deferredPreviews = [];
    let releaseFirstPreview;
    let releaseSecondPreview;
    const firstPreview = new Promise((resolve) => { releaseFirstPreview = resolve; });
    const secondPreview = new Promise((resolve) => { releaseSecondPreview = resolve; });
    await page.route("**/graphql", async (route) => {
      if (!route.request().postData()?.includes("query DeploymentPreview")) return route.fallback();
      deferredPreviews.push(route);
      if (deferredPreviews.length === 1) releaseFirstPreview();
      if (deferredPreviews.length === 2) releaseSecondPreview();
    });
    await page.getByRole("combobox").nth(1).selectOption("REPLACE");
    await firstPreview;
    await page.getByRole("combobox").nth(1).selectOption("CANARY");
    await secondPreview;
    const requestButton = page.getByRole("button", { name: "Request deployment" });
    await requestButton.waitFor({ state: "visible" });
    assert.equal(await requestButton.isDisabled(), true);
    await deferredPreviews[0].fulfill({ contentType: "application/json", body: JSON.stringify(stalePreview) });
    await page.waitForTimeout(100);
    assert.equal(await requestButton.isDisabled(), true);
    await deferredPreviews[1].fulfill({ contentType: "application/json", body: JSON.stringify(currentPreview) });
    await page.getByRole("heading", { name: "Frozen request preview" }).waitFor();
    assert.equal(await requestButton.isDisabled(), false);
    await page.unroute("**/graphql");
    const environmentIds = await page.getByRole("combobox").first().locator("option").evaluateAll((options) => options.map((option) => option.value));
    const alternateEnvironmentId = environmentIds.find((id) => id !== previewEnvironmentId);
    assert.ok(alternateEnvironmentId);
    const alternateEnvironmentPreview = await graphql(service,
      `query DeploymentPreview($agentVersionId: ID!, $environmentDefinitionVersionId: ID!, $strategy: DeploymentStrategy!) { deploymentPreview(agentVersionId: $agentVersionId, environmentDefinitionVersionId: $environmentDefinitionVersionId, strategy: $strategy) { ${previewFields} } }`,
      { agentVersionId: versionId, environmentDefinitionVersionId: alternateEnvironmentId, strategy: "REPLACE" });
    const deferredEnvironmentPreviews = [];
    let releaseFirstEnvironmentPreview;
    let releaseSecondEnvironmentPreview;
    const firstEnvironmentPreview = new Promise((resolve) => { releaseFirstEnvironmentPreview = resolve; });
    const secondEnvironmentPreview = new Promise((resolve) => { releaseSecondEnvironmentPreview = resolve; });
    await page.route("**/graphql", async (route) => {
      if (!route.request().postData()?.includes("query DeploymentPreview")) return route.fallback();
      deferredEnvironmentPreviews.push(route);
      if (deferredEnvironmentPreviews.length === 1) releaseFirstEnvironmentPreview();
      if (deferredEnvironmentPreviews.length === 2) releaseSecondEnvironmentPreview();
    });
    await page.getByRole("combobox").nth(1).selectOption("REPLACE");
    await firstEnvironmentPreview;
    await page.getByRole("combobox").first().selectOption(alternateEnvironmentId);
    await secondEnvironmentPreview;
    assert.equal(await requestButton.isDisabled(), true);
    await deferredEnvironmentPreviews[0].fulfill({ contentType: "application/json", body: JSON.stringify(stalePreview) });
    await page.waitForTimeout(100);
    assert.equal(await requestButton.isDisabled(), true);
    await deferredEnvironmentPreviews[1].fulfill({ contentType: "application/json", body: JSON.stringify(alternateEnvironmentPreview) });
    await page.getByRole("heading", { name: "Frozen request preview" }).waitFor();
    assert.equal(await requestButton.isDisabled(), false);
    await page.unroute("**/graphql");
    await page.getByRole("combobox").first().selectOption(previewEnvironmentId);
    await page.getByRole("combobox").nth(1).selectOption("CANARY");
    await page.getByRole("heading", { name: "Frozen request preview" }).waitFor();
    let releaseSubmission;
    const deferredSubmission = new Promise((resolve) => { releaseSubmission = resolve; });
    await page.route("**/graphql", async (route) => {
      if (!route.request().postData()?.includes("mutation DeployAgentVersion")) return route.fallback();
      releaseSubmission(route);
    });
    await requestButton.click();
    const delayedSubmission = await deferredSubmission;
    await page.goto(`${origin}/projects/${otherProject}/agents/${agentId}/versions/${versionId}/deploy`);
    await page.getByText("This immutable version is unavailable.").waitFor();
    await page.goto(`${origin}${requestPath}`);
    await page.getByRole("heading", { name: "Frozen request preview" }).waitFor();
    await delayedSubmission.fulfill({ contentType: "application/json", body: JSON.stringify({ data: { deployAgentVersion: { deployment: { id: "d1310000-0000-0000-0000-000000000098", projectId: project }, problems: [] } } }) });
    await page.waitForTimeout(100);
    assert.equal(page.url(), `${origin}${requestPath}`);
    await page.unroute("**/graphql");
    await requestButton.focus();
    assert.equal(await requestButton.evaluate((node) => document.activeElement === node), true);
    await page.keyboard.press("Enter");
    await page.getByRole("heading", { name: "M13 browser fixture · v1" }).waitFor();
    await page.getByText("This visible page polls the deployment projection while execution remains active.").waitFor();
    await page.reload();
    await page.getByText("This visible page polls the deployment projection while execution remains active.").waitFor();
    await worker.stop();
    worker = await startLocalDeploymentWorker(database.name, {
      HIVE_DEPLOYMENT_WORKER_INTERVAL_MILLIS: "250"
    });
    await page.getByText("Lifecycle: ACTIVE").waitFor({ timeout: 20_000 });
    await page.getByText("Frozen plan and policy facts").waitFor();
    await page.getByText("Target").waitFor();
    const activeDeploymentId = page.url().split("/").at(-1);
    await page.goto(`${origin}/projects/${otherProject}/deployments/${activeDeploymentId}`);
    await page.getByText("This deployment is unavailable.").waitFor();
    await page.getByRole("button", { name: "Cancel deployment" }).count().then((count) => assert.equal(count, 0));
    await page.goto(`${origin}/projects/${project}/deployments/${activeDeploymentId}`);
    await page.getByText("Lifecycle: ACTIVE").waitFor();
    const deferredProjectRoutes = [];
    let releaseProjectRoute;
    const projectRouteRequests = new Promise((resolve) => { releaseProjectRoute = resolve; });
    await page.route("**/graphql", async (route) => {
      if (!route.request().postData()?.includes("DeploymentPolling")) return route.fallback();
      deferredProjectRoutes.push(route);
      if (deferredProjectRoutes.length === 2) releaseProjectRoute();
    });
    await page.getByRole("button", { name: "Refresh deployment" }).click();
    await page.goto(`${origin}/projects/${otherProject}/deployments/${activeDeploymentId}`);
    await projectRouteRequests;
    await page.getByText("Loading deployment…").waitFor();
    await page.getByText("Lifecycle: ACTIVE").count().then((count) => assert.equal(count, 0));
    await page.getByRole("button", { name: "Cancel deployment" }).count().then((count) => assert.equal(count, 0));
    const staleProjectResponse = await graphql(service,
      "query StaleProjectRoute($id: ID!) { deploymentProjection(deploymentId: $id, first: 100) { deployment { id projectId agentId agentDisplayName agentVersionId agentVersionNumber strategy lifecycleStatus revision projectionRevision requestedBy requestedAt environmentDefinitionVersion { id stableDefinitionId version displayName logicalEnvironmentClass catalogReleaseId catalogReleaseDigest contentDigest } plan { agentVersionId agentContentDigest environmentDefinitionVersionId targetDigest planDigest packageDigest packageReference compilerVersion catalogReleaseId catalogReleaseDigest canonicalPlan review { activeAgentVersionNumber changeSummary addedDependencyVersions removedDependencyVersions } } policy { policyDigest policyRevision logicalEnvironmentClass risk bindingDigest requiredEvidence requiredApprovers evaluationRequirementExpiresAt evidence { kind digest bindingDigest expiresAt state } } currentAttempt { id number status generation startedAt completedAt failureCode failureSummary } runtimeHealth { status summary observedAt generation } rollbackTarget { agentVersionId agentVersionNumber runtimeHealth { status summary } } } timeline { edges { cursor node { id attemptId attemptNumber sequence stage status message source occurredAt } } pageInfo { hasNextPage endCursor } } } }", { id: activeDeploymentId });
    await deferredProjectRoutes[0].fulfill({ contentType: "application/json", body: JSON.stringify(staleProjectResponse) });
    await page.getByText("Loading deployment…").waitFor();
    await page.getByText("Lifecycle: ACTIVE").count().then((count) => assert.equal(count, 0));
    await deferredProjectRoutes[1].fulfill({ contentType: "application/json", body: JSON.stringify(staleProjectResponse) });
    await page.unroute("**/graphql");
    await page.reload();
    await page.getByText("This deployment is unavailable.").waitFor();
    await page.goto(`${origin}/projects/${project}/deployments/${activeDeploymentId}`);
    await page.getByText("Lifecycle: ACTIVE").waitFor();
    const pollingSnapshot = await graphql(service,
      "query PollingSnapshot($id: ID!) { deploymentProjection(deploymentId: $id, first: 100) { deployment { id projectId agentId agentDisplayName agentVersionId agentVersionNumber strategy lifecycleStatus revision projectionRevision requestedBy requestedAt environmentDefinitionVersion { id stableDefinitionId version displayName logicalEnvironmentClass catalogReleaseId catalogReleaseDigest contentDigest } plan { agentVersionId agentContentDigest environmentDefinitionVersionId targetDigest planDigest packageDigest packageReference compilerVersion catalogReleaseId catalogReleaseDigest canonicalPlan review { activeAgentVersionNumber changeSummary addedDependencyVersions removedDependencyVersions } } policy { policyDigest policyRevision logicalEnvironmentClass risk bindingDigest requiredEvidence requiredApprovers evaluationRequirementExpiresAt evidence { kind digest bindingDigest expiresAt state } } currentAttempt { id number status generation startedAt completedAt failureCode failureSummary } runtimeHealth { status summary observedAt generation } rollbackTarget { agentVersionId agentVersionNumber runtimeHealth { status summary } } } timeline { edges { cursor node { id attemptId attemptNumber sequence stage status message source occurredAt } } pageInfo { hasNextPage endCursor } } } }",
      { id: activeDeploymentId });
    const newerPollingSnapshot = structuredClone(pollingSnapshot);
    newerPollingSnapshot.data.deploymentProjection.deployment.projectionRevision += 1;
    newerPollingSnapshot.data.deploymentProjection.timeline.edges.push({ cursor: "deferred-watermark", node: {
      id: "d1310000-0000-0000-0000-000000000099", attemptId: null, attemptNumber: 0, sequence: 999,
      stage: "OUTBOX_LEASE_RECLAIMED", status: "SUCCEEDED", message: "Deferred polling watermark event.", source: "WORKER", occurredAt: new Date().toISOString()
    } });
    let pollingCalls = 0; let releaseOlderRequest;
    const olderRequest = new Promise((resolve) => { releaseOlderRequest = resolve; });
    await page.route("**/graphql", async (route) => {
      if (!route.request().postData()?.includes("DeploymentPolling")) return route.fallback();
      pollingCalls += 1;
      if (pollingCalls === 1) { releaseOlderRequest(route); return; }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(pollingSnapshot) });
    });
    await page.getByRole("button", { name: "Refresh deployment" }).click();
    const delayedRoute = await olderRequest;
    await page.getByRole("button", { name: "Refresh deployment" }).click();
    await delayedRoute.fulfill({ contentType: "application/json", body: JSON.stringify(newerPollingSnapshot) });
    await page.getByText("Deferred polling watermark event.").waitFor();
    await page.unroute("**/graphql");
    await page.getByRole("link", { name: "Back to deployments" }).click();
    await page.getByRole("heading", { name: "Deployments" }).waitFor();
    await page.getByRole("link", { name: "M13 browser fixture · v1" }).first().waitFor();
    const staleDeploymentList = await graphql(service,
      "query Deployments($projectId: ID!, $after: String) { deployments(projectId: $projectId, first: 50, after: $after) { edges { cursor node { id projectId agentDisplayName agentVersionNumber strategy lifecycleStatus requestedAt environmentDefinitionVersion { displayName } } } pageInfo { hasNextPage endCursor } } }",
      { projectId: project, after: null });
    let releaseListRoute;
    const deferredListRoute = new Promise((resolve) => { releaseListRoute = resolve; });
    await page.route("**/graphql", async (route) => {
      const body = route.request().postData();
      if (body?.includes("query Deployments") && body.includes(`\"projectId\":\"${project}\"`)) { releaseListRoute(route); return; }
      await route.fallback();
    });
    await page.goto(`${origin}/projects/${project}/deployments`);
    const delayedListRoute = await deferredListRoute;
    await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1", [otherProjectMembership]);
    await page.goto(`${origin}/projects/${otherProject}/deployments`);
    await page.getByText("Deployments are unavailable.").waitFor();
    await delayedListRoute.fulfill({ contentType: "application/json", body: JSON.stringify(staleDeploymentList) });
    await page.getByText("Deployments are unavailable.").waitFor();
    await page.getByRole("link", { name: "M13 browser fixture · v1" }).count().then((count) => assert.equal(count, 0));
    await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AGENT_DEVELOPER')", [otherProjectMembership]);
    await page.unroute("**/graphql");
    await page.goto(`${origin}/projects/${project}/deployments`);
    await page.getByRole("link", { name: "M13 browser fixture · v1" }).first().waitFor();

    await worker.stop();
    worker = undefined;
    const second = await graphql(service,
      "mutation Second($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }",
      { input: { agentVersionId: versionId, environmentDefinitionVersionId: pollingSnapshot.data.deploymentProjection.deployment.environmentDefinitionVersion.id, strategy: "REPLACE", idempotencyKey: "m13-route-watermark-" + Date.now().toString(36) } });
    assert.deepEqual(second.data.deployAgentVersion.problems, []);
    const secondDeploymentId = second.data.deployAgentVersion.deployment.id;
    await page.goto(`${origin}/projects/${project}/deployments/${activeDeploymentId}`);
    await page.getByText("Lifecycle: ACTIVE").waitFor();
    let delayedFirstDetail;
    const firstRoute = new Promise((resolve) => { delayedFirstDetail = resolve; });
    await page.route("**/graphql", async (route) => {
      if (!route.request().postData()?.includes("DeploymentPolling")) return route.fallback();
      const body = route.request().postData();
      if (body?.includes(activeDeploymentId)) { delayedFirstDetail(route); return; }
      await route.fallback();
    });
    await page.getByRole("button", { name: "Refresh deployment" }).click();
    const delayedA = await firstRoute;
    await page.goto(`${origin}/projects/${project}/deployments/${secondDeploymentId}`);
    await page.getByText("Lifecycle: REQUESTED").waitFor();
    const staleA = structuredClone(pollingSnapshot);
    staleA.data.deploymentProjection.deployment.projectionRevision += 100;
    await delayedA.fulfill({ contentType: "application/json", body: JSON.stringify(staleA) });
    await page.getByText("Lifecycle: REQUESTED").waitFor();
    assert.match(page.url(), new RegExp(`${secondDeploymentId}$`));
    await page.unroute("**/graphql");
    let releaseCancellation;
    const deferredCancellation = new Promise((resolve) => { releaseCancellation = resolve; });
    await page.route("**/graphql", async (route) => {
      if (!route.request().postData()?.includes("mutation CancelDeployment")) return route.fallback();
      releaseCancellation(route);
    });
    await page.getByRole("button", { name: "Cancel deployment" }).click();
    const delayedCancellation = await deferredCancellation;
    await page.goto(`${origin}/projects/${otherProject}/deployments/${secondDeploymentId}`);
    await page.getByText("This deployment is unavailable.").waitFor();
    await delayedCancellation.fulfill({ contentType: "application/json", body: JSON.stringify({ data: { cancelDeployment: { deployment: null, problems: [{ code: "DEPLOYMENT_CONFLICT", message: "Stale cancellation response." }] } } }) });
    await page.waitForTimeout(100);
    await page.getByText("This deployment is unavailable.").waitFor();
    await page.getByText("Stale cancellation response.").count().then((count) => assert.equal(count, 0));
    assert.match(page.url(), new RegExp(`/projects/${otherProject}/deployments/${secondDeploymentId}$`));
    await page.unroute("**/graphql");

    await page.goto(`${origin}${requestPath}`);
    await page.getByRole("heading", { name: "Frozen request preview" }).waitFor();
    await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1", [requesterProjectMembership]);
    await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AUDITOR')", [requesterProjectMembership]);
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await page.getByText("Your current project capabilities do not permit deployment requests.").waitFor();
    assert.equal(await requestButton.isDisabled(), true);
    await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1", [requesterProjectMembership]);
    await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AGENT_DEVELOPER')", [requesterProjectMembership]);
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await page.getByRole("heading", { name: "Frozen request preview" }).waitFor();
    await page.getByRole("combobox").first().selectOption({ label: "Local staging · local-staging@v1" });
    await page.getByRole("button", { name: "Request deployment" }).click();
    await page.getByText("Lifecycle: AWAITING_APPROVAL").waitFor();
    await page.getByRole("button", { name: "Cancel deployment" }).click();
    await page.getByText("Lifecycle: CANCELED").waitFor();

    const environments = await graphql(service, "query Environments($version: ID!) { deploymentEnvironmentDefinitionVersions(agentVersionId: $version, first: 50) { edges { node { id logicalEnvironmentClass } } } }", { version: versionId });
    const development = environments.data.deploymentEnvironmentDefinitionVersions.edges.map((edge) => edge.node).find((entry) => entry.logicalEnvironmentClass === "DEVELOPMENT");
    await worker?.stop();
    worker = undefined;
    const failure = await graphql(service, "mutation Failure($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }", { input: { agentVersionId: versionId, environmentDefinitionVersionId: development.id, strategy: "CANARY", idempotencyKey: "m13-browser-failure-" + Date.now().toString(36) } });
    assert.deepEqual(failure.data.deployAgentVersion.problems, []);
    await client.query("UPDATE deployment_outbox_events SET payload = '{\"mode\":\"FAILURE\"}'::jsonb WHERE deployment_id = $1 AND event_type = 'EXECUTE_DEPLOYMENT'", [failure.data.deployAgentVersion.deployment.id]);
    worker = await startLocalDeploymentWorker(database.name);
    await page.goto(`${origin}/projects/${project}/deployments/${failure.data.deployAgentVersion.deployment.id}`);
    await page.getByRole("heading", { name: "Failure investigation" }).waitFor({ timeout: 15_000 });
    const failureText = await page.locator("main").textContent();
    assert.doesNotMatch(failureText ?? "", /postgres|exception|stack trace|credential|secret/i);

    await page.route("**/graphql", async (route) => {
      if (route.request().postData()?.includes("DeploymentPolling")) {
        await route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ errors: [{ message: "internal diagnostic" }] }) });
      } else await route.fallback();
    });
    await page.getByRole("button", { name: "Refresh deployment" }).click();
    await page.getByRole("alert").filter({ hasText: "We could not refresh this deployment." }).waitFor();
    await page.unroute("**/graphql");
    const readyHealth = await fetch(`${origin}/health/deployment-worker`);
    assert.equal(readyHealth.status, 200);
    assert.equal((await readyHealth.json()).status, "READY");
    await worker.stop();
    await new Promise((resolve) => setTimeout(resolve, 800));
    const workerHealth = await fetch(`${origin}/health/deployment-worker`);
    assert.equal(workerHealth.status, 503);
    assert.equal((await workerHealth.json()).status, "STALE");
    worker = await startLocalDeploymentWorker(database.name);

    await page.route("**/graphql", async (route) => {
      if (route.request().postData()?.includes("query Deployments")) {
        await route.fulfill({ contentType: "application/json", body: JSON.stringify({ data: { deployments: { edges: [], pageInfo: { hasNextPage: false, endCursor: null } } } }) });
      } else await route.fallback();
    });
    await page.goto(`${origin}/projects/${project}/deployments`);
    await page.getByText("No deployments have been requested for this project.").waitFor();
    await page.unroute("**/graphql");
    await page.setViewportSize({ width: 390, height: 844 });
    await page.reload();
    await page.getByRole("heading", { name: "Deployments" }).waitFor();
    await page.getByRole("button", { name: "Refresh deployments" }).waitFor();

    const failedDeploymentId = failure.data.deployAgentVersion.deployment.id;
    const failedDetail = await graphql(service,
      "query FailedDetail($id: ID!) { deploymentProjection(deploymentId: $id, first: 100) { deployment { id revision lifecycleStatus currentAttempt { number status } } } }",
      { id: failedDeploymentId });
    const failedRevision = failedDetail.data.deploymentProjection.deployment.revision;
    assert.equal(failedDetail.data.deploymentProjection.deployment.lifecycleStatus, "FAILED");
    const laterActive = await graphql(service,
      "mutation LaterActive($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }",
      { input: { agentVersionId: versionId, environmentDefinitionVersionId: development.id, strategy: "REPLACE", idempotencyKey: "m15-later-active-" + Date.now().toString(36) } });
    assert.deepEqual(laterActive.data.deployAgentVersion.problems, []);
    await page.goto(`${origin}/projects/${project}/deployments/${laterActive.data.deployAgentVersion.deployment.id}`);
    await page.getByText("Lifecycle: ACTIVE").waitFor({ timeout: 20_000 });
    const laterActiveDetail = await graphql(service,
      "query LaterActiveDetail($id: ID!) { deploymentProjection(deploymentId: $id, first: 100) { deployment { revision } } }",
      { id: laterActive.data.deployAgentVersion.deployment.id });
    const concurrentAdmissions = await Promise.all([
      graphql(service,
        "mutation ConcurrentRollback($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { deployment { id } problems { code } } }",
        { input: { deploymentId: laterActive.data.deployAgentVersion.deployment.id, targetAgentVersionId: versionId, expectedRevision: laterActiveDetail.data.deploymentProjection.deployment.revision, reason: "Exercise concurrent local recovery admission.", productionConfirmation: "M15 local command binding", idempotencyKey: "m15-concurrent-rollback-" + Date.now().toString(36) } }),
      graphql(service,
        "mutation ConcurrentDeployment($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }",
        { input: { agentVersionId: versionId, environmentDefinitionVersionId: development.id, strategy: "CANARY", idempotencyKey: "m15-concurrent-deployment-" + Date.now().toString(36) } })
    ]);
    assert.deepEqual(concurrentAdmissions[0].data.rollbackDeployment.problems, []);
    assert.deepEqual(concurrentAdmissions[1].data.deployAgentVersion.problems, []);
    await client.query("UPDATE deployment_runtime_health SET status = 'UNHEALTHY', summary = 'The local acceptance test observed an unhealthy prior target.', observed_at = CURRENT_TIMESTAMP, generation = generation + 1 WHERE deployment_id = $1", [secondDeploymentId]);
    const latestPriorDeployment = await graphql(service,
      "query LatestPriorDeployment($id: ID!) { deploymentProjection(deploymentId: $id, first: 100) { deployment { lifecycleStatus runtimeHealth { status } } } }",
      { id: secondDeploymentId });
    assert.equal(latestPriorDeployment.data.deploymentProjection.deployment.lifecycleStatus, "ACTIVE");
    assert.equal(latestPriorDeployment.data.deploymentProjection.deployment.runtimeHealth.status, "UNHEALTHY");
    const orderedRollbackTarget = await graphql(service,
      "query OrderedRollbackTarget($id: ID!) { deploymentProjection(deploymentId: $id, first: 100) { deployment { rollbackTarget { deploymentId agentVersionId runtimeHealth { status } } } } }",
      { id: failedDeploymentId });
    assert.equal(orderedRollbackTarget.data.deploymentProjection.deployment.rollbackTarget.deploymentId, secondDeploymentId);
    assert.notEqual(orderedRollbackTarget.data.deploymentProjection.deployment.rollbackTarget.deploymentId, laterActive.data.deployAgentVersion.deployment.id);
    assert.equal(orderedRollbackTarget.data.deploymentProjection.deployment.rollbackTarget.runtimeHealth.status, "UNHEALTHY");
    const rollbackSource = await client.query("SELECT project_id, agent_id, environment_definition_version_id, requested_at FROM deployments WHERE id = $1", [failedDeploymentId]);
    await client.query("SET enable_seqscan = off");
    let rollbackTargetPlan;
    try {
      rollbackTargetPlan = await client.query("EXPLAIN (FORMAT JSON) SELECT candidate.id FROM deployments candidate JOIN deployment_runtime_health health ON health.deployment_id = candidate.id WHERE candidate.project_id = $1 AND candidate.agent_id = $2 AND candidate.environment_definition_version_id = $3 AND candidate.lifecycle_status = 'ACTIVE' AND candidate.id <> $4 AND (candidate.requested_at, candidate.id) < ($5, $4) ORDER BY candidate.requested_at DESC, candidate.id DESC LIMIT 1", [rollbackSource.rows[0].project_id, rollbackSource.rows[0].agent_id, rollbackSource.rows[0].environment_definition_version_id, failedDeploymentId, rollbackSource.rows[0].requested_at]);
    } finally {
      await client.query("RESET enable_seqscan");
    }
    assert.match(JSON.stringify(rollbackTargetPlan.rows[0]["QUERY PLAN"]), /deployments_active_rollback_predecessor/);
    const retryKey = "m15-browser-retry-" + Date.now().toString(36);
    const retry = await graphql(service,
      "mutation Retry($input: RetryDeploymentInput!) { retryDeployment(input: $input) { deployment { id lifecycleStatus projectId } problems { code } } }",
      { input: { deploymentId: failedDeploymentId, expectedRevision: failedRevision, idempotencyKey: retryKey } });
    assert.deepEqual(retry.data.retryDeployment.problems, []);
    assert.notEqual(retry.data.retryDeployment.deployment.id, failedDeploymentId);
    const retryReplay = await graphql(service,
      "mutation RetryReplay($input: RetryDeploymentInput!) { retryDeployment(input: $input) { deployment { id } problems { code } } }",
      { input: { deploymentId: failedDeploymentId, expectedRevision: failedRevision, idempotencyKey: retryKey } });
    assert.equal(retryReplay.data.retryDeployment.deployment.id, retry.data.retryDeployment.deployment.id);
    const retryConflict = await graphql(service,
      "mutation RetryConflict($input: RetryDeploymentInput!) { retryDeployment(input: $input) { deployment { id } problems { code } } }",
      { input: { deploymentId: failedDeploymentId, expectedRevision: failedRevision + 1, idempotencyKey: retryKey } });
    assert.equal(retryConflict.data.retryDeployment.problems[0].code, "IDEMPOTENCY_CONFLICT");
    let actorScopedRetry;
    for (let attempt = 0; attempt < 5; attempt += 1) {
      actorScopedRetry = await graphql(service,
        "mutation ActorScopedRetry($input: RetryDeploymentInput!) { retryDeployment(input: $input) { deployment { id } problems { code } } }",
        { input: { deploymentId: failedDeploymentId, expectedRevision: failedRevision, idempotencyKey: retryKey } }, recoveryActor);
      if (actorScopedRetry.data.retryDeployment.problems[0]?.code !== "REVISION_CONFLICT") break;
    }
    assert.deepEqual(actorScopedRetry.data.retryDeployment.problems, []);
    assert.notEqual(actorScopedRetry.data.retryDeployment.deployment.id, retry.data.retryDeployment.deployment.id);
    const retrySourceFacts = await client.query("SELECT action FROM deployment_audit_events WHERE deployment_id = $1 ORDER BY occurred_at ASC", [failedDeploymentId]);
    assert(retrySourceFacts.rows.some((row) => row.action === "RETRY_RECORDED"));
    const failedAttempts = await client.query("SELECT attempt_number, status FROM deployment_attempts WHERE deployment_id = $1 ORDER BY attempt_number ASC", [failedDeploymentId]);
    assert.deepEqual(failedAttempts.rows.map((row) => row.status), ["FAILED"]);

    const activeDetail = await graphql(service,
      "query ActiveDetail($id: ID!) { deploymentProjection(deploymentId: $id, first: 100) { deployment { id revision lifecycleStatus runtimeHealth { status } } } }",
      { id: activeDeploymentId });
    const activeRevision = activeDetail.data.deploymentProjection.deployment.revision;
    assert.equal(activeDetail.data.deploymentProjection.deployment.runtimeHealth.status, "HEALTHY");
    const promotionKey = "m15-browser-promotion-" + Date.now().toString(36);
    const promotion = await graphql(service,
      "mutation Promote($input: PromoteDeploymentInput!) { promoteDeployment(input: $input) { deployment { id } problems { code } } }",
      { input: { deploymentId: activeDeploymentId, expectedRevision: activeRevision, idempotencyKey: promotionKey } });
    assert.deepEqual(promotion.data.promoteDeployment.problems, []);
    const promotionReplay = await graphql(service,
      "mutation PromoteReplay($input: PromoteDeploymentInput!) { promoteDeployment(input: $input) { deployment { id } problems { code } } }",
      { input: { deploymentId: activeDeploymentId, expectedRevision: activeRevision, idempotencyKey: promotionKey } });
    assert.equal(promotionReplay.data.promoteDeployment.deployment.id, activeDeploymentId);
    assert(recoveryLogs(service).some((line) => line.includes("action=PROMOTE outcome=ACCEPTED problemCode=NONE failureClass=NONE")
      && line.includes(`sourceDeploymentId=${activeDeploymentId}`) && line.includes(`resultDeploymentId=${activeDeploymentId}`)));
    assert(recoveryLogs(service).some((line) => line.includes("action=PROMOTE outcome=REPLAYED problemCode=NONE failureClass=NONE")
      && line.includes(`sourceDeploymentId=${activeDeploymentId}`) && line.includes(`resultDeploymentId=${activeDeploymentId}`)));
    const injectedDeploymentId = "invalid-recovery-log\nforged-recovery-field=" + "x".repeat(1_024);
    const invalidRecoveryInputs = [
      ["Retry", "retryDeployment", "RetryDeploymentInput!", { deploymentId: injectedDeploymentId, expectedRevision: failedRevision, idempotencyKey: "m15-invalid-retry-telemetry" }],
      ["Promote", "promoteDeployment", "PromoteDeploymentInput!", { deploymentId: injectedDeploymentId, expectedRevision: activeRevision, idempotencyKey: "m15-invalid-promote-telemetry" }],
      ["Rollback", "rollbackDeployment", "RollbackDeploymentInput!", { deploymentId: injectedDeploymentId, expectedRevision: failedRevision, targetAgentVersionId: orderedRollbackTarget.data.deploymentProjection.deployment.rollbackTarget.agentVersionId, reason: "Restore the observed healthy target.", productionConfirmation: "M15 local command binding", idempotencyKey: "m15-invalid-rollback-telemetry" }]
    ];
    for (const [action, field, inputType, input] of invalidRecoveryInputs) {
      const refused = await graphql(service,
        `mutation Invalid${action}($input: ${inputType}) { ${field}(input: $input) { problems { code } } }`, { input });
      assert.equal(refused.data[field].problems[0].code, "NOT_FOUND");
      assert(recoveryLogs(service).some((line) => line.includes(`action=${action.toUpperCase()} outcome=REFUSED problemCode=NOT_FOUND failureClass=NONE`)
        && line.includes("sourceDeploymentId=INVALID_ID") && line.includes("resultDeploymentId=NONE")));
    }
    const invalidRetryLogCount = recoveryLogs(service).filter((line) => line.includes("action=RETRY") && line.includes("sourceDeploymentId=INVALID_ID")).length;
    for (const suffix of ["a", "b", "c"]) {
      const refused = await graphql(service,
        "mutation RepeatedInvalidRetry($input: RetryDeploymentInput!) { retryDeployment(input: $input) { problems { code } } }",
        { input: { deploymentId: injectedDeploymentId + suffix, expectedRevision: failedRevision, idempotencyKey: "m15-invalid-retry-telemetry-" + suffix } });
      assert.equal(refused.data.retryDeployment.problems[0].code, "NOT_FOUND");
    }
    assert.equal(recoveryLogs(service).filter((line) => line.includes("action=RETRY") && line.includes("sourceDeploymentId=INVALID_ID")).length, invalidRetryLogCount);
    assert.doesNotMatch(service.output(), /forged-recovery-field/);
    const promotionFacts = await client.query("SELECT action_receipt_id FROM deployment_promotion_facts WHERE deployment_id = $1", [activeDeploymentId]);
    assert.equal(promotionFacts.rowCount, 1);

    await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1", [requesterProjectMembership]);
    await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AUDITOR')", [requesterProjectMembership]);
    const receiptsBeforeUnauthorizedRecovery = await client.query("SELECT id FROM deployment_recovery_action_receipts WHERE source_deployment_id = $1 AND actor_principal_id = $2", [failedDeploymentId, requester]);
    const deploymentsBeforeUnauthorizedRequest = await client.query("SELECT id FROM deployments WHERE project_id = $1", [project]);
    // No pg_advisory_lock simulation here any more: quotaAnchor() no longer takes one (Aurora DSQL
    // rejects it outright; see deployment_project_quota_claims' migration comment for its replacement),
    // so there is nothing left for holding one to block. The assertion that matters -- an unauthorized
    // actor is refused before reaching any project-scoped write, regardless of what else is contending
    // for that project -- still holds without the wrapper.
    const unauthorizedQuotaActions = await Promise.all([
      graphql(service, "mutation UnauthorizedLockedRetry($input: RetryDeploymentInput!) { retryDeployment(input: $input) { problems { code } } }",
        { input: { deploymentId: failedDeploymentId, expectedRevision: failedRevision, idempotencyKey: "m15-browser-locked-denied-retry" } }),
      graphql(service, "mutation UnauthorizedLockedRollback($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { problems { code } } }",
        { input: { deploymentId: failedDeploymentId, targetAgentVersionId: orderedRollbackTarget.data.deploymentProjection.deployment.rollbackTarget.agentVersionId, expectedRevision: failedRevision, reason: "Restore the observed healthy target.", productionConfirmation: "M15 local command binding", idempotencyKey: "m15-browser-locked-denied-rollback" } }),
      graphql(service, "mutation UnauthorizedLockedRequest($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { problems { code } } }",
        { input: { agentVersionId: versionId, environmentDefinitionVersionId: development.id, strategy: "CANARY", idempotencyKey: "m15-browser-locked-denied-request" } })
    ]);
    assert.equal(unauthorizedQuotaActions[0].data.retryDeployment.problems[0].code, "FORBIDDEN");
    assert.equal(unauthorizedQuotaActions[1].data.rollbackDeployment.problems[0].code, "FORBIDDEN");
    assert.equal(unauthorizedQuotaActions[2].data.deployAgentVersion.problems[0].code, "FORBIDDEN");
    const receiptsAfterUnauthorizedRecovery = await client.query("SELECT id FROM deployment_recovery_action_receipts WHERE source_deployment_id = $1 AND actor_principal_id = $2", [failedDeploymentId, requester]);
    assert.equal(receiptsAfterUnauthorizedRecovery.rowCount, receiptsBeforeUnauthorizedRecovery.rowCount);
    const deploymentsAfterUnauthorizedRequest = await client.query("SELECT id FROM deployments WHERE project_id = $1", [project]);
    assert.equal(deploymentsAfterUnauthorizedRequest.rowCount, deploymentsBeforeUnauthorizedRequest.rowCount);
    const promotionFactsBeforeUnauthorizedPromotion = await client.query("SELECT action_receipt_id FROM deployment_promotion_facts WHERE deployment_id = $1", [activeDeploymentId]);
    // No pg_advisory_lock simulation here either: approvalTransitionAnchor() is removed outright (Aurora
    // DSQL rejects pg_advisory_xact_lock; every one of its former call sites already takes a real
    // FOR UPDATE lock on the same deployments row instead), so there is nothing left to hold.
    const unauthorizedPromotion = await graphql(service,
      "mutation UnauthorizedLockedPromotion($input: PromoteDeploymentInput!) { promoteDeployment(input: $input) { problems { code } } }",
      { input: { deploymentId: activeDeploymentId, expectedRevision: activeRevision, idempotencyKey: "m15-browser-locked-denied-promotion" } });
    assert.equal(unauthorizedPromotion.data.promoteDeployment.problems[0].code, "FORBIDDEN");
    const promotionFactsAfterUnauthorizedPromotion = await client.query("SELECT action_receipt_id FROM deployment_promotion_facts WHERE deployment_id = $1", [activeDeploymentId]);
    assert.equal(promotionFactsAfterUnauthorizedPromotion.rowCount, promotionFactsBeforeUnauthorizedPromotion.rowCount);
    const unauthorizedRetry = await graphql(service,
      "mutation UnauthorizedRetry($input: RetryDeploymentInput!) { retryDeployment(input: $input) { problems { code } } }",
      { input: { deploymentId: failedDeploymentId, expectedRevision: failedRevision, idempotencyKey: "m15-browser-denied" } });
    assert.equal(unauthorizedRetry.data.retryDeployment.problems[0].code, "FORBIDDEN");
    await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1", [requesterProjectMembership]);
    await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AGENT_DEVELOPER')", [requesterProjectMembership]);

    const production = environments.data.deploymentEnvironmentDefinitionVersions.edges.map((edge) => edge.node).find((entry) => entry.logicalEnvironmentClass === "PRODUCTION");
    assert.ok(production);
    const productionSource = await graphql(service,
      "mutation ProductionSource($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id revision } problems { code } } }",
      { input: { agentVersionId: versionId, environmentDefinitionVersionId: production.id, strategy: "REPLACE", idempotencyKey: "m15-production-source-" + Date.now().toString(36) } });
    assert.deepEqual(productionSource.data.deployAgentVersion.problems, []);
    const confirmationRequired = await graphql(service,
      "mutation RollbackRequired($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { problems { code } } }",
      { input: { deploymentId: productionSource.data.deployAgentVersion.deployment.id, expectedRevision: productionSource.data.deployAgentVersion.deployment.revision, reason: "Restore the observed healthy target.", idempotencyKey: "m15-browser-confirmation-required" } });
    assert.equal(confirmationRequired.data.rollbackDeployment.problems[0].code, "CONFIRMATION_REQUIRED");
    const confirmationMismatch = await graphql(service,
      "mutation RollbackMismatch($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { problems { code } } }",
      { input: { deploymentId: productionSource.data.deployAgentVersion.deployment.id, expectedRevision: productionSource.data.deployAgentVersion.deployment.revision, reason: "Restore the observed healthy target.", productionConfirmation: "local-production-mismatch", idempotencyKey: "m15-browser-confirmation-mismatch" } });
    assert.equal(confirmationMismatch.data.rollbackDeployment.problems[0].code, "CONFIRMATION_MISMATCH");
    const confirmationAcceptedBeforeLifecycle = await graphql(service,
      "mutation RollbackProductionLifecycle($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { problems { code } } }",
      { input: { deploymentId: productionSource.data.deployAgentVersion.deployment.id, expectedRevision: productionSource.data.deployAgentVersion.deployment.revision, reason: "Restore the observed healthy target.", productionConfirmation: "local-production", idempotencyKey: "m15-browser-confirmation-accepted" } });
    assert.equal(confirmationAcceptedBeforeLifecycle.data.rollbackDeployment.problems[0].code, "LIFECYCLE_CONFLICT");
    const rollbackKey = "m15-browser-rollback-" + Date.now().toString(36);
    let rollback; let rollbackRevision;
    for (let attempt = 0; attempt < 5; attempt += 1) {
      const rollbackSourceDetail = await graphql(service,
        "query RollbackSourceDetail($id: ID!) { deploymentProjection(deploymentId: $id, first: 100) { deployment { revision } } }",
        { id: failedDeploymentId });
      rollbackRevision = rollbackSourceDetail.data.deploymentProjection.deployment.revision;
      rollback = await graphql(service,
        "mutation Rollback($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { deployment { id } problems { code } } }",
        { input: { deploymentId: failedDeploymentId, targetAgentVersionId: orderedRollbackTarget.data.deploymentProjection.deployment.rollbackTarget.agentVersionId.toUpperCase(), expectedRevision: rollbackRevision, reason: "Restore the observed healthy target.", productionConfirmation: recoveryConfirmationSentinel, idempotencyKey: rollbackKey } });
      if (rollback.data.rollbackDeployment.problems[0]?.code !== "REVISION_CONFLICT") break;
    }
    assert.deepEqual(rollback.data.rollbackDeployment.problems, []);
    const rollbackReplay = await graphql(service,
      "mutation RollbackReplay($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { deployment { id } problems { code } } }",
      { input: { deploymentId: failedDeploymentId, targetAgentVersionId: orderedRollbackTarget.data.deploymentProjection.deployment.rollbackTarget.agentVersionId.toLowerCase(), expectedRevision: rollbackRevision, reason: "Restore the observed healthy target.", productionConfirmation: recoveryConfirmationSentinel, idempotencyKey: rollbackKey } });
    assert.equal(rollbackReplay.data.rollbackDeployment.deployment.id, rollback.data.rollbackDeployment.deployment.id);
    const rollbackConflict = await graphql(service,
      "mutation RollbackConflict($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { problems { code } } }",
      { input: { deploymentId: failedDeploymentId, targetAgentVersionId: orderedRollbackTarget.data.deploymentProjection.deployment.rollbackTarget.agentVersionId.toLowerCase(), expectedRevision: rollbackRevision, reason: "Restore the observed healthy target.", productionConfirmation: "Changed local command binding", idempotencyKey: rollbackKey } });
    assert.equal(rollbackConflict.data.rollbackDeployment.problems[0].code, "IDEMPOTENCY_CONFLICT");
    const malformedTarget = await graphql(service,
      "mutation RollbackMalformedTarget($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { problems { code } } }",
      { input: { deploymentId: failedDeploymentId, targetAgentVersionId: "not-an-immutable-version-id", expectedRevision: failedRevision, reason: "Restore the observed healthy target.", productionConfirmation: "M15 local command binding", idempotencyKey: "m15-malformed-target-" + Date.now().toString(36) } });
    assert.equal(malformedTarget.data.rollbackDeployment.problems[0].code, "INVALID_INPUT");
    const rollbackReceipts = await client.query("SELECT result_deployment_id FROM deployment_recovery_action_receipts WHERE source_deployment_id = $1 AND actor_principal_id = $2 AND action = 'ROLLBACK'", [failedDeploymentId, requester]);
    assert.equal(rollbackReceipts.rowCount, 1);
    assert.equal(rollbackReceipts.rows[0].result_deployment_id, rollback.data.rollbackDeployment.deployment.id);

    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`${origin}/projects/${project}/deployments/${activeDeploymentId}`);
    await page.getByRole("button", { name: "Promote deployment" }).waitFor();
    await page.getByRole("button", { name: "Promote deployment" }).click();
    await page.getByRole("dialog", { name: "Promote healthy deployment" }).waitFor();
    await page.getByText("Alias: local:local-development.").waitFor();
    await page.getByText(/Impact: the service records this healthy immutable target locally/).waitFor();
    // Enter confirms only when the dialog already owns focus; on the opener it would reopen the dialog.
    assert.equal(await page.evaluate(() => document.activeElement?.closest("[role=dialog]") !== null && document.activeElement !== document.body), true);
    await page.keyboard.press("Enter");
    await page.getByRole("dialog", { name: "Promote healthy deployment" }).waitFor({ state: "detached" });
    await page.waitForFunction(() => document.activeElement?.textContent === "Promote deployment");
    await page.goto(`${origin}/projects/${project}/deployments/${failedDeploymentId}`);
    await page.getByRole("heading", { name: "Failure investigation" }).waitFor();
    await page.getByRole("button", { name: "Review rollback" }).click();
    await page.getByRole("dialog", { name: "Review rollback" }).waitFor();
    await page.getByText("Current health: UNHEALTHY.").waitFor();
    await page.getByText("Target health: UNHEALTHY.").waitFor();
    await page.getByText(/Evaluation context:/).waitFor();
    const rollbackAction = page.getByRole("button", { name: "Rollback deployment" });
    assert.equal(await rollbackAction.isDisabled(), true);
    await page.getByLabel("Rollback reason").fill("Restore the observed healthy target.");
    await page.getByLabel("Production environment confirmation").count().then((count) => assert.equal(count, 0));
    assert.equal(await rollbackAction.isDisabled(), false);
    await rollbackAction.focus();
    await page.keyboard.press("Enter");
    await page.waitForURL((url) => url.pathname.startsWith(`/projects/${project}/deployments/`)
      && url.pathname.split("/").at(-1) !== failedDeploymentId);
    assert.notEqual(page.url().split("/").at(-1), failedDeploymentId);
    const rollbackFacts = await client.query("SELECT action FROM deployment_audit_events WHERE deployment_id = $1", [failedDeploymentId]);
    assert(rollbackFacts.rows.some((row) => row.action === "ROLLBACK_RECORDED"));
    const recoveryFactsBeforeArchive = await client.query("SELECT id FROM deployment_recovery_action_receipts WHERE source_deployment_id = $1", [failedDeploymentId]);
    const productionRecoveryFactsBeforeArchive = await client.query("SELECT id FROM deployment_recovery_action_receipts WHERE source_deployment_id = $1", [productionSource.data.deployAgentVersion.deployment.id]);
    await client.query("UPDATE projects SET lifecycle_status = 'ARCHIVED', revision = revision + 1 WHERE id = $1", [project]);
    const archivedRetry = await graphql(service,
      "mutation ArchivedRetry($input: RetryDeploymentInput!) { retryDeployment(input: $input) { problems { code } } }",
      { input: { deploymentId: failedDeploymentId, expectedRevision: failedRevision, idempotencyKey: "m15-archived-retry-" + Date.now().toString(36) } });
    assert.equal(archivedRetry.data.retryDeployment.problems[0].code, "LIFECYCLE_CONFLICT");
    const archivedRollback = await graphql(service,
      "mutation ArchivedRollback($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { problems { code } } }",
      { input: { deploymentId: failedDeploymentId, targetAgentVersionId: orderedRollbackTarget.data.deploymentProjection.deployment.rollbackTarget.agentVersionId, expectedRevision: failedRevision, reason: "Restore the observed healthy target.", productionConfirmation: "M15 local command binding", idempotencyKey: "m15-archived-rollback-" + Date.now().toString(36) } });
    assert.equal(archivedRollback.data.rollbackDeployment.problems[0].code, "LIFECYCLE_CONFLICT");
    const archivedBlankRollback = await graphql(service,
      "mutation ArchivedBlankRollback($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { problems { code } } }",
      { input: { deploymentId: failedDeploymentId, targetAgentVersionId: orderedRollbackTarget.data.deploymentProjection.deployment.rollbackTarget.agentVersionId, expectedRevision: failedRevision, reason: " ", productionConfirmation: "M15 local command binding", idempotencyKey: "m15-archived-blank-rollback-" + Date.now().toString(36) } });
    assert.equal(archivedBlankRollback.data.rollbackDeployment.problems[0].code, "REASON_REQUIRED");
    const archivedProductionConfirmation = await graphql(service,
      "mutation ArchivedProductionConfirmation($input: RollbackDeploymentInput!) { rollbackDeployment(input: $input) { problems { code } } }",
      { input: { deploymentId: productionSource.data.deployAgentVersion.deployment.id, expectedRevision: productionSource.data.deployAgentVersion.deployment.revision, reason: "Restore the observed healthy target.", idempotencyKey: "m15-archived-production-confirmation-" + Date.now().toString(36) } });
    assert.equal(archivedProductionConfirmation.data.rollbackDeployment.problems[0].code, "CONFIRMATION_REQUIRED");
    const recoveryFactsAfterArchive = await client.query("SELECT id FROM deployment_recovery_action_receipts WHERE source_deployment_id = $1", [failedDeploymentId]);
    assert.equal(recoveryFactsAfterArchive.rowCount, recoveryFactsBeforeArchive.rowCount);
    const productionRecoveryFactsAfterArchive = await client.query("SELECT id FROM deployment_recovery_action_receipts WHERE source_deployment_id = $1", [productionSource.data.deployAgentVersion.deployment.id]);
    assert.equal(productionRecoveryFactsAfterArchive.rowCount, productionRecoveryFactsBeforeArchive.rowCount);
    await client.query("UPDATE projects SET lifecycle_status = 'ACTIVE', revision = revision + 1 WHERE id = $1", [project]);
    await worker.stop();
    worker = undefined;
    let quotaFillerIndex = 0;
    while (true) {
      const pendingRecoveryQuota = await client.query("SELECT count(*)::int AS count FROM deployments WHERE project_id = $1 AND requested_by = $2 AND lifecycle_status IN ('REQUESTED', 'AWAITING_APPROVAL', 'APPROVED', 'IN_PROGRESS')", [project, recoveryActor]);
      assert(pendingRecoveryQuota.rows[0].count <= 19);
      if (pendingRecoveryQuota.rows[0].count === 19) break;
      const filler = await graphql(service,
        "mutation RecoveryQuotaFiller($input: DeployAgentVersionInput!) { deployAgentVersion(input: $input) { deployment { id } problems { code } } }",
        { input: { agentVersionId: versionId, environmentDefinitionVersionId: development.id, strategy: "CANARY", idempotencyKey: "m15-recovery-quota-filler-" + quotaFillerIndex + "-" + Date.now().toString(36) } }, recoveryActor);
      assert.deepEqual(filler.data.deployAgentVersion.problems, []);
      quotaFillerIndex += 1;
      assert(quotaFillerIndex <= 20, "the isolated recovery actor must reach the pending quota within its bounded fixture page");
    }
    const filledRecoveryQuota = await client.query("SELECT count(*)::int AS count FROM deployments WHERE project_id = $1 AND requested_by = $2 AND lifecycle_status IN ('REQUESTED', 'AWAITING_APPROVAL', 'APPROVED', 'IN_PROGRESS')", [project, recoveryActor]);
    assert.equal(filledRecoveryQuota.rows[0].count, 19);
    const recoveryQuotaResults = await Promise.all(["a", "b"].map((suffix) => graphql(service,
      "mutation RecoveryQuota($input: RetryDeploymentInput!) { retryDeployment(input: $input) { deployment { id } problems { code } } }",
      { input: { deploymentId: failedDeploymentId, expectedRevision: failedRevision, idempotencyKey: "m15-recovery-quota-" + suffix + "-" + Date.now().toString(36) } }, recoveryActor)));
    const recoveryQuotaOutcomes = recoveryQuotaResults.map((result) => result.data.retryDeployment);
    const quotaAfterAdmissions = await client.query("SELECT count(*)::int AS count FROM deployments WHERE project_id = $1 AND requested_by = $2 AND lifecycle_status IN ('REQUESTED', 'AWAITING_APPROVAL', 'APPROVED', 'IN_PROGRESS')", [project, recoveryActor]);
    assert.equal(recoveryQuotaOutcomes.filter((outcome) => outcome.deployment !== null).length, 1,
      "concurrent recovery admissions must stop at the pending quota; pending after admissions: " + quotaAfterAdmissions.rows[0].count);
    assert.equal(recoveryQuotaOutcomes.filter((outcome) => outcome.problems[0]?.code === "RATE_LIMITED").length, 1);
    assert.doesNotMatch(service.output(), /Restore the observed healthy target\./);
    assert.equal(service.output().includes(recoveryConfirmationSentinel), false);
    await client.end();
    client = undefined;
    await database.drop();
    const unavailableInputs = [
      ["Retry", "retryDeployment", "RetryDeploymentInput!", { deploymentId: failedDeploymentId, expectedRevision: failedRevision, idempotencyKey: "m15-unavailable-retry" }],
      ["Promote", "promoteDeployment", "PromoteDeploymentInput!", { deploymentId: activeDeploymentId, expectedRevision: activeRevision, idempotencyKey: "m15-unavailable-promote" }],
      ["Rollback", "rollbackDeployment", "RollbackDeploymentInput!", { deploymentId: failedDeploymentId, expectedRevision: failedRevision, targetAgentVersionId: orderedRollbackTarget.data.deploymentProjection.deployment.rollbackTarget.agentVersionId, reason: "Restore the observed healthy target.", productionConfirmation: "M15 local command binding", idempotencyKey: "m15-unavailable-rollback" }]
    ];
    for (const [action, field, inputType, input] of unavailableInputs) {
      const response = await graphqlResponse(service,
        `mutation Unavailable${action}($input: ${inputType}) { ${field}(input: $input) { problems { code } } }`, { input });
      assert.equal(response.status, 503);
      assert.ok((await response.json()).errors);
      assert(recoveryLogs(service).some((line) => line.includes(`action=${action.toUpperCase()} outcome=UNAVAILABLE problemCode=NONE failureClass=DEPENDENCY_UNAVAILABLE`)
        && line.includes("resultDeploymentId=NONE")));
    }
    await context.close();
  } finally {
    await browser.close();
  }
} finally {
  try {
    if (client) {
      await client.query("DELETE FROM console_role_assignments WHERE id = $1", [role]);
      await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1", [requesterProjectMembership]);
      await client.query("DELETE FROM project_memberships WHERE id = $1", [requesterProjectMembership]);
      await client.query("DELETE FROM project_membership_roles WHERE membership_id = $1", [otherProjectMembership]);
      await client.query("DELETE FROM project_memberships WHERE id = $1", [otherProjectMembership]);
      await client.query("DELETE FROM organization_membership_roles WHERE membership_id = $1", [membership]);
      await client.query("DELETE FROM organization_memberships WHERE id = $1", [membership]);
    }
  } finally {
    if (client) await client.end();
    if (worker) await worker.stop();
    if (service) await service.stop();
    await database.drop();
  }
}
