import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const project = "50000000-0000-0000-0000-000000000003";
const authorMembership = "a1200000-0000-0000-0000-000000000004";
const authorRole = "a1200000-0000-0000-0000-000000000005";
const port = 18113;
const origin = "http://127.0.0.1:" + port;
const agentName = "Browser Agent " + Date.now().toString(36);
const database = await createIsolatedDatabase("hive_agent_authoring_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  // A browser publication is an immutable fact. Place it in the private fixture
  // project and remove the temporary test authority afterwards so it cannot alter
  // the shared directory assertions run elsewhere in the gate.
  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL) ON CONFLICT (id) DO NOTHING",
    [authorMembership, privateOrganization, ada]
  );
  await client.query(
    "INSERT INTO console_role_assignments (id, principal_id, organization_id, project_id, role_code) VALUES ($1, $2, NULL, $3, 'AGENT_DEVELOPER') ON CONFLICT (id) DO NOTHING",
    [authorRole, ada, project]
  );
  const browser = await launchBrowser();
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await context.addCookies([{ name: "sf_session", value: service.signFixtureSession(ada), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const page = await context.newPage();
    await page.goto(origin + "/projects/" + project + "/agents/new");
    await page.getByRole("heading", { name: "Create agent" }).waitFor();
    await page.getByRole("textbox", { name: "Display name" }).fill(agentName);
    await page.getByRole("button", { name: "Create agent draft" }).click();
    await page.getByRole("heading", { name: agentName }).waitFor();
    const path = new URL(page.url()).pathname;
    const agent = path.split("/")[4];
    for (const [section, mode] of [["Instructions", "markdown"], ["Harness", "python"], ["Tools", "json"], ["Skills", "javascript"], ["Capabilities", "typescript"], ["Guardrails", "shell"], ["Observability", "xml"]]) {
      await page.getByRole("button", { name: section, exact: true }).click();
      await page.locator("[data-editor-language='" + mode + "']").waitFor();
    }
    await page.getByRole("button", { name: "Instructions", exact: true }).click();
    const instructions = page.getByRole("textbox", { name: "System instructions" });
    await instructions.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type("# Safe heading\n<script>window.__unsafe = true</script>\n[local label](https://example.invalid)");
    await page.locator(".source-editor-feedback").filter({ hasText: "Source content is not sent to analytics." }).waitFor();
    await page.getByRole("link", { name: "Immutable version history" }).click();
    await page.getByRole("dialog", { name: "Leave unsaved draft?" }).waitFor();
    assert.match(await page.locator("#agent-draft-field-instructions-source").textContent() ?? "", /Safe heading/);
    await page.getByRole("button", { name: "Keep editing" }).click();
    await page.getByRole("heading", { name: agentName }).waitFor();
    void page.goBack().catch(() => undefined);
    await page.getByRole("dialog", { name: "Leave unsaved draft?" }).waitFor();
    assert.match(await page.locator("#agent-draft-field-instructions-source").textContent() ?? "", /Safe heading/);
    await page.getByRole("button", { name: "Keep editing" }).click();
    const preview = page.locator(".markdown-preview");
    await preview.getByRole("heading", { name: "Safe heading" }).waitFor();
    assert.equal(await preview.locator("script").count(), 0);
    assert.equal(await preview.locator("a").count(), 0);
    await page.getByRole("button", { name: "Model", exact: true }).click();
    await page.getByLabel("Approved model").selectOption("model:local-safe-chat@v2");
    const modelDependency = page.getByRole("checkbox", { name: /Local Safe Chat/ });
    assert.equal(await modelDependency.isChecked(), true, "selecting a model binds its exact typed dependency");
    await modelDependency.uncheck();
    await page.getByRole("button", { name: "Save now" }).click();
    await page.getByText("Saved", { exact: true }).waitFor();
    await page.getByRole("button", { name: "Validate saved draft" }).click();
    await page.getByText("Invalid", { exact: false }).first().waitFor();
    await page.getByRole("button", { name: "Open review" }).click();
    await page.getByRole("button", { name: "Prepare review" }).click();
    await page.getByText("Publication is blocked by 1 validation error(s).").waitFor();
    assert.equal(await page.getByRole("button", { name: "Publish immutable version" }).isDisabled(), true);
    await page.setViewportSize({ width: 900, height: 800 });
    const diagnosticsTrigger = page.getByRole("button", { name: "Open diagnostics" });
    await diagnosticsTrigger.click();
    const diagnosticsDialog = page.getByRole("dialog", { name: "Diagnostics and effective values" });
    await diagnosticsDialog.waitFor();
    assert.equal(await page.locator(".agent-draft-context-trigger").getAttribute("aria-controls"), "agent-draft-diagnostics-panel");
    assert.equal(await page.locator(".agent-draft-context-close").evaluate((node) => document.activeElement === node), true);
    await page.keyboard.press("Escape");
    await diagnosticsDialog.waitFor({ state: "detached" });
    await page.waitForFunction(() => document.activeElement?.classList.contains("agent-draft-context-trigger") ?? false);
    await diagnosticsTrigger.click();
    await page.locator(".agent-draft-context-backdrop").click({ position: { x: 2, y: 2 } });
    await diagnosticsDialog.waitFor({ state: "detached" });
    await diagnosticsTrigger.click();
    await diagnosticsDialog.getByRole("button", { name: "ERROR: MODEL_REFERENCE_UNBOUND" }).click();
    await page.locator("#agent-draft-field-model-reference").waitFor();
    await page.waitForFunction((id) => document.activeElement?.id === id, "agent-draft-field-model-reference");
    await modelDependency.check();
    await page.getByRole("checkbox", { name: /HTTP Metadata Connector/ }).check();
    await page.getByRole("button", { name: "General", exact: true }).click();
    await page.getByRole("textbox", { name: "Description" }).fill("");
    await page.getByRole("button", { name: "Save now" }).click();
    await page.getByText("Saved", { exact: true }).waitFor();
    await page.getByRole("button", { name: "Validate saved draft" }).click();
    await page.getByText("Valid", { exact: false }).first().waitFor();
    await page.getByRole("button", { name: "Open review" }).click();
    await page.getByRole("button", { name: "Prepare review" }).click();
    await page.getByText("Exact catalog release:").waitFor();
    await page.getByRole("checkbox", { name: /I reviewed the 1 warning/ }).waitFor();
    assert.equal(await page.getByRole("button", { name: "Publish immutable version" }).isDisabled(), true);
    await page.getByRole("checkbox", { name: /I reviewed the 1 warning/ }).check();
    await page.getByRole("button", { name: "Publish immutable version" }).click();
    await page.getByRole("heading", { name: agentName + " · v1" }).waitFor();
    await page.getByText("Published versions are configuration facts, not deployments.").waitFor();
    await page.goto(origin + "/projects/" + project + "/agents/" + agent + "/edit");
    await page.getByRole("button", { name: "General", exact: true }).click();
    await page.getByRole("textbox", { name: "Description" }).fill("Second immutable publication for comparison.");
    await page.getByRole("button", { name: "Save now" }).click();
    await page.getByText("Saved", { exact: true }).waitFor();
    await page.getByRole("button", { name: "Validate saved draft" }).click();
    await page.getByText("Valid", { exact: false }).first().waitFor();
    await page.getByRole("button", { name: "Open review" }).click();
    await page.getByRole("button", { name: "Prepare review" }).click();
    await page.getByRole("button", { name: "Publish immutable version" }).click();
    await page.getByRole("heading", { name: agentName + " · v2" }).waitFor();
    await page.getByRole("link", { name: "Back to version history" }).click();
    await page.getByRole("heading", { name: "Immutable versions", exact: true }).waitFor();
    await page.getByRole("heading", { name: "Compare two immutable versions" }).waitFor();
    await page.getByRole("link", { name: "Open read-only version" }).nth(1).click();
    await page.getByRole("heading", { name: agentName + " · v1" }).waitFor();
    await page.getByRole("link", { name: "Back to version history" }).click();
    await page.getByRole("button", { name: "Compare selected versions" }).click();
    await page.getByRole("heading", { name: "Immutable version comparison" }).waitFor();
    await page.getByLabel("Read-only version comparison").waitFor();
    await context.close();
  } finally { await browser.close(); }
} finally {
  await client.query("DELETE FROM console_role_assignments WHERE id = $1", [authorRole]);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [authorMembership]);
  await client.end();
  await service.stop();
  await database.drop();
}
