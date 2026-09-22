import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const bea = "00000000-0000-0000-0000-000000000002";
const alpha = "10000000-0000-0000-0000-000000000001";
const commandConsole = "50000000-0000-0000-0000-000000000001";
const fleetAnalytics = "50000000-0000-0000-0000-000000000002";
const commandNavigator = "60000000-0000-0000-0000-000000000001";
const fleetAnalyst = "60000000-0000-0000-0000-000000000002";
const readerMembership = "78000000-0000-0000-0000-000000000001";
const port = 18098;
const origin = "http://127.0.0.1:" + port;

function editorPath(projectId = commandConsole, agentId = commandNavigator) {
  return origin + "/projects/" + projectId + "/agents/" + agentId + "/edit";
}

async function authenticatedContext(browser, principal = ada) {
  const context = await browser.newContext();
  await context.addCookies([{
    name: "sf_session", value: service.signFixtureSession(principal), url: origin, httpOnly: true, sameSite: "Lax"
  }]);
  return context;
}

async function graphql(principal, query, variables) {
  const response = await fetch(origin + "/graphql", {
    method: "POST",
    headers: { "Content-Type": "application/json", Cookie: "sf_session=" + service.signFixtureSession(principal) },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  return response.json();
}

const database = await createIsolatedDatabase("hive_agent_draft_editor_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  const browser = await launchBrowser();
  try {
    const editor = await authenticatedContext(browser);
    const page = await editor.newPage();
    await page.goto(editorPath());
    await page.getByRole("heading", { name: "Feedback Triage Agent" }).waitFor();
    await page.getByText("Draft revision 1").waitFor();
    assert.equal(await page.getByText("Saved", { exact: true }).count(), 1);
    assert.equal(await page.getByRole("button", { name: /publish/i }).count(), 0);
    for (const section of [
      "General", "Instructions", "Harness", "Model", "Tools", "Skills", "Capabilities", "Subagents", "Memory",
      "Guardrails", "Identity", "Observability", "Limits", "Evaluations", "Review"
    ]) {
      await page.getByRole("button", { name: section, exact: true }).click();
      await page.getByRole("heading", { name: section, exact: true }).waitFor();
    }
    await page.getByRole("button", { name: "General", exact: true }).click();
    const description = page.getByRole("textbox", { name: "Description" });
    await description.fill("Local recovery text");
    await page.getByText("Unsaved", { exact: true }).waitFor();

    let failSave = true;
    await page.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (failSave && request.query?.includes("mutation UpdateAgentDraft")) {
        failSave = false;
        await route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ errors: [{ message: "offline" }] }) });
        return;
      }
      await route.continue();
    });
    await page.getByRole("button", { name: "Save now" }).click();
    await page.getByText("We could not save your changes. Your local draft is still available.").waitFor();
    assert.equal(await description.inputValue(), "Local recovery text");
    await page.getByRole("button", { name: "Save now" }).click();
    await page.getByText("Saved", { exact: true }).waitFor();
    await page.getByText("Draft revision 2").waitFor();

    let releaseDelayedSave = () => {};
    let markDelayedSaveDispatched;
    const delayedSaveDispatched = new Promise((resolve) => { markDelayedSaveDispatched = resolve; });
    await page.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (request.query?.includes("mutation UpdateAgentDraft")) {
        markDelayedSaveDispatched();
        await new Promise((resolve) => { releaseDelayedSave = resolve; });
      }
      await route.continue();
    });
    await description.fill("This description was saved by the dispatched request");
    await page.getByRole("button", { name: "Save now" }).click();
    await delayedSaveDispatched;
    await page.getByText("Saving draft…", { exact: true }).waitFor();
    await description.fill("This edit was made after save dispatch and must remain saveable");
    await page.getByText("Unsaved", { exact: true }).waitFor();
    releaseDelayedSave();
    await page.getByText("Draft revision 3").waitFor();
    await page.getByText("Unsaved", { exact: true }).waitFor();
    assert.equal(await description.inputValue(), "This edit was made after save dispatch and must remain saveable");
    await page.unroute("**/graphql");
    await page.getByRole("button", { name: "Save now" }).click();
    await page.getByText("Saved", { exact: true }).waitFor();
    await page.getByText("Draft revision 4").waitFor();

    let releaseDelayedValidation = () => {};
    let markDelayedValidationDispatched;
    const delayedValidationDispatched = new Promise((resolve) => { markDelayedValidationDispatched = resolve; });
    await page.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (request.query?.includes("mutation ValidateAgentDraft")) {
        markDelayedValidationDispatched();
        await new Promise((resolve) => { releaseDelayedValidation = resolve; });
      }
      await route.continue();
    });
    await page.getByRole("button", { name: "Validate saved draft" }).click();
    await delayedValidationDispatched;
    await page.getByText("Validating saved draft…", { exact: true }).waitFor();
    await description.fill("This edit was made after validation dispatch and must remain saveable");
    assert.equal(await description.inputValue(), "This edit was made after validation dispatch and must remain saveable");
    releaseDelayedValidation();
    await page.getByText("Draft revision 5").waitFor();
    await page.getByText("Unsaved", { exact: true }).waitFor();
    assert.equal(await description.inputValue(), "This edit was made after validation dispatch and must remain saveable");
    await page.unroute("**/graphql");
    await page.getByRole("button", { name: "Save now" }).click();
    await page.getByText("Saved", { exact: true }).waitFor();
    await page.getByText("Draft revision 6").waitFor();

    const displayName = page.getByRole("textbox", { name: "Display name" });
    await displayName.fill("");
    await page.getByRole("button", { name: "Save now" }).click();
    await page.getByText("Draft revision 7").waitFor();
    await page.getByRole("button", { name: "Validate saved draft" }).click();
    // Diagnostics live in a panel that stays closed until the author opens it.
    await page.getByRole("button", { name: "Open diagnostics" }).click();
    await page.getByText("ERROR: DISPLAY_NAME_REQUIRED").waitFor();
    await page.getByText("WARNING: DESCRIPTION_RECOMMENDED").waitFor({ state: "detached" }).catch(() => {});
    await page.getByRole("button", { name: "ERROR: DISPLAY_NAME_REQUIRED" }).click();
    await page.getByRole("heading", { name: "General", exact: true }).waitFor();
    await page.getByText("Draft revision 8").waitFor();

    const serverDocument = {
      general: { displayName: "Server name", description: "Server changed this document." },
      instructions: { system: "Follow the project instructions and respond helpfully." },
      harness: {}, model: {}, tools: {}, skills: {}, capabilities: {}, subagents: {}, memory: {}, guardrails: {}, identity: {},
      observability: {}, limits: {}, evaluations: {}
    };
    const concurrent = await graphql(ada,
      "mutation Update($input: UpdateAgentDraftInput!) { updateAgentDraft(input: $input) { agentDraft { revision } problems { code } } }",
      { input: { projectId: commandConsole, agentId: commandNavigator, expectedRevision: 8, document: serverDocument } });
    assert.equal(concurrent.data.updateAgentDraft.agentDraft.revision, 9);
    await description.fill("Local changes must survive a conflict");
    let staleSaveCalls = 0;
    await page.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (request.query?.includes("mutation UpdateAgentDraft")) {
        staleSaveCalls += 1;
      }
      await route.continue();
    });
    await page.getByRole("button", { name: "Save now" }).click();
    await page.getByRole("heading", { name: "Draft revision conflict" }).waitFor();
    assert.equal(await description.inputValue(), "Local changes must survive a conflict");
    assert.equal(staleSaveCalls, 1);
    await page.getByRole("button", { name: "Discard local changes and reload server version" }).click();
    await page.getByText("Draft revision 9").waitFor();

    let releaseRouteChangeValidation = () => {};
    let markRouteChangeValidationDispatched;
    const routeChangeValidationDispatched = new Promise((resolve) => { markRouteChangeValidationDispatched = resolve; });
    await page.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (request.query?.includes("mutation ValidateAgentDraft")) {
        markRouteChangeValidationDispatched();
        await new Promise((resolve) => { releaseRouteChangeValidation = resolve; });
      }
      await route.continue();
    });
    await page.getByRole("button", { name: "Validate saved draft" }).click();
    await routeChangeValidationDispatched;
    await page.evaluate((path) => {
      window.history.pushState({}, "", path);
      window.dispatchEvent(new PopStateEvent("popstate"));
    }, "/projects/" + commandConsole + "/agents/" + fleetAnalyst + "/edit");
    await page.getByRole("heading", { name: "Sentiment Analyst" }).waitFor();
    await page.getByText("Draft revision 1").waitFor();
    releaseRouteChangeValidation();
    await page.waitForTimeout(150);
    await page.getByRole("heading", { name: "Sentiment Analyst" }).waitFor();
    await page.getByText("Draft revision 1").waitFor();
    await page.unroute("**/graphql");
    await editor.close();

    await client.query(
      "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL)",
      [readerMembership, alpha, bea]
    );
    const reader = await authenticatedContext(browser, bea);
    const readerPage = await reader.newPage();
    await readerPage.goto(editorPath());
    await readerPage.getByText("You do not have permission to edit this draft.").waitFor();
    assert.equal(await readerPage.getByRole("button", { name: "Save now" }).count(), 0);
    await reader.close();

    const unavailable = await authenticatedContext(browser);
    const unavailablePage = await unavailable.newPage();
    for (const [projectId, agentId] of [
      [fleetAnalytics, commandNavigator], [commandConsole, "78000000-0000-0000-0000-000000000099"],
      ["not-a-uuid", commandNavigator], [commandConsole, "not-a-uuid"]
    ]) {
      await unavailablePage.goto(editorPath(projectId, agentId));
      if (projectId === "not-a-uuid") {
        await unavailablePage.getByRole("heading", { name: "Access denied" }).waitFor();
      } else {
        await unavailablePage.getByText("This agent is unavailable.").waitFor();
      }
    }
    await unavailable.close();

    const initialError = await authenticatedContext(browser);
    const initialErrorPage = await initialError.newPage();
    let failInitial = true;
    await initialErrorPage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (failInitial && request.query?.includes("query AgentDraft")) {
        failInitial = false;
        await route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ errors: [{ message: "unexpected" }] }) });
      } else {
        await route.continue();
      }
    });
    await initialErrorPage.goto(editorPath());
    await initialErrorPage.getByText("We could not load this agent draft. Try again.").waitFor();
    await initialErrorPage.getByRole("button", { name: "Retry" }).click();
    await initialErrorPage.getByRole("heading", { name: "Feedback Triage Agent" }).waitFor();
    await initialError.close();

    const anonymous = await browser.newContext();
    const anonymousPage = await anonymous.newPage();
    await anonymousPage.goto(editorPath());
    await anonymousPage.getByRole("heading", { name: "Session error" }).waitFor();
    await anonymous.close();
  } finally {
    await browser.close();
  }
} finally {
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [readerMembership]);
  await client.query("DELETE FROM agent_draft_audit_events WHERE agent_id = $1", [commandNavigator]);
  await client.query("DELETE FROM agent_drafts WHERE agent_id = $1", [commandNavigator]);
  await client.end();
  await service.stop();
  await database.drop();
}
