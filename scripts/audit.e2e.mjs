import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { launchBrowser } from "./browser.mjs";
import { createIsolatedDatabase, postgresClient, startLocalService } from "./local-service.mjs";

const administrator = "00000000-0000-0000-0000-000000000001";
const outsider = "d1300000-0000-0000-0000-000000000005";
const revokedViewer = "d1300000-0000-0000-0000-000000000006";
const revokedOrganizationMembership = "d1300000-0000-0000-0000-000000000007";
const revokedProjectMembership = "d1300000-0000-0000-0000-000000000008";
const project = "50000000-0000-0000-0000-000000000001";
const port = 18117;
const origin = `http://127.0.0.1:${port}`;
const database = await createIsolatedDatabase("m17_audit_e2e");
const service = await startLocalService(port, database.name);
const client = await postgresClient(database.name);

async function graphql(principal, query, variables) {
  const response = await fetch(`${origin}/graphql`, {
    method: "POST", headers: { "Content-Type": "application/json", Cookie: `sf_session=${service.signFixtureSession(principal)}` },
    body: JSON.stringify({ query, variables })
  });
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.errors, undefined, JSON.stringify(body.errors));
  return body.data;
}

try {
  await client.query("INSERT INTO platform_role_assignments (principal_id, role_code) VALUES ($1, 'PLATFORM_ADMIN')", [administrator]);
  await client.query("INSERT INTO principals (id, subject, display_name, email) VALUES ($1, 'm17-revoked-viewer', 'M17 Revoked Viewer', 'm17-revoked-viewer@local.invalid')", [revokedViewer]);
  await client.query("INSERT INTO organization_memberships (id, organization_id, principal_id, started_at, revision) VALUES ($1, '10000000-0000-0000-0000-000000000001', $2, CURRENT_TIMESTAMP, 1)", [revokedOrganizationMembership, revokedViewer]);
  await client.query("INSERT INTO project_memberships (id, project_id, principal_id, started_at, revision) VALUES ($1, $2, $3, CURRENT_TIMESTAMP, 1)", [revokedProjectMembership, project, revokedViewer]);
  await client.query("INSERT INTO project_membership_roles (membership_id, role_code) VALUES ($1, 'AUDITOR')", [revokedProjectMembership]);
  const created = await graphql(administrator,
    "mutation M17AuditFixture($input: CreateAgentDraftInput!) { createAgentDraft(input: $input) { agentDraft { agentId } problems { code } } }",
    { input: { projectId: project, displayName: "M17 audit browser fixture", slug: `m17-audit-${randomUUID().slice(0, 8)}` } });
  assert.deepEqual(created.createAgentDraft.problems, []);
  const browser = await launchBrowser();
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
    await context.addCookies([{ name: "sf_session", value: service.signFixtureSession(administrator), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const page = await context.newPage();
    await page.emulateMedia({ reducedMotion: "reduce" });
    const auditRequests = [];
    const externalRequests = [];
    const graphqlPayloads = [];
    page.on("request", (request) => {
      if (new URL(request.url()).origin !== origin) externalRequests.push(request.url());
      if (request.url().endsWith("/graphql")) {
        const payload = request.postData() ?? "";
        graphqlPayloads.push(payload);
        if (payload.includes("AuditEvents")) auditRequests.push(request.url());
      }
    });
    await page.goto(`${origin}/projects/${project}/audit?afterTime=2020-01-01T00%3A00%3A00.000Z`);
    await page.getByRole("heading", { name: "Audit history" }).waitFor();
    await page.getByRole("form", { name: "Audit filters" }).waitFor();
    for (const control of ["Action", "Outcome", "Actor ID", "Resource type", "Resource ID", "Correlation ID", "Occurred after", "Occurred before"]) {
      await page.getByLabel(control).waitFor();
    }
    await page.getByRole("button", { name: "Apply filters" }).waitFor();
    await page.getByRole("button", { name: "Reset filters" }).waitFor();
    await page.getByRole("button", { name: "Refresh" }).waitFor();
    await page.getByRole("table", { name: "Immutable audit events in the selected scope" }).waitFor();
    const rowsBeforeRetryableFailure = await page.locator("tbody tr").count();
    await page.route("**/graphql", async (route) => {
      if (route.request().postData()?.includes("AuditEvents")) {
        await route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ errors: [{ message: "Audit history is temporarily unavailable." }] }) });
      } else await route.fallback();
    });
    await page.getByRole("button", { name: "Refresh" }).click();
    await page.getByRole("alert").filter({ hasText: "temporarily unavailable" }).waitFor();
    assert.match(await page.getByLabel("Action").getAttribute("aria-describedby") ?? "", /-error$/);
    assert.equal(await page.locator("tbody tr").count(), rowsBeforeRetryableFailure);
    await page.getByRole("status").filter({ hasText: "displayed results may be stale" }).waitFor();
    await page.unroute("**/graphql");
    const detail = page.getByRole("button", { name: /View audit event/ }).first();
    await detail.click();
    const dialog = page.getByRole("dialog", { name: "Audit event detail" });
    await dialog.waitFor();
    await page.getByRole("heading", { name: "Audit event detail" }).waitFor();
    await page.waitForFunction(() => document.activeElement?.id === "audit-detail-title");
    assert.equal(await page.evaluate(() => document.activeElement?.id), "audit-detail-title");
    await page.getByRole("button", { name: "Show this correlation" }).click();
    await page.getByRole("dialog", { name: "Audit event detail" }).waitFor({ state: "detached" });
    assert.match(page.url(), /correlation=/);
    await page.getByRole("button", { name: /View audit event/ }).first().click();
    await dialog.waitFor();
    await page.keyboard.press("Escape");
    await page.getByRole("dialog", { name: "Audit event detail" }).waitFor({ state: "detached" });
    await page.evaluate(() => { document.body.style.zoom = "200%"; });
    assert.equal(await page.locator(".audit-table-scroll").evaluate((node) => node.scrollWidth >= node.clientWidth), true);
    const requestsBeforeRefresh = auditRequests.length;
    const refreshedAudit = page.waitForRequest((request) => request.url().endsWith("/graphql") && request.postData()?.includes("AuditEvents"));
    await page.getByRole("button", { name: "Refresh" }).click();
    await refreshedAudit;
    const requestsAfterLoad = auditRequests.length;
    await page.waitForTimeout(350);
    assert.equal(auditRequests.length, requestsAfterLoad, "Audit history must not poll.");
    for (const url of auditRequests) assert.equal(new URL(url).origin, origin);
    assert.deepEqual(externalRequests, []);
    for (const payload of graphqlPayloads) assert.doesNotMatch(payload, /secret|password|access[_-]?key|authorization:\s*bearer/i);
    await context.close();

    const forcedColors = await browser.newContext({ viewport: { width: 768, height: 1024 }, forcedColors: "active" });
    await forcedColors.addCookies([{ name: "sf_session", value: service.signFixtureSession(administrator), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const forcedColorsPage = await forcedColors.newPage();
    await forcedColorsPage.goto(`${origin}/projects/${project}/audit?afterTime=2020-01-01T00%3A00%3A00.000Z`);
    await forcedColorsPage.getByRole("table", { name: "Immutable audit events in the selected scope" }).waitFor();
    assert.equal(await forcedColorsPage.evaluate(() => matchMedia("(forced-colors: active)").matches), true);
    await forcedColors.close();

    const denied = await browser.newContext();
    await denied.addCookies([{ name: "sf_session", value: service.signFixtureSession(outsider), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const deniedPage = await denied.newPage();
    await deniedPage.goto(`${origin}/projects/${project}/audit?afterTime=2020-01-01T00%3A00%3A00.000Z`);
    await deniedPage.getByText("Audit history is unavailable.").waitFor();
    await denied.close();

    const revoked = await browser.newContext();
    await revoked.addCookies([{ name: "sf_session", value: service.signFixtureSession(revokedViewer), url: origin, httpOnly: true, sameSite: "Lax" }]);
    const revokedPage = await revoked.newPage();
    await revokedPage.goto(`${origin}/projects/${project}/audit?afterTime=2020-01-01T00%3A00%3A00.000Z`);
    await revokedPage.getByRole("table", { name: "Immutable audit events in the selected scope" }).waitFor();
    await client.query("UPDATE project_memberships SET ended_at = CURRENT_TIMESTAMP WHERE id = $1", [revokedProjectMembership]);
    await revokedPage.getByRole("button", { name: "Refresh" }).click();
    await revokedPage.getByText("Audit history is unavailable.").waitFor();
    assert.equal(await revokedPage.getByRole("table", { name: "Immutable audit events in the selected scope" }).count(), 0);
    await revoked.close();
  } finally {
    await browser.close();
  }
} finally {
  await client.end();
  await service.stop();
  await database.drop();
}
console.log("M17 audit browser route, refresh contract, focus behavior, redaction boundary, and local-only request checks passed.");
