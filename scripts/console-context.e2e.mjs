import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const commandConsole = "50000000-0000-0000-0000-000000000001";
const commandNavigator = "60000000-0000-0000-0000-000000000001";
const privateProject = "50000000-0000-0000-0000-000000000003";
const port = 18101;
const origin = "http://127.0.0.1:" + port;
const selectionKey = "hive.console-context." + ada;

async function directDraftUpdate() {
  const response = await fetch(origin + "/graphql", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Cookie: "sf_session=" + service.signFixtureSession(ada)
    },
    body: JSON.stringify({
      query: "mutation Update($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { __typename code message } } }",
      variables: {
        input: {
          projectId: commandConsole, agentId: commandNavigator, expectedRevision: 1,
          document: { general: { displayName: "Prohibited update" } }
        }
      }
    })
  });
  assert.equal(response.status, 200);
  return response.json();
}

const database = await createIsolatedDatabase("hive_console_context_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  await client.query("DELETE FROM principal_display_preferences WHERE principal_id = $1", [ada]);
  const browser = await launchBrowser();
  try {
    const authenticated = await browser.newContext();
    await authenticated.addCookies([{ name: "sf_session", value: service.signFixtureSession(ada), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const page = await authenticated.newPage();
    await page.goto(origin + "/");
    await page.locator(".organization-list").getByRole("link", { name: "Product", exact: true }).click();
    await page.waitForURL(origin + "/organizations/" + alpha);
    await page.getByRole("heading", { name: "Product" }).waitFor();
    await page.locator(".project-list").getByRole("link", { name: "Customer Feedback Copilot" }).click();
    await page.waitForURL(origin + "/projects/" + commandConsole);
    await page.getByRole("heading", { name: "Customer Feedback Copilot" }).waitFor();
    await page.waitForFunction(([key, projectId]) => {
      try { return JSON.parse(localStorage.getItem(key) ?? "{}").selectedProject === projectId; } catch { return false; }
    }, [selectionKey, commandConsole]);
    await page.goto(origin + "/");
    await page.waitForURL(origin + "/projects/" + commandConsole);

    await page.goto(origin + "/projects/" + commandConsole + "/agents/" + commandNavigator + "/edit");
    await page.getByRole("button", { name: "Save now" }).waitFor();
    await client.query(
      "DELETE FROM console_role_assignments WHERE project_id = $1 AND principal_id = $2 AND role_code = 'AGENT_DEVELOPER'",
      [commandConsole, ada]
    );
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await page.getByText("You do not have permission to edit this draft.").waitFor();
    assert.equal(await page.getByRole("button", { name: "Save now" }).count(), 0);
    const directRefusal = await directDraftUpdate();
    assert.deepEqual(directRefusal.data.updateAgentDraft.problems, [{
      __typename: "AgentDraftAuthorizationProblem", code: "FORBIDDEN", message: "You do not have permission to edit this draft."
    }]);
    await client.query(
      "INSERT INTO console_role_assignments (id, principal_id, organization_id, project_id, role_code) VALUES ($1, $2, NULL, $3, 'AGENT_DEVELOPER') ON CONFLICT DO NOTHING",
      ["81000000-0000-0000-0000-000000000005", ada, commandConsole]
    );

    await page.goto(origin + "/preferences");
    await page.getByRole("heading", { name: "Display preferences" }).waitFor();
    await page.getByLabel("Color scheme").focus();
    await page.keyboard.press("ArrowDown");
    await page.getByLabel("Density").focus();
    await page.keyboard.press("ArrowDown");
    await page.getByRole("button", { name: "Save", exact: true }).press("Enter");
    await page.getByText("Preview changes apply to this shell immediately.").waitFor();

    await page.goto(origin + "/projects/" + privateProject);
    await page.getByRole("heading", { name: "Access denied" }).waitFor();
    assert.equal(await page.getByText(/private operations/i).count(), 0);
    await authenticated.close();

    const anonymous = await browser.newContext();
    const anonymousPage = await anonymous.newPage();
    await anonymousPage.goto(origin + "/projects/" + commandConsole);
    await anonymousPage.getByRole("heading", { name: "Session error" }).waitFor();
    await anonymous.close();
  } finally {
    await browser.close();
  }
} finally {
  await client.query(
    "INSERT INTO console_role_assignments (id, principal_id, organization_id, project_id, role_code) VALUES ($1, $2, NULL, $3, 'AGENT_DEVELOPER') ON CONFLICT DO NOTHING",
    ["81000000-0000-0000-0000-000000000005", ada, commandConsole]
  );
  await client.query("DELETE FROM principal_display_preferences WHERE principal_id = $1", [ada]);
  await client.end();
  await service.stop();
  await database.drop();
}
