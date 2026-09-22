import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

// A request for any page after the first: the console sends Seaography `pagination: { page: { limit, page } }`.
const laterPage = (request) => (request.variables?.pagination?.page?.page ?? 0) > 0;

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const alphaMembership = "20000000-0000-0000-0000-000000000001";
const commandConsole = "50000000-0000-0000-0000-000000000001";
const privateProject = "50000000-0000-0000-0000-000000000003";
const emptyProject = "64000000-0000-0000-0000-000000000001";
const revokedOrganization = "64000000-0000-0000-0000-000000000002";
const revokedProject = "64000000-0000-0000-0000-000000000003";
const revokedMembership = "64000000-0000-0000-0000-000000000004";
const port = 18090;
const origin = "http://127.0.0.1:" + port;
const alphaDirectory = origin + "/projects/" + commandConsole + "/agents";
// The default page size is 10; the rest of this spec was authored around a clean 25-then-10
// split of the 35-row fixture, so every navigation past the initial default-size check opts
// into pageSize=25 explicitly rather than recomputing alphabetical page boundaries for size 10.
const alphaDirectoryPaged = alphaDirectory + "?pageSize=25";

const database = await createIsolatedDatabase("hive_project_agent_list_e2e");
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

async function expectUnavailable(page, projectId) {
  await page.goto(origin + "/projects/" + projectId + "/agents");
  await page.getByRole("heading", { name: "Access denied" }).waitFor();
  assert.equal(await page.locator(".directory-table-scroll tbody tr").count(), 0);
  await page.getByText("Incident Triage Agent").waitFor({ state: "detached" });
}

