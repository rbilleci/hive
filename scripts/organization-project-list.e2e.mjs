import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const privateOrganization = "10000000-0000-0000-0000-000000000004";
const emptyOrganization = "74100000-0000-0000-0000-000000000001";
const emptyMembership = "74100000-0000-0000-0000-000000000002";
const revokedOrganization = "74100000-0000-0000-0000-000000000003";
const revokedMembership = "74100000-0000-0000-0000-000000000004";
const port = 18086;
const origin = "http://127.0.0.1:" + port;
const alphaDirectory = origin + "/organizations/" + alpha + "/projects";
// The default page size is 10; the rest of this spec was authored around a clean 25-then-7
// split of the 32-row fixture, so every navigation past the initial default-size check opts
// into pageSize=25 explicitly rather than recomputing alphabetical page boundaries for size 10.
const alphaDirectoryPaged = alphaDirectory + "?pageSize=25";

const database = await createIsolatedDatabase("hive_organization_project_list_e2e");
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

async function expectUnavailable(page, organizationId) {
  await page.goto(origin + "/organizations/" + organizationId + "/projects");
  await page.getByRole("heading", { name: "Access denied" }).waitFor();
  assert.equal(await page.locator(".directory-table-scroll tbody tr").count(), 0);
  await page.getByText("Incident Response").waitFor({ state: "detached" });
}

