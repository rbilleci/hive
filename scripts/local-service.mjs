import { createHmac, randomBytes } from "node:crypto";
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { createServer } from "node:net";
import { join } from "node:path";
import pg from "pg";

const { Client } = pg;
let localBinary;

/** Holds an ephemeral loopback port until the caller releases it immediately before local-service startup. */
export async function reserveLocalPort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    const sockets = new Set();
    server.once("error", reject);
    server.on("connection", (socket) => {
      sockets.add(socket);
      socket.once("close", () => sockets.delete(socket));
    });
    server.listen({ host: "127.0.0.1", port: 0 }, () => {
      const address = server.address();
      resolve({ port: address.port, release: () => new Promise((close, fail) => {
        for (const socket of sockets) socket.destroy();
        server.close((error) => error ? fail(error) : close());
      }) });
    });
  });
}

/** Returns a fixture-local port for callers that do not launch a service through the retrying helper. */
export async function allocateLocalPort() {
  const reservation = await reserveLocalPort();
  await reservation.release();
  return reservation.port;
}

/** The console build every fixture serves. */
function consoleBuild() {
  const directory = join(process.cwd(), "crates", "hive-console", "dist");
  if (!existsSync(join(directory, "index.html"))) throw new Error("The console is not built: " + directory + " has no index.html. Run npm run build:console.");
  return directory;
}

function databaseEnvironment(port, signingSecret, databaseName) {
  return {
    HIVE_WEB_DIST: consoleBuild(),
    HIVE_DATABASE_URL: "jdbc:postgresql://127.0.0.1:" + port + "/" + databaseName,
    HIVE_DATABASE_USER: "hive",
    HIVE_DATABASE_PASSWORD: "hive",
    HIVE_IDENTITY_SIGNING_KEY: signingSecret
  };
}

export function postgresPort() {
  const endpointsPath = process.env.AP_ENDPOINTS_FILE;
  if (!endpointsPath) {
    return process.env.HIVE_POSTGRES_PORT ?? "5432";
  }
  const endpoints = JSON.parse(readFileSync(endpointsPath, "utf8"));
  const port = endpoints.services?.postgres?.ports?.["5432"];
  if (!port) {
    throw new Error("The PostgreSQL compose endpoint is unavailable.");
  }
  return String(port);
}

export async function postgresClient(databaseName = "hive") {
  const client = new Client({
    host: "127.0.0.1",
    port: Number(postgresPort()),
    user: "hive",
    password: "hive",
    database: databaseName
  });
  await client.connect();
  return client;
}

export async function createIsolatedDatabase(prefix) {
  const name = (prefix + "_" + randomBytes(8).toString("hex")).replace(/[^a-z0-9_]/g, "_");
  const admin = await postgresClient("postgres");
  try {
    await admin.query("CREATE DATABASE " + name);
  } finally {
    await admin.end();
  }
  return {
    name,
    async drop() {
      const cleanup = await postgresClient("postgres");
      try {
        await cleanup.query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()", [name]);
        await cleanup.query("DROP DATABASE IF EXISTS " + name);
      } finally {
        await cleanup.end();
      }
    }
  };
}

