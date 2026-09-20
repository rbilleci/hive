import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

// A request for any page after the first: the console sends Seaography `pagination: { page: { limit, page } }`.
const laterPage = (request) => (request.variables?.pagination?.page?.page ?? 0) > 0;

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const emptyOrganization = "70000000-0000-0000-0000-000000000001";
const emptyMembership = "70000000-0000-0000-0000-000000000002";
const port = 18084;
const origin = "http://127.0.0.1:" + port;
const database = await createIsolatedDatabase("hive_organization_overview_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);

async function authenticatedContext(browser) {
  const context = await browser.newContext();
  await context.addCookies([{
    name: "sf_session",
    value: service.signFixtureSession(ada),
    url: origin,
    httpOnly: true,
    sameSite: "Lax"
  }]);
  return context;
}

try {
  await client.query(
    "INSERT INTO organizations (id, slug, display_name, lifecycle_status) VALUES ($1, 'empty-overview', 'Empty Overview', 'ACTIVE') ON CONFLICT (id) DO NOTHING",
    [emptyOrganization]
  );
  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP, NULL) ON CONFLICT (id) DO NOTHING",
    [emptyMembership, emptyOrganization, ada]
  );
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) "
      + "SELECT ('71000000-0000-0000-0000-' || lpad(number::text, 12, '0'))::uuid, $1, "
      + "'page-project-' || lpad(number::text, 3, '0'), 'Zone project ' || lpad(number::text, 3, '0'), 'ACTIVE' "
      + "FROM generate_series(1, 25) AS fixture(number) ON CONFLICT (id) DO NOTHING",
    [alpha]
  );
  const browser = await launchBrowser();
  try {
    const overview = await authenticatedContext(browser);
    const page = await overview.newPage();
    await page.goto(origin + "/organizations/" + alpha);
    await page.getByRole("heading", { name: "Product" }).waitFor();
    await assert.doesNotReject(() => page.locator(".organization-overview").getByText(alpha).waitFor());
    await assert.doesNotReject(() => page.locator(".organization-overview").getByText("ID: product").waitFor());
    await assert.doesNotReject(() => page.locator(".organization-overview").getByText("Customer Feedback Copilot").waitFor());
    await assert.doesNotReject(() => page.locator(".organization-overview").getByText("ARCHIVED").waitFor());
    const firstPageNames = await page.locator(".project-list h3").allTextContents();
    assert.deepEqual(firstPageNames, [...firstPageNames].sort());
    await page.getByRole("button", { name: "Load more projects" }).click();
    await page.locator(".organization-overview").getByText("Zone project 025").waitFor();
    await overview.close();

    const empty = await authenticatedContext(browser);
    const emptyPage = await empty.newPage();
    await emptyPage.goto(origin + "/organizations/" + emptyOrganization);
    await assert.doesNotReject(() => emptyPage.locator(".organization-overview").getByText("This organization has no projects.").waitFor());
    await empty.close();

    const unavailable = await authenticatedContext(browser);
    const unavailablePage = await unavailable.newPage();
    for (const organizationId of [privateOrganization, "not-a-uuid", "10000000-0000-0000-0000-000000000099"]) {
      await unavailablePage.goto(origin + "/organizations/" + organizationId);
      await unavailablePage.getByRole("heading", { name: "Access denied" }).waitFor();
      await unavailablePage.getByText("Incident Response").waitFor({ state: "detached" });
    }
    await unavailable.close();

    const error = await authenticatedContext(browser);
    const errorPage = await error.newPage();
    let firstRequest = true;
    await errorPage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (firstRequest && request.query?.includes("query OrganizationOverview")) {
        firstRequest = false;
        await route.fulfill({
          status: 500,
          contentType: "application/json",
          body: JSON.stringify({ errors: [{ message: "unexpected" }] })
        });
        return;
      }
      await route.continue();
    });
    await errorPage.goto(origin + "/organizations/" + alpha);
    await errorPage.locator(".organization-overview").getByText("We could not load this organization. Try again.").waitFor();
    await errorPage.getByRole("button", { name: "Retry" }).click();
    await errorPage.getByRole("heading", { name: "Product" }).waitFor();
    await error.close();

    const anonymous = await browser.newContext();
    const anonymousPage = await anonymous.newPage();
    await anonymousPage.goto(origin + "/organizations/" + alpha);
    await anonymousPage.getByRole("heading", { name: "Session error" }).waitFor();
    await anonymous.close();

    const race = await authenticatedContext(browser);
    const racePage = await race.newPage();
    let releaseContinuation;
    const continuationReleased = new Promise((resolve) => { releaseContinuation = resolve; });
    let continuationStarted;
    const continuationStartedPromise = new Promise((resolve) => { continuationStarted = resolve; });
    await racePage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (laterPage(request)) {
        const response = await route.fetch();
        continuationStarted();
        await continuationReleased;
        try {
          await route.fulfill({ response });
        } catch {
          // Navigation aborts a stale continuation before it can update the overview.
        }
        return;
      }
      await route.continue();
    });
    await racePage.goto(origin + "/organizations/" + alpha);
    await racePage.getByRole("button", { name: "Load more projects" }).click();
    await continuationStartedPromise;
    await racePage.goto(origin + "/organizations/" + emptyOrganization);
    await racePage.locator(".organization-overview").getByText("This organization has no projects.").waitFor();
    releaseContinuation();
    await racePage.waitForTimeout(250);
    await racePage.locator(".organization-overview").getByText("Zone project 025").waitFor({ state: "detached" });
    await race.close();
  } finally {
    await browser.close();
  }
} finally {
  await client.query("DELETE FROM projects WHERE id::text LIKE '71000000-0000-0000-0000-%'");
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [emptyMembership]);
  await client.query("DELETE FROM organizations WHERE id = $1", [emptyOrganization]);
  await client.end();
  await service.stop();
  await database.drop();
}
