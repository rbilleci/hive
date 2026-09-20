import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const noMembership = "00000000-0000-0000-0000-000000000099";
const port = 18082;
const origin = "http://127.0.0.1:" + port;
const database = await createIsolatedDatabase("hive_organization_selector_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
await client.query(
  "INSERT INTO organizations (id, slug, display_name, lifecycle_status) "
    + "SELECT ('30000000-0000-0000-0000-' || lpad(number::text, 12, '0'))::uuid, "
    + "'page-fixture-' || lpad(number::text, 3, '0'), "
    + "'Program fixture ' || lpad(number::text, 3, '0'), 'ACTIVE' "
    + "FROM generate_series(1, 51) AS fixture(number) ON CONFLICT (id) DO NOTHING"
);
await client.query(
  "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) "
    + "SELECT ('40000000-0000-0000-0000-' || lpad(number::text, 12, '0'))::uuid, "
    + "('30000000-0000-0000-0000-' || lpad(number::text, 12, '0'))::uuid, $1, CURRENT_TIMESTAMP, NULL "
    + "FROM generate_series(1, 51) AS fixture(number) ON CONFLICT (id) DO NOTHING",
  [ada]
);
await client.query(
  "INSERT INTO organizations (id, slug, display_name, lifecycle_status) "
    + "VALUES ($1, 'archived-race-fixture', 'Archived race fixture', 'ARCHIVED') ON CONFLICT (id) DO NOTHING",
  ["30000000-0000-0000-0000-000000000099"]
);
await client.query(
  "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) "
    + "VALUES ($1, $2, $3, CURRENT_TIMESTAMP, NULL) ON CONFLICT (id) DO NOTHING",
  [
    "40000000-0000-0000-0000-000000000099",
    "30000000-0000-0000-0000-000000000099",
    ada
  ]
);
const browser = await launchBrowser();
try {
  const authenticated = await browser.newContext();
  await authenticated.addCookies([{
    name: "sf_session",
    value: service.signFixtureSession(ada),
    url: origin,
    httpOnly: true,
    sameSite: "Lax"
  }]);
  const page = await authenticated.newPage();
  await page.goto(origin + "/organizations");
  await page.locator(".organization-list").getByRole("link", { name: "Product" }).waitFor();
  await assert.doesNotReject(() => page.locator(".organization-list").getByText("Quality Assurance").waitFor({ state: "detached", timeout: 100 }));
  await page.getByRole("checkbox", { name: "Include archived organizations" }).check();
  await page.getByRole("button", { name: "Load more organizations" }).click();
  await page.locator(".organization-list").getByRole("link", { name: "Program fixture 051" }).waitFor();
  await page.locator(".organization-list").getByRole("link", { name: /Quality Assurance/ }).waitFor();
  await page.locator(".organization-list").getByRole("link", { name: "Program fixture 051" }).click();
  await page.waitForURL(origin + "/organizations/30000000-0000-0000-0000-000000000051");
  await assert.doesNotReject(() => page.getByRole("heading", { name: "Program fixture 051" }).waitFor());
  await authenticated.close();

  const race = await browser.newContext();
  await race.addCookies([{ name: "sf_session", value: service.signFixtureSession(ada), url: origin }]);
  const racePage = await race.newPage();
  let releaseContinuation;
  const continuationReleased = new Promise((resolve) => { releaseContinuation = resolve; });
  let continuationStarted;
  const continuationStartedPromise = new Promise((resolve) => { continuationStarted = resolve; });
  await racePage.route("**/graphql", async (route) => {
    const body = JSON.parse(route.request().postData() ?? "{}");
    if (body.variables?.after && body.variables.filter?.includeArchived === false) {
      const response = await route.fetch();
      continuationStarted();
      await continuationReleased;
      try {
        await route.fulfill({ response });
      } catch {
        // Filter changes abort the browser request before this delayed response can commit.
      }
      return;
    }
    await route.continue();
  });
  await racePage.goto(origin + "/organizations");
  await racePage.locator(".organization-list").getByRole("link", { name: "Product" }).waitFor();
  await racePage.getByRole("button", { name: "Load more organizations" }).click();
  await continuationStartedPromise;
  await racePage.getByRole("checkbox", { name: "Include archived organizations" }).check();
  await racePage.getByRole("link", { name: /Archived race fixture/ }).waitFor();
  releaseContinuation();
  await racePage.waitForTimeout(250);
  await assert.doesNotReject(() => racePage.getByRole("link", { name: /Archived race fixture/ }).waitFor());
  await race.close();

  const empty = await browser.newContext();
  await empty.addCookies([{ name: "sf_session", value: service.signFixtureSession(noMembership), url: origin }]);
  const emptyPage = await empty.newPage();
  await emptyPage.goto(origin + "/organizations");
  await assert.doesNotReject(() => emptyPage.getByText("You do not have access to any organizations.").waitFor());
  await empty.close();

  const anonymous = await browser.newContext();
  const anonymousPage = await anonymous.newPage();
  await anonymousPage.goto(origin + "/organizations");
  await assert.doesNotReject(() => anonymousPage.getByRole("alert").waitFor());
  await anonymous.close();

  const rejected = await browser.newContext();
  await rejected.addCookies([{
    name: "sf_session",
    value: ada + ".incorrect-signature",
    url: origin
  }]);
  const rejectedPage = await rejected.newPage();
  await rejectedPage.goto(origin + "/organizations");
  await assert.doesNotReject(() => rejectedPage.getByRole("alert").waitFor());
  await rejected.close();
} finally {
  await browser.close();
  await client.end();
  await service.stop();
  await database.drop();
}