try {
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) "
      + "SELECT ('74000000-0000-0000-0000-' || lpad(number::text, 12, '0'))::uuid, $1, "
      + "'directory-project-' || lpad(number::text, 3, '0'), 'Directory project ' || lpad(number::text, 3, '0'), "
      + "CASE WHEN number % 2 = 0 THEN 'ARCHIVED' ELSE 'ACTIVE' END "
      + "FROM generate_series(1, 28) AS fixture(number) ON CONFLICT (id) DO NOTHING",
    [alpha]
  );
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES "
      + "('74000000-0000-0000-0000-000000000101', $1, 'literal-percent-underscore', 'Literal %_ Signal', 'ACTIVE'), "
      + "('74000000-0000-0000-0000-000000000102', $1, 'literal-near-match', 'Literal ZZ Signal', 'ACTIVE') "
      + "ON CONFLICT (id) DO NOTHING",
    [alpha]
  );
  await client.query(
    "INSERT INTO organizations (id, slug, display_name, lifecycle_status) VALUES "
      + "($1, 'empty-project-directory', 'Empty project directory', 'ACTIVE'), "
      + "($2, 'revoked-project-directory', 'Revoked project directory', 'ACTIVE') ON CONFLICT (id) DO NOTHING",
    [emptyOrganization, revokedOrganization]
  );
  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES "
      + "($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL), "
      + "($4, $5, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', NULL) ON CONFLICT (id) DO NOTHING",
    [emptyMembership, emptyOrganization, ada, revokedMembership, revokedOrganization]
  );
  await client.query("UPDATE organization_memberships SET ended_at = CURRENT_TIMESTAMP WHERE id = $1", [revokedMembership]);

  const browser = await launchBrowser();
  try {
    const directory = await authenticatedContext(browser);
    const page = await directory.newPage();
    await page.goto(alphaDirectory);
    await page.getByRole("heading", { name: "Projects" }).waitFor();
    assert.equal(await page.getByLabel("Rows per page").inputValue(), "10");
    assert.match(await page.locator("main.project-directory").innerText(), /10 of 32 projects shown/);
    await page.getByLabel("Rows per page").selectOption("25");
    assert.match(page.url(), /pageSize=25/);
    await page.getByText("25 of 32 projects shown").waitFor();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Customer Feedback Copilot" }).waitFor();
    const commandRow = page.locator(".directory-table-scroll tbody tr").filter({ hasText: "Customer Feedback Copilot" });
    assert.equal((await commandRow.innerText()).replace(/\s+/g, " ").trim(), "Customer Feedback Copilot customer-feedback-copilot ACTIVE");
    assert.equal(await page.getByText(alpha, { exact: false }).count(), 0);
    const projectDirectory = page.locator(".directory-table-scroll");
    assert.equal(await projectDirectory.getByRole("link", { name: /dashboard|settings|metrics/i }).count(), 0);
    assert.equal(await projectDirectory.getByRole("button", { name: /create|edit|archive|restore/i }).count(), 0);
    const firstPageNames = await page.locator(".directory-table-scroll tbody th").allTextContents();
    assert.deepEqual(firstPageNames, [...firstPageNames].sort());
    assert.equal(await page.getByRole("button", { name: "Previous" }).isDisabled(), true);
    assert.equal(await page.getByRole("button", { name: "Next" }).isDisabled(), false);

    await page.getByLabel("Lifecycle").selectOption("ARCHIVED");
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Usage Analytics" }).waitFor();
    assert.match(page.url(), /lifecycle=ARCHIVED/);
    assert.equal(await page.locator(".directory-table-scroll tbody tr").count(), 15);
    await page.getByLabel("Search projects").fill("uSaGe");
    // "Usage Analytics" already matches the lifecycle-only filter, so waiting for it to render is
    // not proof the debounced search term reached the URL yet; poll the URL itself instead.
    await page.waitForURL(/lifecycle=ARCHIVED&search=uSaGe/);
    await page.getByText("1 of 1 projects shown").waitFor();
    assert.equal(await page.locator(".directory-table-scroll tbody tr").count(), 1);

    await page.getByLabel("Lifecycle").selectOption("");
    await page.getByLabel("Search projects").fill("%_");
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Literal %_ Signal" }).waitFor();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor({ state: "detached" });
    await page.getByLabel("Search projects").fill("not present");
    await page.getByText("No projects match these filters.").waitFor();

    await page.goto(alphaDirectoryPaged);
    await page.getByRole("button", { name: "Next" }).click();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor();
    assert.match(await page.locator("main.project-directory").innerText(), /7 of 32 projects shown/);
    assert.equal(await page.getByRole("button", { name: "Previous" }).isDisabled(), false);
    assert.equal(await page.getByRole("button", { name: "Next" }).isDisabled(), true);
    await page.getByRole("button", { name: "Previous" }).click();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Customer Feedback Copilot" }).waitFor();
    assert.match(await page.locator("main.project-directory").innerText(), /25 of 32 projects shown/);
    await page.getByRole("button", { name: "Next" }).click();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor();
    await page.getByLabel("Search projects").fill("copilot");
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Customer Feedback Copilot" }).waitFor();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor({ state: "detached" });
    assert.match(await page.locator("main.project-directory").innerText(), /1 of 1 projects shown/);
    assert.doesNotMatch(page.url(), /after=|before=/);
    await directory.close();

    const continuationFailure = await authenticatedContext(browser);
    const failurePage = await continuationFailure.newPage();
    let failedContinuation = false;
    await failurePage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (request.variables?.after && !failedContinuation) {
        failedContinuation = true;
        await route.fulfill({
          status: 500,
          contentType: "application/json",
          body: JSON.stringify({ errors: [{ message: "unexpected" }] })
        });
        return;
      }
      await route.continue();
    });
    await failurePage.goto(alphaDirectoryPaged);
    await failurePage.locator(".directory-table-scroll").getByRole("link", { name: "Customer Feedback Copilot" }).waitFor();
    await failurePage.getByRole("button", { name: "Next" }).click();
    await failurePage.getByText("We could not refresh this list. Results shown may be stale.").waitFor();
    await failurePage.locator(".directory-table-scroll").getByRole("link", { name: "Customer Feedback Copilot" }).waitFor();
    await failurePage.getByRole("button", { name: "Next" }).click();
    await failurePage.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor();
    const loadedNames = await failurePage.locator(".directory-table-scroll tbody th").allTextContents();
    assert.equal(new Set(loadedNames).size, loadedNames.length);
    await continuationFailure.close();

    const stale = await authenticatedContext(browser);
    const stalePage = await stale.newPage();
    let delayContinuation = true;
    await stalePage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (request.variables?.after && delayContinuation) {
        delayContinuation = false;
        const response = await route.fetch();
        await new Promise((resolve) => setTimeout(resolve, 500));
        try {
          await route.fulfill({ response });
        } catch {
          // The filter change aborts this stale continuation before it can reach the page.
        }
        return;
      }
      await route.continue();
    });
    await stalePage.goto(alphaDirectoryPaged);
    const continuationRequest = stalePage.waitForRequest((request) => {
      const body = JSON.parse(request.postData() ?? "{}");
      return request.url().endsWith("/graphql") && Boolean(body.variables?.after);
    });
    await stalePage.getByRole("button", { name: "Next" }).click();
    await continuationRequest;
    await stalePage.getByLabel("Lifecycle").selectOption("ARCHIVED");
    await stalePage.locator(".directory-table-scroll").getByRole("link", { name: "Usage Analytics" }).waitFor();
    await stalePage.waitForTimeout(750);
    await stalePage.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor({ state: "detached" });
    await stale.close();

    const empty = await authenticatedContext(browser);
    const emptyPage = await empty.newPage();
    await emptyPage.goto(origin + "/organizations/" + emptyOrganization + "/projects");
    await emptyPage.getByText("This organization has no projects.").waitFor();
    await empty.close();

    const unavailable = await authenticatedContext(browser);
    const unavailablePage = await unavailable.newPage();
    for (const organizationId of [
      privateOrganization,
      revokedOrganization,
      "10000000-0000-0000-0000-000000000099",
      "not-a-uuid"
    ]) {
      await expectUnavailable(unavailablePage, organizationId);
    }
    await unavailable.close();

    const initialError = await authenticatedContext(browser);
    const initialErrorPage = await initialError.newPage();
    let failInitialRequest = true;
    await initialErrorPage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (failInitialRequest && request.query?.includes("query OrganizationProjects")) {
        failInitialRequest = false;
        await route.fulfill({
          status: 500,
          contentType: "application/json",
          body: JSON.stringify({ errors: [{ message: "unexpected" }] })
        });
        return;
      }
      await route.continue();
    });
    await initialErrorPage.goto(alphaDirectory);
    await initialErrorPage.getByText("We could not load this project directory. Try again.").waitFor();
    await initialErrorPage.getByRole("button", { name: "Retry" }).click();
    await initialErrorPage.getByRole("heading", { name: "Projects" }).waitFor();
    await initialError.close();

    const skeleton = await authenticatedContext(browser);
    const skeletonPage = await skeleton.newPage();
    let delayInitial = true;
    await skeletonPage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (delayInitial && request.query?.includes("query OrganizationProjects")) {
        delayInitial = false;
        const response = await route.fetch();
        await new Promise((resolve) => setTimeout(resolve, 500));
        await route.fulfill({ response });
        return;
      }
      await route.continue();
    });
    const skeletonNavigation = skeletonPage.goto(alphaDirectory);
    await skeletonPage.getByLabel("Loading project directory").waitFor();
    await skeletonNavigation;
    await skeletonPage.getByRole("heading", { name: "Projects" }).waitFor();
    await skeleton.close();

    const anonymous = await browser.newContext();
    const anonymousPage = await anonymous.newPage();
    await anonymousPage.goto(alphaDirectory);
    await anonymousPage.getByRole("heading", { name: "Session error" }).waitFor();
    await anonymous.close();
  } finally {
    await browser.close();
  }
} finally {
  await client.query("DELETE FROM projects WHERE id::text LIKE '74000000-0000-0000-0000-%'");
  await client.query("DELETE FROM organization_memberships WHERE id = ANY($1::uuid[])",
    [[emptyMembership, revokedMembership]]);
  await client.query("DELETE FROM organizations WHERE id = ANY($1::uuid[])", [[emptyOrganization, revokedOrganization]]);
  await client.end();
  await service.stop();
  await database.drop();
}
