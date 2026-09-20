import { readFileSync } from "node:fs";
import { buildSchema } from "graphql";
import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const alphaMembership = "20000000-0000-0000-0000-000000000001";
const commandConsole = "50000000-0000-0000-0000-000000000001";
const fleetAnalytics = "50000000-0000-0000-0000-000000000002";
const privateProject = "50000000-0000-0000-0000-000000000003";
const commandNavigator = "60000000-0000-0000-0000-000000000001";
const fleetAnalyst = "60000000-0000-0000-0000-000000000002";
const privateAgent = "60000000-0000-0000-0000-000000000004";
const emptyAgent = "68000000-0000-0000-0000-000000000001";
const revokedOrganization = "68000000-0000-0000-0000-000000000002";
const revokedProject = "68000000-0000-0000-0000-000000000003";
const revokedAgent = "68000000-0000-0000-0000-000000000004";
const revokedMembership = "68000000-0000-0000-0000-000000000005";
const port = 18092;
const origin = "http://127.0.0.1:" + port;

function overviewPath(projectId, agentId) {
  return origin + "/projects/" + projectId + "/agents/" + agentId;
}

async function authenticatedContext(browser, recordTimeouts = false) {
  const context = await browser.newContext();
  if (recordTimeouts) {
    await context.addInitScript(() => {
      const original = window.setTimeout;
      window.__agentOverviewTimeouts = [];
      Math.random = () => 0.5;
      window.setTimeout = (handler, delay, ...arguments_) => {
        window.__agentOverviewTimeouts.push(Number(delay));
        return original(handler, delay, ...arguments_);
      };
    });
  }
  await context.addCookies([{
    name: "sf_session",
    value: service.signFixtureSession(ada),
    url: origin,
    httpOnly: true,
    sameSite: "Lax"
  }]);
  return context;
}

async function expectUnavailable(page, projectId, agentId) {
  await page.goto(overviewPath(projectId, agentId));
  if (projectId === fleetAnalytics || projectId === commandConsole) {
    await page.getByText("This agent is unavailable.").waitFor();
  } else {
    await page.getByRole("heading", { name: "Access denied" }).waitFor();
  }
  assert.equal(await page.locator(".agent-overview-card").count(), 0);
  const routeContent = page.locator("main");
  await routeContent.getByText("Feedback Triage Agent").waitFor({ state: "detached" });
  await routeContent.getByText("Incident Triage Agent").waitFor({ state: "detached" });
}

