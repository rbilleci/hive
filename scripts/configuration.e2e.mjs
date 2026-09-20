import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const bea = "00000000-0000-0000-0000-000000000002";
const alpha = "10000000-0000-0000-0000-000000000001";
const project = "50000000-0000-0000-0000-000000000001";
const port = 18119;
const origin = "http://127.0.0.1:" + port;
const fixture = Date.now().toString(36);
const promptName = "Browser M11 Prompt " + fixture;
const policyName = "Browser M11 Policy " + fixture;
const profileName = "Browser M11 Profile " + fixture;
const toolName = "Browser M11 Tool " + fixture;
const serverId = "browser-mcp-" + fixture;
const database = await createIsolatedDatabase("hive_configuration_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
const browser = await launchBrowser();
let promptId = null;
let toolId = null;

// `path` pins the response to the page being opened. These waiters are registered before the
// navigation starts, and the page being left also rechecks console access in the background; a
// response to that request would match by operation name, but its body is gone once the page unloads.
function graphqlOperation(response, operationName, path) {
  return response.url() === origin + "/graphql" && response.request().method() === "POST"
    && response.request().postData()?.includes("query " + operationName) === true
    && response.request().headers().referer === origin + path;
}

async function navigateToCatalog(page, path, heading) {
  const consoleResponse = page.waitForResponse((response) => graphqlOperation(response, "ConsoleShell", path));
  const catalogResponse = page.waitForResponse((response) => graphqlOperation(response, "ConfigurationCatalog", path));
  await page.goto(origin + path);
  const [console, catalog] = await Promise.all([consoleResponse, catalogResponse]);
  assert.equal(console.status(), 200, "shared console access resolves before the route is asserted");
  assert.equal(catalog.status(), 200, "catalog data resolves before the route is asserted");
  const [consolePayload, catalogPayload] = await Promise.all([console.json(), catalog.json()]);
  assert.ok(consolePayload.data?.principals?.nodes?.length, "the signed fixture session owns the shared console response");
  assert.equal(catalogPayload.data?.catalogRelease?.id, "local-2026-08-10", "the catalog route owns its local release response");
  await page.getByRole("heading", { name: heading, exact: true }).waitFor();
}

async function navigateToResource(page, path, heading) {
  const consoleResponse = page.waitForResponse((response) => graphqlOperation(response, "ConsoleShell", path));
  const resourcesResponse = page.waitForResponse((response) => graphqlOperation(response, "ConfigurationResources", path));
  await page.goto(origin + path);
  const [console, resources] = await Promise.all([consoleResponse, resourcesResponse]);
  assert.equal(console.status(), 200, "shared console access resolves before a resource route is asserted");
  assert.equal(resources.status(), 200, "resource data resolves before authoring is asserted");
  const [consolePayload, resourcesPayload] = await Promise.all([console.json(), resources.json()]);
  assert.ok(consolePayload.data?.principals?.nodes?.length, "the signed fixture session owns the resource-route console response");
  assert.ok(Array.isArray(resourcesPayload.data?.reusableResources), "the resource route receives an authorized resource collection");
  const resourceRoute = page.locator('main[aria-labelledby="resource-title"]');
  await resourceRoute.getByRole("heading", { name: heading, exact: true }).waitFor();
  const kind = ({ Prompts: "PROMPT", Policies: "POLICY", "Model profiles": "MODEL_PROFILE" })[heading];
  if (resourcesPayload.data.reusableResources.some((resource) => resource.kind === kind)) {
    await resourceRoute.getByRole("heading", { name: "Resources", exact: true }).waitFor();
  } else {
    await resourceRoute.getByText("No " + heading.toLowerCase() + " are available.", { exact: true }).waitFor();
  }
  return resourceRoute;
}

async function beginNewResource(page, name) {
  await page.getByRole("button", { name }).click();
  await page.getByRole("form", { name: "Create reusable resource" }).waitFor();
  await page.getByRole("button", { name: "Create draft" }).waitFor();
}

async function navigateToTools(page, expectedToolName = null) {
  const path = "/projects/" + project + "/tools";
  const consoleResponse = page.waitForResponse((response) => graphqlOperation(response, "ConsoleShell", path));
  const toolsResponse = page.waitForResponse((response) => graphqlOperation(response, "ProjectMcpServers", path));
  await page.goto(origin + path);
  const [console, tools] = await Promise.all([consoleResponse, toolsResponse]);
  assert.equal(console.status(), 200, "shared console access resolves before the tools route is asserted");
  assert.equal(tools.status(), 200, "tool metadata resolves before editing is asserted");
  const [consolePayload, toolsPayload] = await Promise.all([console.json(), tools.json()]);
  assert.ok(consolePayload.data?.principals?.nodes?.length, "the signed fixture session owns the tools-route console response");
  const connections = toolsPayload.data?.projectMcpServers;
  assert.ok(Array.isArray(connections), "the tools route receives an authorized MCP server collection");
  if (expectedToolName) {
    assert.ok(connections.some((tool) => tool.name === expectedToolName), "the refreshed tools route contains the saved connection");
  }
  const toolsRoute = page.locator('main[aria-labelledby="mcp-title"]');
  await toolsRoute.getByRole("heading", { name: "MCP servers", exact: true }).waitFor();
  if (expectedToolName) {
    const savedTool = toolsRoute.locator(".mcp-server-list > li").filter({ hasText: expectedToolName });
    await savedTool.getByRole("heading", { name: expectedToolName, exact: true }).waitFor();
  }
  return toolsRoute;
}

async function navigateToUnavailableConfiguration(page, route) {
  const path = "/projects/" + project + "/" + route;
  const consoleResponse = page.waitForResponse((response) => graphqlOperation(response, "ConsoleShell", path));
  await page.goto(origin + path);
  const console = await consoleResponse;
  assert.equal(console.status(), 200, "shared console access resolves before an unavailable configuration route is asserted");
  const consolePayload = await console.json();
  assert.ok(consolePayload.data, "the signed fixture session receives a shared-console response for an unavailable route");
  await page.locator("main.console-terminal, main.configuration-page").waitFor();
  const terminal = page.getByRole("heading", { name: "Access denied", exact: true });
  const unavailableRoute = page.locator("main.configuration-page").getByRole("alert", { name: "This route is unavailable or you no longer have access.", exact: true });
  if (await terminal.count()) {
    await terminal.waitFor();
  } else {
    await unavailableRoute.waitFor();
  }
  assert.equal(await page.getByText("No local configuration is available yet.", { exact: true }).count(), 0, "an inaccessible project route is not represented as an empty route");
}

try {
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await context.addCookies([{ name: "sf_session", value: service.signFixtureSession(ada), url: origin, httpOnly: true, sameSite: "Lax" }]);
  const page = await context.newPage();
  await navigateToCatalog(page, "/organizations/" + alpha + "/catalog", "Catalog");
  await page.getByText("local-2026-08-10", { exact: true }).waitFor();
  await page.getByText("Local Safe Chat").waitFor();
  assert.equal(await page.getByRole("button", { name: /create|save|publish/i }).count(), 0, "catalog has no mutation control");
  await navigateToCatalog(page, "/organizations/" + alpha + "/environments", "Environments");
  await page.getByText("PRODUCTION").waitFor();

  await page.goto(origin + "/projects/" + project + "/prompts");
  await page.getByRole("heading", { name: "Prompts", exact: true }).waitFor();
  await page.getByRole("link", { name: "Create", exact: true }).click();
  await page.getByRole("heading", { name: "Create prompt", exact: true }).waitFor();
  await page.getByRole("textbox", { name: "Prompt name" }).fill(promptName);
  const promptContent = page.getByRole("textbox", { name: "Prompt source" });
  await promptContent.fill("Hello {{operator}}");
  await page.getByText(/18 characters/).waitFor();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(new RegExp("/projects/" + project + "/prompts/.+/edit$"));
  await page.getByRole("button", { name: "Validate", exact: true }).click();
  await page.getByText("VALID", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Review", exact: true }).click();
  await page.getByRole("dialog", { name: "Review prompt" }).waitFor();
  await page.getByRole("button", { name: "Continue editing" }).click();
  await page.getByRole("button", { name: "Publish", exact: true }).click();
  await page.getByText("Latest: version 1").waitFor();
  const created = await client.query("SELECT id FROM reusable_resources WHERE project_id = $1 AND name = $2", [project, promptName]);
  promptId = created.rows[0].id;

  await navigateToResource(page, "/projects/" + project + "/policies", "Policies");
  await beginNewResource(page, "New policy");
  await page.getByRole("textbox", { name: "Name" }).fill(policyName);
  assert.equal(await page.getByRole("textbox", { name: "Content" }).count(), 1);
  await page.getByRole("button", { name: "Create draft" }).click();
  await page.getByRole("button", { name: "Validate draft" }).click();
  await page.getByText("VALID", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Publish immutable version" }).click();
  await page.getByText("Version 1").waitFor();
  await navigateToResource(page, "/projects/" + project + "/model-profiles", "Model profiles");
  await beginNewResource(page, "New model profile");
  await page.getByRole("textbox", { name: "Name" }).fill(profileName);
  const profileModel = page.getByRole("checkbox", { name: /Local Safe Chat/ });
  await profileModel.check();
  assert.equal(await profileModel.isChecked(), true, "model-profile dependencies are selectable approved references");
  await page.getByRole("button", { name: "Create draft" }).click();
  await page.getByRole("button", { name: "Validate draft" }).click();
  await page.getByText("VALID", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Publish immutable version" }).click();
  await page.getByText("Version 1").waitFor();
  const toolsRoute = await navigateToTools(page);
  await toolsRoute.getByRole("button", { name: "Add MCP server" }).click();
  const createMcp = toolsRoute.getByRole("form", { name: "Add MCP server" });
  await createMcp.getByRole("textbox", { name: "Server ID" }).fill(serverId);
  await createMcp.getByRole("textbox", { name: "Display name" }).fill(toolName);
  await createMcp.getByRole("textbox", { name: "Command" }).fill("local-mcp-host");
  await createMcp.getByRole("textbox", { name: "Redacted binding references (one reference per line)" }).fill("redacted://local/browser-token");
  await createMcp.getByRole("textbox", { name: "Allowed tools (one ID per line)" }).fill("metadata.lookup");
  const argumentInput = createMcp.getByRole("textbox", { name: "Arguments (one non-secret argument per line)" });
  for (const scheme of ["bearer", "basic"]) {
    await argumentInput.fill("--" + scheme + "\nplain-text-value");
    const refused = page.waitForResponse((response) => response.url() === origin + "/graphql"
      && response.request().method() === "POST" && response.request().postData()?.includes("mutation CreateProjectMcpServer") === true);
    await createMcp.getByRole("button", { name: "Create MCP server" }).click();
    await refused;
    await toolsRoute.getByRole("alert").waitFor();
    assert.equal((await client.query("SELECT count(*)::int AS count FROM project_tool_connections WHERE project_id = $1 AND server_id = $2", [project, serverId])).rows[0].count,
      0, `M12UXC-MCP-SECRET-PERSIST: UI refusal for split --${scheme} changes no descriptor row`);
  }
  await argumentInput.fill("");
  await createMcp.getByRole("button", { name: "Create MCP server" }).click();
  await toolsRoute.getByRole("heading", { name: toolName, exact: true }).waitFor();
  assert.equal(await toolsRoute.getByRole("form", { name: "Add MCP server" }).count(), 0,
    "saving a new MCP server returns to the list instead of becoming an edit");
  await toolsRoute.getByRole("button", { name: "Add MCP server" }).click();
  const secondCreate = toolsRoute.getByRole("form", { name: "Add MCP server" });
  assert.equal(await secondCreate.getByRole("textbox", { name: "Server ID" }).inputValue(), "");
  await secondCreate.getByRole("button", { name: "Cancel" }).click();
  const tool = await client.query("SELECT id FROM project_tool_connections WHERE project_id = $1 AND name = $2", [project, toolName]);
  toolId = tool.rows[0].id;
  const refreshedToolsRoute = await navigateToTools(page, toolName);
  const savedTool = refreshedToolsRoute.locator(".mcp-server-list > li").filter({ hasText: toolName });
  await savedTool.getByRole("button", { name: "Edit " + toolName, exact: true }).click();
  const editForm = refreshedToolsRoute.getByRole("form", { name: "Edit MCP server", exact: true });
  await editForm.getByRole("radio", { name: "Remote HTTPS" }).check();
  assert.equal(await editForm.getByRole("textbox", { name: "Command" }).count(), 0, "remote transport excludes STDIO fields");
  await editForm.getByRole("textbox", { name: "Remote URL" }).fill("https://mcp.invalid/browser-metadata");
  await editForm.getByRole("checkbox", { name: "Enabled" }).uncheck();
  await editForm.getByRole("button", { name: "Save MCP server" }).click();
  await savedTool.locator(".status-with-text").filter({ hasText: "DISABLED" }).waitFor();
  const persistedMcp = (await client.query("SELECT enabled, transport_type, stdio_command, remote_url, redacted_bindings, declared_tools FROM project_tool_connections WHERE id = $1", [toolId])).rows[0];
  assert.equal(persistedMcp.enabled, false, "the MCP route maintains enablement metadata");
  assert.deepEqual({ transport: persistedMcp.transport_type, command: persistedMcp.stdio_command, remoteUrl: persistedMcp.remote_url },
    { transport: "REMOTE", command: null, remoteUrl: "https://mcp.invalid/browser-metadata" });
  assert.deepEqual(persistedMcp.redacted_bindings, ["redacted://local/browser-token"]);
  assert.deepEqual(persistedMcp.declared_tools, ["metadata.lookup"]);

  await client.query("DELETE FROM console_role_assignments WHERE principal_id = $1 AND project_id = $2 AND role_code = 'AGENT_DEVELOPER'", [ada, project]);
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await page.goto(origin + "/projects/" + project + "/prompts");
  await page.getByRole("heading", { name: "Prompts", exact: true }).waitFor();
  assert.equal(await page.getByRole("link", { name: "Create", exact: true }).count(), 0);
  await client.query("INSERT INTO console_role_assignments (id, principal_id, organization_id, project_id, role_code) VALUES ('81000000-0000-0000-0000-000000000005', $1, NULL, $2, 'AGENT_DEVELOPER') ON CONFLICT DO NOTHING", [ada, project]);
  await context.close();

  const inaccessible = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await inaccessible.addCookies([{ name: "sf_session", value: service.signFixtureSession(bea), url: origin, httpOnly: true, sameSite: "Lax" }]);
  const inaccessiblePage = await inaccessible.newPage();
  for (const route of ["prompts", "tools"]) {
    await navigateToUnavailableConfiguration(inaccessiblePage, route);
  }
  await inaccessible.close();
} finally {
  await browser.close();
  await client.query("INSERT INTO console_role_assignments (id, principal_id, organization_id, project_id, role_code) VALUES ('81000000-0000-0000-0000-000000000005', $1, NULL, $2, 'AGENT_DEVELOPER') ON CONFLICT DO NOTHING", [ada, project]);
  // Published browser evidence intentionally remains immutable.
  if (toolId) await client.query("DELETE FROM project_tool_connections WHERE id = $1", [toolId]);
  await client.end();
  await service.stop();
  await database.drop();
}