try {
  await client.query(
    "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) "
      + "SELECT ('62000000-0000-0000-0000-' || lpad(number::text, 12, '0'))::uuid, $1, "
      + "'agent-page-' || lpad(number::text, 3, '0'), 'Agent Page ' || lpad(number::text, 3, '0'), "
      + "CASE WHEN number % 3 = 0 THEN 'ARCHIVED' WHEN number % 2 = 0 THEN 'DEPRECATED' ELSE 'ACTIVE' END "
      + "FROM generate_series(1, 28) AS fixture(number) ON CONFLICT (id) DO NOTHING",
    [commandConsole]
  );
  await client.query(
    "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES "
      + "('62000000-0000-0000-0000-000000000101', $1, 'literal-percent-underscore', 'Literal %_ Signal', 'ACTIVE'), "
      + "('62000000-0000-0000-0000-000000000102', $1, 'literal-near-match', 'Literal ZZ Signal', 'ACTIVE'), "
      + "('62000000-0000-0000-0000-000000000103', $1, 'duplicate-earlier', 'Same Agent', 'ACTIVE'), "
      + "('62000000-0000-0000-0000-000000000104', $1, 'duplicate-later', 'Same Agent', 'ACTIVE') "
      + "ON CONFLICT (id) DO NOTHING",
    [commandConsole]
  );
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'empty-agent-directory', 'Empty agent directory', 'ACTIVE')",
    [emptyProject, alpha]
  );
  await client.query(
    "INSERT INTO organizations (id, slug, display_name, lifecycle_status) VALUES ($1, 'revoked-agent-directory', 'Revoked agent directory', 'ACTIVE')",
    [revokedOrganization]
  );
  await client.query(
    "INSERT INTO projects (id, organization_id, slug, display_name, lifecycle_status) VALUES ($1, $2, 'revoked-agent-directory', 'Revoked agent directory', 'ACTIVE')",
    [revokedProject, revokedOrganization]
  );
  await client.query(
    "INSERT INTO agents (id, project_id, slug, display_name, lifecycle_status) VALUES ('64000000-0000-0000-0000-000000000005', $1, 'revoked-agent', 'Revoked Agent', 'ACTIVE')",
    [revokedProject]
  );
  await client.query(
    "INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, ended_at) VALUES ($1, $2, $3, CURRENT_TIMESTAMP - INTERVAL '1 hour', CURRENT_TIMESTAMP)",
    [revokedMembership, revokedOrganization, ada]
  );

  const browser = await launchBrowser();
  try {
    const directory = await authenticatedContext(browser);
    const page = await directory.newPage();
    await page.goto(alphaDirectory);
    await page.getByRole("heading", { name: "Agents" }).waitFor();
    assert.equal(await page.getByLabel("Rows per page").inputValue(), "10");
    assert.match(await page.locator("main.agent-directory").innerText(), /10 of 35 agents shown/);
    await page.getByLabel("Rows per page").selectOption("25");
    assert.match(page.url(), /pageSize=25/);
    await page.getByText("25 of 35 agents shown").waitFor();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Agent Page 001" }).waitFor();
    const agentRow = page.locator(".directory-table-scroll tbody tr").filter({ hasText: "Agent Page 001" });
    assert.equal(
      (await agentRow.innerText()).replace(/\s+/g, " ").trim(),
      "Agent Page 001 agent-page-001 Not published Not published ACTIVE"
    );
    const agentLink = page.locator(".directory-table-scroll").getByRole("link", { name: "Agent Page 001" });
    assert.equal(await agentLink.getAttribute("href"), "/projects/" + commandConsole + "/agents/62000000-0000-0000-0000-000000000001");
    assert.equal(await page.getByText(commandConsole, { exact: false }).count(), 0);
    assert.equal(await page.locator("main.agent-directory").getByRole("button", {
      name: /create|edit|archive|restore|deploy/i
    }).count(), 0, "the read-only directory content has no mutation controls");
    const firstPageNames = await page.locator(".directory-table-scroll tbody th").allTextContents();
    assert.deepEqual(firstPageNames.map((name) => name.toLowerCase()), [...firstPageNames.map((name) => name.toLowerCase())].sort());
    assert.match(await page.locator("main.agent-directory").innerText(), /25 of 35 agents shown/);
    assert.equal(await page.getByRole("button", { name: "Previous" }).isDisabled(), true);
    assert.equal(await page.getByRole("button", { name: "Next" }).isDisabled(), false);

    await page.getByLabel("Lifecycle").selectOption("DEPRECATED");
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Sentiment Analyst" }).waitFor();
    assert.match(page.url(), /lifecycle=DEPRECATED/);
    await page.getByLabel("Search agents").fill("sEnTiMeNt");
    // "Sentiment Analyst" already matches the lifecycle-only filter, so waiting for it to render is not
    // proof the debounced search term reached the URL yet; poll the URL itself instead of snapshotting it.
    await page.waitForURL(/lifecycle=DEPRECATED&search=sEnTiMeNt/);
    await page.getByText("1 of 1 agents shown").waitFor();

    await page.getByLabel("Lifecycle").selectOption("");
    await page.getByLabel("Search agents").fill("%_");
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Literal %_ Signal" }).waitFor();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor({ state: "detached" });
    await page.getByLabel("Search agents").fill("not present");
    await page.getByText("No agents match these filters.").waitFor();

    await page.goto(alphaDirectoryPaged);
    await page.getByRole("button", { name: "Next" }).click();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor();
    assert.match(await page.locator("main.agent-directory").innerText(), /10 of 35 agents shown/);
    assert.match(page.url(), /[?&]page=2\b/);
    assert.equal(await page.getByRole("button", { name: "Previous" }).isDisabled(), false);
    assert.equal(await page.getByRole("button", { name: "Next" }).isDisabled(), true);
    await page.getByRole("button", { name: "Previous" }).click();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Agent Page 001" }).waitFor();
    assert.doesNotMatch(page.url(), /[?&]page=/);
    assert.match(await page.locator("main.agent-directory").innerText(), /25 of 35 agents shown/);
    await page.getByRole("button", { name: "Next" }).click();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor();
    await page.getByLabel("Search agents").fill("triage");
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Feedback Triage Agent" }).waitFor();
    await page.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor({ state: "detached" });
    assert.match(await page.locator("main.agent-directory").innerText(), /1 of 1 agents shown/);
    assert.doesNotMatch(page.url(), /[?&]page=/);

    const schema = await page.evaluate(async () => {
      const response = await fetch("/graphql", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          query: "query ProjectAgentSchema { __schema { mutationType { name } queryType { fields { name } } } }"
        })
      });
      return response.json();
    });
    assert.deepEqual(schema.data.__schema.mutationType, { name: "Mutation" });
    // The directories are Seaography's generated, tenant-scoped fields; the hand-built roots are gone.
    const rootFields = schema.data.__schema.queryType.fields.map((field) => field.name);
    for (const generated of ["organizations", "projects", "agents", "agentVersions"]) assert(rootFields.includes(generated), generated);
    for (const handBuilt of ["accessibleOrganizations", "organization", "project"]) assert(!rootFields.includes(handBuilt), handBuilt);
    await directory.close();

    const continuationFailure = await authenticatedContext(browser);
    const failurePage = await continuationFailure.newPage();
    let failedContinuation = false;
    await failurePage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (laterPage(request) && !failedContinuation) {
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
    await failurePage.locator(".directory-table-scroll").getByRole("link", { name: "Agent Page 001" }).waitFor();
    await failurePage.getByRole("button", { name: "Next" }).click();
    await failurePage.getByText("We could not refresh this list. Results shown may be stale.").waitFor();
    await failurePage.locator(".directory-table-scroll").getByRole("link", { name: "Agent Page 001" }).waitFor();
    await failurePage.getByRole("button", { name: "Next" }).click();
    await failurePage.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor();
    const loadedHrefs = await failurePage.locator(".directory-table-scroll a").evaluateAll((links) => links.map((link) => link.getAttribute("href")));
    assert.equal(new Set(loadedHrefs).size, loadedHrefs.length);
    await continuationFailure.close();

    const stale = await authenticatedContext(browser);
    const stalePage = await stale.newPage();
    let delayContinuation = true;
    await stalePage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (laterPage(request) && delayContinuation) {
        delayContinuation = false;
        const response = await route.fetch();
        await new Promise((resolve) => setTimeout(resolve, 500));
        try {
          await route.fulfill({ response });
        } catch {
          // The filter change aborts this stale continuation before it can reach the console.
        }
        return;
      }
      await route.continue();
    });
    await stalePage.goto(alphaDirectoryPaged);
    const continuationRequest = stalePage.waitForRequest((request) => {
      const body = JSON.parse(request.postData() ?? "{}");
      return request.url().endsWith("/graphql") && laterPage(body);
    });
    await stalePage.getByRole("button", { name: "Next" }).click();
    await continuationRequest;
    await stalePage.getByLabel("Lifecycle").selectOption("ARCHIVED");
    await stalePage.locator(".directory-table-scroll").getByRole("link", { name: "Feedback Digest Scribe" }).waitFor();
    await stalePage.waitForTimeout(750);
    await stalePage.locator(".directory-table-scroll").getByRole("link", { name: "Literal ZZ Signal" }).waitFor({ state: "detached" });
    await stale.close();

    const empty = await authenticatedContext(browser);
    const emptyPage = await empty.newPage();
    await emptyPage.goto(origin + "/projects/" + emptyProject + "/agents");
    await emptyPage.getByText("This project has no agents.").waitFor();
    await empty.close();

    const unavailable = await authenticatedContext(browser);
    const unavailablePage = await unavailable.newPage();
    for (const projectId of [privateProject, revokedProject, "50000000-0000-0000-0000-000000000099", "not-a-uuid"]) {
      await expectUnavailable(unavailablePage, projectId);
    }
    await unavailable.close();

    const initialError = await authenticatedContext(browser);
    const initialErrorPage = await initialError.newPage();
    let failInitialRequest = true;
    await initialErrorPage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (failInitialRequest && request.query?.includes("query ProjectAgents")) {
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
    await initialErrorPage.getByText("We could not load this project agent directory. Try again.").waitFor();
    await initialErrorPage.getByRole("button", { name: "Retry" }).click();
    await initialErrorPage.getByRole("heading", { name: "Agents" }).waitFor();
    await initialError.close();

    const skeleton = await authenticatedContext(browser);
    const skeletonPage = await skeleton.newPage();
    let delayInitial = true;
    await skeletonPage.route("**/graphql", async (route) => {
      const request = JSON.parse(route.request().postData() ?? "{}");
      if (delayInitial && request.query?.includes("query ProjectAgents")) {
        delayInitial = false;
        const response = await route.fetch();
        await new Promise((resolve) => setTimeout(resolve, 500));
        await route.fulfill({ response });
        return;
      }
      await route.continue();
    });
    const skeletonNavigation = skeletonPage.goto(alphaDirectory);
    await skeletonPage.getByLabel("Loading project agent directory").waitFor();
    await skeletonNavigation;
    await skeletonPage.getByRole("heading", { name: "Agents" }).waitFor();
    await skeleton.close();

    const revocation = await authenticatedContext(browser);
    const revocationPage = await revocation.newPage();
    await revocationPage.goto(alphaDirectory);
    await revocationPage.locator(".directory-table-scroll").getByRole("link", { name: "Agent Page 001" }).waitFor();
    await client.query("UPDATE organization_memberships SET ended_at = CURRENT_TIMESTAMP WHERE id = $1", [alphaMembership]);
    await revocationPage.reload();
    await revocationPage.getByRole("heading", { name: "Access denied" }).waitFor();
    await revocationPage.locator(".directory-table-scroll").getByRole("link", { name: "Agent Page 001" }).waitFor({ state: "detached" });
    await client.query("UPDATE organization_memberships SET ended_at = NULL WHERE id = $1", [alphaMembership]);
    await revocation.close();

    const anonymous = await browser.newContext();
    const anonymousPage = await anonymous.newPage();
    await anonymousPage.goto(alphaDirectory);
    await anonymousPage.getByRole("heading", { name: "Session error" }).waitFor();
    await anonymous.close();
  } finally {
    await browser.close();
  }
} finally {
  await client.query("DELETE FROM agents WHERE id::text LIKE '62000000-0000-0000-0000-%'");
  await client.query("DELETE FROM agents WHERE id = '64000000-0000-0000-0000-000000000005'");
  await client.query("DELETE FROM organization_memberships WHERE id = $1", [revokedMembership]);
  await client.query("DELETE FROM projects WHERE id = ANY($1::uuid[])", [[emptyProject, revokedProject]]);
  await client.query("DELETE FROM organizations WHERE id = $1", [revokedOrganization]);
  await client.end();
  await service.stop();
  await database.drop();
}
