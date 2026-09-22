import assert from "node:assert/strict";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const ada = "00000000-0000-0000-0000-000000000001";
const alpha = "10000000-0000-0000-0000-000000000001";
const commandConsole = "50000000-0000-0000-0000-000000000001";
const commandNavigator = "60000000-0000-0000-0000-000000000001";
const privateProject = "50000000-0000-0000-0000-000000000003";
const alphaMembership = "20000000-0000-0000-0000-000000000001";
const port = 18111;
const origin = "http://127.0.0.1:" + port;

const directRoutes = [
  "/organizations",
  "/organizations/" + alpha,
  "/organizations/" + alpha + "/projects",
  "/organizations/" + alpha + "/settings",
  "/projects/" + commandConsole,
  "/projects/" + commandConsole + "/agents",
  "/projects/" + commandConsole + "/agents/" + commandNavigator,
  "/projects/" + commandConsole + "/agents/" + commandNavigator + "/edit",
  "/projects/" + commandConsole + "/settings",
  "/preferences"
];

const database = await createIsolatedDatabase("hive_console_navigation_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);
try {
  await client.query("DELETE FROM principal_display_preferences WHERE principal_id = $1", [ada]);
  const browser = await launchBrowser();
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
    await context.addCookies([{ name: "sf_session", value: service.signFixtureSession(ada), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const page = await context.newPage();
    const pageErrors = [];
    page.on("pageerror", (error) => pageErrors.push(error.message));

    await page.goto(origin + "/projects/" + commandConsole);
    await page.getByRole("heading", { name: "Customer Feedback Copilot" }).waitFor();
    await page.getByLabel("Primary navigation").waitFor();
    assert.equal(await page.getByRole("button", { name: "Open navigation" }).count(), 0);
    assert.equal(await page.getByRole("link", { name: "Customer Feedback Copilot", exact: true }).count(), 1);
    assert.equal(await page.getByRole("link", { name: "Agents" }).count(), 1);
    assert.equal(await page.getByRole("link", { name: "Preferences" }).count(), 1);
    const switcher = page.locator(".organization-switcher-trigger");
    await switcher.waitFor();
    assert.match(await switcher.innerText(), /Product/);
    await switcher.focus();
    await page.keyboard.press("Enter");
    const organizationOptions = page.getByRole("menu", { name: "Accessible organizations" });
    const alphaOption = organizationOptions.getByRole("menuitemradio", { name: /Product/ });
    await alphaOption.waitFor();
    assert.equal(await alphaOption.getAttribute("aria-checked"), "true");
    await page.waitForFunction(() => document.activeElement?.getAttribute("role") === "menuitemradio");
    // The menu lists organizations by display name: Product, Quality Assurance (archived), Support.
    await page.keyboard.press("ArrowDown");
    const archivedOption = organizationOptions.getByRole("menuitemradio", { name: /Quality Assurance/ });
    assert.equal(await archivedOption.evaluate((element) => element === document.activeElement), true,
      "M12UXC-NAV-POPOVER-A11Y: ArrowDown moves menu focus");
    await page.keyboard.press("ArrowDown");
    const betaOption = organizationOptions.getByRole("menuitemradio", { name: /Support/ });
    assert.equal(await betaOption.evaluate((element) => element === document.activeElement), true,
      "M12UXC-NAV-POPOVER-A11Y: ArrowDown keeps moving menu focus");
    await page.keyboard.press("Enter");
    await page.waitForURL(origin + "/organizations/10000000-0000-0000-0000-000000000002/projects");
    await page.getByRole("heading", { name: "Projects", exact: true }).waitFor();
    await page.goto(origin + "/projects/" + commandConsole);
    const navigation = page.getByRole("navigation", { name: "Resource navigation" });
    await navigation.getByRole("link", { name: "Projects", exact: true }).waitFor();
    const primaryLabels = await navigation.locator(".navigation-tree-root > li > .navigation-tree-link .navigation-tree-label").allTextContents();
    assert.deepEqual(primaryLabels.slice(0, 3), ["Projects", "Catalog", "Environments"]);
    const projectsSection = navigation.locator(".navigation-tree-projects-section");
    assert.deepEqual(await projectsSection.locator(":scope > ul > li.navigation-tree-project .navigation-tree-row .navigation-tree-label").allTextContents(),
      ["Customer Feedback Copilot", "Usage Analytics"], "M12UXC-NAV-ORDER: project rows belong to the Projects subtree");
    assert.equal(await page.getByRole("navigation", { name: "Resource navigation" }).getByRole("link", { name: "Product" }).count(), 0,
      "organizations are selected by the shell and never appear as tree nodes");
    const resize = page.getByRole("separator", { name: "Resize sidebar" });
    await resize.focus();
    await page.keyboard.press("End");
    assert.equal(await resize.getAttribute("aria-valuenow"), "420");
    assert.equal(await page.evaluate(() => sessionStorage.getItem("hive.console-sidebar-width")), "420");
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true);
    const resizeBox = await resize.boundingBox();
    assert(resizeBox);
    await page.mouse.move(resizeBox.x + resizeBox.width / 2, resizeBox.y + 10);
    await page.mouse.down(); await page.mouse.move(resizeBox.x - 40, resizeBox.y + 10); await page.mouse.up();
    assert(Number(await resize.getAttribute("aria-valuenow")) < 420, "pointer drag changes the bounded desktop width");
    const retainedWidth = await resize.getAttribute("aria-valuenow");
    await page.reload();
    assert.equal(await page.getByRole("separator", { name: "Resize sidebar" }).getAttribute("aria-valuenow"), retainedWidth,
      "sidebar width is reload-safe within the browser session");
    const activeStatus = page.locator(".lifecycle-badge");
    await activeStatus.waitFor();
    assert.equal(await activeStatus.textContent(), "ACTIVE");
    assert.match(await activeStatus.evaluate((element) => getComputedStyle(element, "::before").content), /●/);
    await page.getByRole("link", { name: "Agents" }).click();
    await page.waitForURL(origin + "/projects/" + commandConsole + "/agents");
    await page.getByRole("heading", { name: "Agents" }).waitFor();

    for (const route of directRoutes) {
      await page.goto(origin + route);
      try {
        await page.getByLabel("Primary navigation").waitFor({ timeout: 10_000 });
      } catch (cause) {
        const visibleText = (await page.locator("body").innerText()).replace(/\s+/g, " ").slice(0, 500);
        throw new Error(route + " did not render the authenticated shell; browser errors: "
          + (pageErrors.join(" | ") || "none") + "; visible page text: " + visibleText, { cause });
      }
      assert.equal(await page.getByRole("heading", { name: "Session error" }).count(), 0, route + " preserved its authenticated deep link");
    }
    await page.goto(origin + "/projects/" + privateProject);
    await page.getByRole("heading", { name: "Access denied" }).waitFor();

    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(origin + "/projects/" + commandConsole);
    const opener = page.getByRole("button", { name: "Open navigation" });
    assert.equal(await page.getByRole("separator", { name: "Resize sidebar" }).count(), 0, "the responsive drawer has no resize handle");
    await opener.focus();
    await page.keyboard.press("Enter");
    const drawer = page.getByRole("dialog", { name: "Resource navigation" });
    await drawer.waitFor();
    assert.equal(await page.locator(".console-workspace").getAttribute("aria-hidden"), "true");
    assert.equal(await page.locator(".console-workspace").evaluate((element) => element.inert), true);
    assert.equal(await drawer.evaluate((element) => element.contains(document.activeElement)), true);
    await page.keyboard.press("Shift+Tab");
    assert.equal(await drawer.evaluate((element) => element.contains(document.activeElement)), true);
    await page.keyboard.press("Escape");
    assert.equal(await drawer.count(), 0);
    await page.waitForFunction(() => document.activeElement?.getAttribute("aria-label") === "Open navigation");

    await opener.click();
    await page.getByRole("dialog", { name: "Resource navigation" }).getByRole("link", { name: "Agents", exact: true }).click();
    await page.waitForURL(origin + "/projects/" + commandConsole + "/agents");
    assert.equal(await page.getByRole("dialog", { name: "Resource navigation" }).count(), 0);
    await page.getByRole("heading", { name: "Agents" }).waitFor();

    await page.goto(origin + "/preferences");
    assert.equal(await page.getByLabel("Color scheme").inputValue(), "LIGHT");
    assert.equal(await page.getByLabel("Density").inputValue(), "COMFORTABLE");
    await page.getByLabel("Color scheme").selectOption("LIGHT");
    assert.equal(await page.locator("html").getAttribute("data-theme"), "light");
    await page.getByLabel("Color scheme").selectOption("DARK");
    assert.equal(await page.locator("html").getAttribute("data-theme"), "dark");
    await page.getByLabel("Density").selectOption("COMPACT");
    assert.equal(await page.locator("html").getAttribute("data-density"), "compact");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await page.getByText("Preview changes apply to this shell immediately.").waitFor();

    await page.setViewportSize({ width: 1280, height: 900 });
    await page.goto(origin + "/organizations/" + alpha);
    const sidebar = page.locator("aside.console-sidebar");
    await sidebar.getByRole("button", { name: "Collapse sidebar" }).click();
    await sidebar.getByRole("button", { name: "Expand sidebar" }).waitFor();
    await sidebar.getByRole("link", { name: "Organization settings" }).waitFor();
    await client.query("DELETE FROM organization_membership_roles WHERE membership_id = $1 AND role_code = 'ORGANIZATION_ADMIN'", [alphaMembership]);
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await sidebar.getByRole("link", { name: "Organization settings" }).waitFor({ state: "detached" });
    await page.goto(origin + "/organizations/" + alpha + "/projects/new");
    await page.getByLabel("Primary navigation").waitFor();
    await page.getByText("You do not have permission to create a project in this organization.").waitFor();
    assert.equal(await page.getByRole("button", { name: "Create project" }).count(), 0, "the direct project-creation route remains server-controlled after capability loss");
    await context.close();
  } finally {
    await browser.close();
  }
} finally {
  await client.query(
    "INSERT INTO organization_membership_roles (membership_id, role_code) VALUES ($1, 'ORGANIZATION_ADMIN') ON CONFLICT DO NOTHING",
    [alphaMembership]
  );
  await client.query("DELETE FROM principal_display_preferences WHERE principal_id = $1", [ada]);
  await client.end();
  await service.stop();
  await database.drop();
}
