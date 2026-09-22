import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const port = 18114;
const origin = "http://127.0.0.1:" + port;
const projectSlug = "browser-project-" + Date.now().toString(36);
const projectName = "Browser Project " + Date.now().toString(36);

const database = await createIsolatedDatabase("hive_project_authoring_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
const browser = await launchBrowser();
try {
  const context = await browser.newContext();
  await context.addCookies([{ name: "sf_session", value: service.signFixtureSession(ada), url: origin, httpOnly: true, sameSite: "Lax" }]);
  const page = await context.newPage();

  await page.goto(origin + "/organizations/" + alpha + "/projects");
  await page.getByRole("heading", { name: "Projects" }).waitFor();
  await page.getByRole("link", { name: "Create project" }).click();
  await page.waitForURL(origin + "/organizations/" + alpha + "/projects/new");
  await page.getByRole("heading", { name: "Create project" }).waitFor();

  await page.getByLabel("Project ID").fill(projectSlug);
  await page.getByLabel("Name").fill(projectName);
  await page.getByLabel("Description").fill("Created by an end-to-end check.");
  await page.getByRole("button", { name: "Create project" }).click();

  await page.waitForURL(/\/projects\/[0-9a-f-]{36}$/);
  await page.getByRole("heading", { name: projectName }).waitFor();

  const created = await client.query("SELECT organization_id, display_name FROM projects WHERE slug = $1", [projectSlug]);
  assert.equal(created.rows.length, 1, "the mutation invoked from the browser actually persisted a project");
  assert.equal(created.rows[0].organization_id, alpha);
  assert.equal(created.rows[0].display_name, projectName);
} finally {
  await browser.close();
  await client.end();
  await service.stop();
  await database.drop();
}