const database = await createIsolatedDatabase("hive_agent_operational_view_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  await client.query(
    "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'empty-overview', 'Empty overview', 'ACTIVE')",
    [emptyAgent, commandConsole]
  );
  await client.query(
    "INSERT INTO organizations (id, slug, display_name, lifecycle_status) VALUES ($1, 'revoked-agent-overview', 'Revoked agent overview', 'ACTIVE')",
    [revokedOrganization]
  );
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'revoked-agent-overview', 'Revoked agent overview', 'ACTIVE')",
    [revokedProject, revokedOrganization]
  );
  await client.query(
    "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'revoked-agent-overview', 'Revoked agent overview', 'ACTIVE')",
    [revokedAgent, revokedProject]
  );
  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', CURRENT_TIMESTAMP)",
    [revokedMembership, revokedOrganization, ada]
  );
  const browser = await launchBrowser();
  try {
    const overview = await authenticatedContext(browser, true);
    const page = await overview.newPage();
    await page.goto(overviewPath(commandConsole, commandNavigator));
    await page.getByRole("heading", { name: "Feedback Triage Agent" }).waitFor();
    assert.equal(await page.locator(".agent-overview-card").count(), 6);
    for (const label of [
      "Draft validation", "Latest published version", "Alias targets", "Active deployment", "Recent evaluation", "Runtime health"
    ]) {
      await page.getByRole("heading", { name: label }).waitFor();
    }
    await page.getByText("Valid", { exact: true }).waitFor();
    await page.getByText("v1.4.0", { exact: true }).waitFor();
    await page.getByText("2 of 3 active", { exact: true }).waitFor();
    await page.getByText("Active", { exact: true }).waitFor();
    await page.getByText("Passed", { exact: true }).waitFor();
    await page.getByText(/^Fresh; observed .*\.$/).waitFor();
    const firstFreshness = await page.locator(".agent-overview-freshness").innerText();
    await page.waitForTimeout(2_100);
    assert.notEqual(await page.locator(".agent-overview-freshness").innerText(), firstFreshness);
    assert.equal(await page.getByRole("link", { name: "Preferences" }).count(), 1);
    assert.equal(await page.locator("main.agent-overview").getByRole("button").count(), 0,
      "the read-only overview content has no mutation controls");
    assert.equal(await page.getByText(alpha, { exact: false }).count(), 0);
    const schema = await page.evaluate(async () => {
      const response = await fetch("/graphql", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ query: "query AgentOverviewSchema { __schema { mutationType { name fields { name } } } }" })
      });
      return response.json();
    });
    // Field order is not part of a GraphQL contract; the frozen contract file is the reference set.
    assert.equal(schema.data.__schema.mutationType.name, "Mutation");
    assert.deepEqual(schema.data.__schema.mutationType.fields.map((field) => field.name).sort(),
      Object.keys(buildSchema(readFileSync("schema/contract.graphql", "utf8")).getMutationType().getFields()).sort());

    let requests = 0;
    let delayNext = false;
    let failNext = false;
    let releaseDelayed = () => {};
    let delayedStarted = () => {};
    await page.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (!request.query?.includes("AgentOperationalView")) {
        await route.continue();
        return;
      }
      requests += 1;
      if (failNext) {
        failNext = false;
        await route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ errors: [{ message: "unexpected" }] }) });
        return;
      }
      if (delayNext) {
        delayNext = false;
        const response = await route.fetch();
        delayedStarted();
        await new Promise((resolve) => { releaseDelayed = resolve; });
        try {
          await route.fulfill({ response });
        } catch {
          // Navigating away aborts a late refresh before it can replace the current route.
        }
        return;
      }
      await route.continue();
    });

    await page.evaluate(() => {
      Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
      document.dispatchEvent(new Event("visibilitychange"));
      window.dispatchEvent(new Event("focus"));
    });
    await page.waitForTimeout(100);
    assert.equal(requests, 0);
    await page.evaluate(() => {
      Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await page.getByText(/^Last successful agent overview refresh at .* \(.* seconds ago\)\.$/).waitFor();
    await page.waitForTimeout(100);
    assert.equal(requests, 1);

    const singleFlightStart = new Promise((resolve) => { delayedStarted = resolve; });
    const beforeSingleFlight = requests;
    delayNext = true;
    await page.evaluate(() => {
      window.dispatchEvent(new Event("focus"));
      window.dispatchEvent(new Event("focus"));
    });
    await singleFlightStart;
    assert.equal(requests, beforeSingleFlight + 1);
    releaseDelayed();
    await page.getByText(/^Last successful agent overview refresh at .* \(.* seconds ago\)\.$/).waitFor();

    failNext = true;
    await page.evaluate(() => window.dispatchEvent(new Event("online")));
    await page.getByText(/^We could not refresh this agent overview. Stale data from .* remains visible\.$/).waitFor();
    await page.getByRole("heading", { name: "Feedback Triage Agent" }).waitFor();
    const refreshDelays = (await page.evaluate(() => window.__agentOverviewTimeouts))
      .filter((delay) => delay >= 15_000 && delay <= 30_000);
    assert(refreshDelays.some((delay) => delay === 22_500));
    assert(refreshDelays.some((delay) => delay === 25_000));

    const requestsBeforeOffline = requests;
    await page.evaluate(() => window.dispatchEvent(new Event("offline")));
    await page.getByText(/^Offline. Agent data remains from .*\.$/).waitFor();
    await page.waitForTimeout(100);
    assert.equal(requests, requestsBeforeOffline);
    await page.evaluate(() => window.dispatchEvent(new Event("online")));
    await page.waitForTimeout(100);
    assert.equal(requests, requestsBeforeOffline + 1);

    await client.query("UPDATE organization_memberships SET ended_at = CURRENT_TIMESTAMP WHERE id = $1", [alphaMembership]);
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await page.getByRole("heading", { name: "Access denied" }).waitFor();
    await page.getByRole("heading", { name: "Feedback Triage Agent" }).waitFor({ state: "detached" });
    await overview.close();
    await client.query("UPDATE organization_memberships SET ended_at = NULL WHERE id = $1", [alphaMembership]);

    const staleRoute = await authenticatedContext(browser);
    const stalePage = await staleRoute.newPage();
    let staleRelease = () => {};
    let staleStarted = () => {};
    const staleRefreshStarted = new Promise((resolve) => { staleStarted = resolve; });
    let holdRefresh = false;
    await stalePage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (!request.query?.includes("AgentOperationalView") || !holdRefresh) {
        await route.continue();
        return;
      }
      holdRefresh = false;
      const response = await route.fetch();
      staleStarted();
      await new Promise((resolve) => { staleRelease = resolve; });
      try {
        await route.fulfill({ response });
      } catch {
        // A stale response is route-scoped and must not replace a newer target.
      }
    });
    await stalePage.goto(overviewPath(commandConsole, commandNavigator));
    await stalePage.getByRole("heading", { name: "Feedback Triage Agent" }).waitFor();
    holdRefresh = true;
    await stalePage.evaluate(() => window.dispatchEvent(new Event("focus")));
    await staleRefreshStarted;
    await stalePage.goto(overviewPath(commandConsole, emptyAgent));
    await stalePage.getByRole("heading", { name: "Empty overview" }).waitFor();
    staleRelease();
    await stalePage.waitForTimeout(100);
    await stalePage.getByRole("heading", { name: "Feedback Triage Agent" }).waitFor({ state: "detached" });
    await staleRoute.close();

    const empty = await authenticatedContext(browser);
    const emptyPage = await empty.newPage();
    await emptyPage.goto(overviewPath(commandConsole, emptyAgent));
    await emptyPage.getByRole("heading", { name: "Empty overview" }).waitFor();
    await emptyPage.getByText("Not Validated", { exact: true }).waitFor();
    await emptyPage.getByText("No published version", { exact: true }).waitFor();
    await emptyPage.getByText("No alias targets", { exact: true }).waitFor();
    await emptyPage.getByText("Not Deployed", { exact: true }).waitFor();
    await emptyPage.getByText("No Evaluation", { exact: true }).waitFor();
    await emptyPage.getByText("Unknown", { exact: true }).waitFor();
    await empty.close();

    const stale = await authenticatedContext(browser);
    const serverStalePage = await stale.newPage();
    await serverStalePage.goto(overviewPath(commandConsole, fleetAnalyst));
    await serverStalePage.getByRole("heading", { name: "Sentiment Analyst" }).waitFor();
    await serverStalePage.getByText(/^Stale; observed .*\.$/).waitFor();
    await stale.close();

    const unavailable = await authenticatedContext(browser);
    const unavailablePage = await unavailable.newPage();
    for (const [projectId, agentId] of [
      [fleetAnalytics, commandNavigator],
      [privateProject, privateAgent],
      [revokedProject, revokedAgent],
      [commandConsole, "68000000-0000-0000-0000-000000000099"],
      ["not-a-uuid", commandNavigator],
      [commandConsole, "not-a-uuid"]
    ]) {
      await expectUnavailable(unavailablePage, projectId, agentId);
    }
    await unavailable.close();

    const initialError = await authenticatedContext(browser);
    const initialErrorPage = await initialError.newPage();
    let failInitial = true;
    await initialErrorPage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (failInitial && request.query?.includes("query AgentOperationalView")) {
        failInitial = false;
        await route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ errors: [{ message: "unexpected" }] }) });
        return;
      }
      await route.continue();
    });
    await initialErrorPage.goto(overviewPath(commandConsole, commandNavigator));
    await initialErrorPage.getByText("We could not load this agent overview. Try again.").waitFor();
    await initialErrorPage.getByRole("button", { name: "Retry" }).click();
    await initialErrorPage.getByRole("heading", { name: "Feedback Triage Agent" }).waitFor();
    await initialError.close();

    const skeleton = await authenticatedContext(browser);
    const skeletonPage = await skeleton.newPage();
    let delayInitial = true;
    await skeletonPage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (delayInitial && request.query?.includes("query AgentOperationalView")) {
        delayInitial = false;
        const response = await route.fetch();
        await new Promise((resolve) => setTimeout(resolve, 300));
        await route.fulfill({ response });
        return;
      }
      await route.continue();
    });
    const skeletonNavigation = skeletonPage.goto(overviewPath(commandConsole, commandNavigator));
    await skeletonNavigation;
    await skeletonPage.getByRole("heading", { name: "Feedback Triage Agent" }).waitFor();
    await skeleton.close();

    const anonymous = await browser.newContext();
    const anonymousPage = await anonymous.newPage();
    await anonymousPage.goto(overviewPath(commandConsole, commandNavigator));
    await anonymousPage.getByRole("heading", { name: "Session error" }).waitFor();
    await anonymous.close();
  } finally {
    await browser.close();
  }
} finally {
  await client.query("UPDATE organization_memberships SET ended_at = NULL WHERE id = $1", [alphaMembership]);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [revokedMembership]);
  await client.query("DELETE FROM projects WHERE id = $1", [revokedProject]);
  await client.query("DELETE FROM organizations WHERE id = $1", [revokedOrganization]);
  await client.query("DELETE FROM agents WHERE id = $1", [emptyAgent]);
  await client.end();
  await service.stop();
  await database.drop();
}
