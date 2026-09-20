import { readFileSync } from "node:fs";
import { buildSchema } from "graphql";
import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const alphaMembership = "20000000-0000-0000-0000-000000000001";
const commandConsole = "50000000-0000-0000-0000-000000000001";
const privateProject = "50000000-0000-0000-0000-000000000003";
const allZeroProject = "76000000-0000-0000-0000-000000000001";
const revokedOrganization = "76000000-0000-0000-0000-000000000002";
const revokedProject = "76000000-0000-0000-0000-000000000003";
const revokedMembership = "76000000-0000-0000-0000-000000000004";
const unavailableCostProject = "76000000-0000-0000-0000-000000000005";
const port = 18088;
const origin = "http://127.0.0.1:" + port;

function dashboardPath(projectId) {
  return origin + "/projects/" + projectId;
}

async function authenticatedContext(browser, recordTimeouts = false) {
  const context = await browser.newContext();
  if (recordTimeouts) {
    await context.addInitScript(() => {
      const original = window.setTimeout;
      window.__dashboardTimeouts = [];
      Math.random = () => 0.5;
      window.setTimeout = (handler, delay, ...arguments_) => {
        window.__dashboardTimeouts.push(Number(delay));
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

async function expectUnavailable(page, projectId) {
  await page.goto(dashboardPath(projectId));
  await page.getByRole("heading", { name: "Access denied" }).waitFor();
  assert.equal(await page.locator(".project-dashboard-card").count(), 0);
  await page.getByText("Incident Response").waitFor({ state: "detached" });
}

const database = await createIsolatedDatabase("hive_project_dashboard_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'all-zero-dashboard', 'All-zero dashboard', 'ACTIVE')",
    [allZeroProject, alpha]
  );
  await client.query(
    "INSERT INTO organizations (id, slug, display_name, lifecycle_status) VALUES ($1, 'revoked-dashboard', 'Revoked dashboard', 'ACTIVE')",
    [revokedOrganization]
  );
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'revoked-dashboard', 'Revoked dashboard', 'ACTIVE')",
    [revokedProject, revokedOrganization]
  );
  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', CURRENT_TIMESTAMP)",
    [revokedMembership, revokedOrganization, ada]
  );
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'unavailable-cost-dashboard', 'Unavailable cost dashboard', 'ACTIVE')",
    [unavailableCostProject, alpha]
  );
  await client.query(
    "INSERT INTO project_dashboard_metrics (project_id, cost_availability) VALUES ($1, 'UNAVAILABLE')",
    [unavailableCostProject]
  );
  const browser = await launchBrowser();
  try {
    const dashboard = await authenticatedContext(browser, true);
    const page = await dashboard.newPage();
    await page.goto(dashboardPath(commandConsole));
    await page.getByRole("heading", { name: "Customer Feedback Copilot" }).waitFor();
    assert.equal(await page.locator(".project-dashboard-card").count(), 6);
    for (const label of [
      "Active agents",
      "Active deployments",
      "Failed deployments",
      "Pending approvals",
      "Unhealthy resources",
      "Current-period cost"
    ]) {
      await page.getByRole("heading", { name: label }).waitFor();
    }
    await page.getByText("USD 123.45").waitFor();
    await page.getByText(/^Period .* to .*; reported .*\.$/).waitFor();
    const firstFreshness = await page.locator(".project-dashboard-freshness").innerText();
    await page.waitForTimeout(2_100);
    assert.notEqual(await page.locator(".project-dashboard-freshness").innerText(), firstFreshness);
    assert.equal(await page.getByRole("link", { name: "Preferences" }).count(), 1);
    assert.equal(await page.locator("main.project-dashboard").getByRole("button").count(), 0,
      "the read-only dashboard content has no mutation controls");
    assert.equal(await page.getByText(alpha, { exact: false }).count(), 0);
    const schema = await page.evaluate(async () => {
      const response = await fetch("/graphql", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ query: "query ProjectDashboardSchema { __schema { mutationType { name fields { name } } } }" })
      });
      return response.json();
    });
    // Field order is not part of a GraphQL contract; the frozen contract file is the reference set.
    assert.equal(schema.data.__schema.mutationType.name, "Mutation");
    assert.deepEqual(schema.data.__schema.mutationType.fields.map((field) => field.name).sort(),
      Object.keys(buildSchema(readFileSync("schema/contract.graphql", "utf8")).getMutationType().getFields()).sort());

    let dashboardRequests = 0;
    let delayNext = false;
    let failNext = false;
    let releaseDelayed = () => {};
    let delayedStarted = () => {};
    await page.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (!request.query?.includes("ProjectDashboard")) {
        await route.continue();
        return;
      }
      dashboardRequests += 1;
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
          // The route change aborts a stale refresh before the delayed response can reach React.
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
    assert.equal(dashboardRequests, 0);
    await page.evaluate(() => {
      Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await page.getByText(/^Last successful dashboard refresh at .* \(.* seconds ago\)\.$/).waitFor();
    await page.waitForTimeout(100);
    assert.equal(dashboardRequests, 1);

    const singleFlightStart = new Promise((resolve) => { delayedStarted = resolve; });
    const beforeSingleFlight = dashboardRequests;
    delayNext = true;
    await page.evaluate(() => {
      window.dispatchEvent(new Event("focus"));
      window.dispatchEvent(new Event("focus"));
    });
    await singleFlightStart;
    assert.equal(dashboardRequests, beforeSingleFlight + 1);
    releaseDelayed();
    await page.getByText(/^Last successful dashboard refresh at .* \(.* seconds ago\)\.$/).waitFor();

    failNext = true;
    await page.evaluate(() => window.dispatchEvent(new Event("online")));
    await page.getByText(/^We could not refresh this dashboard. Stale data from .* remains visible\.$/).waitFor();
    await page.getByRole("heading", { name: "Customer Feedback Copilot" }).waitFor();
    const refreshDelays = (await page.evaluate(() => window.__dashboardTimeouts))
      .filter((delay) => delay >= 15_000 && delay <= 30_000);
    assert(refreshDelays.some((delay) => delay === 22_500));
    assert(refreshDelays.some((delay) => delay === 25_000));

    const requestsBeforeOffline = dashboardRequests;
    await page.evaluate(() => window.dispatchEvent(new Event("offline")));
    await page.getByText(/^Offline. Dashboard data remains from .*\.$/).waitFor();
    await page.waitForTimeout(100);
    assert.equal(dashboardRequests, requestsBeforeOffline);
    await page.evaluate(() => window.dispatchEvent(new Event("online")));
    await page.waitForTimeout(100);
    assert.equal(dashboardRequests, requestsBeforeOffline + 1);

    await client.query("UPDATE organization_memberships SET ended_at = CURRENT_TIMESTAMP WHERE id = $1", [alphaMembership]);
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await page.getByRole("heading", { name: "Access denied" }).waitFor();
    await page.getByRole("heading", { name: "Customer Feedback Copilot" }).waitFor({ state: "detached" });
    await dashboard.close();
    await client.query("UPDATE organization_memberships SET ended_at = NULL WHERE id = $1", [alphaMembership]);

    const stale = await authenticatedContext(browser);
    const stalePage = await stale.newPage();
    let staleRelease = () => {};
    let staleStarted = () => {};
    const staleRefreshStarted = new Promise((resolve) => { staleStarted = resolve; });
    let holdRefresh = false;
    await stalePage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (!request.query?.includes("ProjectDashboard") || !holdRefresh) {
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
        // Navigation aborts this delayed result, which cannot replace the new target's dashboard.
      }
    });
    await stalePage.goto(dashboardPath(commandConsole));
    await stalePage.getByRole("heading", { name: "Customer Feedback Copilot" }).waitFor();
    holdRefresh = true;
    await stalePage.evaluate(() => window.dispatchEvent(new Event("focus")));
    await staleRefreshStarted;
    await stalePage.goto(dashboardPath(allZeroProject));
    await stalePage.getByRole("heading", { name: "All-zero dashboard" }).waitFor();
    staleRelease();
    await stalePage.waitForTimeout(100);
    await stalePage.getByRole("heading", { name: "Customer Feedback Copilot" }).waitFor({ state: "detached" });
    await stale.close();

    const setup = await authenticatedContext(browser);
    const setupPage = await setup.newPage();
    await setupPage.goto(dashboardPath(allZeroProject));
    await setupPage.getByRole("heading", { name: "Project setup guidance" }).waitFor();
    await setupPage.getByText("Unknown").waitFor();
    await setupPage.getByText("No governed current-period cost source is available.").waitFor();
    assert.equal(await setupPage.locator("main.project-dashboard").getByRole("button").count(), 0,
      "setup guidance remains read-only");
    assert.equal(await setupPage.getByRole("link", { name: "Preferences" }).count(), 1);
    await setup.close();

    const unavailableCost = await authenticatedContext(browser);
    const unavailableCostPage = await unavailableCost.newPage();
    await unavailableCostPage.goto(dashboardPath(unavailableCostProject));
    await unavailableCostPage.getByRole("heading", { name: "Unavailable cost dashboard" }).waitFor();
    await unavailableCostPage.getByText("Unavailable", { exact: true }).waitFor();
    await unavailableCostPage.getByText("The governed current-period cost source reported unavailable.").waitFor();
    await unavailableCost.close();

    const unavailable = await authenticatedContext(browser);
    const unavailablePage = await unavailable.newPage();
    for (const projectId of [privateProject, revokedProject, "50000000-0000-0000-0000-000000000099", "not-a-uuid"]) {
      await expectUnavailable(unavailablePage, projectId);
    }
    await unavailable.close();

    const initialError = await authenticatedContext(browser);
    const initialErrorPage = await initialError.newPage();
    let failInitial = true;
    await initialErrorPage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (failInitial && request.query?.includes("query ProjectDashboard")) {
        failInitial = false;
        await route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ errors: [{ message: "unexpected" }] }) });
        return;
      }
      await route.continue();
    });
    await initialErrorPage.goto(dashboardPath(commandConsole));
    await initialErrorPage.getByText("We could not load this project dashboard. Try again.").waitFor();
    await initialErrorPage.getByRole("button", { name: "Retry" }).click();
    await initialErrorPage.getByRole("heading", { name: "Customer Feedback Copilot" }).waitFor();
    await initialError.close();

    const skeleton = await authenticatedContext(browser);
    const skeletonPage = await skeleton.newPage();
    let delayInitial = true;
    await skeletonPage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (delayInitial && request.query?.includes("query ProjectDashboard")) {
        delayInitial = false;
        const response = await route.fetch();
        await new Promise((resolve) => setTimeout(resolve, 300));
        await route.fulfill({ response });
        return;
      }
      await route.continue();
    });
    const skeletonNavigation = skeletonPage.goto(dashboardPath(commandConsole));
    await skeletonNavigation;
    await skeletonPage.getByRole("heading", { name: "Customer Feedback Copilot" }).waitFor();
    await skeleton.close();

    const anonymous = await browser.newContext();
    const anonymousPage = await anonymous.newPage();
    await anonymousPage.goto(dashboardPath(commandConsole));
    await anonymousPage.getByRole("heading", { name: "Session error" }).waitFor();
    await anonymous.close();
  } finally {
    await browser.close();
  }
} finally {
  await client.query("UPDATE organization_memberships SET ended_at = NULL WHERE id = $1", [alphaMembership]);
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [revokedMembership]);
  await client.query("DELETE FROM projects WHERE id = ANY($1::uuid[])", [[allZeroProject, revokedProject, unavailableCostProject]]);
  await client.query("DELETE FROM organizations WHERE id = $1", [revokedOrganization]);
  await client.end();
  await service.stop();
  await database.drop();
}
