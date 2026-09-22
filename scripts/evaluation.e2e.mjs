import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startIsolatedLocalService, startLocalEvaluationWorker } from "./local-service.mjs";

const administrator = "00000000-0000-0000-0000-000000000001";
const project = "50000000-0000-0000-0000-000000000001";
const database = await createIsolatedDatabase("hive_m16_e2e");
let service;
let worker;
let client;

async function graphql(service, query, variables) {
  const response = await fetch(`http://127.0.0.1:${service.port}/graphql`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(administrator)}` },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.errors, undefined, JSON.stringify(body.errors));
  return body.data;
}

async function publishAgentFixture(service) {
  const slug = `m16-browser-agent-${Date.now().toString(36)}`;
  const created = await graphql(service,
    "mutation Create($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId revision } problems { code } } }",
    { input: { projectId: project, displayName: "M16 browser evaluation target", slug } });
  assert.deepEqual(created.createAgentDraft.problems, []);
  const agentId = created.createAgentDraft.agentDraft.agentId;
  const document = {
    general: { displayName: "M16 browser evaluation target", description: "Local browser fixture target." },
    instructions: { source: "# local browser fixture", language: "markdown" }, harness: { source: "def run(value): return value", language: "python" },
    model: { reference: "model:local-safe-chat@v2" }, tools: { source: "{}", language: "json" }, skills: { source: "export const skills = [];", language: "javascript" },
    capabilities: { source: "export type Capability = string;", language: "typescript" }, subagents: { enabled: false }, memory: { strategy: "project" },
    guardrails: { source: "# local guardrail", language: "shell" }, identity: { persona: "" }, observability: { source: "<observability/>", language: "xml" },
    limits: { maxTokens: 256 }, evaluations: { required: false }, dependencies: ["model:local-safe-chat@v2"]
  };
  const saved = await graphql(service,
    "mutation Save($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: created.createAgentDraft.agentDraft.revision, document } });
  assert.deepEqual(saved.updateAgentDraft.problems, []);
  const validated = await graphql(service,
    "mutation Validate($input: ValidateAgentDraftInput!) { validateAgentDraft(input: $input) { agentDraft { revision validationStatus } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: saved.updateAgentDraft.agentDraft.revision } });
  assert.equal(validated.validateAgentDraft.agentDraft.validationStatus, "VALID");
  const published = await graphql(service,
    "mutation Publish($input: PublishAgentDraftInput!) { publishAgentDraft(input: $input) { agentVersion { id } problems { code } } }",
    { input: { projectId: project, agentId, expectedRevision: validated.validateAgentDraft.agentDraft.revision, warningsAcknowledged: true } });
  assert.deepEqual(published.publishAgentDraft.problems, []);
}

try {
  service = await startIsolatedLocalService(database.name);
  client = await postgresClient(database.name);
  await client.query("INSERT INTO platform_role_assignments (principal_id, role_code) VALUES ($1, 'PLATFORM_ADMIN') ON CONFLICT DO NOTHING", [administrator]);
  await publishAgentFixture(service);
  worker = await startLocalEvaluationWorker(database.name);
  const origin = `http://127.0.0.1:${service.port}`;
  const browser = await launchBrowser();
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await context.addCookies([{ name: "sf_session", value: service.signFixtureSession(administrator), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const rejectedOrigins = new Set();
    await context.route("**/*", async (route) => {
      if (new URL(route.request().url()).origin === origin) return route.continue();
      rejectedOrigins.add(new URL(route.request().url()).origin);
      return route.abort();
    });
    const page = await context.newPage();
    await page.goto(`${origin}/projects/${project}/evaluations`);
    await page.getByRole("heading", { name: "Evaluations" }).waitFor();
    await page.getByRole("textbox", { name: "Evaluation definition slug" }).fill(`m16-browser-${Date.now().toString(36)}`);
    await page.getByRole("button", { name: "Create definition" }).click();
    await page.getByRole("link", { name: "Review draft before publication" }).click();
    await page.getByRole("button", { name: "Confirm publication" }).click();
    await page.getByRole("heading", { name: "Immutable evaluation versions" }).waitFor();
    await page.getByRole("link", { name: "Back to evaluation definition" }).click();
    await page.getByRole("button", { name: "Queue evaluation" }).click();
    await page.getByRole("heading", { name: "Evaluation run" }).waitFor();
    await page.getByRole("button", { name: "Refresh run" }).click();
    await page.getByRole("heading", { name: "Case projection" }).waitFor();
    assert.deepEqual([...rejectedOrigins], [], "The local MVP journey must not request an external origin.");
    await context.close();
  } finally {
    await browser.close();
  }
} finally {
  if (worker) await worker.stop();
  if (client) await client.end();
  if (service) await service.stop();
  await database.drop();
}