/** Stops the fixture process and its detached process group before an isolated fixture releases its database. */
async function stopLocalProcess(child) {
  // A process ended by a signal reports signalCode and leaves exitCode null.
  const ended = () => child.exitCode !== null || child.signalCode !== null;
  if (!child.pid || ended()) return;
  const deliver = (signal) => {
    let delivered = false;
    if (process.platform !== "win32") {
      try {
        process.kill(-child.pid, signal);
        delivered = true;
      } catch (error) {
        if (!(error && typeof error === "object" && error.code === "ESRCH")) throw error;
      }
    }
    if (!ended()) {
      try {
        child.kill(signal);
        delivered = true;
      } catch (error) {
        if (!(error && typeof error === "object" && error.code === "ESRCH")) throw error;
      }
    }
    return delivered;
  };
  deliver("SIGTERM");
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (ended()) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  deliver("SIGKILL");
  for (let attempt = 0; attempt < 20; attempt += 1) {
    if (ended()) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error("The local fixture process did not exit after termination.");
}

/** Builds the release binary once per harness process, so every fixture launches the candidate tree. */
async function hiveBinary() {
  if (process.env.HIVE_BINARY) return process.env.HIVE_BINARY;
  if (localBinary) return localBinary;
  localBinary = new Promise((resolve, reject) => {
    const build = spawn("cargo", ["build", "--release", "--quiet", "--bin", "hive"], { stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    build.stdout.on("data", (chunk) => { output += chunk; });
    build.stderr.on("data", (chunk) => { output += chunk; });
    build.once("error", (error) => reject(new Error("The local release build could not start: " + error.message)));
    build.once("close", (code) => {
      const binary = join(process.cwd(), "target", "release", process.platform === "win32" ? "hive.exe" : "hive");
      if (code !== 0) { reject(new Error("The local release build failed with exit code " + code + ": " + output)); return; }
      if (!existsSync(binary)) { reject(new Error("The local release build did not produce " + binary + ".")); return; }
      resolve(binary);
    });
  });
  try { return await localBinary; }
  catch (error) { localBinary = undefined; throw error; }
}

async function startHiveFixture(subcommand, environment) {
  const child = spawn(await hiveBinary(), [subcommand], {
    env: environment,
    detached: process.platform !== "win32",
    stdio: ["ignore", "pipe", "pipe"]
  });
  let output = "";
  let spawnFailure;
  // HIVE_FIXTURE_LOG names a file that receives every fixture's output, for diagnosing a failed check.
  const record = (chunk) => { output += chunk; if (process.env.HIVE_FIXTURE_LOG) appendFileSync(process.env.HIVE_FIXTURE_LOG, "[" + subcommand + "] " + chunk); };
  child.stdout.on("data", record);
  child.stderr.on("data", record);
  child.once("error", (error) => { spawnFailure = error; });
  return { child, output: () => output, failure: () => spawnFailure };
}

export async function startLocalService(port, databaseName = "hive", additionalEnvironment = {}) {
  const signingSecret = randomBytes(32).toString("base64url");
  const requireEvaluationWorker = additionalEnvironment.HIVE_REQUIRE_EVALUATION_WORKER === "true";
  const signFixtureSession = (principalId) =>
    principalId + "." + createHmac("sha256", signingSecret).update(principalId).digest("hex");
  const fixture = await startHiveFixture("serve", {
    ...process.env, ...databaseEnvironment(postgresPort(), signingSecret, databaseName), ...additionalEnvironment, HIVE_PORT: String(port)
  });
  const { child } = fixture;
  const stop = () => stopLocalProcess(child);
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (fixture.failure()) { await stop(); throw new Error("The local service could not start: " + fixture.failure().message + ": " + fixture.output()); }
    if (child.exitCode !== null) {
      await stop();
      throw new Error("The local service ended before health verification: " + fixture.output());
    }
    const readiness = new AbortController();
    const deadline = setTimeout(() => readiness.abort(), 250);
    try {
      const response = await fetch("http://127.0.0.1:" + port + "/graphql", {
        method: "POST",
        signal: readiness.signal,
        headers: {
          "Content-Type": "application/json",
          Cookie: "sf_session=" + signFixtureSession("00000000-0000-0000-0000-000000000001")
        },
        body: JSON.stringify({ query: 'query LocalServiceReadiness { __typename evaluationDefinition(definitionId: "00000000-0000-0000-0000-000000000000") { id } }' })
      });
      const payload = response.ok ? await response.json() : null;
      if (payload?.data?.__typename === "Query" && Object.hasOwn(payload.data, "evaluationDefinition")) {
        const evaluationWorker = requireEvaluationWorker ? await startLocalEvaluationWorker(databaseName, additionalEnvironment) : undefined;
        return {
          signFixtureSession,
          output: fixture.output,
          stop: async () => { if (evaluationWorker) await evaluationWorker.stop(); await stop(); }
        };
      }
    } catch {
      // The service has not bound its port or accepted the fixture session yet.
    } finally {
      clearTimeout(deadline);
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  await stop();
  throw new Error("The local service did not become healthy: " + fixture.output());
}

/** Retries a released loopback reservation when another local process wins the bind race. */
export async function startIsolatedLocalService(databaseName = "hive", additionalEnvironment = {}) {
  let lastFailure;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const reservation = await reserveLocalPort();
    try {
      await reservation.release();
      const service = await startLocalService(reservation.port, databaseName, additionalEnvironment);
      return { ...service, port: reservation.port };
    } catch (error) {
      lastFailure = error;
      try { await reservation.release(); } catch { /* The successful release closed the reservation. */ }
      if (!String(error?.message ?? error).includes("Address already in use")) throw error;
    }
  }
  throw lastFailure;
}

export async function startLocalDeploymentWorker(databaseName = "hive", additionalEnvironment = {}) {
  const fixture = await startHiveFixture("deployment-worker", {
    ...process.env, ...databaseEnvironment(postgresPort(), randomBytes(32).toString("base64url"), databaseName), ...additionalEnvironment
  });
  const { child } = fixture;
  const stop = () => stopLocalProcess(child);
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (fixture.failure()) { await stop(); throw new Error("The local deployment worker could not start: " + fixture.failure().message + ": " + fixture.output()); }
    if (child.exitCode !== null) throw new Error("The local deployment worker ended before readiness verification: " + fixture.output());
    try {
      if (fixture.output().includes("component=local-deployment-worker event=started")) return { stop };
    } catch {
      // The worker has not completed migration startup yet.
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  await stop();
  throw new Error("The local deployment worker did not become ready: " + fixture.output());
}

export async function startLocalEvaluationWorker(databaseName = "hive", additionalEnvironment = {}, observedAfter = undefined) {
  const workerId = additionalEnvironment.HIVE_EVALUATION_WORKER_ID || process.env.HIVE_EVALUATION_WORKER_ID || "local-evaluation-worker";
  const fixture = await startHiveFixture("evaluation-worker", {
    ...process.env, ...databaseEnvironment(postgresPort(), randomBytes(32).toString("base64url"), databaseName), ...additionalEnvironment
  });
  const { child } = fixture;
  const stop = () => stopLocalProcess(child);
  let client;
  try {
    client = await postgresClient(databaseName);
    for (let attempt = 0; attempt < 100; attempt += 1) {
      if (fixture.failure()) { await stop(); throw new Error("The local evaluation worker could not start: " + fixture.failure().message + ": " + fixture.output()); }
      if (child.exitCode !== null) throw new Error("The local evaluation worker ended before readiness verification: " + fixture.output());
      try {
        const heartbeat = await client.query("SELECT status, observed_at >= CURRENT_TIMESTAMP - INTERVAL '30 seconds' AS current, ($2::timestamptz IS NULL OR observed_at > $2::timestamptz) AS observed_after FROM evaluation_worker_heartbeats WHERE worker_id = $1", [workerId, observedAfter ?? null]);
        if (heartbeat.rows[0]?.status === "READY" && heartbeat.rows[0]?.current && heartbeat.rows[0]?.observed_after) return { stop };
      } catch {
        // Migration startup has not created the heartbeat relation yet.
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    await stop();
    throw new Error("The local evaluation worker did not write a current READY heartbeat: " + fixture.output());
  } finally {
    if (client) await client.end();
  }
}
