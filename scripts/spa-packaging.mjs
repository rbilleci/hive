import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createIsolatedDatabase, startIsolatedLocalService } from "./local-service.mjs";

// RTD-SPA-SERVING: the container image copies the console build beside the binary and names it with
// HIVE_WEB_DIST. This check serves such a copy, never the workspace build directory, and proves the
// browser-history fallback, the static not-found rule, and every server-owned path.
const sourceBuild = join(process.cwd(), "crates", "hive-console", "dist");

/** Every servable file of the build as a URL path, without the entry point or the compressed siblings. */
function servedFiles(directory, prefix = "") {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => entry.isDirectory()
    ? servedFiles(join(directory, entry.name), prefix + "/" + entry.name)
    : /\.(br|gz)$/.test(entry.name) || (prefix === "" && entry.name === "index.html") ? [] : [prefix + "/" + entry.name]);
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

async function response(url, options) {
  const result = await fetch(url, options);
  return { result, bytes: Buffer.from(await result.arrayBuffer()) };
}

async function verify() {
  assert.equal(existsSync(join(sourceBuild, "index.html")), true, "The console build has no index.html; run npm run build:console.");
  const assets = servedFiles(sourceBuild);
  assert.ok(assets.length > 0, "The console build has no assets.");
  const packagedRoot = mkdtempSync(join(tmpdir(), "hive-spa-package-"));
  const packagedBuild = join(packagedRoot, "web");
  cpSync(sourceBuild, packagedBuild, { recursive: true });
  const index = readFileSync(join(packagedBuild, "index.html"), "utf8");
  let database;
  let service;
  try {
    database = await createIsolatedDatabase("spa_package");
    service = await startIsolatedLocalService(database.name, { HIVE_WEB_DIST: packagedBuild });
    const base = "http://127.0.0.1:" + service.port;
    const home = await response(base + "/", { headers: { Accept: "text/html" } });
    assert.equal(home.result.status, 200);
    assert.equal(home.bytes.toString("utf8"), index);
    for (const route of ["/organizations", "/organizations/10000000-0000-0000-0000-000000000001", "/projects/50000000-0000-0000-0000-000000000001"]) {
      const browserRoute = await response(base + route, { headers: { Accept: "text/html" } });
      assert.equal(browserRoute.result.status, 200, "The browser route did not return index.html: " + route);
      assert.equal(browserRoute.bytes.toString("utf8"), index, "The browser route did not return the packaged entry point: " + route);
    }
    for (const asset of assets) {
      const packagedAsset = await response(base + asset);
      assert.equal(packagedAsset.result.status, 200, "The packaged asset did not load: " + asset);
      assert.equal(sha256(packagedAsset.bytes), sha256(readFileSync(join(sourceBuild, asset.slice(1)))), "The packaged asset differs from the Vite output: " + asset);
      // A client that accepts brotli receives the precompressed sibling, and a fingerprinted file is cacheable for good.
      if (existsSync(join(sourceBuild, asset.slice(1) + ".br"))) {
        const compressed = await fetch(base + asset, { headers: { "Accept-Encoding": "br" } });
        assert.equal(compressed.headers.get("content-encoding"), "br", "The precompressed sibling was not served: " + asset);
        await compressed.arrayBuffer();
      }
    }
    assert.equal(home.result.headers.get("cache-control"), "no-cache", "The entry point must revalidate on every use.");
    const asset = assets.find((path) => path.endsWith(".js")) ?? assets[0];
    const missingAsset = await response(base + asset.replace(/\.[^.]+$/, ".missing.js"));
    assert.equal(missingAsset.result.status, 404, "A missing static asset did not return not found.");
    assert.notEqual(missingAsset.bytes.toString("utf8"), index, "A missing static asset returned the SPA entry point.");

    const graphql = await response(base + "/graphql", {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        Cookie: "sf_session=" + service.signFixtureSession("00000000-0000-0000-0000-000000000001")
      },
      body: JSON.stringify({ query: "query PackagingContract { __typename }" })
    });
    assert.equal(graphql.result.status, 200, "POST /graphql did not retain its server handler.");
    assert.match(graphql.result.headers.get("content-type") ?? "", /application\/json/);
    assert.equal(JSON.parse(graphql.bytes.toString("utf8")).data.__typename, "Query");
    const graphqlGet = await response(base + "/graphql", { headers: { Accept: "text/html" } });
    assert.notEqual(graphqlGet.bytes.toString("utf8"), index, "GET /graphql returned the SPA entry point.");
    for (const path of ["/health", "/health/deployment-worker", "/health/evaluation-worker"]) {
      const health = await response(base + path, { headers: { Accept: "application/json" } });
      assert.match(health.result.headers.get("content-type") ?? "", /application\/json/, path + " did not retain its server handler.");
      assert.notEqual(health.bytes.toString("utf8"), index, path + " returned the SPA entry point.");
      const body = JSON.parse(health.bytes.toString("utf8"));
      assert.equal(typeof body.status, "string", path + " returned no health status.");
      if (path === "/health") {
        for (const field of ["approvalMaintenance", "approvalMaintenanceAttempted", "approvalMaintenanceReconciled", "approvalMaintenanceFailed", "approvalUpgradeMaintenance", "graphqlRequests", "graphqlFailures", "graphqlUnavailable", "graphqlLastDurationNanos", "graphqlMaxDurationNanos"]) {
          assert.equal(Object.hasOwn(body, field), true, path + " omitted " + field + ".");
        }
        assert.ok(["ok", "degraded"].includes(body.status), path + " returned an unknown aggregate status.");
        assert.ok(["ok", "failed"].includes(body.approvalMaintenance), path + " returned an unknown approval-maintenance status.");
        assert.ok(["ok", "failed"].includes(body.approvalUpgradeMaintenance), path + " returned an unknown approval-upgrade-maintenance status.");
        assert.equal(health.result.status, body.status === "ok" ? 200 : 503, path + " did not retain aggregate status semantics.");
      } else {
        for (const field of path === "/health/deployment-worker"
          ? ["workerId", "observedAt", "pendingEvents", "oldestPendingAt", "detail", "pendingApprovalHandoffs", "oldestApprovalHandoffAt"]
          : ["pendingEvents"]) {
          assert.equal(Object.hasOwn(body, field), true, path + " omitted " + field + ".");
        }
        assert.ok(["READY", "STALE", "DEGRADED", "UNAVAILABLE"].includes(body.status), path + " returned an unknown worker status.");
        assert.equal(health.result.status, body.status === "READY" ? 200 : 503, path + " did not retain worker status semantics.");
      }
    }
    console.log("spa-packaging: copied-build serving, browser-history routing, server paths, and static not-found behavior pass.");
  } finally {
    if (service) await service.stop();
    if (database) await database.drop();
    rmSync(packagedRoot, { recursive: true, force: true });
  }
}

await verify();
