import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const project = "50000000-0000-0000-0000-000000000001";
const budgetBatch = "84000000-0000-0000-0000-000000000001";
const port = 18110;
const origin = "http://127.0.0.1:" + port;

const database = await createIsolatedDatabase("hive_administration_settings_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
const browser = await launchBrowser();
try {
  await client.query(
    "INSERT INTO frozen_spend_import_batches (id, project_id, period_start, period_end, currency, state, amount_cents, includes_estimates, data_as_of, completed_at, imported_at) "
      + "VALUES ($1, $2, date_trunc('month', CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC', date_trunc('month', CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' + INTERVAL '1 month', 'USD', 'COMPLETE', 12345, TRUE, CURRENT_TIMESTAMP - INTERVAL '2 hours', CURRENT_TIMESTAMP - INTERVAL '1 hour', CURRENT_TIMESTAMP) "
      + "ON CONFLICT (id) DO UPDATE SET project_id = EXCLUDED.project_id, period_start = EXCLUDED.period_start, period_end = EXCLUDED.period_end, currency = EXCLUDED.currency, state = EXCLUDED.state, amount_cents = EXCLUDED.amount_cents, includes_estimates = EXCLUDED.includes_estimates, data_as_of = EXCLUDED.data_as_of, completed_at = EXCLUDED.completed_at, imported_at = EXCLUDED.imported_at",
    [budgetBatch, project]
  );
  const selectedBatch = await client.query(
    "SELECT id FROM frozen_spend_import_batches WHERE project_id = $1 ORDER BY imported_at DESC LIMIT 1",
    [project]
  );
  assert.equal(selectedBatch.rows[0].id, budgetBatch);

  const context = await browser.newContext();
  await context.addCookies([{ name: "sf_session", value: service.signFixtureSession(ada), url: origin, httpOnly: true, sameSite: "Lax" }]);
  const page = await context.newPage();
  const reloadForBudgetStatus = async (state, reason) => {
    const response = page.waitForResponse((candidate) => candidate.url() === origin + "/graphql"
      && candidate.request().method() === "POST"
      && String(candidate.request().postData()).includes("query ProjectAdministration"));
    await page.reload();
    const payload = await (await response).json();
    const status = payload.data.projects.nodes[0].projectBudgetPolicies.status;
    assert.equal(status.state, state);
    assert.equal(status.reason, reason);
    assert.equal(status.includesEstimates, true);
  };
  const waitForRenderedBudgetStatus = async (state, expected) => {
    const status = page.getByRole("heading", { name: "Budgets" }).locator("..").getByRole("status");
    let rendered = "";
    for (let attempt = 0; attempt < 300; attempt += 1) {
      rendered = await status.innerText();
      if (rendered === expected) {
        assert.equal(await page.getByRole("img", { name: "Budget status " + state }).count(), 1);
        return;
      }
      await page.waitForTimeout(100);
    }
    assert.equal(rendered, expected);
  };

  await page.goto(origin + "/organizations/" + alpha + "/settings");
  await page.getByRole("heading", { name: "Product administration" }).waitFor();
  await page.getByText("ada.lovelace@local.invalid").waitFor();
  const organizationMembers = page.getByRole("heading", { name: "Members" }).locator("..");
  await organizationMembers.getByText(/Project access:/).waitFor();
  assert.equal(await organizationMembers.getByRole("checkbox", { name: "organization admin" }).evaluateAll((inputs) =>
    inputs.filter((input) => input instanceof HTMLInputElement && input.checked).length), 1,
    "assigned organization roles are visible selectable values");
  await organizationMembers.getByLabel("Known principal").waitFor();
  await organizationMembers.getByText("No eligible known principals are available in this organization.").waitFor();

  const archiveOrganization = page.getByRole("button", { name: "Archive organization" });
  await archiveOrganization.click();
  const organizationDialog = page.getByRole("dialog", { name: "Archive organization" });
  await organizationDialog.getByLabel("Archive reason").waitFor();
  assert.equal(await organizationDialog.getByLabel("Archive reason").evaluate((element) => element === document.activeElement), true);
  await page.keyboard.press("Tab");
  await page.keyboard.press("Tab");
  await page.keyboard.press("Tab");
  assert.equal(await organizationDialog.getByLabel("Archive reason").evaluate((element) => element === document.activeElement), true);
  await page.keyboard.press("Escape");
  assert.equal(await archiveOrganization.evaluate((element) => element === document.activeElement), true);

  await page.route("**/graphql", async (route) => {
    const request = route.request();
    const body = request.postDataJSON();
    if (String(body.query).includes("archiveAdministrationScope")) {
      await route.fulfill({ contentType: "application/json", body: JSON.stringify({ data: {
        archiveAdministrationScope: { organization: null, project: null, problems: [{
          __typename: "Problem", code: "FORBIDDEN", message: "This administration resource is unavailable."
        }] }
      } }) });
      return;
    }
    await route.continue();
  });
  await archiveOrganization.click();
  await organizationDialog.getByLabel("Archive reason").fill("Retain history");
  const confirmation = organizationDialog.getByLabel("Type organization ID product exactly");
  await confirmation.focus();
  await confirmation.pressSequentially("product");
  assert.equal(await confirmation.inputValue(), "product");
  assert.equal(await confirmation.evaluate((element) => element === document.activeElement), true);
  await organizationDialog.getByRole("button", { name: "Archive organization" }).click();
  await organizationDialog.getByRole("alert").getByText("This administration resource is unavailable.").waitFor();
  assert.equal(await organizationDialog.count(), 1);
  await page.unroute("**/graphql");
  await organizationDialog.getByRole("button", { name: "Cancel" }).click();

  await page.goto(origin + "/projects/" + project + "/settings");
  await page.getByRole("heading", { name: "Customer Feedback Copilot settings" }).waitFor();
  await page.getByRole("button", { name: "Save general settings" }).waitFor();
  const projectMembers = page.getByRole("heading", { name: "Members" }).locator("..");
  await projectMembers.getByLabel("Known principal").selectOption(ada);
  assert.equal(await projectMembers.getByRole("checkbox", { name: "agent developer" }).isChecked(), true);
  await projectMembers.getByRole("checkbox", { name: "project admin" }).check();
  assert.match(await projectMembers.locator(".member-form p").filter({ hasText: "Selected:" }).innerText(), /project admin/);
  const mcpSection = page.getByRole("heading", { name: "MCP servers" }).locator("..");
  await mcpSection.getByRole("link", { name: "Open MCP servers" }).waitFor();
  assert.equal(await mcpSection.getByRole("textbox").count(), 0, "project settings owns no competing connection writer");
  await waitForRenderedBudgetStatus("NORMAL", "✓ NORMAL: USD 123.45 (includes estimates)");
  await page.getByText(/Fixed local P-05 policy revision 1/).waitFor();
  await client.query("UPDATE frozen_spend_import_batches SET completed_at = CURRENT_TIMESTAMP - INTERVAL '2 days' WHERE id = $1", [budgetBatch]);
  await reloadForBudgetStatus("UNKNOWN", "STALE_DATA");
  await waitForRenderedBudgetStatus("UNKNOWN", "? UNKNOWN: Unknown (includes estimates) — STALE_DATA");
  await client.query("UPDATE frozen_spend_import_batches SET completed_at = CURRENT_TIMESTAMP, currency = 'EUR' WHERE id = $1", [budgetBatch]);
  await reloadForBudgetStatus("UNKNOWN", "CURRENCY_MISMATCH");
  await waitForRenderedBudgetStatus("UNKNOWN", "? UNKNOWN: Unknown (includes estimates) — CURRENCY_MISMATCH");
  await client.query("UPDATE frozen_spend_import_batches SET state = 'INCOMPLETE', currency = 'USD' WHERE id = $1", [budgetBatch]);
  await reloadForBudgetStatus("UNKNOWN", "INCOMPLETE_DATA");
  await waitForRenderedBudgetStatus("UNKNOWN", "? UNKNOWN: Unknown (includes estimates) — INCOMPLETE_DATA");
  await client.query("UPDATE frozen_spend_import_batches SET state = 'COMPLETE', completed_at = CURRENT_TIMESTAMP - INTERVAL '1 hour' WHERE id = $1", [budgetBatch]);
  await reloadForBudgetStatus("NORMAL", null);
  await waitForRenderedBudgetStatus("NORMAL", "✓ NORMAL: USD 123.45 (includes estimates)");

  const archiveProject = page.getByRole("button", { name: "Archive project" });
  await archiveProject.click();
  const projectDialog = page.getByRole("dialog", { name: "Archive project" });
  await projectDialog.getByLabel("Reason").waitFor();
  assert.equal(await projectDialog.getByLabel("Reason").evaluate((element) => element === document.activeElement), true);
  await page.keyboard.press("Escape");
  assert.equal(await archiveProject.evaluate((element) => element === document.activeElement), true);
  await archiveProject.click();
  await projectDialog.getByLabel("Reason").fill("Exercise lifecycle guard");
  await projectDialog.getByRole("button", { name: "Archive project" }).click();
  await page.getByRole("button", { name: "Restore project" }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Save budget policy" }).count(), 0);
  await page.getByRole("button", { name: "Restore project" }).click();
  await page.getByRole("button", { name: "Archive project" }).waitFor();
} finally {
  await browser.close();
  await client.query("DELETE FROM frozen_spend_import_batches WHERE id = $1", [budgetBatch]);
  await client.end();
  await service.stop();
  await database.drop();
}
